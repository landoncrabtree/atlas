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
}

impl From<io::Error> for AtlasError {
    fn from(source: io::Error) -> Self {
        Self::Io { path: None, source }
    }
}
