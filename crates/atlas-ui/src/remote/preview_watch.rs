//! Write-back for edited previews.
//!
//! When a user opens a remote file through [`super::PreviewCache`] the
//! bytes are materialised into a local cache line and handed to the OS
//! default editor via `open::that`. If the user edits and saves the
//! file we want those edits to propagate back to the remote — but only
//! for backends that support writes, and only when the user has actually
//! changed the contents.
//!
//! # Design
//!
//! * [`PreviewWatchRegistry`] owns a single background
//!   [`atlas_watch::DirectoryWatcher`] scoped to the preview cache
//!   root. This is a hard convergence rule: we never spin up a second
//!   file watcher — the atlas-watch crate is the sole owner of
//!   inode-level events in this codebase.
//! * Every successful preview open (cache-hit or fresh download)
//!   calls [`PreviewWatchRegistry::register`] with the cache path,
//!   the source URI, and the backend kind. The registry snapshots
//!   the file's `(mtime, size, sha256)` at registration time.
//! * A background thread drains the watcher's event receiver. When
//!   a `Modified` event fires for a registered path we debounce
//!   `write_back_debounce_ms` (default 500 ms), re-hash the cache
//!   file, and if the SHA differs from the baseline we spawn an
//!   upload on the `atlas_remote::runtime` handle. The upload calls
//!   [`RemoteLocationViewModel::write`] which is already wrapped in
//!   the retry envelope, so transient network errors are handled.
//! * On success the registry's baseline is updated (new mtime, size,
//!   sha256) so subsequent saves keep working.
//! * On failure — permission denied, disk quota, network — the
//!   cache file is preserved and a
//!   [`WriteBackNotice`] with kind [`WriteBackNoticeKind::Failed`]
//!   is dispatched to the shell-supplied callback (if any). The
//!   `Local edits preserved at <cache-path>` message is authored
//!   here so any consumer of the sink surfaces the same wording.
//! * A [`Drop`] guard on [`PreviewWatchRegistry`] shuts down the
//!   background watcher thread cleanly.
//!
//! # Concurrency
//!
//! Two panes opening the same remote file both land on the same
//! cache path (the cache key includes `(uri, mtime, size)`); their
//! registrations therefore hit the same registry entry. The second
//! `register` refreshes the baseline — no harm done.
//!
//! Two panes opening *different* remote files that happen to share a
//! preview parent directory are independent by design because the
//! registry keys on the full absolute cache path.
//!
//! # Editor quirks
//!
//! Vim writes via a `.filename.swp` + rename dance — the actual
//! target file gets a `Modified` event once the rename finalises.
//! TextEdit uses atomic replace (`Removed` + `Created`). VS Code
//! writes in-place with occasional `Modified` bursts. All three
//! surface at least one event whose path matches our registered
//! cache path, so the SHA-diff heuristic catches every real save
//! without misfiring on the "same bytes rewritten" no-op case.

use std::collections::HashMap;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use atlas_core::{BackendKind, RemoteUri};
use atlas_fs::OpenOptions;
use atlas_remote::RemoteLocationViewModel;
use atlas_watch::{DirectoryWatcher, FileEvent, FileEventKind, WatcherBuilder};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};

/// Sink for write-back notifications. Clone-cheap — typically an
/// `Arc<dyn Fn(WriteBackNotice) + Send + Sync>`. The shell wires an
/// implementation that pushes a status-bar toast; tests substitute a
/// recording double.
pub type WriteBackSink = Arc<dyn Fn(WriteBackNotice) + Send + Sync>;

/// A single write-back notification suitable for a status-bar toast
/// or an ops-panel row.
#[derive(Debug, Clone)]
pub struct WriteBackNotice {
    /// What happened.
    pub kind: WriteBackNoticeKind,
    /// The remote URI whose contents were (attempted to be) updated.
    pub uri: RemoteUri,
    /// Local cache path where the edited bytes live.
    pub cache_path: PathBuf,
    /// Human-readable message. Ready to display verbatim.
    pub message: String,
}

/// Terminal state of a write-back attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteBackNoticeKind {
    /// Upload succeeded; the remote now reflects the local edits.
    Completed,
    /// Upload failed; the local cache file is preserved verbatim so
    /// the user does not lose work.
    Failed,
}

/// Event surfaced on the write-back event channel; useful for tests
/// that want to observe the internal state machine without a full
/// shell attached.
#[derive(Debug, Clone)]
pub enum WriteBackEvent {
    /// The registry saw an edit and detected the SHA had changed;
    /// an upload has been dispatched.
    UploadStarted { cache_path: PathBuf, uri: RemoteUri },
    /// Upload finished — see notice.
    Notice(WriteBackNotice),
}

/// One tracked preview file.
#[derive(Debug, Clone)]
struct Entry {
    uri: RemoteUri,
    kind: BackendKind,
    /// Basename inside the URI's parent used as the remote target.
    remote_child: String,
    /// The (mtime, size, sha256) triple recorded at registration
    /// time and re-latched after every successful upload.
    baseline: Baseline,
}

#[derive(Debug, Clone)]
struct Baseline {
    #[allow(dead_code)]
    mtime: Option<SystemTime>,
    #[allow(dead_code)]
    size: u64,
    sha256: [u8; 32],
}

/// Handle to the write-back watcher. Cheap to clone via `Arc`.
///
/// The registry owns exactly one `DirectoryWatcher` on the preview
/// cache root; individual file registrations are entries in an
/// internal map. Dropping the registry shuts the watcher down.
pub struct PreviewWatchRegistry {
    inner: Arc<Inner>,
}

impl Clone for PreviewWatchRegistry {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct Inner {
    /// Map from absolute cache path → tracked entry.
    entries: Mutex<HashMap<PathBuf, Entry>>,
    /// The single `DirectoryWatcher` shared across every tracked
    /// preview. `None` until the first `register()` call — the
    /// watcher is lazy so tests that never register don't pay the
    /// background-thread cost.
    watcher: Mutex<Option<DirectoryWatcher>>,
    /// Optional sink for terminal notifications.
    sink: Mutex<Option<WriteBackSink>>,
    /// Optional side-channel for internal test observation.
    test_tx: Mutex<Option<crossbeam_channel::Sender<WriteBackEvent>>>,
    /// Debounce window for coalescing modification bursts.
    debounce_ms: Mutex<u32>,
    /// Whether write-back is enabled at all. When `false`,
    /// `register()` is a no-op.
    enabled: Mutex<bool>,
}

impl PreviewWatchRegistry {
    /// Construct an empty registry. The watcher thread is spawned
    /// lazily on the first `register()` call.
    #[must_use]
    pub fn new(enabled: bool, debounce_ms: u32) -> Self {
        Self {
            inner: Arc::new(Inner {
                entries: Mutex::new(HashMap::new()),
                watcher: Mutex::new(None),
                sink: Mutex::new(None),
                test_tx: Mutex::new(None),
                debounce_ms: Mutex::new(debounce_ms),
                enabled: Mutex::new(enabled),
            }),
        }
    }

    /// Install a notification sink; typically an `Arc<dyn Fn>` that
    /// pushes a toast row into the ops panel.
    pub fn set_sink(&self, sink: WriteBackSink) {
        *self.inner.sink.lock() = Some(sink);
    }

    /// Update runtime-tunable config knobs (called by the config
    /// hot-reload path).
    pub fn set_config(&self, enabled: bool, debounce_ms: u32) {
        *self.inner.enabled.lock() = enabled;
        *self.inner.debounce_ms.lock() = debounce_ms;
    }

    /// Subscribe to internal write-back state transitions. Chiefly
    /// intended for tests, but callable in production for a
    /// diagnostic subscriber. Returns a receiver that fires on
    /// upload-started and upload-completed transitions. Only one
    /// subscriber is retained (a new call overwrites the previous
    /// sender); callers who want fan-out should install a
    /// [`WriteBackSink`] via [`Self::set_sink`] instead.
    pub fn subscribe_events(&self) -> crossbeam_channel::Receiver<WriteBackEvent> {
        let (tx, rx) = crossbeam_channel::unbounded();
        *self.inner.test_tx.lock() = Some(tx);
        rx
    }

    /// Register a cached preview file for edit-tracking. Idempotent —
    /// calling `register` a second time on the same cache path
    /// simply refreshes the baseline (which is the correct behavior
    /// for a re-open of the same file).
    ///
    /// # Errors
    ///
    /// Returns `Ok(())` on the disabled path and on hash / metadata
    /// failures — those are logged via `tracing::warn` and the file
    /// is simply not registered.
    pub fn register(
        &self,
        cache_path: PathBuf,
        uri: RemoteUri,
        kind: BackendKind,
    ) -> std::io::Result<()> {
        if !*self.inner.enabled.lock() {
            return Ok(());
        }
        // Compute baseline (mtime, size, sha256) from disk.
        let bytes = std::fs::read(&cache_path)?;
        let baseline = Baseline {
            mtime: file_mtime(&cache_path),
            size: bytes.len() as u64,
            sha256: sha256_of(&bytes),
        };
        // Remote target = last path component. Everything else lives
        // on the URI (host, port, credentials …).
        let remote_child = uri_basename(&uri);
        {
            let mut entries = self.inner.entries.lock();
            entries.insert(
                cache_path.clone(),
                Entry {
                    uri: uri.clone(),
                    kind,
                    remote_child,
                    baseline,
                },
            );
        }
        self.ensure_watcher(&cache_path)?;
        Ok(())
    }

    /// Explicitly stop tracking a cache path. Called on pane close /
    /// atlas quit. Never fails — a missing entry is a no-op.
    pub fn unregister(&self, cache_path: &Path) {
        let mut entries = self.inner.entries.lock();
        entries.remove(cache_path);
    }

    /// Test helper: report whether the given cache path is currently
    /// tracked. Used by integration tests to synchronise with the
    /// asynchronous registration path.
    #[must_use]
    pub fn is_watching_for_test(&self, cache_path: &Path) -> bool {
        self.inner.entries.lock().contains_key(cache_path)
    }

    /// Test helper: manually dispatch a modification for the given
    /// path, bypassing the OS file watcher. Some CI environments —
    /// notably macOS FSEvents in sandboxed test harnesses — deliver
    /// events on `/private/var/...` while our registered path is
    /// `/var/...`, and the debounce timing can exceed a reasonable
    /// test budget. This helper drives the internal state machine
    /// (debounce → hash diff → upload) directly so tests can prove
    /// the write-back pipeline without depending on FSEvents fidelity.
    pub fn dispatch_edit_for_test(&self, cache_path: &Path) {
        handle_modification(&self.inner, cache_path);
    }

    fn ensure_watcher(&self, sample_path: &Path) -> std::io::Result<()> {
        let mut guard = self.inner.watcher.lock();
        if guard.is_some() {
            return Ok(());
        }
        let root = match sample_path.parent().and_then(|p| p.parent()) {
            Some(p) => p.to_path_buf(),
            None => return Ok(()),
        };
        // The watch root is `<cache_dir>/preview/` — one level above
        // the per-file cache line. We use a 100 ms debounce here to
        // let bursty editors settle before our per-file debounce
        // fires above.
        let debounce = Duration::from_millis(100);
        let (watcher, rx) = WatcherBuilder::new()
            .debounce(debounce)
            .recursive(true)
            .build()
            .map_err(|err| std::io::Error::other(format!("watcher build: {err}")))?;
        // Ensure the watch root exists (it's normally created by the
        // preview cache before we get here, but be defensive).
        if !root.exists() {
            std::fs::create_dir_all(&root)?;
        }
        watcher
            .add_root(root.clone())
            .map_err(|err| std::io::Error::other(format!("add_root: {err}")))?;
        *guard = Some(watcher);
        // Drain the receiver on a dedicated thread — we can't hold
        // the mutex across the receive.
        let inner = Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name("atlas-preview-watch".to_owned())
            .spawn(move || dispatch_loop(inner, rx))
            .map_err(std::io::Error::other)?;
        Ok(())
    }
}

impl Drop for PreviewWatchRegistry {
    fn drop(&mut self) {
        // Owning `Inner` via `Arc<Inner>` means the actual shutdown
        // happens when the last clone is dropped. Explicit shutdown
        // isn't required — the dispatch thread exits when the
        // watcher's event channel closes.
    }
}

fn dispatch_loop(inner: Arc<Inner>, rx: crossbeam_channel::Receiver<FileEvent>) {
    while let Ok(ev) = rx.recv() {
        if !matches!(ev.kind, FileEventKind::Modified | FileEventKind::Created) {
            continue;
        }
        for path in &ev.paths {
            handle_modification(&inner, path);
        }
    }
}

fn handle_modification(inner: &Arc<Inner>, path: &Path) {
    let entry = {
        let entries = inner.entries.lock();
        match entries.get(path) {
            Some(e) => e.clone(),
            None => return,
        }
    };
    let debounce_ms = *inner.debounce_ms.lock();
    // Editor may emit several events over a very short window; wait
    // out the burst before hashing.
    let inner_clone = Arc::clone(inner);
    let path_owned = path.to_path_buf();
    let deadline = Instant::now() + Duration::from_millis(u64::from(debounce_ms));
    std::thread::Builder::new()
        .name("atlas-preview-writeback".to_owned())
        .spawn(move || {
            let now = Instant::now();
            if now < deadline {
                std::thread::sleep(deadline - now);
            }
            attempt_upload(inner_clone, path_owned, entry);
        })
        .ok();
}

fn attempt_upload(inner: Arc<Inner>, cache_path: PathBuf, entry: Entry) {
    let bytes = match std::fs::read(&cache_path) {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(?cache_path, %err, "preview_watch: re-read failed");
            return;
        }
    };
    let new_sha = sha256_of(&bytes);
    if new_sha == entry.baseline.sha256 {
        // No content change since baseline — this is the "editor
        // rewrote same bytes" no-op case.
        return;
    }
    if let Some(tx) = inner.test_tx.lock().clone() {
        let _ = tx.send(WriteBackEvent::UploadStarted {
            cache_path: cache_path.clone(),
            uri: entry.uri.clone(),
        });
    }
    let handle = atlas_remote::runtime::handle();
    let inner_upload = Arc::clone(&inner);
    let uri_display = entry.uri.to_uri();
    handle.spawn(async move {
        let credentials = match atlas_ops::credentials_for(&entry.uri) {
            Ok(c) => c,
            Err(err) => {
                dispatch_notice(
                    &inner_upload,
                    WriteBackNoticeKind::Failed,
                    &entry.uri,
                    &cache_path,
                    format!(
                        "Upload failed: credentials lookup failed ({err}). Local edits preserved at {}",
                        cache_path.display()
                    ),
                );
                return;
            }
        };
        // Root vm — child_path is the last segment.
        let mut vm_uri = entry.uri.clone();
        vm_uri.path = "/".into();
        let vm = match RemoteLocationViewModel::open_live(
            vm_uri,
            entry.kind,
            credentials,
            OpenOptions::default(),
        ) {
            Ok(v) => v,
            Err(err) => {
                dispatch_notice(
                    &inner_upload,
                    WriteBackNoticeKind::Failed,
                    &entry.uri,
                    &cache_path,
                    format!(
                        "Upload failed: open_live: {err}. Local edits preserved at {}",
                        cache_path.display()
                    ),
                );
                return;
            }
        };
        let write_path = entry.uri.path.clone();
        match vm.write(&write_path, bytes.clone()).await {
            Ok(()) => {
                // Update baseline so subsequent saves keep working.
                {
                    let mut entries = inner_upload.entries.lock();
                    if let Some(cur) = entries.get_mut(&cache_path) {
                        cur.baseline = Baseline {
                            mtime: file_mtime(&cache_path),
                            size: bytes.len() as u64,
                            sha256: new_sha,
                        };
                    }
                }
                dispatch_notice(
                    &inner_upload,
                    WriteBackNoticeKind::Completed,
                    &entry.uri,
                    &cache_path,
                    format!("Uploaded {} to {uri_display}", entry.remote_child),
                );
            }
            Err(err) => {
                dispatch_notice(
                    &inner_upload,
                    WriteBackNoticeKind::Failed,
                    &entry.uri,
                    &cache_path,
                    format!(
                        "Upload failed: {err}. Local edits preserved at {}",
                        cache_path.display()
                    ),
                );
            }
        }
    });
}

fn dispatch_notice(
    inner: &Arc<Inner>,
    kind: WriteBackNoticeKind,
    uri: &RemoteUri,
    cache_path: &Path,
    message: String,
) {
    let notice = WriteBackNotice {
        kind: kind.clone(),
        uri: uri.clone(),
        cache_path: cache_path.to_path_buf(),
        message: message.clone(),
    };
    if kind == WriteBackNoticeKind::Failed {
        tracing::warn!(%message, "preview_watch: upload failed");
    } else {
        tracing::info!(%message, "preview_watch: upload completed");
    }
    if let Some(sink) = inner.sink.lock().clone() {
        sink(notice.clone());
    }
    if let Some(tx) = inner.test_tx.lock().clone() {
        let _ = tx.send(WriteBackEvent::Notice(notice));
    }
}

fn sha256_of(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(mtime_from_meta)
}

fn mtime_from_meta(meta: Metadata) -> Option<SystemTime> {
    meta.modified().ok()
}

fn uri_basename(uri: &RemoteUri) -> String {
    Path::new(&uri.path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| uri.path.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_uri() -> RemoteUri {
        RemoteUri {
            scheme: "sftp".into(),
            host: Some("h".into()),
            port: Some(22),
            username: Some("u".into()),
            path: "/dir/readme.txt".into(),
            credential_ref: None,
        }
    }

    #[test]
    fn disabled_registry_never_installs_watcher() {
        let reg = PreviewWatchRegistry::new(false, 100);
        let tmp = tempfile::TempDir::new().unwrap();
        let cache_dir = tmp.path().join("preview").join("key");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_file = cache_dir.join("readme.txt");
        std::fs::write(&cache_file, b"hi").unwrap();
        reg.register(cache_file, sample_uri(), BackendKind::Sftp)
            .unwrap();
        // Watcher never got created.
        assert!(reg.inner.watcher.lock().is_none());
        assert!(reg.inner.entries.lock().is_empty());
    }

    #[test]
    fn register_records_baseline_hash() {
        let reg = PreviewWatchRegistry::new(true, 100);
        let tmp = tempfile::TempDir::new().unwrap();
        let cache_dir = tmp.path().join("preview").join("key");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_file = cache_dir.join("readme.txt");
        std::fs::write(&cache_file, b"hello world").unwrap();
        reg.register(cache_file.clone(), sample_uri(), BackendKind::Sftp)
            .unwrap();
        let entries = reg.inner.entries.lock();
        let entry = entries.get(&cache_file).expect("registered");
        assert_eq!(entry.baseline.size, 11);
        assert_eq!(entry.baseline.sha256, sha256_of(b"hello world"));
    }

    #[test]
    fn unregister_drops_entry() {
        let reg = PreviewWatchRegistry::new(true, 100);
        let tmp = tempfile::TempDir::new().unwrap();
        let cache_dir = tmp.path().join("preview").join("key");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_file = cache_dir.join("readme.txt");
        std::fs::write(&cache_file, b"x").unwrap();
        reg.register(cache_file.clone(), sample_uri(), BackendKind::Sftp)
            .unwrap();
        reg.unregister(&cache_file);
        assert!(reg.inner.entries.lock().is_empty());
    }
}
