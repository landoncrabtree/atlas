//! Per-backend remote view models.
//!
//! Each submodule implements [`common::BackendClient`] for one
//! network protocol using a pure-Rust, cross-platform crate:
//!
//! | Backend | Crate |
//! |---------|-------|
//! | SFTP    | `russh` + `russh-sftp` |
//! | FTP     | `suppaftp` |
//! | WebDAV  | `reqwest` + `quick-xml` (roll-own) |
//! | S3      | `object_store` (Apache Arrow) |
//!
//! The outer [`RemoteLocationViewModel`] wraps an
//! `Arc<dyn BackendClient>` and owns the async→sync bridge
//! (tokio runtime handle + background listing task + subscriber
//! notifications). See [`common`] for the shared plumbing.

pub(crate) mod common;
pub(crate) mod ftp;
pub(crate) mod s3;
pub mod sftp;
pub(crate) mod webdav;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use atlas_core::{BackendKind, RemoteUri, Result as AtlasResult};
use atlas_fs::{
    CompiledFilter, Entry, EntryKind, Filter, LocationViewModel, Metadata, OpenOptions, SortSpec,
    ViewModelEvent,
};
use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Mutex, RwLock};
use tokio::runtime::Handle;

use crate::error::{RemoteError, RemoteMetadata, RemoteResult};
use crate::retry::{with_retry, RetryHooks, RetryPolicy};
pub use common::{BackendClient, BoxedAsyncRead, BoxedAsyncWrite, RemoteEntry};

/// Fingerprint the configured retry policy globally. Callers plumb
/// their config in via [`set_default_retry_policy`]; each new view
/// model reads this snapshot at construction time.
static GLOBAL_RETRY_POLICY: once_cell::sync::Lazy<parking_lot::RwLock<RetryPolicy>> =
    once_cell::sync::Lazy::new(|| parking_lot::RwLock::new(RetryPolicy::default()));

/// Return a copy of the process-wide default retry policy. Newly
/// constructed view models pick this up automatically.
#[must_use]
pub fn default_retry_policy() -> RetryPolicy {
    *GLOBAL_RETRY_POLICY.read()
}

/// Overwrite the process-wide default retry policy. Called by
/// `AppShell` after loading the config so subsequent
/// `RemoteLocationViewModel` instances inherit the user's tuning.
pub fn set_default_retry_policy(policy: RetryPolicy) {
    *GLOBAL_RETRY_POLICY.write() = policy;
}

/// Internal state protected by the outer `RwLock`.
struct Inner {
    raw: Vec<Entry>,
    view: Vec<Entry>,
    sort: SortSpec,
    filter: Filter,
    compiled: CompiledFilter,
    loaded: bool,
}

impl Inner {
    fn recompute(&mut self) {
        let mut view: Vec<Entry> = self
            .raw
            .iter()
            .filter(|e| self.compiled.matches(e))
            .cloned()
            .collect();
        atlas_fs::sort_in_place(&mut view, &self.sort);
        self.view = view;
    }
}

/// Remote-backed [`LocationViewModel`], dispatched onto one of the
/// per-backend adapters at construction time.
///
/// Instances are always shared via [`Arc`]; the background loader
/// task holds a strong clone until the initial listing completes.
///
/// `location()` returns the *remote path portion* only (e.g.
/// `/tmp` for `sftp://host/tmp`). Consumers that need the full URI
/// (for logging, address-bar rendering, …) should call
/// [`RemoteLocationViewModel::remote_uri`] or
/// [`RemoteLocationViewModel::backend_kind`].
pub struct RemoteLocationViewModel {
    uri: RemoteUri,
    kind: BackendKind,
    /// Cached `PathBuf` view of `uri.path`, so `location() -> &Path`
    /// can hand out a stable borrow.
    path_cache: PathBuf,
    client: Arc<dyn BackendClient>,
    state: RwLock<Inner>,
    subscribers: Mutex<Vec<Sender<ViewModelEvent>>>,
    runtime: Handle,
    /// Retry policy for the seven network ops surfaced by the trait.
    /// Cloned from the process-wide default at construction time;
    /// tests can rewrite it via [`Self::set_retry_policy`].
    retry_policy: RwLock<RetryPolicy>,
    /// Observer sink shared with any consumer that wants retry
    /// notifications (ops panel, per-pane status glyph). Cheap to
    /// clone via [`Self::retry_hooks`].
    retry_hooks: RetryHooks,
}

impl RemoteLocationViewModel {
    /// Public façade that mirrors the old
    /// pre-refactor `open_live` shape.
    ///
    /// Callers that just need an `Arc<dyn LocationViewModel>` should
    /// use [`crate::backend::open`] — this constructor is only useful
    /// when the concrete type is required (tests, direct
    /// read/write/rename/delete access).
    ///
    /// # Errors
    ///
    /// Returns [`crate::backend::BackendError`] if the URI or
    /// credentials are ill-shaped for the requested [`BackendKind`].
    /// Any network-side failure surfaces asynchronously via
    /// [`atlas_fs::ViewModelEvent::Error`] on the subscribe channel.
    pub fn open_live(
        uri: RemoteUri,
        kind: BackendKind,
        credentials: crate::backend::Credentials,
        opts: OpenOptions,
    ) -> Result<Arc<Self>, crate::backend::BackendError> {
        use crate::backend::BackendError;
        let pool = crate::pool::global();
        let key = crate::pool::PoolKey::new(
            kind,
            uri.host.clone().unwrap_or_default(),
            uri.port,
            uri.username.clone(),
            &credentials,
        );
        let client = pool.get_or_open(&key, || {
            let built: Arc<dyn BackendClient> = match kind {
                BackendKind::Local => {
                    return Err(BackendError::UnsupportedBackend(
                        "local kind on remote location".to_owned(),
                    ));
                }
                BackendKind::Sftp => Arc::new(crate::vm::sftp::SftpBackend::new(
                    &uri,
                    credentials.clone(),
                )?),
                BackendKind::Ftp => {
                    Arc::new(crate::vm::ftp::FtpBackend::new(&uri, credentials.clone())?)
                }
                BackendKind::WebDav => Arc::new(crate::vm::webdav::WebDavBackend::new(
                    &uri,
                    credentials.clone(),
                )?),
                BackendKind::S3 => {
                    Arc::new(crate::vm::s3::S3Backend::new(&uri, credentials.clone())?)
                }
            };
            Ok(built)
        })?;
        Ok(Self::from_client(uri, kind, client, opts))
    }

    /// SFTP-specific opening path that accepts caller-supplied
    /// [`crate::vm::sftp::SftpOptions`]. This is the seam used by:
    ///
    /// * `ConnectController::run_connect` — attaches an interactive
    ///   [`crate::host_key::HostKeyResolver`] so unknown host keys
    ///   trigger the modal TOFU prompt.
    /// * Integration tests — pass
    ///   [`crate::host_key::KnownHostsMode::AutoTrust`] against the
    ///   ephemeral paramiko mock server.
    ///
    /// The connection is inserted into the same process-wide pool the
    /// plain [`Self::open_live`] uses, so later cross-backend ops queue
    /// hits reuse the same handshake for free. Because the pool key
    /// only fingerprints `(kind, host, port, user, credentials)` — not
    /// options — a later plain `open_live` for the same target hits the
    /// already-authorised entry and never invokes the resolver again.
    ///
    /// # Errors
    ///
    /// Returns [`crate::backend::BackendError`] on ill-shaped URI or
    /// credentials. Handshake failures surface asynchronously via
    /// [`atlas_fs::ViewModelEvent::Error`], same as `open_live`.
    pub fn open_live_sftp_with_options(
        uri: RemoteUri,
        credentials: crate::backend::Credentials,
        opts: OpenOptions,
        sftp_opts: crate::vm::sftp::SftpOptions,
    ) -> Result<Arc<Self>, crate::backend::BackendError> {
        let pool = crate::pool::global();
        let key = crate::pool::PoolKey::new(
            BackendKind::Sftp,
            uri.host.clone().unwrap_or_default(),
            uri.port,
            uri.username.clone(),
            &credentials,
        );
        let client = pool.get_or_open(&key, || {
            let built: Arc<dyn BackendClient> =
                Arc::new(crate::vm::sftp::SftpBackend::with_options(
                    &uri,
                    credentials.clone(),
                    sftp_opts.clone(),
                )?);
            Ok(built)
        })?;
        Ok(Self::from_client(uri, BackendKind::Sftp, client, opts))
    }

    /// Construct a live view model around an already-built backend
    /// client and start the background listing task.
    pub(crate) fn from_client(
        uri: RemoteUri,
        kind: BackendKind,
        client: Arc<dyn BackendClient>,
        opts: OpenOptions,
    ) -> Arc<Self> {
        let (compiled, filter_err) = match opts.filter.compile() {
            Ok(c) => (c, None),
            Err(e) => (
                Filter::default()
                    .compile()
                    .expect("empty filter always compiles"),
                Some(e.to_string()),
            ),
        };
        let filter = if filter_err.is_some() {
            Filter::default()
        } else {
            opts.filter.clone()
        };

        let inner = Inner {
            raw: Vec::new(),
            view: Vec::new(),
            sort: opts.sort.clone(),
            filter,
            compiled,
            loaded: false,
        };

        let handle = common::resolve_runtime_handle();
        let path_cache = PathBuf::from(&uri.path);

        let this = Arc::new(Self {
            uri: uri.clone(),
            kind,
            path_cache,
            client,
            state: RwLock::new(inner),
            subscribers: Mutex::new(Vec::new()),
            runtime: handle.clone(),
            retry_policy: RwLock::new(default_retry_policy()),
            retry_hooks: RetryHooks::new(),
        });

        if let Some(msg) = filter_err {
            this.notify(ViewModelEvent::Error(msg));
        }

        let worker = Arc::clone(&this);
        // The backend's `root` (constructed in each `*::new`) already
        // encodes the URI path, so the initial listing lives at "" —
        // the backend's own root. Passing the URI path again here
        // would double-prepend it (e.g. list "atlas/atlas/") and
        // produce a spurious NotFound.
        let list_path = String::new();
        handle.spawn(async move {
            worker.run_loader(list_path).await;
        });

        this
    }

    /// The full remote URI this view model represents.
    #[must_use]
    pub fn remote_uri(&self) -> &RemoteUri {
        &self.uri
    }

    /// The backend kind (sftp/s3/…) driving this view model.
    #[must_use]
    pub fn backend_kind(&self) -> BackendKind {
        self.kind
    }

    /// Read the contents of `path` from the remote store.
    ///
    /// # Errors
    ///
    /// Propagates any [`RemoteError`] surfaced by the backend after
    /// the retry envelope has exhausted its budget.
    pub async fn read(&self, path: &str) -> RemoteResult<Vec<u8>> {
        let policy = self.retry_policy();
        let hooks = self.retry_hooks.clone();
        with_retry("remote.read", &policy, Some(&hooks), || {
            let client = Arc::clone(&self.client);
            let path = path.to_owned();
            async move { client.read(&path).await }
        })
        .await
    }

    /// Fetch a single entry's metadata (size, kind, modified time).
    ///
    /// # Errors
    ///
    /// Propagates any [`RemoteError`] surfaced by the backend after
    /// the retry envelope has exhausted its budget.
    pub async fn stat(&self, path: &str) -> RemoteResult<RemoteMetadata> {
        let policy = self.retry_policy();
        let hooks = self.retry_hooks.clone();
        with_retry("remote.stat", &policy, Some(&hooks), || {
            let client = Arc::clone(&self.client);
            let path = path.to_owned();
            async move { client.stat(&path).await }
        })
        .await
    }

    /// Upload `bytes` to `path`, replacing any existing object.
    ///
    /// # Errors
    ///
    /// Propagates any [`RemoteError`] surfaced by the backend after
    /// the retry envelope has exhausted its budget.
    pub async fn write(&self, path: &str, bytes: Vec<u8>) -> RemoteResult<()> {
        let policy = self.retry_policy();
        let hooks = self.retry_hooks.clone();
        with_retry("remote.write", &policy, Some(&hooks), || {
            let client = Arc::clone(&self.client);
            let path = path.to_owned();
            let bytes = bytes.clone();
            async move { client.write(&path, bytes).await }
        })
        .await
    }

    /// Create a directory at `path`. Backends without a first-class
    /// "directory" concept synthesise one via a zero-byte marker.
    ///
    /// # Errors
    ///
    /// Propagates any [`RemoteError`] surfaced by the backend after
    /// the retry envelope has exhausted its budget.
    pub async fn create_dir(&self, path: &str) -> RemoteResult<()> {
        let policy = self.retry_policy();
        let hooks = self.retry_hooks.clone();
        with_retry("remote.mkdir", &policy, Some(&hooks), || {
            let client = Arc::clone(&self.client);
            let path = path.to_owned();
            async move { client.create_dir(&path).await }
        })
        .await
    }

    /// Rename `from` to `to`. Both paths are interpreted relative to
    /// the backend's root.
    ///
    /// # Errors
    ///
    /// Propagates any [`RemoteError`] surfaced by the backend after
    /// the retry envelope has exhausted its budget.
    pub async fn rename(&self, from: &str, to: &str) -> RemoteResult<()> {
        let policy = self.retry_policy();
        let hooks = self.retry_hooks.clone();
        with_retry("remote.rename", &policy, Some(&hooks), || {
            let client = Arc::clone(&self.client);
            let from = from.to_owned();
            let to = to.to_owned();
            async move { client.rename(&from, &to).await }
        })
        .await
    }

    /// Delete `path`. Absent entries surface as
    /// [`crate::error::RemoteErrorKind::NotFound`].
    ///
    /// # Errors
    ///
    /// Propagates any [`RemoteError`] surfaced by the backend after
    /// the retry envelope has exhausted its budget.
    pub async fn delete(&self, path: &str) -> RemoteResult<()> {
        let policy = self.retry_policy();
        let hooks = self.retry_hooks.clone();
        with_retry("remote.delete", &policy, Some(&hooks), || {
            let client = Arc::clone(&self.client);
            let path = path.to_owned();
            async move { client.delete(&path).await }
        })
        .await
    }

    /// Open a streaming reader for `path`, optionally bounded by
    /// `total` bytes.
    ///
    /// The default implementation fetches the whole payload via
    /// `read()` and wraps it in an in-memory cursor. Backend
    /// implementations that support ranged GETs (S3, WebDAV) may
    /// override this in the future to avoid buffering — the trait
    /// door is open.
    ///
    /// # Errors
    ///
    /// Propagates any [`RemoteError`] surfaced by the backend.
    pub async fn reader(&self, path: &str, _total: Option<u64>) -> RemoteResult<BoxedAsyncRead> {
        let bytes = self.client.read(path).await?;
        Ok(common::cursor_reader(bytes))
    }

    /// Open a streaming writer for `path`.
    ///
    /// Currently buffers all writes into memory and uploads on
    /// `close()` — sufficient for atlas-ops' cross-backend copy of
    /// small-to-medium files.
    ///
    /// # Errors
    ///
    /// Always succeeds today; errors surface on `close()` when the
    /// underlying backend rejects the upload.
    pub async fn writer(&self, path: &str) -> RemoteResult<BoxedAsyncWrite> {
        let backend = Arc::clone(&self.client);
        let writer =
            common::BufferedRemoteWriter::new(backend, path.to_owned(), self.runtime.clone());
        Ok(Box::pin(writer))
    }

    /// Access the raw backend client. Rarely needed; usually one of
    /// the async methods above is more convenient.
    #[must_use]
    pub fn client(&self) -> &Arc<dyn BackendClient> {
        &self.client
    }

    /// Read a symbolic link at `path` and return the [`atlas_core::Location`]
    /// its target resolves to.
    ///
    /// The link's target is resolved relative to `path`'s parent (so a
    /// backend that returns `../foo/bar` from `read_link("baz/link")`
    /// yields `<root>/foo/bar`). Absolute targets are honoured
    /// verbatim. The target is stat'd to confirm reachability before
    /// the [`Location`] is returned — callers can therefore trust the
    /// result is safe to hand to the view controller.
    ///
    /// Backends that don't support symlinks
    /// (WebDAV / S3 / plain FTP) surface
    /// [`crate::error::RemoteErrorKind::Unsupported`].
    ///
    /// # Errors
    ///
    /// Propagates the backend's `read_link` and `stat` errors after
    /// the retry envelope.
    pub async fn follow_symlink(&self, path: &str) -> RemoteResult<atlas_core::Location> {
        let target_raw = self.client.read_link(path).await?;
        let resolved_path = resolve_symlink_target(path, &target_raw);
        // Stat the target to bubble up NotFound / permission errors
        // before we hand the location off to the view controller.
        let _target_meta = self.client.stat(&resolved_path).await?;
        let mut new_uri = self.uri.clone();
        new_uri.path = resolved_path;
        Ok(atlas_core::Location::Remote(new_uri, self.kind))
    }

    /// Read-only snapshot of the retry policy this view model uses.
    #[must_use]
    pub fn retry_policy(&self) -> RetryPolicy {
        *self.retry_policy.read()
    }

    /// Overwrite the retry policy. Tests and dynamic reconfiguration
    /// (config-file reload) use this.
    pub fn set_retry_policy(&self, policy: RetryPolicy) {
        *self.retry_policy.write() = policy;
    }

    /// Clone the retry-observer sink. Callers (ops panel, status bar)
    /// attach their own observers via this handle.
    #[must_use]
    pub fn retry_hooks(&self) -> RetryHooks {
        self.retry_hooks.clone()
    }

    fn notify(&self, event: ViewModelEvent) {
        let mut subs = self.subscribers.lock();
        subs.retain(|tx| tx.send(event.clone()).is_ok());
    }

    async fn run_loader(self: Arc<Self>, list_path: String) {
        let policy = self.retry_policy();
        let hooks = self.retry_hooks.clone();
        let list_result = with_retry("remote.list", &policy, Some(&hooks), || {
            let client = Arc::clone(&self.client);
            let path = list_path.clone();
            async move { client.list(&path).await }
        })
        .await;
        let entries = match list_result {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(path = %list_path, error = %e, "remote lister failed");
                self.notify(ViewModelEvent::Error(e.to_string()));
                {
                    let mut state = self.state.write();
                    state.loaded = true;
                }
                self.notify(ViewModelEvent::Loaded);
                return;
            }
        };

        let mut batch: Vec<Entry> = Vec::with_capacity(entries.len());
        for e in entries {
            if let Some(entry) = build_atlas_entry(&self.uri, &e) {
                batch.push(entry);
            }
        }

        let first_load;
        {
            let mut state = self.state.write();
            first_load = !state.loaded;
            state.raw.extend(batch);
            state.loaded = true;
            state.recompute();
        }
        if first_load {
            self.notify(ViewModelEvent::Loaded);
        }
        self.notify(ViewModelEvent::EntriesChanged);
    }
}

impl LocationViewModel for RemoteLocationViewModel {
    fn location(&self) -> &Path {
        &self.path_cache
    }

    fn entries(&self) -> Vec<Entry> {
        self.state.read().view.clone()
    }

    fn len(&self) -> usize {
        self.state.read().view.len()
    }

    fn is_loaded(&self) -> bool {
        self.state.read().loaded
    }

    fn sort(&self) -> SortSpec {
        self.state.read().sort.clone()
    }

    fn set_sort(&self, spec: SortSpec) {
        {
            let mut state = self.state.write();
            state.sort = spec;
            state.recompute();
        }
        self.notify(ViewModelEvent::EntriesChanged);
    }

    fn filter(&self) -> Filter {
        self.state.read().filter.clone()
    }

    fn set_filter(&self, filter: Filter) -> AtlasResult<()> {
        let compiled = filter.compile()?;
        {
            let mut state = self.state.write();
            state.filter = filter;
            state.compiled = compiled;
            state.recompute();
        }
        self.notify(ViewModelEvent::EntriesChanged);
        Ok(())
    }

    fn subscribe(&self) -> Receiver<ViewModelEvent> {
        let (tx, rx) = crossbeam_channel::unbounded();
        self.subscribers.lock().push(tx);
        rx
    }
}

/// Convert a raw [`RemoteEntry`] into an [`atlas_fs::Entry`], skipping
/// the listing root when the backend surfaces it.
fn build_atlas_entry(uri: &RemoteUri, e: &RemoteEntry) -> Option<Entry> {
    if e.path.trim_end_matches('/') == uri.path.trim_end_matches('/') {
        return None;
    }
    if e.path.is_empty() {
        return None;
    }
    let name = common::basename(&e.path);
    if name.is_empty() {
        return None;
    }
    // Symlinks: the backend's SFTP list has already stat-followed the
    // target and stored its kind in `e.mode` (so nav/dispatch works
    // transparently for symlinks-to-dirs and symlinks-to-files). We
    // *do* emit an explicit `EntryKind::Symlink { .. }` when the
    // target could not be resolved — that is the "broken symlink"
    // case, and the view can render the ↳ glyph accordingly.
    let kind = match (e.mode, e.symlink_target.as_ref()) {
        (crate::error::RemoteMode::Other, Some(target)) => EntryKind::Symlink {
            target: Some(PathBuf::from(target)),
            broken: true,
        },
        (crate::error::RemoteMode::File, _) => EntryKind::File,
        (crate::error::RemoteMode::Dir, _) => EntryKind::Dir,
        (crate::error::RemoteMode::Other, None) => EntryKind::Other,
    };
    let size = if matches!(e.mode, crate::error::RemoteMode::Dir) {
        0
    } else {
        e.size
    };
    Some(Entry {
        path: PathBuf::from(&e.path),
        name: name.clone(),
        kind,
        metadata: Metadata {
            size,
            modified: e.modified,
            created: None,
            accessed: None,
            permissions_mode: None,
            // Match the local walker: dotfile-style entries are marked
            // hidden so the per-pane `Filter::include_hidden` toggle
            // (Cmd+.) applies uniformly to remote and local panes.
            // Every backend's list() surfaces dot-prefixed entries
            // (verified in `tests/dotfiles_*.rs`) so this is the only
            // hidden-classification hook that runs for remote entries.
            is_hidden: name.starts_with('.'),
        },
    })
}

/// Resolve a symlink target (as returned by
/// [`BackendClient::read_link`]) against the link's *own* path.
///
/// * Absolute targets (starting with `/`) are honoured verbatim.
/// * Relative targets are joined against the link's parent directory.
/// * `..` segments walk up; `.` segments are elided.
fn resolve_symlink_target(link_path: &str, target: &str) -> String {
    if target.starts_with('/') {
        return normalise_dotdots(target);
    }
    let parent_owned = std::path::Path::new(link_path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = if parent_owned.is_empty() {
        "/"
    } else {
        parent_owned.as_str()
    };
    let joined = if parent.ends_with('/') {
        format!("{parent}{target}")
    } else {
        format!("{parent}/{target}")
    };
    normalise_dotdots(&joined)
}

/// Very small POSIX-style path collapser: drop `.`, walk `..` up,
/// keep the leading `/` if present. Used only by
/// [`resolve_symlink_target`] — never call this on Windows-style
/// paths.
fn normalise_dotdots(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

// A cross-crate error alias that keeps rustdoc happy — the outer
// module's public error type is [`RemoteError`].
type _RemoteError = RemoteError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_atlas_entry_skips_root() {
        let uri = RemoteUri {
            scheme: "sftp".into(),
            host: Some("h".into()),
            port: None,
            username: None,
            path: "/foo".into(),
            credential_ref: None,
        };
        let root_marker = RemoteEntry {
            path: "/foo".into(),
            mode: crate::error::RemoteMode::Dir,
            size: 0,
            modified: None,
            symlink_target: None,
        };
        assert!(build_atlas_entry(&uri, &root_marker).is_none());
    }

    #[test]
    fn build_atlas_entry_yields_child() {
        let uri = RemoteUri {
            scheme: "sftp".into(),
            host: Some("h".into()),
            port: None,
            username: None,
            path: "/".into(),
            credential_ref: None,
        };
        let child = RemoteEntry {
            path: "hello.txt".into(),
            mode: crate::error::RemoteMode::File,
            size: 5,
            modified: None,
            symlink_target: None,
        };
        let atlas_e = build_atlas_entry(&uri, &child).expect("child kept");
        assert_eq!(atlas_e.name, "hello.txt");
        assert_eq!(atlas_e.metadata.size, 5);
        assert!(matches!(atlas_e.kind, EntryKind::File));
    }

    #[test]
    fn build_atlas_entry_symlink_to_dir_becomes_dir() {
        // A resolvable symlink pointing at a directory should surface
        // as `EntryKind::Dir` so dispatch (fs::View → navigate) works
        // transparently.
        let uri = RemoteUri {
            scheme: "sftp".into(),
            host: Some("h".into()),
            port: None,
            username: None,
            path: "/".into(),
            credential_ref: None,
        };
        let link = RemoteEntry {
            path: "docs".into(),
            mode: crate::error::RemoteMode::Dir,
            size: 0,
            modified: None,
            symlink_target: Some("/srv/docs".into()),
        };
        let entry = build_atlas_entry(&uri, &link).expect("link kept");
        assert!(matches!(entry.kind, EntryKind::Dir));
    }

    #[test]
    fn build_atlas_entry_broken_symlink_becomes_symlink_kind() {
        // Broken symlinks — where stat-follow failed — surface as
        // `EntryKind::Symlink { broken: true, .. }` so the view
        // renders the ↳ glyph and skips activation.
        let uri = RemoteUri {
            scheme: "sftp".into(),
            host: Some("h".into()),
            port: None,
            username: None,
            path: "/".into(),
            credential_ref: None,
        };
        let link = RemoteEntry {
            path: "dangling".into(),
            mode: crate::error::RemoteMode::Other,
            size: 0,
            modified: None,
            symlink_target: Some("/nope/never".into()),
        };
        let entry = build_atlas_entry(&uri, &link).expect("link kept");
        match entry.kind {
            EntryKind::Symlink { broken, target } => {
                assert!(broken, "expected broken=true");
                assert_eq!(
                    target.as_ref().and_then(|p| p.to_str()),
                    Some("/nope/never"),
                );
            }
            other => panic!("expected Symlink, got {other:?}"),
        }
    }

    #[test]
    fn resolve_symlink_target_absolute() {
        assert_eq!(
            resolve_symlink_target("/a/b/link", "/etc/hosts"),
            "/etc/hosts",
        );
    }

    #[test]
    fn resolve_symlink_target_relative() {
        assert_eq!(resolve_symlink_target("/a/b/link", "target"), "/a/b/target",);
    }

    #[test]
    fn resolve_symlink_target_relative_dotdot() {
        assert_eq!(resolve_symlink_target("/a/b/link", "../c/d"), "/a/c/d",);
    }

    #[test]
    fn resolve_symlink_target_relative_root() {
        // Link at root: parent is "/", target "sibling" → "/sibling".
        assert_eq!(resolve_symlink_target("/link", "sibling"), "/sibling");
    }
}
