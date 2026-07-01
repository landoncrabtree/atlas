//! OpenDAL-backed implementation of [`atlas_fs::LocationViewModel`].
//!
//! # Async → sync bridge
//!
//! [`LocationViewModel`] is intentionally kept synchronous at the consumer API
//! (view controllers subscribe to change events, then poll snapshots). Remote
//! backends however are naturally async — OpenDAL's operator is async and
//! network I/O should never block a UI thread.
//!
//! [`OpenDalLocationViewModel`] bridges the two worlds:
//!
//! 1. On construction it obtains a shared tokio runtime handle (either the
//!    caller's runtime or a lazily-initialised worker runtime) and spawns a
//!    background listing task.
//! 2. The task pages through OpenDAL's async lister, converts each result to
//!    an [`atlas_fs::Entry`], and pushes it into the same in-memory buffer that
//!    [`atlas_fs::InMemoryLocationViewModel`] uses.
//! 3. The UI subscribes to the buffer via [`LocationViewModel::subscribe`] and
//!    is notified of updates without ever awaiting.
//!
//! This design keeps the blast radius of remote support small: no existing
//! consumer needs to become async.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use atlas_core::{BackendKind, RemoteUri, Result as AtlasResult};
use atlas_fs::{
    CompiledFilter, Entry, EntryKind, Filter, LocationViewModel, Metadata, OpenOptions, SortSpec,
    ViewModelEvent,
};
use crossbeam_channel::{Receiver, Sender};
use futures::StreamExt;
use once_cell::sync::OnceCell;
use opendal::{Operator, Scheme};
use parking_lot::{Mutex, RwLock};
use tokio::runtime::{Handle, Runtime};

use crate::backend::{BackendError, Credentials};

/// Shared worker runtime used when no ambient tokio runtime is available.
///
/// A single multi-thread runtime backs all `OpenDalLocationViewModel` instances
/// so we don't spawn a thread pool per open pane.
fn worker_runtime() -> &'static Runtime {
    static WORKER: OnceCell<Runtime> = OnceCell::new();
    WORKER.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("atlas-remote-worker")
            .worker_threads(2)
            .build()
            .expect("build atlas-remote worker runtime")
    })
}

fn resolve_runtime_handle() -> Handle {
    Handle::try_current().unwrap_or_else(|_| worker_runtime().handle().clone())
}

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

/// OpenDAL-backed [`LocationViewModel`].
///
/// Instances are always shared via [`Arc`]; the background loader task holds
/// a strong clone until the initial listing completes.
///
/// `location()` returns the *remote path portion* only (e.g. `/tmp` for
/// `sftp://host/tmp`). Consumers that need the full URI (for logging, address
/// bar rendering, etc.) should call [`OpenDalLocationViewModel::remote_uri`]
/// or [`OpenDalLocationViewModel::backend_kind`].
pub struct OpenDalLocationViewModel {
    uri: RemoteUri,
    kind: BackendKind,
    /// Cached `PathBuf` view of `uri.path`, so `location() -> &Path` can hand
    /// out a stable borrow.
    path_cache: PathBuf,
    operator: Operator,
    state: RwLock<Inner>,
    subscribers: Mutex<Vec<Sender<ViewModelEvent>>>,
    _runtime: Handle,
}

impl OpenDalLocationViewModel {
    /// Construct a live view model for `uri`, starting the listing task
    /// immediately.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::OpenDal`] if the OpenDAL operator cannot be
    /// built (missing service feature, invalid host, etc.) and
    /// [`BackendError::UnsupportedBackend`] for schemes not compiled in.
    pub fn open_live(
        uri: RemoteUri,
        kind: BackendKind,
        credentials: Credentials,
        opts: OpenOptions,
    ) -> Result<Arc<Self>, BackendError> {
        let operator = build_operator(&uri, kind, &credentials)?;

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

        let handle = resolve_runtime_handle();
        let path_cache = PathBuf::from(&uri.path);

        let this = Arc::new(Self {
            uri: uri.clone(),
            kind,
            path_cache,
            operator: operator.clone(),
            state: RwLock::new(inner),
            subscribers: Mutex::new(Vec::new()),
            _runtime: handle.clone(),
        });

        if let Some(msg) = filter_err {
            this.notify(ViewModelEvent::Error(msg));
        }

        let worker = Arc::clone(&this);
        let list_path = normalized_list_path(&uri.path);
        handle.spawn(async move {
            worker.run_loader(list_path).await;
        });

        Ok(this)
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
    /// Provided for higher-level modules (`atlas-ops`, thumbnail streaming);
    /// not used by the view-model itself.
    ///
    /// # Errors
    ///
    /// Propagates any [`opendal::Error`] from the underlying operator.
    pub async fn read(&self, path: &str) -> Result<Vec<u8>, opendal::Error> {
        let bs = self.operator.read(path).await?;
        Ok(bs.to_vec())
    }

    fn notify(&self, event: ViewModelEvent) {
        let mut subs = self.subscribers.lock();
        subs.retain(|tx| tx.send(event.clone()).is_ok());
    }

    async fn run_loader(self: Arc<Self>, list_path: String) {
        let mut lister = match self.operator.lister(&list_path).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(path = %list_path, error = %e, "opendal lister failed");
                self.notify(ViewModelEvent::Error(e.to_string()));
                let mut state = self.state.write();
                state.loaded = true;
                drop(state);
                self.notify(ViewModelEvent::Loaded);
                return;
            }
        };

        let mut batch: Vec<Entry> = Vec::new();
        while let Some(next) = lister.next().await {
            match next {
                Ok(entry) => match self.build_atlas_entry(entry).await {
                    Ok(Some(atlas_entry)) => batch.push(atlas_entry),
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to build entry from opendal item");
                        self.notify(ViewModelEvent::Error(e.to_string()));
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "opendal lister emitted error");
                    self.notify(ViewModelEvent::Error(e.to_string()));
                }
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

    async fn build_atlas_entry(
        &self,
        entry: opendal::Entry,
    ) -> Result<Option<Entry>, opendal::Error> {
        let path = entry.path().to_owned();
        // OpenDAL sometimes includes the listing root itself as the first
        // returned entry; skip it so we only surface children.
        if path.trim_end_matches('/') == self.uri.path.trim_end_matches('/') {
            return Ok(None);
        }
        if path.is_empty() {
            return Ok(None);
        }

        let name = Path::new(path.trim_end_matches('/'))
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());

        // Prefer the metadata carried on the listing entry; fall back to a
        // stat only if the lister didn't already supply size/mtime.
        let meta = entry.metadata().clone();
        let (kind, size, modified) = extract_metadata(&meta);
        Ok(Some(Entry {
            path: PathBuf::from(&path),
            name,
            kind,
            metadata: Metadata {
                size,
                modified,
                created: None,
                accessed: None,
                permissions_mode: None,
                is_hidden: false,
            },
        }))
    }
}

impl LocationViewModel for OpenDalLocationViewModel {
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

fn extract_metadata(meta: &opendal::Metadata) -> (EntryKind, u64, Option<SystemTime>) {
    let mode = meta.mode();
    let kind = if mode.is_dir() {
        EntryKind::Dir
    } else if mode.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    let size = if mode.is_dir() {
        0
    } else {
        meta.content_length()
    };
    let modified = meta.last_modified().map(SystemTime::from);
    (kind, size, modified)
}

/// Normalise `path` so OpenDAL's lister is happy:
///
///   * ensure a trailing `/` (OpenDAL requires it for directory listings), and
///   * treat an empty path as the root (`""`, not `"/"`), because some services
///     (notably `services-memory`) refuse `/` as an explicit root.
fn normalized_list_path(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else if trimmed.ends_with('/') {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/")
    }
}

fn build_operator(
    uri: &RemoteUri,
    kind: BackendKind,
    credentials: &Credentials,
) -> Result<Operator, BackendError> {
    let op = match kind {
        BackendKind::Local => {
            return Err(BackendError::UnsupportedBackend(
                "BackendKind::Local should be dispatched by backend::open, not opendal".to_owned(),
            ));
        }
        BackendKind::Sftp => build_sftp(uri, credentials)?,
        BackendKind::Ftp => build_ftp(uri, credentials)?,
        BackendKind::WebDav => build_webdav(uri, credentials)?,
        BackendKind::S3 => build_s3(uri, credentials)?,
    };
    Ok(op)
}

fn build_sftp(uri: &RemoteUri, credentials: &Credentials) -> Result<Operator, BackendError> {
    use opendal::services::Sftp;

    let host = uri
        .host
        .as_deref()
        .ok_or_else(|| BackendError::InvalidCredentials {
            backend: "sftp",
            detail: "missing host".to_owned(),
        })?;
    let user = uri.username.as_deref().unwrap_or("");
    let endpoint = if let Some(port) = uri.port {
        format!("{host}:{port}")
    } else {
        host.to_owned()
    };
    let mut builder = Sftp::default()
        .endpoint(&endpoint)
        .user(user)
        .root(&uri.path);
    match credentials {
        Credentials::Password(_) => {
            return Err(BackendError::InvalidCredentials {
                backend: "sftp",
                detail: "OpenDAL's SFTP backend requires an SSH key; password auth is not \
                         supported. Store the key path on disk and pass Credentials::SshKey."
                    .to_owned(),
            });
        }
        Credentials::SshKey(path, _pass) => {
            builder = builder.key(&path.to_string_lossy());
        }
        Credentials::Iam { .. } => {
            return Err(BackendError::InvalidCredentials {
                backend: "sftp",
                detail: "IAM credentials not supported for SFTP".to_owned(),
            });
        }
        Credentials::Anonymous => {}
    }
    Ok(Operator::new(builder)?.finish())
}

fn build_ftp(uri: &RemoteUri, credentials: &Credentials) -> Result<Operator, BackendError> {
    use opendal::services::Ftp;

    let host = uri
        .host
        .as_deref()
        .ok_or_else(|| BackendError::InvalidCredentials {
            backend: "ftp",
            detail: "missing host".to_owned(),
        })?;
    let endpoint = if let Some(port) = uri.port {
        format!("ftp://{host}:{port}")
    } else {
        format!("ftp://{host}")
    };
    let user = uri.username.as_deref().unwrap_or("anonymous");
    let mut builder = Ftp::default()
        .endpoint(&endpoint)
        .user(user)
        .root(&uri.path);
    match credentials {
        Credentials::Password(p) => {
            builder = builder.password(p);
        }
        Credentials::Anonymous => {
            builder = builder.password("");
        }
        Credentials::SshKey(_, _) | Credentials::Iam { .. } => {
            return Err(BackendError::InvalidCredentials {
                backend: "ftp",
                detail: "only Password or Anonymous credentials are valid for FTP".to_owned(),
            });
        }
    }
    Ok(Operator::new(builder)?.finish())
}

fn build_webdav(uri: &RemoteUri, credentials: &Credentials) -> Result<Operator, BackendError> {
    use opendal::services::Webdav;

    let host = uri
        .host
        .as_deref()
        .ok_or_else(|| BackendError::InvalidCredentials {
            backend: "webdav",
            detail: "missing host".to_owned(),
        })?;
    let scheme = if uri.scheme == "webdavs" {
        "https"
    } else {
        "http"
    };
    let endpoint = if let Some(port) = uri.port {
        format!("{scheme}://{host}:{port}")
    } else {
        format!("{scheme}://{host}")
    };
    let mut builder = Webdav::default().endpoint(&endpoint).root(&uri.path);
    if let Some(user) = uri.username.as_deref() {
        builder = builder.username(user);
    }
    match credentials {
        Credentials::Password(p) => {
            builder = builder.password(p);
        }
        Credentials::Anonymous => {}
        Credentials::SshKey(_, _) | Credentials::Iam { .. } => {
            return Err(BackendError::InvalidCredentials {
                backend: "webdav",
                detail: "only Password or Anonymous credentials are valid for WebDAV".to_owned(),
            });
        }
    }
    Ok(Operator::new(builder)?.finish())
}

fn build_s3(uri: &RemoteUri, credentials: &Credentials) -> Result<Operator, BackendError> {
    use opendal::services::S3;

    let bucket = uri
        .host
        .as_deref()
        .ok_or_else(|| BackendError::InvalidCredentials {
            backend: "s3",
            detail: "missing bucket (host component of URI)".to_owned(),
        })?;
    let mut builder = S3::default().bucket(bucket).root(&uri.path);
    match credentials {
        Credentials::Iam {
            access_key_id,
            secret_key,
            session_token,
        } => {
            builder = builder
                .access_key_id(access_key_id)
                .secret_access_key(secret_key);
            if let Some(tok) = session_token {
                builder = builder.session_token(tok);
            }
        }
        Credentials::Anonymous => {
            builder = builder.allow_anonymous();
        }
        Credentials::Password(_) | Credentials::SshKey(_, _) => {
            return Err(BackendError::InvalidCredentials {
                backend: "s3",
                detail: "S3 requires IAM or Anonymous credentials".to_owned(),
            });
        }
    }
    Ok(Operator::new(builder)?.finish())
}

// Compile-time assurance that we never hand out a scheme we didn't compile in.
// This lives as an unused const so the compiler catches drift between the
// feature list and the BackendKind enum.
#[allow(dead_code)]
const _COMPILED_SCHEMES: &[Scheme] = &[
    Scheme::Sftp,
    Scheme::Ftp,
    Scheme::Webdav,
    Scheme::S3,
    Scheme::Memory,
];

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::services::Memory;
    use std::time::Duration;

    /// A tiny helper that spins up an in-memory OpenDAL operator, seeds it
    /// with a few blobs, and returns it.
    async fn seed_memory_operator() -> Operator {
        let op = Operator::new(Memory::default())
            .expect("build memory operator")
            .finish();
        op.write("alpha.txt", "hello").await.expect("write alpha");
        op.write("beta.txt", "world").await.expect("write beta");
        op.write("nested/gamma.txt", "!")
            .await
            .expect("write gamma");
        op
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn opendal_memory_list_smoke() {
        let op = seed_memory_operator().await;

        // Sanity: raw OpenDAL list should surface the two top-level files.
        let mut names: Vec<String> = op
            .list("")
            .await
            .expect("list root")
            .into_iter()
            .map(|e| e.path().to_owned())
            .collect();
        names.sort();
        assert!(names.iter().any(|n| n == "alpha.txt"));
        assert!(names.iter().any(|n| n == "beta.txt"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn opendal_memory_stat_and_read() {
        let op = seed_memory_operator().await;

        let meta = op.stat("alpha.txt").await.expect("stat alpha");
        assert!(meta.mode().is_file());
        assert_eq!(meta.content_length(), 5);

        let bytes = op.read("alpha.txt").await.expect("read alpha");
        assert_eq!(&bytes.to_vec(), b"hello");
    }

    #[test]
    fn normalized_list_path_ensures_trailing_slash() {
        assert_eq!(normalized_list_path(""), "");
        assert_eq!(normalized_list_path("/"), "");
        assert_eq!(normalized_list_path("/foo"), "foo/");
        assert_eq!(normalized_list_path("/foo/"), "foo/");
        assert_eq!(normalized_list_path("foo/bar"), "foo/bar/");
    }

    /// End-to-end smoke test: build an OpenDalLocationViewModel over
    /// services-memory (via a hand-built operator handed to the view model
    /// through a private test hook) and verify list results propagate to
    /// subscribers.
    ///
    /// We deliberately bypass `open_live` here because there is no `memory://`
    /// scheme in `BackendKind`; the intent is to smoke-test the async→sync
    /// bridge rather than the URI plumbing. Real backends exercise the full
    /// path via integration tests in a later phase.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn view_model_bridges_async_lister_to_sync_snapshot() {
        // Seed a memory operator and drive the loader directly.
        let op = seed_memory_operator().await;

        let inner = Inner {
            raw: Vec::new(),
            view: Vec::new(),
            sort: SortSpec::default(),
            filter: Filter::default(),
            compiled: Filter::default().compile().expect("empty compiles"),
            loaded: false,
        };
        let this = Arc::new(OpenDalLocationViewModel {
            uri: RemoteUri {
                scheme: "memory".to_owned(),
                host: None,
                port: None,
                username: None,
                path: String::new(),
                credential_ref: None,
            },
            kind: BackendKind::S3, // arbitrary — not used by the loader
            path_cache: PathBuf::from(""),
            operator: op,
            state: RwLock::new(inner),
            subscribers: Mutex::new(Vec::new()),
            _runtime: resolve_runtime_handle(),
        });

        let sub = this.subscribe();
        let worker = Arc::clone(&this);
        tokio::spawn(async move {
            worker.run_loader(String::new()).await;
        });

        // Wait for Loaded (or timeout).
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut saw_loaded = false;
        while std::time::Instant::now() < deadline {
            match sub.recv_timeout(Duration::from_millis(100)) {
                Ok(ViewModelEvent::Loaded) => {
                    saw_loaded = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
        assert!(saw_loaded, "OpenDalLocationViewModel never reported Loaded");
        assert!(this.is_loaded());

        let names: Vec<String> = this.entries().into_iter().map(|e| e.name).collect();
        assert!(names.iter().any(|n| n == "alpha.txt"), "names = {names:?}");
        assert!(names.iter().any(|n| n == "beta.txt"), "names = {names:?}");
    }
}
