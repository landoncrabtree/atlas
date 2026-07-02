//! Shared infrastructure for the per-backend view models.
//!
//! # Async → sync bridge
//!
//! [`atlas_fs::LocationViewModel`] is intentionally synchronous at the
//! consumer API (view controllers subscribe to change events, then
//! poll snapshots). Remote backends however are naturally async.
//! Each backend adapter here implements [`BackendClient`]; the outer
//! [`super::RemoteLocationViewModel`] wrapper then handles:
//!
//! 1. Owning a shared tokio runtime handle (the caller's runtime when
//!    one is available, otherwise a lazily-initialised worker
//!    runtime).
//! 2. Spawning a background listing task that pushes results into the
//!    same in-memory buffer that [`atlas_fs::InMemoryLocationViewModel`]
//!    uses.
//! 3. Notifying subscribers via a crossbeam channel without ever
//!    awaiting on the consumer side.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::SystemTime;

use async_trait::async_trait;
use futures::io::{AsyncRead, AsyncWrite};
use once_cell::sync::OnceCell;
use tokio::runtime::{Handle, Runtime};

use crate::error::{RemoteError, RemoteMetadata, RemoteMode, RemoteResult};

/// Shared worker runtime used when no ambient tokio runtime is
/// available. A single multi-thread runtime backs all
/// [`super::RemoteLocationViewModel`] instances so we don't spawn a
/// thread pool per open pane.
///
/// # Note
///
/// The runtime factory is defined in [`crate::runtime`]; the local
/// wrapper is kept for the historical call sites so tests can still
/// grab the same handle via `common::worker_runtime`.
#[allow(dead_code)]
pub(crate) fn worker_runtime() -> &'static Runtime {
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

pub(crate) fn resolve_runtime_handle() -> Handle {
    crate::runtime::handle()
}

/// A directory-listing entry surfaced by a [`BackendClient`].
#[derive(Debug, Clone)]
pub struct RemoteEntry {
    /// Path returned by the backend, relative to the listing root.
    /// May or may not end with `/` — the caller uses `mode` to decide.
    pub path: String,
    /// Coarse kind — file / dir / other. For symlinks that could be
    /// resolved by the backend (SFTP: via `SFTP_STAT` on the target),
    /// this reflects the *target's* kind so navigation and preview
    /// dispatch work transparently. When the target is unresolvable
    /// (broken symlink) the mode falls back to
    /// [`RemoteMode::Other`] and [`Self::symlink_target`] is populated
    /// with the raw link target.
    pub mode: RemoteMode,
    /// Content length in bytes. Directories should report 0.
    pub size: u64,
    /// Modification timestamp when the backend surfaces one.
    pub modified: Option<SystemTime>,
    /// For symbolic links: the raw link target string as reported by
    /// the backend (SFTP `readlink`). `None` for regular files,
    /// directories, and backends that don't have a symlink concept
    /// (WebDAV, S3). Populated for both resolvable and broken
    /// symlinks so callers can display the target and offer
    /// "follow link" affordances.
    pub symlink_target: Option<String>,
}

/// A boxed async reader used by [`crate::stream::stream_copy`].
pub type BoxedAsyncRead = Pin<Box<dyn AsyncRead + Send + Unpin>>;

/// A boxed async writer used by [`crate::stream::stream_copy`].
pub type BoxedAsyncWrite = Pin<Box<dyn AsyncWrite + Send + Unpin>>;

/// The uniform contract every backend adapter implements.
///
/// Each method surfaces the same behaviour across
/// SFTP / FTP / WebDAV / S3 so higher layers never need to branch on
/// backend kind. Behavioural quirks (e.g. S3 treating rename as
/// copy+delete, or FTP requiring uppercase `MKD`) are hidden behind
/// the trait.
#[async_trait]
pub trait BackendClient: Send + Sync {
    /// List the directory at `path` (root-relative). An empty
    /// string means the operator's root.
    async fn list(&self, path: &str) -> RemoteResult<Vec<RemoteEntry>>;

    /// Read the entire file at `path` into memory.
    async fn read(&self, path: &str) -> RemoteResult<Vec<u8>>;

    /// Fetch a single entry's metadata.
    async fn stat(&self, path: &str) -> RemoteResult<RemoteMetadata>;

    /// Upload `bytes` to `path`, overwriting any existing object.
    async fn write(&self, path: &str, bytes: Vec<u8>) -> RemoteResult<()>;

    /// Create a directory at `path`. Backends that don't have a
    /// first-class "directory" concept (S3-flat namespaces) synthesise
    /// one via a zero-byte marker.
    async fn create_dir(&self, path: &str) -> RemoteResult<()>;

    /// Move / rename `from` → `to`.
    async fn rename(&self, from: &str, to: &str) -> RemoteResult<()>;

    /// Delete the object at `path`.
    async fn delete(&self, path: &str) -> RemoteResult<()>;

    /// Read the raw target of a symbolic link at `path`.
    ///
    /// Backends without a first-class symlink concept
    /// (WebDAV, S3, plain FTP) return
    /// [`RemoteErrorKind::Unsupported`](crate::error::RemoteErrorKind::Unsupported).
    /// SFTP overrides this with `SSH_FXP_READLINK`.
    async fn read_link(&self, _path: &str) -> RemoteResult<String> {
        Err(RemoteError::unsupported(
            "backend does not support symbolic links",
        ))
    }
}

/// AsyncRead built from an in-memory buffer. Used by
/// [`super::RemoteLocationViewModel::reader`] for backends that don't
/// implement ranged streaming. The double-boxing via
/// [`BoxedAsyncRead`] keeps the outer signature simple.
pub(crate) fn cursor_reader(bytes: Vec<u8>) -> BoxedAsyncRead {
    Box::pin(futures::io::Cursor::new(bytes))
}

/// Buffered AsyncWrite that streams into memory and, on `close()`,
/// uploads the payload via the wrapped `BackendClient`.
///
/// This lets `stream_copy` treat every backend the same way even when
/// the underlying crate (e.g. `object_store` for very small objects)
/// prefers "put whole payload" over multipart streaming.
pub(crate) struct BufferedRemoteWriter {
    buffer: Vec<u8>,
    /// Taken on first `poll_close`; `None` afterwards.
    backend: Option<Arc<dyn BackendClient>>,
    path: String,
    handle: Handle,
    finalize: Option<tokio::task::JoinHandle<io::Result<()>>>,
    closed: bool,
}

impl BufferedRemoteWriter {
    pub(crate) fn new(backend: Arc<dyn BackendClient>, path: String, handle: Handle) -> Self {
        Self {
            buffer: Vec::new(),
            backend: Some(backend),
            path,
            handle,
            finalize: None,
            closed: false,
        }
    }
}

impl AsyncWrite for BufferedRemoteWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.buffer.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.closed {
            return Poll::Ready(Ok(()));
        }
        if self.finalize.is_none() {
            // Spawn the upload task on the runtime handle we captured at
            // construction — we can't rely on the current-thread
            // context inside poll_close.
            let backend = self
                .backend
                .take()
                .expect("BufferedRemoteWriter close bug: backend already taken");
            let bytes = std::mem::take(&mut self.buffer);
            let path = std::mem::take(&mut self.path);
            let jh = self
                .handle
                .spawn(async move { backend.write(&path, bytes).await.map_err(io::Error::from) });
            self.finalize = Some(jh);
        }
        let jh = self
            .finalize
            .as_mut()
            .expect("BufferedRemoteWriter finalize slot must be populated");
        let pinned = Pin::new(jh);
        match futures::Future::poll(pinned, cx) {
            Poll::Ready(Ok(res)) => {
                self.closed = true;
                Poll::Ready(res)
            }
            Poll::Ready(Err(join_err)) => {
                self.closed = true;
                Poll::Ready(Err(io::Error::other(join_err)))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Normalise `path` so backends' listers are happy:
///
///   * strip a leading `/` (relative-to-root),
///   * ensure a trailing `/` (directory listing convention), and
///   * treat an empty path as the root (`""`, not `"/"`).
pub(crate) fn normalized_list_path(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else if trimmed.ends_with('/') {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/")
    }
}

/// Return the last path component of `path` (post `/`-trim),
/// suitable for the `name` column in the file list.
pub(crate) fn basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(idx) => trimmed[idx + 1..].to_owned(),
        None => trimmed.to_owned(),
    }
}

/// Join `base` and `child`, collapsing any double slash. Handles the
/// three common shapes returned by the various listers:
/// `""`+`child`, `"foo/"`+`child`, `"foo"`+`child`.
pub(crate) fn join_path(base: &str, child: &str) -> String {
    if base.is_empty() {
        return child.trim_start_matches('/').to_owned();
    }
    let base = base.trim_end_matches('/');
    let child = child.trim_start_matches('/');
    if child.is_empty() {
        base.to_owned()
    } else {
        format!("{base}/{child}")
    }
}

/// Ensure `path` ends with a `/`. Used by S3 marker objects and
/// WebDAV directory URLs where trailing-slash semantics matter.
pub(crate) fn ensure_dir_slash(path: &str) -> String {
    if path.is_empty() {
        return "/".to_owned();
    }
    if path.ends_with('/') {
        path.to_owned()
    } else {
        format!("{path}/")
    }
}

/// Map a common family of `std::io::Error` kinds surfaced by network
/// crates into the closest [`RemoteError`] kind.
///
/// Used by backend adapters as `.map_err(map_io_err)?` shorthand.
#[allow(dead_code)]
pub(crate) fn map_io_err(err: io::Error) -> RemoteError {
    RemoteError::from(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_list_path_ensures_trailing_slash() {
        assert_eq!(normalized_list_path(""), "");
        assert_eq!(normalized_list_path("/"), "");
        assert_eq!(normalized_list_path("/foo"), "foo/");
        assert_eq!(normalized_list_path("/foo/"), "foo/");
        assert_eq!(normalized_list_path("foo/bar"), "foo/bar/");
    }

    #[test]
    fn basename_grabs_last_component() {
        assert_eq!(basename(""), "");
        assert_eq!(basename("foo"), "foo");
        assert_eq!(basename("foo/"), "foo");
        assert_eq!(basename("foo/bar.txt"), "bar.txt");
        assert_eq!(basename("a/b/c/"), "c");
    }

    #[test]
    fn join_path_collapses_slashes() {
        assert_eq!(join_path("", "a.txt"), "a.txt");
        assert_eq!(join_path("", "/a.txt"), "a.txt");
        assert_eq!(join_path("foo", "a.txt"), "foo/a.txt");
        assert_eq!(join_path("foo/", "a.txt"), "foo/a.txt");
        assert_eq!(join_path("foo/", "/a.txt"), "foo/a.txt");
    }

    #[test]
    fn map_io_err_preserves_kind() {
        let err = io::Error::from(io::ErrorKind::NotFound);
        let re = map_io_err(err);
        assert!(matches!(re.kind(), crate::error::RemoteErrorKind::NotFound));
    }

    #[test]
    fn ensure_dir_slash_appends() {
        assert_eq!(ensure_dir_slash(""), "/");
        assert_eq!(ensure_dir_slash("foo"), "foo/");
        assert_eq!(ensure_dir_slash("foo/"), "foo/");
        assert_eq!(ensure_dir_slash("/"), "/");
    }
}
