//! Cross-backend error, metadata and mode types.
//!
//! Every per-backend module ([`crate::vm::sftp`], [`crate::vm::ftp`],
//! [`crate::vm::webdav`], [`crate::vm::s3`]) uses these types on the
//! public API surface so callers (view controllers, `atlas-ops`,
//! integration tests) don't have to reach into the underlying crate's
//! bespoke error type.
//!
//! `RemoteError::kind()` mirrors the naming convention the OpenDAL
//! ancestor used, which keeps the integration tests' pattern matches
//! (`err.kind() == RemoteErrorKind::NotFound`) readable.

use std::io;

use thiserror::Error;

/// Kind classification for a [`RemoteError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemoteErrorKind {
    /// The object at the requested path does not exist.
    NotFound,
    /// The server rejected our credentials or the current session is
    /// not authorized to perform this operation.
    PermissionDenied,
    /// The backend implementation does not support this operation
    /// (e.g. `rename` on some S3-compatible stores).
    Unsupported,
    /// The object already exists and the operation refused to
    /// overwrite it.
    AlreadyExists,
    /// Transport-level failure (TCP, TLS, protocol handshake, DNS,
    /// unreachable host, timeout, …).
    Network,
    /// Any other backend-, transport-, or protocol-level failure.
    Unexpected,
}

/// A backend-agnostic error surfaced by the remote view models.
///
/// Backend adapters normalise their crate-specific error into a
/// [`RemoteErrorKind`] + a free-form message so callers can pattern-
/// match on `kind()` without pulling in every underlying crate's
/// error type.
#[derive(Debug, Error)]
#[error("{kind:?}: {message}")]
pub struct RemoteError {
    kind: RemoteErrorKind,
    message: String,
}

impl RemoteError {
    /// Construct a new [`RemoteError`] from a kind and a human-readable
    /// message.
    #[must_use]
    pub fn new(kind: RemoteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The kind bucket this error falls into.
    #[must_use]
    pub fn kind(&self) -> RemoteErrorKind {
        self.kind
    }

    /// The human-readable message. Suitable for user-facing status
    /// banners; not suitable for driving control flow.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Convenience: [`RemoteErrorKind::NotFound`] with `msg`.
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(RemoteErrorKind::NotFound, msg)
    }

    /// Convenience: [`RemoteErrorKind::PermissionDenied`] with `msg`.
    pub fn permission_denied(msg: impl Into<String>) -> Self {
        Self::new(RemoteErrorKind::PermissionDenied, msg)
    }

    /// Convenience: [`RemoteErrorKind::Unsupported`] with `msg`.
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::new(RemoteErrorKind::Unsupported, msg)
    }

    /// Convenience: [`RemoteErrorKind::Unexpected`] with `msg`.
    pub fn unexpected(msg: impl Into<String>) -> Self {
        Self::new(RemoteErrorKind::Unexpected, msg)
    }
}

impl From<io::Error> for RemoteError {
    fn from(err: io::Error) -> Self {
        let kind = match err.kind() {
            io::ErrorKind::NotFound => RemoteErrorKind::NotFound,
            io::ErrorKind::PermissionDenied => RemoteErrorKind::PermissionDenied,
            io::ErrorKind::AlreadyExists => RemoteErrorKind::AlreadyExists,
            io::ErrorKind::Unsupported => RemoteErrorKind::Unsupported,
            _ => RemoteErrorKind::Unexpected,
        };
        Self::new(kind, err.to_string())
    }
}

impl From<RemoteError> for io::Error {
    fn from(err: RemoteError) -> Self {
        let kind = match err.kind {
            RemoteErrorKind::NotFound => io::ErrorKind::NotFound,
            RemoteErrorKind::PermissionDenied => io::ErrorKind::PermissionDenied,
            RemoteErrorKind::AlreadyExists => io::ErrorKind::AlreadyExists,
            RemoteErrorKind::Unsupported => io::ErrorKind::Unsupported,
            RemoteErrorKind::Network => io::ErrorKind::ConnectionAborted,
            RemoteErrorKind::Unexpected => io::ErrorKind::Other,
        };
        io::Error::new(kind, err.message)
    }
}

/// Coarse filesystem "mode" — the smallest subset needed by consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteMode {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Anything else (symlink, socket, special, unknown).
    Other,
}

impl RemoteMode {
    /// Was this a regular file?
    #[must_use]
    pub fn is_file(&self) -> bool {
        matches!(self, RemoteMode::File)
    }

    /// Was this a directory?
    #[must_use]
    pub fn is_dir(&self) -> bool {
        matches!(self, RemoteMode::Dir)
    }
}

/// Metadata surfaced from a backend `stat()` call.
///
/// The three fields cover what every consumer (view model, tests,
/// atlas-ops) needs to render a row in the file list; if a specific
/// backend can supply more (permissions bits, checksums, …) that
/// can be added here without touching the trait.
#[derive(Debug, Clone)]
pub struct RemoteMetadata {
    mode: RemoteMode,
    content_length: u64,
    last_modified: Option<std::time::SystemTime>,
}

impl RemoteMetadata {
    /// Construct a new [`RemoteMetadata`].
    #[must_use]
    pub fn new(
        mode: RemoteMode,
        content_length: u64,
        last_modified: Option<std::time::SystemTime>,
    ) -> Self {
        Self {
            mode,
            content_length,
            last_modified,
        }
    }

    /// The coarse kind (file / dir / other).
    #[must_use]
    pub fn mode(&self) -> RemoteMode {
        self.mode
    }

    /// Size in bytes (0 for directories).
    #[must_use]
    pub fn content_length(&self) -> u64 {
        self.content_length
    }

    /// Modification timestamp, if the backend reports it.
    #[must_use]
    pub fn last_modified(&self) -> Option<std::time::SystemTime> {
        self.last_modified
    }
}

/// Convenience alias used by trait signatures below.
pub type RemoteResult<T> = Result<T, RemoteError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_helpers_produce_matching_variants() {
        assert_eq!(
            RemoteError::not_found("x").kind(),
            RemoteErrorKind::NotFound
        );
        assert_eq!(
            RemoteError::permission_denied("x").kind(),
            RemoteErrorKind::PermissionDenied
        );
        assert_eq!(
            RemoteError::unsupported("x").kind(),
            RemoteErrorKind::Unsupported
        );
        assert_eq!(
            RemoteError::unexpected("x").kind(),
            RemoteErrorKind::Unexpected
        );
    }

    #[test]
    fn io_error_maps_bidirectionally() {
        let io_nf = io::Error::from(io::ErrorKind::NotFound);
        let rem: RemoteError = io_nf.into();
        assert_eq!(rem.kind(), RemoteErrorKind::NotFound);
        let back: io::Error = rem.into();
        assert_eq!(back.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn mode_predicates_are_mutually_exclusive() {
        let f = RemoteMode::File;
        let d = RemoteMode::Dir;
        let o = RemoteMode::Other;
        assert!(f.is_file() && !f.is_dir());
        assert!(d.is_dir() && !d.is_file());
        assert!(!o.is_file() && !o.is_dir());
    }
}
