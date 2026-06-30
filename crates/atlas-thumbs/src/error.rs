//! Error types for atlas-thumbs.

use thiserror::Error;

/// Errors that can occur during thumbnail generation or cache operations.
#[derive(Debug, Error)]
pub enum ThumbError {
    /// The file format is not supported for thumbnailing.
    #[error("unsupported format: {0:?}")]
    UnsupportedFormat(Option<String>),
    /// Image decoding failed.
    #[error("decode failed: {0}")]
    Decode(String),
    /// Image encoding failed.
    #[error("encode failed: {0}")]
    Encode(String),
    /// SQLite database error.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Image processing error.
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
}

/// Result type for atlas-thumbs operations.
pub type Result<T> = std::result::Result<T, ThumbError>;
