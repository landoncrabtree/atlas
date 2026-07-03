//! Persistent catalogue of remote servers the user has connected to.
//!
//! The store lives at `~/.config/atlas/servers.toml` (via
//! [`crate::paths::servers_file_path`]). Each entry is a [`SavedServer`]:
//! the metadata Atlas needs to re-open the connection later
//! (backend, host, port, path, optional username, optional
//! `credential_ref` keychain handle) plus a friendly `label` for display
//! in UI lists.
//!
//! # Secrets never live here
//!
//! A [`SavedServer`] stores at most a `credential_ref` — the opaque
//! keychain lookup key returned by `atlas_remote::secrets::store`. The
//! actual password / SSH key / IAM secret only lives in the OS keychain.
//! When [`delete`] returns the deleted record, callers should also purge
//! the associated keychain entry via `atlas_remote::secrets::delete`.
//! `atlas-config` intentionally does not depend on `atlas-remote`, so the
//! purge step is the caller's responsibility.
//!
//! # Timestamps
//!
//! `last_connected` is serialised as unix epoch seconds (u64). The
//! canonical `chrono::DateTime<Utc>` type would add a heavy workspace
//! dependency; the raw u64 is precise enough for "sort recents by recency"
//! and round-trips through TOML without ambiguity.
//!
//! # Atomicity
//!
//! [`save`] writes to `servers.toml.tmp` and then `rename`s it into place
//! (POSIX atomic rename). A partial write can never leave the store in a
//! torn state.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use atlas_core::{BackendKind, Result};
use serde::{Deserialize, Serialize};

use crate::paths::{ensure_config_dir, servers_file_path};

/// A single saved-server record, persisted to `servers.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedServer {
    /// Stable identifier (UUID-like string). Used by [`delete`] and by
    /// UI code to reference an entry without matching on the full tuple.
    pub id: String,
    /// User-facing label shown in the saved-server list. Auto-populated
    /// from `user@host` on first save, editable in the UI.
    pub label: String,
    /// Backend that services the connection.
    pub backend: BackendKind,
    /// Host / S3 bucket / WebDAV origin.
    pub address: String,
    /// Optional TCP port (only meaningful for backends with a numeric port).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Absolute remote path. Empty string when the backend addresses via
    /// bucket + prefix only (some S3 configurations).
    #[serde(default)]
    pub path: String,
    /// Optional username. Absent for anonymous connections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Opaque keychain lookup key. `None` means the entry stores no
    /// credentials — typically an anonymous connection or a user who
    /// unchecked "Save to keychain" in the Connect modal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    /// Unix epoch seconds of the most recent successful connect. `None`
    /// means never (dry-run saves with no connect yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_connected: Option<u64>,
}

impl SavedServer {
    /// Return the current unix-epoch seconds, safe to store in
    /// [`Self::last_connected`].
    #[must_use]
    pub fn now_epoch_seconds() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Dedup key: two servers matching this tuple are considered the same
    /// entry by [`add_or_replace`], regardless of `label` or `id`.
    ///
    /// The port is normalised via [`BackendKind::default_port`] when
    /// absent so an entry stored as `port: None` and a fresh entry
    /// assembled with `port: Some(default)` collapse into a single row
    /// — otherwise the user would see two "same server" entries in the
    /// palette after upgrading from older saved-server data.
    #[must_use]
    fn dedup_key(&self) -> (BackendKind, String, Option<u16>, Option<String>) {
        (
            self.backend,
            self.address.clone(),
            self.port.or_else(|| self.backend.default_port()),
            self.username.clone(),
        )
    }
}

/// On-disk shape of `servers.toml`. Only exposes the servers vector; a
/// wrapping struct lets us add fields later without a migration.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedServersFile {
    /// All persisted server entries.
    pub servers: Vec<SavedServer>,
}

impl SavedServersFile {
    /// Return a mutable reference to an entry with the given dedup key, if
    /// one exists.
    fn find_matching_mut(&mut self, needle: &SavedServer) -> Option<&mut SavedServer> {
        let key = needle.dedup_key();
        self.servers.iter_mut().find(|s| s.dedup_key() == key)
    }

    /// Canonicalise every entry's `port` field via
    /// [`BackendKind::default_port`]. Called from [`load_from_path`]
    /// so that older `servers.toml` files (persisted before URI
    /// normalisation present as if they had
    /// been re-saved: `port: None` becomes `port: Some(22)` for
    /// SFTP, `Some(21)` for FTP, `Some(443)` for WebDAV. S3 and
    /// Local stay `None` (no canonical port).
    ///
    /// Idempotent — running twice is a no-op. Called on every load so
    /// deserialised structs always match the shape produced by
    /// `RemoteUri::with_default_port` in `atlas-ui::remote::connect`.
    fn normalize_ports(&mut self) {
        for server in &mut self.servers {
            if server.port.is_none() {
                server.port = server.backend.default_port();
            }
        }
    }
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Load the servers file from disk. Missing file is treated as empty
/// (returned [`SavedServersFile`] has an empty `servers` vector).
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed. A
/// missing file, however, is not an error.
pub fn load() -> Result<SavedServersFile> {
    let path = servers_file_path()?;
    load_from_path(&path)
}

/// Load from a specific path; used by tests. See [`load`].
///
/// # Errors
///
/// Same as [`load`], but scoped to the given path.
pub fn load_from_path(path: &std::path::Path) -> Result<SavedServersFile> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let mut file: SavedServersFile = toml::from_str(&text)
                .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
            // Canonicalise persisted `port: None` entries to the
            // backend default so post-load structs match the shape
            // produced by `atlas_core::RemoteUri::with_default_port`.
            // Older files may carry
            // `port: None` for SFTP entries — without this, dedup and
            // downstream cache-key lookups would treat them as
            // distinct from freshly-assembled `port: Some(22)` URIs.
            file.normalize_ports();
            Ok(file)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SavedServersFile::default()),
        Err(e) => Err(anyhow::anyhow!("failed to read {}: {e}", path.display()).into()),
    }
}

/// Persist a full [`SavedServersFile`] atomically. Writes to `.tmp` then
/// renames into place.
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created, the tmp
/// file cannot be written, or the rename fails.
pub fn save(file: &SavedServersFile) -> Result<()> {
    ensure_config_dir()?;
    let path = servers_file_path()?;
    save_to_path(&path, file)
}

/// Save to a specific path; used by tests.
///
/// # Errors
///
/// See [`save`].
pub fn save_to_path(path: &std::path::Path, file: &SavedServersFile) -> Result<()> {
    let text = toml::to_string_pretty(file)
        .map_err(|e| anyhow::anyhow!("failed to serialise servers.toml: {e}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("failed to create parent {}: {e}", parent.display()))?;
    }
    let tmp = tmp_path(path);
    std::fs::write(&tmp, text)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        anyhow::anyhow!(
            "failed to atomically rename {} → {}: {e}",
            tmp.display(),
            path.display()
        )
    })?;
    Ok(())
}

/// Insert `entry` into the on-disk store, or update the existing entry
/// with a matching `(backend, address, port, username)` tuple.
///
/// The returned server is the record that ended up in the file (with the
/// possibly-preserved `id`, and the caller-supplied `label` /
/// `credential_ref` / `last_connected` fields taking precedence).
///
/// # Errors
///
/// See [`load`] and [`save`].
pub fn add_or_replace(mut entry: SavedServer) -> Result<SavedServer> {
    let mut file = load()?;
    if let Some(existing) = file.find_matching_mut(&entry) {
        entry.id = existing.id.clone();
        *existing = entry.clone();
    } else {
        file.servers.push(entry.clone());
    }
    save(&file)?;
    Ok(entry)
}

/// Remove the entry with `id`. Returns the deleted record so the caller
/// can purge the associated keychain entry via
/// `atlas_remote::secrets::delete(server.credential_ref.as_str())`.
///
/// `Ok(None)` means no entry matched — treated as success.
///
/// # Errors
///
/// See [`load`] and [`save`].
pub fn delete(id: &str) -> Result<Option<SavedServer>> {
    let mut file = load()?;
    let idx = file.servers.iter().position(|s| s.id == id);
    let removed = idx.map(|i| file.servers.remove(i));
    if removed.is_some() {
        save(&file)?;
    }
    Ok(removed)
}

/// Return the saved servers, sorted by `last_connected` descending (most
/// recent first) then by `label` ascending. Entries with no
/// `last_connected` sort after those that have one.
///
/// # Errors
///
/// See [`load`].
pub fn list() -> Result<Vec<SavedServer>> {
    let mut file = load()?;
    file.servers.sort_by(|a, b| {
        b.last_connected
            .unwrap_or(0)
            .cmp(&a.last_connected.unwrap_or(0))
            .then_with(|| a.label.cmp(&b.label))
    });
    Ok(file.servers)
}

// ── Internal helpers ────────────────────────────────────────────────────────

fn tmp_path(path: &std::path::Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample(id: &str, backend: BackendKind, host: &str, user: Option<&str>) -> SavedServer {
        SavedServer {
            id: id.into(),
            label: format!("{}@{host}", user.unwrap_or("anon")),
            backend,
            address: host.into(),
            port: None,
            path: "/".into(),
            username: user.map(str::to_owned),
            credential_ref: None,
            last_connected: None,
        }
    }

    #[test]
    fn round_trip_serialises_and_parses() {
        let file = SavedServersFile {
            servers: vec![SavedServer {
                id: "abc".into(),
                label: "prod".into(),
                backend: BackendKind::Sftp,
                address: "prod.example.com".into(),
                port: Some(2222),
                path: "/var/log".into(),
                username: Some("landon".into()),
                credential_ref: Some("com.atlas.remote.sftp::landon@prod".into()),
                last_connected: Some(1_720_000_000),
            }],
        };
        let text = toml::to_string_pretty(&file).expect("serialise");
        let back: SavedServersFile = toml::from_str(&text).expect("deserialise");
        assert_eq!(back.servers.len(), 1);
        assert_eq!(back.servers[0], file.servers[0]);
    }

    #[test]
    fn empty_file_serialises_and_parses() {
        let file = SavedServersFile::default();
        let text = toml::to_string_pretty(&file).expect("serialise");
        let back: SavedServersFile = toml::from_str(&text).expect("deserialise");
        assert!(back.servers.is_empty());
    }

    #[test]
    fn save_and_load_from_path_round_trip() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("servers.toml");
        // Use an explicit non-default port to keep the roundtrip
        // assertion exact — otherwise `load_from_path` would
        // canonicalise the persisted `port: None` up to the SFTP
        // default (22), which is the desired schema-migration
        // behaviour but noise for a plain roundtrip check.
        let mut entry = sample("s1", BackendKind::Sftp, "h1", Some("u1"));
        entry.port = Some(2222);
        let file = SavedServersFile {
            servers: vec![entry],
        };
        save_to_path(&path, &file).expect("save");
        let back = load_from_path(&path).expect("load");
        assert_eq!(back.servers, file.servers);
    }

    #[test]
    fn load_missing_file_is_empty_not_error() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("servers.toml");
        let back = load_from_path(&path).expect("load missing");
        assert!(back.servers.is_empty());
    }

    #[test]
    fn atomic_save_uses_tmp_and_rename() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("servers.toml");
        let file = SavedServersFile {
            servers: vec![sample("s1", BackendKind::Sftp, "h1", None)],
        };
        save_to_path(&path, &file).expect("save");
        // Final file exists, tmp is gone.
        assert!(path.exists(), "final servers.toml must exist");
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "tmp must be renamed away"
        );
    }

    #[test]
    fn dedup_key_matches_backend_host_port_user() {
        let a = SavedServer {
            id: "id-a".into(),
            label: "A".into(),
            backend: BackendKind::Sftp,
            address: "host.example.com".into(),
            port: Some(22),
            path: "/foo".into(),
            username: Some("alice".into()),
            credential_ref: None,
            last_connected: None,
        };
        let b = SavedServer {
            id: "id-b".into(),
            label: "different label".into(),
            backend: BackendKind::Sftp,
            address: "host.example.com".into(),
            port: Some(22),
            path: "/bar".into(), // path is NOT part of dedup key
            username: Some("alice".into()),
            credential_ref: Some("ref".into()),
            last_connected: None,
        };
        assert_eq!(a.dedup_key(), b.dedup_key());

        let c = SavedServer {
            id: "id-c".into(),
            label: "C".into(),
            backend: BackendKind::Sftp,
            address: "host.example.com".into(),
            port: Some(2222), // different port
            path: "/foo".into(),
            username: Some("alice".into()),
            credential_ref: None,
            last_connected: None,
        };
        assert_ne!(a.dedup_key(), c.dedup_key());

        let d = SavedServer {
            id: "id-d".into(),
            label: "D".into(),
            backend: BackendKind::S3, // different backend
            address: "host.example.com".into(),
            port: Some(22),
            path: "/foo".into(),
            username: Some("alice".into()),
            credential_ref: None,
            last_connected: None,
        };
        assert_ne!(a.dedup_key(), d.dedup_key());
    }

    #[test]
    fn find_matching_mut_updates_in_place() {
        let mut file = SavedServersFile {
            servers: vec![sample("id-1", BackendKind::Sftp, "host", Some("alice"))],
        };
        let probe = sample("id-2-new", BackendKind::Sftp, "host", Some("alice"));
        let existing = file.find_matching_mut(&probe).expect("must match");
        existing.label = "updated".into();
        assert_eq!(file.servers[0].label, "updated");
    }

    #[test]
    fn dedup_key_treats_missing_port_as_backend_default() {
        // Regression: two entries that differ only by
        // `port: None` vs `port: Some(default_for_backend)` must
        // dedup as the same server. Before this fix, an entry
        // freshly assembled by the connect controller (with
        // `port: Some(22)` for SFTP) would not deduplicate against
        // an older stored entry with `port: None`, so the palette
        // showed a "ghost" duplicate.
        let no_port = SavedServer {
            id: "id-a".into(),
            label: "A".into(),
            backend: BackendKind::Sftp,
            address: "host.dedup-test.example".into(),
            port: None,
            path: "/foo".into(),
            username: Some("alice".into()),
            credential_ref: None,
            last_connected: None,
        };
        let default_port = SavedServer {
            id: "id-b".into(),
            label: "B".into(),
            port: Some(22),
            ..no_port.clone()
        };
        assert_eq!(no_port.dedup_key(), default_port.dedup_key());

        // FTP + WebDAV get the same treatment.
        let ftp_no_port = SavedServer {
            id: "id-ftp".into(),
            label: "FTP".into(),
            backend: BackendKind::Ftp,
            address: "ftp.dedup-test.example".into(),
            port: None,
            path: "/pub".into(),
            username: None,
            credential_ref: None,
            last_connected: None,
        };
        let ftp_default = SavedServer {
            port: Some(21),
            ..ftp_no_port.clone()
        };
        assert_eq!(ftp_no_port.dedup_key(), ftp_default.dedup_key());

        // Explicit non-default port stays distinct.
        let alt = SavedServer {
            port: Some(2222),
            ..no_port.clone()
        };
        assert_ne!(no_port.dedup_key(), alt.dedup_key());
    }

    #[test]
    fn load_normalises_missing_port_to_backend_default() {
        // Regression: an `servers.toml` file written before Phase
        // 2.12 landed URI normalisation may carry entries without a
        // `port` field. After load, callers expect `port: Some(22)`
        // for SFTP so cache-key comparisons collapse to a single
        // canonical form. Verified end-to-end via
        // [`load_from_path`], not just the private helper, so any
        // future refactor of the load pipeline that forgets to run
        // normalisation is caught by this test.
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("servers.toml");
        // Synthetic legacy content — no port fields.
        let toml_text = r#"
[[servers]]
id = "legacy-sftp"
label = "legacy-sftp"
backend = "sftp"
address = "legacy.example.com"
path = "/var/log"

[[servers]]
id = "legacy-ftp"
label = "legacy-ftp"
backend = "ftp"
address = "ftp.example.com"
path = "/pub"

[[servers]]
id = "legacy-s3"
label = "legacy-s3"
backend = "s3"
address = "bucket"
path = "/prefix"
"#;
        std::fs::write(&path, toml_text).expect("write");
        let loaded = load_from_path(&path).expect("load");
        assert_eq!(loaded.servers.len(), 3);
        let by_id = |id: &str| {
            loaded
                .servers
                .iter()
                .find(|s| s.id == id)
                .cloned()
                .expect("must exist")
        };
        assert_eq!(by_id("legacy-sftp").port, Some(22));
        assert_eq!(by_id("legacy-ftp").port, Some(21));
        // S3 has no canonical port — must stay None.
        assert_eq!(by_id("legacy-s3").port, None);
    }

    #[test]
    fn now_epoch_seconds_is_nonzero_and_recent() {
        let now = SavedServer::now_epoch_seconds();
        // 2020-01-01 UTC was 1577836800.
        assert!(now > 1_577_836_800, "should be > 2020-01-01 epoch");
    }

    /// Sorting semantics used by [`list`]. Split into a helper so tests
    /// can exercise ordering without touching a shared config dir.
    fn sort_for_list(mut servers: Vec<SavedServer>) -> Vec<SavedServer> {
        servers.sort_by(|a, b| {
            b.last_connected
                .unwrap_or(0)
                .cmp(&a.last_connected.unwrap_or(0))
                .then_with(|| a.label.cmp(&b.label))
        });
        servers
    }

    #[test]
    fn list_sort_orders_by_recency_then_label() {
        let mut s1 = sample("1", BackendKind::Sftp, "a", None);
        s1.label = "beta".into();
        s1.last_connected = Some(200);
        let mut s2 = sample("2", BackendKind::Sftp, "b", None);
        s2.label = "alpha".into();
        s2.last_connected = Some(200);
        let mut s3 = sample("3", BackendKind::Sftp, "c", None);
        s3.label = "gamma".into();
        s3.last_connected = Some(300);
        let mut s4 = sample("4", BackendKind::Sftp, "d", None);
        s4.label = "delta".into();
        s4.last_connected = None; // never connected → last

        let sorted = sort_for_list(vec![s1.clone(), s2.clone(), s3.clone(), s4.clone()]);
        // s3 (last_connected 300) first, then s2 (200, "alpha"), s1 (200, "beta"), s4 (never)
        assert_eq!(sorted[0].id, "3");
        assert_eq!(sorted[1].id, "2");
        assert_eq!(sorted[2].id, "1");
        assert_eq!(sorted[3].id, "4");
    }

    // ── Integration-style tests exercising add/replace/delete via a
    // ── temp servers.toml under a scoped ATLAS_CONFIG_DIR. Serialised
    // ── because they mutate a process-global env var.

    use serial_test::serial;

    struct ScopedConfigDir {
        _tmp: TempDir,
        prev: Option<std::ffi::OsString>,
    }

    impl ScopedConfigDir {
        fn new() -> Self {
            let tmp = TempDir::new().expect("tempdir");
            let prev = std::env::var_os("ATLAS_CONFIG_DIR");
            // Safety: single-threaded test-only mutation, gated by `#[serial]`
            // in every caller.
            std::env::set_var("ATLAS_CONFIG_DIR", tmp.path());
            Self { _tmp: tmp, prev }
        }
    }

    impl Drop for ScopedConfigDir {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("ATLAS_CONFIG_DIR", v),
                None => std::env::remove_var("ATLAS_CONFIG_DIR"),
            }
        }
    }

    #[test]
    #[serial]
    fn add_or_replace_inserts_new_entry() {
        let _guard = ScopedConfigDir::new();
        let entry = sample("first", BackendKind::Sftp, "prod", Some("landon"));
        let stored = add_or_replace(entry.clone()).expect("add");
        assert_eq!(stored.id, "first");

        let all = list().expect("list");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "first");
    }

    #[test]
    #[serial]
    fn add_or_replace_dedupes_on_backend_host_port_user() {
        let _guard = ScopedConfigDir::new();
        add_or_replace(sample("first", BackendKind::Sftp, "prod", Some("landon")))
            .expect("add first");
        let mut second = sample("second-id", BackendKind::Sftp, "prod", Some("landon"));
        second.label = "renamed".into();
        second.path = "/var/other".into();
        second.credential_ref = Some("ref-2".into());
        let stored = add_or_replace(second).expect("add second");

        // The dedup path preserves the ORIGINAL id, not the new one.
        assert_eq!(stored.id, "first");

        let all = list().expect("list");
        assert_eq!(all.len(), 1, "duplicate insert should not create a 2nd row");
        assert_eq!(all[0].label, "renamed");
        assert_eq!(all[0].path, "/var/other");
        assert_eq!(all[0].credential_ref.as_deref(), Some("ref-2"));
    }

    #[test]
    #[serial]
    fn delete_returns_removed_record_with_credential_ref() {
        let _guard = ScopedConfigDir::new();
        let mut entry = sample("gone", BackendKind::Sftp, "h", Some("u"));
        entry.credential_ref = Some("com.atlas.remote.sftp::u@h".into());
        add_or_replace(entry).expect("add");

        let removed = delete("gone").expect("delete").expect("must exist");
        assert_eq!(removed.id, "gone");
        assert_eq!(
            removed.credential_ref.as_deref(),
            Some("com.atlas.remote.sftp::u@h"),
            "caller uses this to purge the keychain entry"
        );

        // Second delete of the same id: Ok(None), no error.
        assert!(delete("gone").expect("delete missing").is_none());
        assert!(list().expect("list").is_empty());
    }
}
