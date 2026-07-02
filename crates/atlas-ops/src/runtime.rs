//! Shared tokio runtime used by [`crate::execute::execute_op`].
//!
//! Every operation the queue picks up is dispatched onto this runtime
//! via [`shared_runtime_handle`]. Using a single background runtime —
//! instead of spinning one per worker thread — keeps the process
//! thread count bounded and lets `atlas-remote`'s per-backend
//! connection state (`RemoteLocationViewModel::open_live`) share a
//! runtime with the ops loop.
//!
//! The runtime is created lazily on first use so tests that never
//! touch remote paths don't pay the setup cost.

use once_cell::sync::OnceCell;
use tokio::runtime::{Handle, Runtime};

pub(crate) fn shared_runtime_handle() -> Handle {
    Handle::try_current().unwrap_or_else(|_| worker_runtime().handle().clone())
}

fn worker_runtime() -> &'static Runtime {
    static WORKER: OnceCell<Runtime> = OnceCell::new();
    WORKER.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("atlas-ops-worker")
            .worker_threads(2)
            .build()
            .expect("build atlas-ops worker runtime")
    })
}
