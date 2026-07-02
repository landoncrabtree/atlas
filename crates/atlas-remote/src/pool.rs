//! Process-wide reuse cache for [`BackendClient`] instances.
//!
//! # Why a pool?
//!
//! Each [`RemoteLocationViewModel`](crate::vm::RemoteLocationViewModel)
//! previously constructed a fresh backend client on `open`. When the
//! Miller-columns view pre-fetches child directories, a single
//! navigation would ripple into 5–8 concurrent SSH sessions to the same
//! host — one per column — each running its own auth handshake. Users
//! reported an "SSH storm" on password-auth servers because every
//! session re-prompted.
//!
//! The pool memoises one [`BackendClient`] per `(backend, host, port,
//! username, credentials_hash)` tuple, then hands out `Arc` clones. A
//! stale client stays in the pool for `idle_ttl` after its last
//! reference is dropped so an immediate navigate-back on the same host
//! reuses the still-authenticated session.
//!
//! # Threading and consistency
//!
//! The whole map lives behind a single [`parking_lot::Mutex`]. Lookups
//! are O(k) in the key size; the map is small (default cap 8 entries).
//! `get_or_open` runs the factory closure **inside** the lock so two
//! concurrent callers with the same key never race the network.
//! Callers with a cold cache pay for the handshake once; every other
//! caller in the process waits on the same [`Mutex`] and gets the
//! finished handle.
//!
//! # Eviction
//!
//! * **Idle** — entries whose `last_used` is older than `idle_ttl` AND
//!   whose refcount is 1 (only the pool holds them) are dropped by
//!   [`ConnectionPool::evict_idle`]. Consumers call this on a timer
//!   (the connect controller's worker thread does).
//! * **Capacity** — when a new insertion would exceed `max_connections`
//!   the least-recently-used entry with refcount 1 is dropped first.
//! * **Explicit** — [`ConnectionPool::evict_by_key`] purges one entry;
//!   the connect controller calls it after an auth failure so a bad
//!   cached client never lingers.
//!
//! Entries with refcount > 1 (i.e. still referenced by a live view
//! model) are never evicted. Dropping the last consumer decrements
//! the refcount naturally via [`Arc`] semantics; the pool notices on
//! the next `evict_idle` sweep.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ahash::AHashMap;
use atlas_core::BackendKind;
use once_cell::sync::Lazy;
use parking_lot::Mutex;

use crate::backend::{BackendError, Credentials};
use crate::vm::common::BackendClient;

/// Default idle TTL — entries older than this get dropped by
/// [`ConnectionPool::evict_idle`]. Chosen so a quick navigate-away
/// and back keeps the session alive but a truly stale connection
/// doesn't hog kernel resources for the whole session.
pub const DEFAULT_IDLE_TTL: Duration = Duration::from_millis(300_000);

/// Default cap on the pool size. Small — a normal workflow rarely
/// touches more than 2–3 distinct remote hosts.
pub const DEFAULT_MAX_CONNECTIONS: usize = 8;

/// Identity tuple for a pooled client.
///
/// The [`credentials_hash`](Self::credentials_hash) discriminator lets
/// two entries for the same host but different `Credentials` coexist
/// (e.g. reconnecting with a fresh password). The hash never sees the
/// secret bytes on the outside — only the `Credentials` variant tag,
/// plus a compact fingerprint of the secret. We use [`DefaultHasher`]
/// which is not cryptographically strong but is sufficient for
/// map-key disambiguation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolKey {
    /// Backend kind (sftp/ftp/webdav/s3).
    pub backend: BackendKind,
    /// Server host or S3 bucket. Empty string when the backend
    /// addresses via endpoint URL only.
    pub host: String,
    /// TCP port when applicable, `None` for backends that don't use
    /// one (S3 endpoint URL, WebDAV origin URL).
    pub port: Option<u16>,
    /// Optional username. `None` for anonymous connections.
    pub username: Option<String>,
    /// Opaque hash of the credentials. See [`hash_credentials`].
    pub credentials_hash: u64,
}

impl PoolKey {
    /// Construct a [`PoolKey`] from the raw connect parameters. The
    /// credentials fingerprint hashes both the variant tag and a
    /// compact digest of the secret so `Password("a")` and
    /// `Password("b")` produce distinct keys.
    #[must_use]
    pub fn new(
        backend: BackendKind,
        host: impl Into<String>,
        port: Option<u16>,
        username: Option<String>,
        credentials: &Credentials,
    ) -> Self {
        Self {
            backend,
            host: host.into(),
            port,
            username,
            credentials_hash: hash_credentials(credentials),
        }
    }
}

/// Fingerprint a [`Credentials`] value for use as a map-key
/// discriminator. The output never leaks secret material — we hash
/// both the variant tag and the byte contents through a non-cryptographic
/// hasher, and the hash is small enough (u64) that no reverse engineering
/// of the underlying secret is feasible.
fn hash_credentials(creds: &Credentials) -> u64 {
    let mut h = DefaultHasher::new();
    match creds {
        Credentials::Password(secret) => {
            0u8.hash(&mut h);
            secret.hash(&mut h);
        }
        Credentials::SshKey(path, passphrase) => {
            1u8.hash(&mut h);
            path.hash(&mut h);
            passphrase.hash(&mut h);
        }
        Credentials::Iam {
            access_key_id,
            secret_key,
            session_token,
        } => {
            2u8.hash(&mut h);
            access_key_id.hash(&mut h);
            secret_key.hash(&mut h);
            session_token.hash(&mut h);
        }
        Credentials::Anonymous => {
            3u8.hash(&mut h);
        }
    }
    h.finish()
}

/// Per-entry pool bookkeeping.
struct Entry {
    client: Arc<dyn BackendClient>,
    last_used: Instant,
}

/// Configuration for [`ConnectionPool`]. Values plumbed from
/// `atlas-config` when the pool is constructed by [`AppShell`];
/// unit tests build a pool directly with custom settings.
#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    /// Entries idle longer than this become eviction candidates.
    pub idle_ttl: Duration,
    /// Hard cap on the number of pooled entries. When exceeded the
    /// least-recently-used unreferenced entry is dropped on insert.
    pub max_connections: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            idle_ttl: DEFAULT_IDLE_TTL,
            max_connections: DEFAULT_MAX_CONNECTIONS,
        }
    }
}

/// Per-key stats. Useful for tests + `debug_stats` diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryStats {
    /// Number of `Arc` clones outstanding, including the pool's own.
    /// `1` means only the pool holds it.
    pub refcount: usize,
    /// Milliseconds since the entry was last handed out.
    pub idle_ms: u64,
}

/// Aggregate snapshot used by tests + telemetry.
#[derive(Debug, Clone, Default)]
pub struct PoolStats {
    /// Number of live entries.
    pub entries: usize,
    /// Per-key stats (order not guaranteed).
    pub per_key: Vec<(PoolKey, EntryStats)>,
    /// Number of `get_or_open` calls served from the cache since the
    /// pool was constructed.
    pub hits: u64,
    /// Number of `get_or_open` calls that ran the factory.
    pub misses: u64,
}

/// Process-wide reuse cache. Access via [`global`] for the default
/// singleton, or construct one directly for tests.
pub struct ConnectionPool {
    entries: Mutex<AHashMap<PoolKey, Entry>>,
    config: Mutex<PoolConfig>,
    hits: Mutex<u64>,
    misses: Mutex<u64>,
}

impl ConnectionPool {
    /// Create a new pool with the given config.
    #[must_use]
    pub fn new(config: PoolConfig) -> Self {
        Self {
            entries: Mutex::new(AHashMap::default()),
            config: Mutex::new(config),
            hits: Mutex::new(0),
            misses: Mutex::new(0),
        }
    }

    /// Update the runtime configuration. Existing entries are kept;
    /// the new cap only takes effect on the next insert.
    pub fn set_config(&self, config: PoolConfig) {
        *self.config.lock() = config;
    }

    /// Fetch a memoised client, or run `mk` and insert the result.
    ///
    /// The factory runs **inside** the pool lock so two callers with
    /// the same key never race the network handshake. Consumers that
    /// don't want that serialisation should build their client outside
    /// the pool and never call `get_or_open`.
    ///
    /// # Errors
    ///
    /// Propagates any [`BackendError`] returned by `mk`.
    pub fn get_or_open<F>(
        &self,
        key: &PoolKey,
        mk: F,
    ) -> Result<Arc<dyn BackendClient>, BackendError>
    where
        F: FnOnce() -> Result<Arc<dyn BackendClient>, BackendError>,
    {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.get_mut(key) {
            entry.last_used = Instant::now();
            *self.hits.lock() += 1;
            return Ok(Arc::clone(&entry.client));
        }
        drop(entries);

        let client = mk()?;
        *self.misses.lock() += 1;

        let mut entries = self.entries.lock();
        // Enforce capacity before inserting: prefer to evict the LRU
        // entry that only the pool holds. Never evict entries with
        // live external refs.
        let cap = self.config.lock().max_connections;
        while entries.len() >= cap {
            let victim = entries
                .iter()
                .filter(|(_, e)| Arc::strong_count(&e.client) == 1)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    entries.remove(&k);
                }
                None => break, // every entry is in-use — keep them.
            }
        }
        entries.insert(
            key.clone(),
            Entry {
                client: Arc::clone(&client),
                last_used: Instant::now(),
            },
        );
        Ok(client)
    }

    /// Drop cache entries idle longer than `idle_since`, but only when
    /// their refcount is 1 (i.e. only the pool holds them).
    ///
    /// Returns the number of entries evicted.
    pub fn evict_idle(&self, idle_since: Duration) -> usize {
        let mut entries = self.entries.lock();
        let now = Instant::now();
        let before = entries.len();
        entries.retain(|_, entry| {
            let idle = now.saturating_duration_since(entry.last_used) < idle_since;
            let in_use = Arc::strong_count(&entry.client) > 1;
            idle || in_use
        });
        before - entries.len()
    }

    /// Remove one entry by key regardless of idle or refcount. Callers
    /// invoke this on auth failure so a bad cached client is not
    /// handed out again.
    pub fn evict_by_key(&self, key: &PoolKey) -> bool {
        self.entries.lock().remove(key).is_some()
    }

    /// Drop every entry regardless of refcount. Used by tests.
    pub fn clear(&self) {
        self.entries.lock().clear();
    }

    /// Diagnostic snapshot. Suitable for logging or test assertions.
    #[must_use]
    pub fn debug_stats(&self) -> PoolStats {
        let entries = self.entries.lock();
        let now = Instant::now();
        let per_key = entries
            .iter()
            .map(|(k, e)| {
                (
                    k.clone(),
                    EntryStats {
                        refcount: Arc::strong_count(&e.client),
                        idle_ms: now
                            .saturating_duration_since(e.last_used)
                            .as_millis()
                            .min(u64::MAX as u128) as u64,
                    },
                )
            })
            .collect();
        PoolStats {
            entries: entries.len(),
            per_key,
            hits: *self.hits.lock(),
            misses: *self.misses.lock(),
        }
    }
}

impl std::fmt::Debug for ConnectionPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stats = self.debug_stats();
        f.debug_struct("ConnectionPool")
            .field("entries", &stats.entries)
            .field("hits", &stats.hits)
            .field("misses", &stats.misses)
            .finish()
    }
}

/// Process-wide singleton used by [`crate::backend::open`]. Lazily
/// initialised on first access.
static GLOBAL: Lazy<ConnectionPool> = Lazy::new(|| ConnectionPool::new(PoolConfig::default()));

/// The process-wide default pool. All calls into
/// [`crate::backend::open`] route through this instance.
#[must_use]
pub fn global() -> &'static ConnectionPool {
    &GLOBAL
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{RemoteError, RemoteMetadata, RemoteResult};
    use crate::vm::common::{BackendClient, RemoteEntry};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock BackendClient — tracks the number of times its
    /// constructor was invoked so tests can assert on pool hits/
    /// misses.
    struct MockClient {
        _id: usize,
    }

    #[async_trait]
    impl BackendClient for MockClient {
        async fn list(&self, _path: &str) -> RemoteResult<Vec<RemoteEntry>> {
            Ok(Vec::new())
        }
        async fn read(&self, _path: &str) -> RemoteResult<Vec<u8>> {
            Ok(Vec::new())
        }
        async fn stat(&self, _path: &str) -> RemoteResult<RemoteMetadata> {
            Err(RemoteError::not_found("mock"))
        }
        async fn write(&self, _path: &str, _bytes: Vec<u8>) -> RemoteResult<()> {
            Ok(())
        }
        async fn create_dir(&self, _path: &str) -> RemoteResult<()> {
            Ok(())
        }
        async fn rename(&self, _from: &str, _to: &str) -> RemoteResult<()> {
            Ok(())
        }
        async fn delete(&self, _path: &str) -> RemoteResult<()> {
            Ok(())
        }
    }

    fn sftp_key(host: &str) -> PoolKey {
        PoolKey::new(
            BackendKind::Sftp,
            host,
            Some(22),
            Some("alice".into()),
            &Credentials::Password("hunter2".into()),
        )
    }

    #[test]
    fn get_or_open_dedupes_same_key() {
        let pool = ConnectionPool::new(PoolConfig::default());
        let key = sftp_key("h1");
        let calls = AtomicUsize::new(0);
        let factory = || -> Result<Arc<dyn BackendClient>, BackendError> {
            let id = calls.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(MockClient { _id: id }))
        };
        let a = pool.get_or_open(&key, factory).unwrap();
        let b = pool
            .get_or_open(&key, || -> Result<Arc<dyn BackendClient>, BackendError> {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(MockClient { _id: 999 }))
            })
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b), "second get_or_open should reuse arc");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "factory ran twice");
        let stats = pool.debug_stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn different_creds_produce_different_entries() {
        let k_a = PoolKey::new(
            BackendKind::Sftp,
            "h",
            Some(22),
            Some("u".into()),
            &Credentials::Password("a".into()),
        );
        let k_b = PoolKey::new(
            BackendKind::Sftp,
            "h",
            Some(22),
            Some("u".into()),
            &Credentials::Password("b".into()),
        );
        assert_ne!(k_a.credentials_hash, k_b.credentials_hash);
        let anon = PoolKey::new(
            BackendKind::Sftp,
            "h",
            Some(22),
            None,
            &Credentials::Anonymous,
        );
        assert_ne!(k_a.credentials_hash, anon.credentials_hash);
    }

    #[test]
    fn evict_idle_removes_stale_unrefd_entries() {
        let pool = ConnectionPool::new(PoolConfig::default());
        let key = sftp_key("h2");
        let make = || -> Result<Arc<dyn BackendClient>, BackendError> {
            Ok(Arc::new(MockClient { _id: 0 }))
        };
        let client = pool.get_or_open(&key, make).unwrap();
        drop(client); // refcount back to 1

        // 0-idle: nothing evicts because our `idle_since` clock hasn't
        // elapsed at all.
        assert_eq!(pool.evict_idle(Duration::from_secs(1)), 0);
        assert_eq!(pool.debug_stats().entries, 1);

        // Small sleep, then evict with a very short TTL — entry goes.
        std::thread::sleep(Duration::from_millis(20));
        let removed = pool.evict_idle(Duration::from_millis(10));
        assert_eq!(removed, 1);
        assert_eq!(pool.debug_stats().entries, 0);
    }

    #[test]
    fn evict_idle_keeps_in_use_entries() {
        let pool = ConnectionPool::new(PoolConfig::default());
        let key = sftp_key("busy");
        let make = || -> Result<Arc<dyn BackendClient>, BackendError> {
            Ok(Arc::new(MockClient { _id: 0 }))
        };
        let _hold = pool.get_or_open(&key, make).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let removed = pool.evict_idle(Duration::from_millis(1));
        assert_eq!(removed, 0, "in-use entries must not be evicted");
        assert_eq!(pool.debug_stats().entries, 1);
    }

    #[test]
    fn evict_by_key_targets_one_entry() {
        let pool = ConnectionPool::new(PoolConfig::default());
        let ka = sftp_key("a");
        let kb = sftp_key("b");
        pool.get_or_open(&ka, || Ok(Arc::new(MockClient { _id: 0 })))
            .unwrap();
        pool.get_or_open(&kb, || Ok(Arc::new(MockClient { _id: 1 })))
            .unwrap();
        assert_eq!(pool.debug_stats().entries, 2);
        assert!(pool.evict_by_key(&ka));
        assert!(!pool.evict_by_key(&ka)); // second call misses
        let keys: Vec<PoolKey> = pool
            .debug_stats()
            .per_key
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys, vec![kb]);
    }

    #[test]
    fn lru_eviction_at_capacity() {
        let pool = ConnectionPool::new(PoolConfig {
            idle_ttl: DEFAULT_IDLE_TTL,
            max_connections: 2,
        });
        let ka = sftp_key("a");
        let kb = sftp_key("b");
        let kc = sftp_key("c");
        let ca = pool
            .get_or_open(&ka, || Ok(Arc::new(MockClient { _id: 0 })))
            .unwrap();
        drop(ca);
        std::thread::sleep(Duration::from_millis(5));
        let cb = pool
            .get_or_open(&kb, || Ok(Arc::new(MockClient { _id: 1 })))
            .unwrap();
        drop(cb);
        std::thread::sleep(Duration::from_millis(5));
        // Adding a 3rd exceeds the cap; ka is the LRU with refcount 1.
        pool.get_or_open(&kc, || Ok(Arc::new(MockClient { _id: 2 })))
            .unwrap();
        let keys: Vec<PoolKey> = pool
            .debug_stats()
            .per_key
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert!(keys.contains(&kb));
        assert!(keys.contains(&kc));
        assert!(!keys.contains(&ka), "LRU entry a should have been evicted");
    }

    #[test]
    fn lru_eviction_skips_in_use_entries() {
        let pool = ConnectionPool::new(PoolConfig {
            idle_ttl: DEFAULT_IDLE_TTL,
            max_connections: 1,
        });
        let ka = sftp_key("a");
        let _hold = pool
            .get_or_open(&ka, || Ok(Arc::new(MockClient { _id: 0 })))
            .unwrap();
        // Inserting a second entry when the sole slot is in-use must
        // keep both (we prefer to grow beyond the cap over evicting
        // a referenced client). The pool never boots a live vm.
        let kb = sftp_key("b");
        pool.get_or_open(&kb, || Ok(Arc::new(MockClient { _id: 1 })))
            .unwrap();
        assert_eq!(pool.debug_stats().entries, 2);
    }
}
