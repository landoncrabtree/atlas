//! Preview cache for remote file open.
//!
//! When the user activates (double-click / Enter / view action) a file
//! entry on a remote pane, `fs::View` cannot ship the bare
//! backend-relative `PathBuf` off to `open::that` — the OS default
//! handler doesn't speak SFTP/S3/WebDAV/FTP. Instead we:
//!
//! 1. Compute a stable cache key from `(uri, mtime, size)`; a change
//!    in either backend metadata invalidates the cache line.
//! 2. Consult `<cache_dir>/<key>/<name>`. On a hit, hand the local
//!    copy off to `open::that` synchronously.
//! 3. On a miss, spawn a download on the shared `atlas_remote::runtime`
//!    handle: open a live `RemoteLocationViewModel` at the URI's
//!    parent, read the file bytes, write atomically to the cache
//!    line, then call `open::that`.
//!
//! # Cross-cutting policy
//!
//! * The cap on cached bytes and stale-preview age is configured via
//!   `[remote.preview]` in `~/.config/atlas/config.toml`. See
//!   [`atlas_config::RemotePreview`].
//! * Files larger than `max_open_bytes` are not previewed — the shell
//!   logs a hint suggesting a local-pane copy first.
//! * A background thread trims the cache after every successful
//!   download so the on-disk footprint stays bounded.
//! * The cache never leaves the local disk; opened files are not
//!   deleted after the OS handler closes so a second view of the same
//!   file is instant.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use atlas_config::RemotePreview;
use atlas_core::{BackendKind, RemoteUri};
use atlas_fs::{Entry, OpenOptions};
use atlas_remote::RemoteLocationViewModel;
use directories::ProjectDirs;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};

/// Trait used to open a materialised local path with the OS default
/// handler. Production code always uses the real [`open::that`]
/// implementation; tests substitute a recording double.
///
/// Cheap to clone because implementations are always small structs
/// (`RealOpener` is a ZST; tests use `Arc<...>`).
pub trait OpenHandler: Send + Sync {
    /// Hand `path` to the OS default handler. Blocks until the
    /// handler process is spawned; behaviour on close is up to the
    /// OS.
    ///
    /// # Errors
    ///
    /// Any error surfaced by the platform's default-open crate.
    fn open(&self, path: &Path) -> io::Result<()>;
}

/// Production opener that delegates to the `open` crate.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealOpener;

impl OpenHandler for RealOpener {
    fn open(&self, path: &Path) -> io::Result<()> {
        open::that(path).map_err(|e| io::Error::other(format!("{e}")))
    }
}

/// Shared handle to the preview cache. Cheap to clone via
/// `Arc::clone`.
pub struct PreviewCache {
    inner: Arc<Inner>,
}

impl Clone for PreviewCache {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct Inner {
    config: Mutex<RemotePreview>,
    cache_dir: Mutex<Option<PathBuf>>,
    opener: Arc<dyn OpenHandler>,
    // A crude counter of downloads served — the tests use this to
    // assert that a second open on the same file is a cache hit.
    downloads: parking_lot::Mutex<u64>,
    /// Write-back watcher; registers each successfully-opened file
    /// so subsequent local edits are uploaded back to the remote.
    watch_registry: super::preview_watch::PreviewWatchRegistry,
    /// Optional handle used to surface long-running download
    /// progress in the ops panel. When `None`, downloads still
    /// work — they just don't emit rows (matches the pre-progress
    /// behaviour). Populated once via
    /// [`PreviewCache::attach_ops_controller`].
    ops_controller: parking_lot::Mutex<Option<std::sync::Weak<crate::ops::OpsController>>>,
}

impl PreviewCache {
    /// Build a cache backed by the platform default `open::that`
    /// handler.
    #[must_use]
    pub fn new(config: RemotePreview) -> Self {
        Self::with_opener(config, Arc::new(RealOpener))
    }

    /// Build a cache with a caller-supplied [`OpenHandler`]. Tests
    /// use this to substitute a recording double for `open::that`.
    #[must_use]
    pub fn with_opener(config: RemotePreview, opener: Arc<dyn OpenHandler>) -> Self {
        let watch_registry = super::preview_watch::PreviewWatchRegistry::new(
            config.write_back_enabled,
            config.write_back_debounce_ms,
        );
        Self {
            inner: Arc::new(Inner {
                config: Mutex::new(config),
                cache_dir: Mutex::new(None),
                opener,
                downloads: parking_lot::Mutex::new(0),
                watch_registry,
                ops_controller: parking_lot::Mutex::new(None),
            }),
        }
    }

    /// Wire the shell's [`OpsController`] into the preview cache so
    /// large downloads surface as ops-panel rows with progress and a
    /// cancel button. Called once during shell startup after both
    /// controllers exist.
    pub fn attach_ops_controller(&self, ops: std::sync::Weak<crate::ops::OpsController>) {
        *self.inner.ops_controller.lock() = Some(ops);
    }

    /// Access the write-back registry so the shell can install a
    /// notification sink (typically a status-bar toast pusher).
    #[must_use]
    pub fn watch_registry(&self) -> &super::preview_watch::PreviewWatchRegistry {
        &self.inner.watch_registry
    }

    /// Overwrite the runtime-tunable preview config. Called by the
    /// config hot-reload watcher.
    pub fn set_config(&self, config: RemotePreview) {
        self.inner
            .watch_registry
            .set_config(config.write_back_enabled, config.write_back_debounce_ms);
        *self.inner.config.lock() = config;
    }

    /// Return a snapshot of the current config.
    #[must_use]
    pub fn config(&self) -> RemotePreview {
        self.inner.config.lock().clone()
    }

    /// Compute (once, then cache) the effective on-disk cache
    /// directory. Falls back to `<system_temp>/atlas-preview` when
    /// [`ProjectDirs`] can't resolve a home (`$HOME` unset) — never
    /// panics.
    fn resolve_cache_dir(&self) -> PathBuf {
        if let Some(dir) = self.inner.cache_dir.lock().clone() {
            return dir;
        }
        let override_dir = self.inner.config.lock().cache_dir.clone();
        let resolved = override_dir.unwrap_or_else(|| {
            ProjectDirs::from("dev", "atlas", "atlas")
                .map(|d| d.cache_dir().join("preview"))
                .unwrap_or_else(|| std::env::temp_dir().join("atlas-preview"))
        });
        *self.inner.cache_dir.lock() = Some(resolved.clone());
        resolved
    }

    /// Total number of downloads served since this cache was
    /// instantiated. Tests use this to assert that a second open of
    /// the same file is a cache hit.
    #[must_use]
    pub fn download_count(&self) -> u64 {
        *self.inner.downloads.lock()
    }

    /// Try to preview a remote file. The dispatch is:
    ///
    /// 1. If the file exceeds `max_open_bytes`, log + refuse (the
    ///    shell shows a hint chip).
    /// 2. Compute the cache key from `(uri, mtime, size)`.
    /// 3. Cache hit → `open::that(cached)` synchronously.
    /// 4. Cache miss → spawn a download on
    ///    [`atlas_remote::runtime::handle`]; call `open::that` once
    ///    the write completes.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewError::TooLarge`] when the file exceeds the
    /// configured cap; all other failures (network, disk) are
    /// surfaced via `tracing::warn` from the spawned task rather than
    /// bubbling synchronously, because the caller (a Slint callback)
    /// has no place to render them.
    pub fn open_remote_file(
        &self,
        uri: RemoteUri,
        kind: BackendKind,
        entry: Entry,
    ) -> PreviewOutcome {
        let cfg = self.config();
        if entry.metadata.size > cfg.max_open_bytes {
            tracing::info!(
                path = %uri.path,
                size = entry.metadata.size,
                cap = cfg.max_open_bytes,
                "preview: file exceeds max_open_bytes; refusing"
            );
            return PreviewOutcome::TooLarge {
                size: entry.metadata.size,
                cap: cfg.max_open_bytes,
            };
        }

        let key = cache_key(&uri, entry.metadata.modified, entry.metadata.size);
        let cache_line = self.resolve_cache_dir().join(&key);
        let cached_file = cache_line.join(&entry.name);

        // Cache hit fast path.
        if cached_file.exists() && !is_stale(&cached_file, cfg.max_age_secs) {
            tracing::debug!(?cached_file, "preview: cache hit");
            match self.inner.opener.open(&cached_file) {
                Ok(()) => {
                    if let Err(err) =
                        self.inner
                            .watch_registry
                            .register(cached_file.clone(), uri.clone(), kind)
                    {
                        tracing::warn!(?cached_file, %err, "preview: watch register failed");
                    }
                    return PreviewOutcome::CachedOpen(cached_file);
                }
                Err(err) => {
                    tracing::warn!(?cached_file, %err, "preview: OS open failed on cached file");
                    return PreviewOutcome::OpenFailed(err.to_string());
                }
            }
        }

        // Cache miss — spawn the download.
        let inner = Arc::clone(&self.inner);
        let handle = atlas_remote::runtime::handle();
        let uri_for_watch = uri.clone();
        let kind_for_watch = kind;
        handle.spawn(async move {
            match download_and_open(&inner, uri, kind, entry, cache_line.clone(), cached_file).await
            {
                Ok(path) => {
                    tracing::info!(?path, "preview: download + open complete");
                    if let Err(err) =
                        inner
                            .watch_registry
                            .register(path.clone(), uri_for_watch, kind_for_watch)
                    {
                        tracing::warn!(?path, %err, "preview: watch register failed after download");
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, "preview: download failed");
                }
            }
        });

        PreviewOutcome::Downloading
    }

    /// Test-only helper: return the cache path for a given URI + entry
    /// without triggering a download.
    #[cfg(test)]
    #[must_use]
    pub fn cache_path_for(&self, uri: &RemoteUri, entry: &Entry) -> PathBuf {
        let key = cache_key(uri, entry.metadata.modified, entry.metadata.size);
        self.resolve_cache_dir().join(key).join(&entry.name)
    }

    /// Test-only helper that mimics the tail of a successful download:
    /// writes `bytes` to the cache line for `(uri, entry)` atomically,
    /// bumps the download counter, runs LRU eviction, and invokes the
    /// [`OpenHandler`]. Callers avoid the tokio-driven remote read but
    /// exercise every subsequent surface — atomic rename, opener
    /// dispatch, eviction — so [`open_remote_file`] on the next call
    /// takes the cache-hit fast path.
    #[cfg(test)]
    pub fn stage_bytes_for_test(
        &self,
        uri: &RemoteUri,
        entry: &Entry,
        bytes: &[u8],
    ) -> io::Result<PathBuf> {
        let key = cache_key(uri, entry.metadata.modified, entry.metadata.size);
        let cache_line = self.resolve_cache_dir().join(&key);
        std::fs::create_dir_all(&cache_line)?;
        let cached_file = cache_line.join(&entry.name);
        let staging = cache_line.join(format!(".{}.part", &entry.name));
        std::fs::write(&staging, bytes)?;
        std::fs::rename(&staging, &cached_file)?;
        {
            let mut n = self.inner.downloads.lock();
            *n = n.saturating_add(1);
        }
        let _ = evict_lru(
            &guess_cache_root(&cache_line),
            self.inner.config.lock().max_bytes,
        );
        self.inner.opener.open(&cached_file)?;
        Ok(cached_file)
    }
}

/// Result of a preview attempt.
#[derive(Debug)]
pub enum PreviewOutcome {
    /// Cache hit — the local copy was handed to the OS handler
    /// synchronously.
    CachedOpen(PathBuf),
    /// Cache miss — a background task is fetching the file and will
    /// call `open::that` on completion.
    Downloading,
    /// File exceeds `max_open_bytes`; the caller should surface a
    /// "copy to local first" hint.
    TooLarge { size: u64, cap: u64 },
    /// Cache-hit `open::that` returned an error; the caller may
    /// display a toast.
    OpenFailed(String),
}

/// Errors returned by the preview cache (mostly for tests today; the
/// production path logs and swallows).
#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    /// The remote file is larger than `max_open_bytes`.
    #[error("remote file too large to preview: {size} bytes (cap {cap} bytes)")]
    TooLarge {
        /// Reported size of the file.
        size: u64,
        /// Configured cap.
        cap: u64,
    },
    /// Any I/O error surfaced by the download / write pipeline.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// Any remote-side error surfaced by the download.
    #[error("{0}")]
    Remote(String),
    /// The URI did not carry a filename component.
    #[error("remote URI has no filename component: {0}")]
    NoFilename(String),
}

async fn download_and_open(
    inner: &Arc<Inner>,
    uri: RemoteUri,
    kind: BackendKind,
    entry: Entry,
    cache_line: PathBuf,
    cached_file: PathBuf,
) -> Result<PathBuf, PreviewError> {
    let credentials = atlas_ops::credentials_for(&uri)
        .map_err(|err| PreviewError::Remote(format!("credentials: {err}")))?;

    // Open a VM at the entry's remote *parent*, then call `read` on
    // the child name — this mirrors `atlas_ops::remote::open_remote`
    // and side-steps the "double-prepend URI path" edge case in the
    // backend's `abs()` helper.
    //
    // `open_live` uses the process-wide default `SftpOptions`. In
    // production that is `KnownHostsMode::Prompt` with no resolver,
    // which post-connect behaves identically to `Strict`: the pane's
    // initial connect (via `ConnectController`) accepted the host key
    // and persisted it to `known_hosts`, so subsequent handshakes
    // find it and short-circuit to Trusted. In integration tests the
    // mock harness installs an `AutoTrust` default that accepts the
    // paramiko ephemeral host key.
    let mut vm_uri = uri.clone();
    let child_path = uri.path.clone();
    vm_uri.path = "/".into();

    let vm = RemoteLocationViewModel::open_live(
        vm_uri.clone(),
        kind,
        credentials.clone(),
        OpenOptions::default(),
    )
    .map_err(|err| PreviewError::Remote(format!("open_live: {err}")))?;

    // Materialise the cache line atomically. `.part` write, then
    // rename — a partial download never masquerades as a cache hit.
    std::fs::create_dir_all(&cache_line)?;
    let staging = cache_line.join(format!(".{}.part", &entry.name));

    let (threshold, chunk_bytes) = {
        let cfg = inner.config.lock();
        (cfg.stream_threshold_bytes, cfg.stream_chunk_bytes)
    };

    if entry.metadata.size < threshold {
        // Small file — one buffered `read()` is the fast path. No
        // ops row: the download is short by definition, matching
        // the FOREGROUND_DEFER contract in ops::controller.
        let bytes = vm
            .read(&child_path)
            .await
            .map_err(|err| PreviewError::Remote(format!("read: {err}")))?;
        std::fs::write(&staging, &bytes)?;
    } else {
        // Large file — stream chunks straight to the `.part` file so
        // memory stays bounded at `stream_chunk_bytes`. Progress
        // events flow into the ops panel via `PreviewDownloadHandle`
        // when an ops controller is attached — see
        // `PreviewCache::attach_ops_controller`. Cache misses under
        // `stream_threshold_bytes` don't reach this branch, so
        // cache-hit / small-file paths stay silent by construction.
        let mut reader = vm
            .reader(&child_path, Some(entry.metadata.size))
            .await
            .map_err(|err| PreviewError::Remote(format!("reader: {err}")))?;
        let staging_owned = staging.clone();
        let file = tokio::task::spawn_blocking(move || std::fs::File::create(&staging_owned))
            .await
            .map_err(|err| PreviewError::Remote(format!("spawn: {err}")))??;
        let mut writer = futures::io::AllowStdIo::new(file);
        let chunk = usize::try_from(chunk_bytes).unwrap_or(usize::MAX).max(1);

        let ops_handle = inner
            .ops_controller
            .lock()
            .as_ref()
            .and_then(|weak| weak.upgrade())
            .map(|ops| {
                ops.start_preview_download(entry.name.clone(), uri.to_uri(), entry.metadata.size)
            });
        let progress_tx = ops_handle.as_ref().map(|h| h.progress_tx.clone());

        let copy_result = atlas_remote::stream::stream_copy(
            &mut reader,
            &mut writer,
            Some(chunk),
            Some(entry.metadata.size),
            progress_tx.as_ref(),
        )
        .await;
        // Explicit `drop` to release the Sender so the progress
        // bridge thread exits its `recv()` loop deterministically.
        drop(progress_tx);

        match copy_result {
            Ok(_) => {
                if let Some(handle) = ops_handle {
                    // Check for post-hoc cancellation. `stream_copy`
                    // doesn't currently short-circuit on the flag,
                    // so a "cancel just as it finished" race falls
                    // through to Completed. That's fine — the
                    // preview cache still lands the file.
                    if handle.is_cancelled() {
                        handle.cancelled();
                        // Roll back the .part file so we don't leave
                        // a half-committed cache line behind.
                        let staging_cleanup = staging.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            let _ = std::fs::remove_file(&staging_cleanup);
                        })
                        .await;
                        return Err(PreviewError::Io(io::Error::other(
                            "preview download cancelled",
                        )));
                    }
                    handle.complete();
                }
            }
            Err(err) => {
                if let Some(handle) = ops_handle {
                    handle.fail(err.to_string());
                }
                // Leave `.part` cleanup to the caller via
                // `Err(PreviewError::Io)` — the atomic rename to
                // `cached_file` below never happens on error, so
                // the partial download stays orphaned. Best-effort
                // cleanup here to avoid leaking bytes into the cache.
                let staging_cleanup = staging.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = std::fs::remove_file(&staging_cleanup);
                })
                .await;
                return Err(PreviewError::Io(io::Error::other(format!(
                    "stream_copy: {err}"
                ))));
            }
        }
    }
    std::fs::rename(&staging, &cached_file)?;

    // Record a successful download for tests + evict older entries.
    {
        let mut n = inner.downloads.lock();
        *n = n.saturating_add(1);
    }
    if let Err(err) = evict_lru(
        &guess_cache_root(&cache_line),
        inner.config.lock().max_bytes,
    ) {
        tracing::warn!(%err, "preview: LRU eviction failed");
    }

    inner.opener.open(&cached_file).map_err(PreviewError::Io)?;

    Ok(cached_file)
}

/// Return the cache root (the parent of a per-key cache line).
/// Falls back to the cache line itself if it has no parent — safe
/// no-op in that case, eviction won't run.
fn guess_cache_root(cache_line: &Path) -> PathBuf {
    cache_line.parent().unwrap_or(cache_line).to_path_buf()
}

/// Compute the cache key for `(uri, mtime, size)`. mtime is
/// serialised as seconds-since-epoch; when the backend didn't report
/// one the size is combined with the URI only — good enough for a
/// content-addressable preview cache.
fn cache_key(uri: &RemoteUri, modified: Option<SystemTime>, size: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(uri.to_uri().as_bytes());
    hasher.update(b":");
    if let Some(mtime) = modified {
        let secs = mtime
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        hasher.update(secs.to_le_bytes());
    } else {
        hasher.update(b"?");
    }
    hasher.update(b":");
    hasher.update(size.to_le_bytes());
    let out = hasher.finalize();
    hex_encode(&out)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Is the on-disk cache file older than `max_age_secs`?
fn is_stale(path: &Path, max_age_secs: u64) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return true;
    };
    let modified = meta.modified().unwrap_or_else(|_| SystemTime::now());
    modified
        .elapsed()
        .map(|d| d > Duration::from_secs(max_age_secs))
        .unwrap_or(false)
}

/// Best-effort LRU eviction: enumerate every file under `root`,
/// sort by last-accessed (falling back to modified), and drop the
/// oldest until the total footprint is under `max_bytes`.
fn evict_lru(root: &Path, max_bytes: u64) -> io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    let mut entries: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let mut total: u64 = 0;
    walk_files(root, &mut |path, meta| {
        let sz = meta.len();
        let ts = meta
            .accessed()
            .or_else(|_| meta.modified())
            .unwrap_or_else(|_| SystemTime::now());
        total = total.saturating_add(sz);
        entries.push((path.to_path_buf(), sz, ts));
    })?;
    if total <= max_bytes {
        return Ok(());
    }
    entries.sort_by_key(|(_, _, ts)| *ts);
    let mut to_free = total - max_bytes;
    for (path, sz, _) in entries {
        if to_free == 0 {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            to_free = to_free.saturating_sub(sz);
        }
    }
    Ok(())
}

fn walk_files(root: &Path, cb: &mut dyn FnMut(&Path, &std::fs::Metadata)) -> io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            let _ = walk_files(&path, cb);
        } else if meta.is_file() {
            cb(&path, &meta);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::UNIX_EPOCH;

    use atlas_core::RemoteUri;
    use atlas_fs::{Entry, EntryKind, Metadata};
    use tempfile::TempDir;

    use super::*;

    fn sample_uri(path: &str) -> RemoteUri {
        RemoteUri {
            scheme: "sftp".into(),
            host: Some("localhost".into()),
            port: Some(22),
            username: Some("me".into()),
            path: path.into(),
            credential_ref: None,
        }
    }

    fn sample_entry(name: &str, size: u64) -> Entry {
        Entry {
            path: PathBuf::from(name),
            name: name.into(),
            kind: EntryKind::File,
            metadata: Metadata {
                size,
                modified: Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
                ..Metadata::default()
            },
        }
    }

    #[derive(Default)]
    struct RecordingOpener {
        calls: AtomicU64,
        last_path: parking_lot::Mutex<Option<PathBuf>>,
    }

    impl OpenHandler for RecordingOpener {
        fn open(&self, path: &Path) -> io::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_path.lock() = Some(path.to_path_buf());
            Ok(())
        }
    }

    #[test]
    fn cache_key_is_stable_for_same_uri_mtime_size() {
        let uri = sample_uri("/pub/readme.txt");
        let ts = Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        let a = cache_key(&uri, ts, 42);
        let b = cache_key(&uri, ts, 42);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64, "sha-256 hex is 64 chars");
    }

    #[test]
    fn cache_key_changes_when_size_changes() {
        let uri = sample_uri("/pub/readme.txt");
        let ts = Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        let a = cache_key(&uri, ts, 42);
        let b = cache_key(&uri, ts, 43);
        assert_ne!(a, b);
    }

    #[test]
    fn cache_hit_calls_opener_synchronously() {
        let tmp = TempDir::new().expect("tempdir");
        let cfg = RemotePreview {
            cache_dir: Some(tmp.path().to_path_buf()),
            max_bytes: 10_000_000,
            max_age_secs: 86_400,
            max_open_bytes: 10_000_000,
            stream_threshold_bytes: 4_194_304,
            stream_chunk_bytes: 262_144,
            write_back_enabled: false,
            write_back_debounce_ms: 500,
        };
        let opener = Arc::new(RecordingOpener::default());
        let cache = PreviewCache::with_opener(cfg, opener.clone());

        let uri = sample_uri("/pub/readme.txt");
        let entry = sample_entry("readme.txt", 42);
        // Pre-populate the cache line to simulate a hit.
        let cache_path = cache.cache_path_for(&uri, &entry);
        std::fs::create_dir_all(cache_path.parent().expect("parent")).expect("create cache dir");
        std::fs::write(&cache_path, b"hello world").expect("write cache line");

        let outcome = cache.open_remote_file(uri, BackendKind::Sftp, entry);
        assert!(
            matches!(outcome, PreviewOutcome::CachedOpen(_)),
            "expected CachedOpen, got {outcome:?}"
        );
        assert_eq!(opener.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            opener.last_path.lock().as_deref(),
            Some(cache_path.as_path())
        );
    }

    #[test]
    fn too_large_files_are_refused_without_open() {
        let tmp = TempDir::new().expect("tempdir");
        let cfg = RemotePreview {
            cache_dir: Some(tmp.path().to_path_buf()),
            max_bytes: 10_000_000,
            max_age_secs: 86_400,
            max_open_bytes: 10,
            stream_threshold_bytes: 4_194_304,
            stream_chunk_bytes: 262_144,
            write_back_enabled: false,
            write_back_debounce_ms: 500,
        };
        let opener = Arc::new(RecordingOpener::default());
        let cache = PreviewCache::with_opener(cfg, opener.clone());

        let outcome = cache.open_remote_file(
            sample_uri("/pub/big.iso"),
            BackendKind::Sftp,
            sample_entry("big.iso", 1_000),
        );
        assert!(
            matches!(outcome, PreviewOutcome::TooLarge { .. }),
            "expected TooLarge, got {outcome:?}"
        );
        assert_eq!(opener.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn evict_lru_drops_oldest_first() {
        let tmp = TempDir::new().expect("tempdir");
        std::fs::write(tmp.path().join("old.bin"), vec![0u8; 200]).expect("write old");
        // Ensure a distinct mtime bucket.
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(tmp.path().join("new.bin"), vec![0u8; 200]).expect("write new");
        // Cap set below the total, so one file must be dropped. LRU
        // eviction is best-effort; we assert only that we get under
        // the cap.
        evict_lru(tmp.path(), 250).expect("evict");
        let remaining = std::fs::read_dir(tmp.path())
            .expect("read cache dir")
            .count();
        assert!(remaining <= 1, "expected ≤1 file to survive");
    }

    /// Simulate the file-activate → download → open path, then
    /// simulate a second activate on the same file. The second call
    /// MUST NOT re-download.
    ///
    /// This is the regression test for the reported "open remote file
    /// blows up because open::that gets a bare basename" bug — the
    /// only supported code path now is "materialise to disk, then
    /// `open::that(cached_path)`". Cache locality is what makes the
    /// second Enter feel instant, so a bug that re-downloads every
    /// time would still be user-visible.
    #[test]
    fn second_open_of_same_file_reuses_cache_line() {
        let tmp = TempDir::new().expect("tempdir");
        let cfg = RemotePreview {
            cache_dir: Some(tmp.path().to_path_buf()),
            max_bytes: 10_000_000,
            max_age_secs: 86_400,
            max_open_bytes: 10_000_000,
            stream_threshold_bytes: 4_194_304,
            stream_chunk_bytes: 262_144,
            write_back_enabled: false,
            write_back_debounce_ms: 500,
        };
        let opener = Arc::new(RecordingOpener::default());
        let cache = PreviewCache::with_opener(cfg, opener.clone());

        let uri = sample_uri("/pub/readme.txt");
        let entry = sample_entry("readme.txt", 11);

        // First activate: "download" (bytes staged) + open.
        let cached_path = cache
            .stage_bytes_for_test(&uri, &entry, b"hello world")
            .expect("stage");
        assert_eq!(
            std::fs::read(&cached_path).expect("read cache"),
            b"hello world"
        );
        assert_eq!(cache.download_count(), 1, "first activate downloads");
        assert_eq!(opener.calls.load(Ordering::SeqCst), 1);

        // Second activate should be a synchronous cache hit — no new
        // download and the opener is called against the same path.
        let outcome = cache.open_remote_file(uri, BackendKind::Sftp, entry);
        assert!(
            matches!(outcome, PreviewOutcome::CachedOpen(_)),
            "expected CachedOpen, got {outcome:?}"
        );
        assert_eq!(
            cache.download_count(),
            1,
            "cache hit must not increment download counter"
        );
        assert_eq!(opener.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            opener.last_path.lock().as_deref(),
            Some(cached_path.as_path())
        );
    }

    /// A cached file whose backing metadata changed (new mtime or
    /// size) is invalidated — the cache key includes both.
    #[test]
    fn cache_key_changes_invalidate_prior_cache_line() {
        let uri = sample_uri("/pub/readme.txt");
        let older = Entry {
            metadata: Metadata {
                size: 11,
                modified: Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
                ..Metadata::default()
            },
            ..sample_entry("readme.txt", 11)
        };
        let newer = Entry {
            metadata: Metadata {
                size: 11,
                modified: Some(UNIX_EPOCH + Duration::from_secs(1_800_000_000)),
                ..Metadata::default()
            },
            ..sample_entry("readme.txt", 11)
        };

        let a = cache_key(&uri, older.metadata.modified, older.metadata.size);
        let b = cache_key(&uri, newer.metadata.modified, newer.metadata.size);
        assert_ne!(a, b, "changing mtime must invalidate the cache line",);
    }
}
