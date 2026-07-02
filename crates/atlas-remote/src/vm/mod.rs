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
pub(crate) mod sftp;
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
pub use common::{BackendClient, BoxedAsyncRead, BoxedAsyncWrite, RemoteEntry};

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
        let client: Arc<dyn BackendClient> = match kind {
            BackendKind::Local => {
                return Err(BackendError::UnsupportedBackend(
                    "local kind on remote location".to_owned(),
                ));
            }
            BackendKind::Sftp => Arc::new(crate::vm::sftp::SftpBackend::new(&uri, credentials)?),
            BackendKind::Ftp => Arc::new(crate::vm::ftp::FtpBackend::new(&uri, credentials)?),
            BackendKind::WebDav => {
                Arc::new(crate::vm::webdav::WebDavBackend::new(&uri, credentials)?)
            }
            BackendKind::S3 => Arc::new(crate::vm::s3::S3Backend::new(&uri, credentials)?),
        };
        Ok(Self::from_client(uri, kind, client, opts))
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
        });

        if let Some(msg) = filter_err {
            this.notify(ViewModelEvent::Error(msg));
        }

        let worker = Arc::clone(&this);
        let list_path = common::normalized_list_path(&uri.path);
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
    /// Propagates any [`RemoteError`] surfaced by the backend.
    pub async fn read(&self, path: &str) -> RemoteResult<Vec<u8>> {
        self.client.read(path).await
    }

    /// Fetch a single entry's metadata (size, kind, modified time).
    ///
    /// # Errors
    ///
    /// Propagates any [`RemoteError`] surfaced by the backend.
    pub async fn stat(&self, path: &str) -> RemoteResult<RemoteMetadata> {
        self.client.stat(path).await
    }

    /// Upload `bytes` to `path`, replacing any existing object.
    ///
    /// # Errors
    ///
    /// Propagates any [`RemoteError`] surfaced by the backend.
    pub async fn write(&self, path: &str, bytes: Vec<u8>) -> RemoteResult<()> {
        self.client.write(path, bytes).await
    }

    /// Create a directory at `path`. Backends without a first-class
    /// "directory" concept synthesise one via a zero-byte marker.
    ///
    /// # Errors
    ///
    /// Propagates any [`RemoteError`] surfaced by the backend.
    pub async fn create_dir(&self, path: &str) -> RemoteResult<()> {
        self.client.create_dir(path).await
    }

    /// Rename `from` to `to`. Both paths are interpreted relative to
    /// the backend's root.
    ///
    /// # Errors
    ///
    /// Propagates any [`RemoteError`] surfaced by the backend.
    pub async fn rename(&self, from: &str, to: &str) -> RemoteResult<()> {
        self.client.rename(from, to).await
    }

    /// Delete `path`. Absent entries surface as
    /// [`crate::error::RemoteErrorKind::NotFound`].
    ///
    /// # Errors
    ///
    /// Propagates any [`RemoteError`] surfaced by the backend.
    pub async fn delete(&self, path: &str) -> RemoteResult<()> {
        self.client.delete(path).await
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

    fn notify(&self, event: ViewModelEvent) {
        let mut subs = self.subscribers.lock();
        subs.retain(|tx| tx.send(event.clone()).is_ok());
    }

    async fn run_loader(self: Arc<Self>, list_path: String) {
        let entries = match self.client.list(&list_path).await {
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
    let kind = match e.mode {
        crate::error::RemoteMode::File => EntryKind::File,
        crate::error::RemoteMode::Dir => EntryKind::Dir,
        crate::error::RemoteMode::Other => EntryKind::Other,
    };
    let size = if matches!(e.mode, crate::error::RemoteMode::Dir) {
        0
    } else {
        e.size
    };
    Some(Entry {
        path: PathBuf::from(&e.path),
        name,
        kind,
        metadata: Metadata {
            size,
            modified: e.modified,
            created: None,
            accessed: None,
            permissions_mode: None,
            is_hidden: false,
        },
    })
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
        };
        let atlas_e = build_atlas_entry(&uri, &child).expect("child kept");
        assert_eq!(atlas_e.name, "hello.txt");
        assert_eq!(atlas_e.metadata.size, 5);
        assert!(matches!(atlas_e.kind, EntryKind::File));
    }
}
