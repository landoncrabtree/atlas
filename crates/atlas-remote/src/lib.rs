//! Atlas remote-filesystem backends.
//!
//! Provides a per-protocol Rust-native backend suite and a thin
//! credential wrapper over the OS keychain that lets Atlas talk to
//! SFTP, S3, WebDAV and FTP endpoints alongside the local
//! filesystem.
//!
//! # Scope
//!
//! The crate is intentionally minimal — this is the Phase 2
//! *foundation* only:
//!
//!   * [`backend::open`] returns an [`atlas_fs::LocationViewModel`]
//!     for a [`atlas_core::Location`], delegating to
//!     [`atlas_fs::InMemoryLocationViewModel`] for
//!     [`atlas_core::BackendKind::Local`] and to
//!     [`vm::RemoteLocationViewModel`] for every remote scheme.
//!   * [`secrets`] wraps the `keyring` crate so credentials can be
//!     stored out-of-tree from the workspace state.
//!
//! Higher-level policy (connection pooling, retries, saved-server
//! catalogues) lives in later phases. The [`stream`] module already
//! provides a chunked async→async copy pipeline that `atlas-ops`
//! will reuse for cross-backend transfers.
//!
//! # Cross-platform backend stack
//!
//! Phase 2.3.5 replaced the earlier OpenDAL dependency with four
//! pure-Rust crates so Windows builds work out of the box:
//!
//! | Backend | Crate |
//! |---------|-------|
//! | SFTP    | `russh` + `russh-sftp` |
//! | FTP     | `suppaftp` (sync + `spawn_blocking`) |
//! | WebDAV  | `reqwest` + `quick-xml` (roll-own) |
//! | S3      | `object_store` (Apache Arrow) |
//!
//! Each is exposed via [`vm::BackendClient`] and stitched into
//! [`vm::RemoteLocationViewModel`], preserving the async→sync bridge
//! that keeps [`atlas_fs::LocationViewModel`] a synchronous API.

#![deny(rustdoc::broken_intra_doc_links)]

pub mod backend;
pub mod error;
pub mod pool;
pub mod secrets;
pub mod stream;
pub mod vm;
pub mod walk;

pub use backend::{open, BackendError, Credentials};
pub use error::{RemoteError, RemoteErrorKind, RemoteMetadata, RemoteMode};
pub use pool::{
    ConnectionPool, PoolConfig, PoolKey, PoolStats, DEFAULT_IDLE_TTL, DEFAULT_MAX_CONNECTIONS,
};
pub use secrets::{
    delete as delete_secret, retrieve as retrieve_secret, store as store_secret, SecretError,
};
pub use stream::{stream_copy, StreamProgress, DEFAULT_CHUNK_BYTES};
pub use vm::RemoteLocationViewModel;
pub use walk::{enumerate_recursive, WalkEntry};
