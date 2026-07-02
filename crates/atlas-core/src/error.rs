use std::io;
use std::path::PathBuf;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, AtlasError>;

#[derive(Debug, Error)]
pub enum AtlasError {
    #[error("io error at {path:?}: {source}")]
    Io {
        path: Option<PathBuf>,
        #[source]
        source: io::Error,
    },

    #[error("invalid path: {0}")]
    InvalidPath(String),

    #[error("operation cancelled")]
    Cancelled,

    /// A remote backend rejected our credentials, or the current
    /// session has become unauthorized. Distinct from a generic
    /// `Other` so higher layers (e.g. the ops panel's status chip)
    /// can suggest "Reconnect" without regex-matching error text.
    #[error("auth required for {location}: {detail}")]
    AuthRequired {
        /// Human-readable identifier for the affected location
        /// (typically the URI display form).
        location: String,
        /// Backend-supplied detail message.
        detail: String,
    },

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

impl AtlasError {
    pub fn io(path: impl Into<Option<PathBuf>>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// Construct an [`AtlasError::AuthRequired`].
    pub fn auth_required(location: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::AuthRequired {
            location: location.into(),
            detail: detail.into(),
        }
    }
}

impl From<io::Error> for AtlasError {
    fn from(source: io::Error) -> Self {
        Self::Io { path: None, source }
    }
}
