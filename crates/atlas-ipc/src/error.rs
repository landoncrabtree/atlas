//! IPC error types.

use std::path::PathBuf;

use crate::protocol::ErrorCode;

/// Errors that can occur in atlas-ipc operations.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode/decode: {0}")]
    Codec(String),
    #[error("frame too large: {0} bytes")]
    FrameTooLarge(usize),
    #[error("protocol mismatch: server={server}, client={client}")]
    ProtocolMismatch { server: u32, client: u32 },
    #[error("not running: no listener at {path:?}")]
    NotRunning { path: PathBuf },
    #[error("connection closed")]
    Closed,
    #[error("request timed out")]
    Timeout,
    #[error("server error [{code:?}]: {message}")]
    ServerError { code: ErrorCode, message: String },
}

/// Convenience alias for IpcError results.
pub type Result<T> = std::result::Result<T, IpcError>;
