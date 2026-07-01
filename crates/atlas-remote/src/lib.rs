//! Atlas remote-filesystem backends.
//!
//! This crate provides the OpenDAL-backed [`LocationViewModel`] implementation
//! and a thin credential wrapper over the OS keychain that lets Atlas talk to
//! SFTP, S3, WebDAV and FTP endpoints alongside the local filesystem.
//!
//! # Scope
//!
//! The crate is intentionally minimal — this is the Phase 2 *foundation* only:
//!
//!   * [`backend::open`] returns an [`atlas_fs::LocationViewModel`] for a
//!     [`atlas_core::Location`], delegating to
//!     [`atlas_fs::InMemoryLocationViewModel`] for [`atlas_core::BackendKind::Local`]
//!     and to [`opendal_vm::OpenDalLocationViewModel`] for every remote scheme.
//!   * [`secrets`] wraps the `keyring` crate so credentials can be stored
//!     out-of-tree from the workspace state.
//!
//! Higher-level policy (connection pooling, retries, saved-server catalogues)
//! lives in later phases. The [`stream`] module already provides a
//! chunked async→async copy pipeline that `atlas-ops` will reuse for
//! cross-backend transfers.
//!
//! # Async model
//!
//! The consumer API of [`atlas_fs::LocationViewModel`] stays **synchronous**:
//! views subscribe to change events and pull snapshots. Under the hood,
//! [`opendal_vm::OpenDalLocationViewModel`] owns a private tokio runtime handle
//! and drives OpenDAL's async API on a background task, appending results to
//! the same in-memory buffer that the local view model uses. See
//! [`OpenDalLocationViewModel`](opendal_vm::OpenDalLocationViewModel) for the
//! rationale.

#![deny(rustdoc::broken_intra_doc_links)]

pub mod backend;
pub mod opendal_vm;
pub mod secrets;
pub mod stream;

pub use backend::{open, BackendError, Credentials};
pub use opendal_vm::OpenDalLocationViewModel;
pub use secrets::{
    delete as delete_secret, retrieve as retrieve_secret, store as store_secret, SecretError,
};
pub use stream::{stream_copy, StreamProgress, DEFAULT_CHUNK_BYTES};
