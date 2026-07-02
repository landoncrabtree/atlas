//! Shared tokio runtime handle for every remote-backed operation.
//!
//! The single-runtime rule keeps the process thread count bounded:
//! all SFTP / FTP / WebDAV / S3 clients — plus every consumer that
//! needs to spawn a helper task on the runtime (preview cache,
//! background listing) — share the same handle.
//!
//! # Contract
//!
//! * If a tokio runtime is already installed on the current thread
//!   (typical for `atlas-app`'s ops queue), [`handle`] returns that
//!   handle so callers avoid a runtime hop.
//! * Otherwise a lazily-initialised, two-thread multi-thread runtime
//!   is spun up in the background and its handle is returned. The
//!   runtime is a `OnceCell`, so the second-and-later callers pay
//!   only the cost of an atomic load.
//!
//! # When to use
//!
//! Any crate that would otherwise call `tokio::runtime::Handle::try_current`
//! plus a private fallback must call this function instead. The
//! canonical example is [`RemoteLocationViewModel::from_client`],
//! which spawns the initial listing task; the preview cache in
//! `atlas-ui` uses the same handle for background downloads.

use once_cell::sync::OnceCell;
use tokio::runtime::{Handle, Runtime};

/// Return the shared tokio runtime handle: the ambient runtime when
/// one is installed on the current thread, otherwise the shared
/// worker runtime.
#[must_use]
pub fn handle() -> Handle {
    Handle::try_current().unwrap_or_else(|_| worker_runtime().handle().clone())
}

/// The lazy worker runtime used when no ambient tokio runtime is
/// available.
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
