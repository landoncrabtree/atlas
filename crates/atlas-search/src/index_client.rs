//! IPC-backed facade for Atlas path-index searches.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use atlas_index::{DocKind, Hit, Query, SearchOptions, SortBy};
use atlas_ipc::{
    client::Client,
    error::IpcError,
    protocol::{ErrorCode, Request, Response},
    transport::default_socket_path,
};

/// Serde mirror of [`atlas_index::Query`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QueryMirror {
    /// Mirrors [`Query::NamePrefix`].
    NamePrefix {
        /// Prefix value.
        value: String,
    },
    /// Mirrors [`Query::NameSubstring`].
    NameSubstring {
        /// Substring value.
        value: String,
    },
    /// Mirrors [`Query::NameFuzzy`].
    NameFuzzy {
        /// Term value.
        term: String,
        /// Edit distance.
        distance: u8,
    },
    /// Mirrors [`Query::Extension`].
    Extension {
        /// Extension value.
        value: String,
    },
    /// Mirrors [`Query::ExactPath`].
    ExactPath {
        /// Exact path.
        path: PathBuf,
    },
    /// Mirrors [`Query::All`].
    All {
        /// Nested queries.
        queries: Vec<QueryMirror>,
    },
    /// Mirrors [`Query::Any`].
    Any {
        /// Nested queries.
        queries: Vec<QueryMirror>,
    },
    /// Mirrors [`Query::InSubtree`].
    InSubtree {
        /// Root path.
        path: PathBuf,
    },
    /// Mirrors [`Query::KindAnyOf`].
    KindAnyOf {
        /// Kinds to match.
        kinds: Vec<DocKindMirror>,
    },
}

/// Serde mirror of [`atlas_index::SearchOptions`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchOptionsMirror {
    /// Maximum number of hits to return.
    pub limit: usize,
    /// Whether hidden entries are included.
    pub include_hidden: bool,
    /// Result ordering.
    pub sort: SortByMirror,
}

/// Serde mirror of [`atlas_index::SortBy`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortByMirror {
    /// Sort by relevance score.
    #[default]
    Score,
    /// Sort by name.
    Name,
    /// Sort by size.
    Size,
    /// Sort by modification time.
    Mtime,
}

/// Serde mirror of [`atlas_index::Hit`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitMirror {
    /// Full absolute path.
    pub path: PathBuf,
    /// Entry name.
    pub name: String,
    /// Parent directory.
    pub parent: PathBuf,
    /// Lowercased extension without dot.
    pub extension: Option<String>,
    /// Entry kind.
    pub kind: DocKindMirror,
    /// Entry size.
    pub size: u64,
    /// Modification time as Unix seconds.
    pub mtime: Option<i64>,
    /// Relevance score.
    pub score: f32,
}

/// Serde mirror of [`atlas_index::DocKind`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocKindMirror {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link.
    Symlink,
    /// Other filesystem node.
    Other,
}

impl From<&Query> for QueryMirror {
    fn from(value: &Query) -> Self {
        match value {
            Query::NamePrefix(value) => Self::NamePrefix {
                value: value.clone(),
            },
            Query::NameSubstring(value) => Self::NameSubstring {
                value: value.clone(),
            },
            Query::NameFuzzy { term, distance } => Self::NameFuzzy {
                term: term.clone(),
                distance: *distance,
            },
            Query::Extension(value) => Self::Extension {
                value: value.clone(),
            },
            Query::ExactPath(path) => Self::ExactPath { path: path.clone() },
            Query::All(queries) => Self::All {
                queries: queries.iter().map(Self::from).collect(),
            },
            Query::Any(queries) => Self::Any {
                queries: queries.iter().map(Self::from).collect(),
            },
            Query::InSubtree(path) => Self::InSubtree { path: path.clone() },
            Query::KindAnyOf(kinds) => Self::KindAnyOf {
                kinds: kinds.iter().copied().map(DocKindMirror::from).collect(),
            },
        }
    }
}

impl From<&SearchOptions> for SearchOptionsMirror {
    fn from(value: &SearchOptions) -> Self {
        Self {
            limit: value.limit,
            include_hidden: value.include_hidden,
            sort: value.sort.into(),
        }
    }
}

impl From<SortBy> for SortByMirror {
    fn from(value: SortBy) -> Self {
        match value {
            SortBy::Score => Self::Score,
            SortBy::Name => Self::Name,
            SortBy::Size => Self::Size,
            SortBy::Mtime => Self::Mtime,
        }
    }
}

impl From<SortByMirror> for SortBy {
    fn from(value: SortByMirror) -> Self {
        match value {
            SortByMirror::Score => Self::Score,
            SortByMirror::Name => Self::Name,
            SortByMirror::Size => Self::Size,
            SortByMirror::Mtime => Self::Mtime,
        }
    }
}

impl From<DocKind> for DocKindMirror {
    fn from(value: DocKind) -> Self {
        match value {
            DocKind::File => Self::File,
            DocKind::Dir => Self::Dir,
            DocKind::Symlink => Self::Symlink,
            DocKind::Other => Self::Other,
        }
    }
}

impl From<DocKindMirror> for DocKind {
    fn from(value: DocKindMirror) -> Self {
        match value {
            DocKindMirror::File => Self::File,
            DocKindMirror::Dir => Self::Dir,
            DocKindMirror::Symlink => Self::Symlink,
            DocKindMirror::Other => Self::Other,
        }
    }
}

impl From<HitMirror> for Hit {
    fn from(value: HitMirror) -> Self {
        Self {
            path: value.path,
            name: value.name,
            parent: value.parent,
            extension: value.extension,
            kind: value.kind.into(),
            size: value.size,
            mtime: value.mtime,
            score: value.score,
        }
    }
}

/// IPC-backed Atlas index client.
pub struct IndexClient {
    inner: Client,
}

impl IndexClient {
    /// Connect to the default Atlas index socket.
    pub async fn connect_default() -> Result<Self, IndexClientError> {
        let socket_path = default_socket_path().map_err(IndexClientError::from)?;
        let inner = Client::connect(&socket_path)
            .await
            .map_err(IndexClientError::from)?;
        Ok(Self { inner })
    }

    /// Search indexed paths through atlas-indexd.
    pub async fn search_paths(
        &self,
        query: &Query,
        opts: &SearchOptions,
    ) -> Result<Vec<Hit>, IndexClientError> {
        let query_json = serde_json::to_string(&QueryMirror::from(query))
            .map_err(|error| IndexClientError::Encode(error.to_string()))?;
        let options_json = serde_json::to_string(&SearchOptionsMirror::from(opts))
            .map_err(|error| IndexClientError::Encode(error.to_string()))?;

        match self
            .inner
            .request(Request::Search {
                query_json,
                options_json,
            })
            .await
            .map_err(IndexClientError::from)?
        {
            Response::SearchHits { hits_json } => {
                let hits = serde_json::from_str::<Vec<HitMirror>>(&hits_json)
                    .map_err(|error| IndexClientError::Encode(error.to_string()))?;
                Ok(hits.into_iter().map(Hit::from).collect())
            }
            response => Err(IndexClientError::Encode(format!(
                "expected SearchHits response, got {response:?}"
            ))),
        }
    }

    /// Fetch daemon statistics as `(docs, on_disk_bytes)`.
    pub async fn stats(&self) -> Result<(u64, u64), IndexClientError> {
        match self
            .inner
            .request(Request::Stats)
            .await
            .map_err(IndexClientError::from)?
        {
            Response::Stats {
                docs,
                on_disk_bytes,
            } => Ok((docs, on_disk_bytes)),
            response => Err(IndexClientError::Encode(format!(
                "expected Stats response, got {response:?}"
            ))),
        }
    }

    /// Ping the daemon.
    pub async fn ping(&self) -> Result<(), IndexClientError> {
        self.inner.ping().await.map_err(IndexClientError::from)
    }

    /// Ask the daemon to add a new root directory to its watched set.
    ///
    /// The daemon walks the root, indexes every entry, and continues watching
    /// it for filesystem changes. Idempotent — adding a root that is already
    /// present is a no-op on the daemon side.
    pub async fn add_root(&self, path: PathBuf) -> Result<(), IndexClientError> {
        match self
            .inner
            .request(Request::AddRoot { path })
            .await
            .map_err(IndexClientError::from)?
        {
            Response::Ok => Ok(()),
            Response::Error { code, message } => Err(IndexClientError::Server {
                code: error_code_name(code).to_owned(),
                message,
            }),
            response => Err(IndexClientError::Encode(format!(
                "expected Ok response, got {response:?}"
            ))),
        }
    }
}

/// Errors returned by [`IndexClient`].
#[derive(Debug, thiserror::Error)]
pub enum IndexClientError {
    /// The daemon is not listening on the default socket.
    #[error("atlas-indexd is not running")]
    NotRunning,
    /// Client and server protocol versions differ.
    #[error("protocol mismatch: server={server}, client={client}")]
    ProtocolMismatch {
        /// Server protocol version.
        server: u32,
        /// Client protocol version.
        client: u32,
    },
    /// atlas-indexd returned an application error.
    #[error("server error [{code}]: {message}")]
    Server {
        /// Error code string.
        code: String,
        /// Human-readable message.
        message: String,
    },
    /// Underlying I/O failure.
    #[error("io error: {0}")]
    Io(std::io::Error),
    /// JSON or IPC codec failure.
    #[error("encode/decode error: {0}")]
    Encode(String),
}

impl From<IpcError> for IndexClientError {
    fn from(value: IpcError) -> Self {
        match value {
            IpcError::NotRunning { .. } => Self::NotRunning,
            IpcError::ProtocolMismatch { server, client } => {
                Self::ProtocolMismatch { server, client }
            }
            IpcError::ServerError { code, message } => Self::Server {
                code: error_code_name(code).to_owned(),
                message,
            },
            IpcError::Io(error) => Self::Io(error),
            IpcError::Codec(message) => Self::Encode(message),
            IpcError::Closed => Self::Encode("connection closed".to_owned()),
            IpcError::Timeout => Self::Encode("request timed out".to_owned()),
            IpcError::FrameTooLarge(size) => Self::Encode(format!("frame too large: {size} bytes")),
        }
    }
}

fn error_code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::InvalidRequest => "invalid_request",
        ErrorCode::ProtocolMismatch => "protocol_mismatch",
        ErrorCode::NotReady => "not_ready",
        ErrorCode::InternalError => "internal_error",
        ErrorCode::NotImplemented => "not_implemented",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    use super::{IndexClient, IndexClientError};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn connect_default_returns_not_running_for_missing_socket() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let socket_path = std::env::current_dir()
            .expect("current dir should resolve")
            .join("target/atlas-search-tests/missing-indexd.sock");
        let previous = std::env::var_os("ATLAS_IPC_SOCKET");

        // SAFETY: This test serializes all environment access with a process-wide
        // mutex, so no concurrent environment mutation occurs while the variable
        // is temporarily overridden.
        unsafe {
            std::env::set_var("ATLAS_IPC_SOCKET", &socket_path);
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should build");
        let result = runtime.block_on(IndexClient::connect_default());

        match previous {
            Some(value) => {
                // SAFETY: Protected by the same mutex as the earlier override.
                unsafe {
                    std::env::set_var("ATLAS_IPC_SOCKET", value);
                }
            }
            None => {
                // SAFETY: Protected by the same mutex as the earlier override.
                unsafe {
                    std::env::remove_var("ATLAS_IPC_SOCKET");
                }
            }
        }

        assert!(matches!(result, Err(IndexClientError::NotRunning)));
    }

    #[test]
    fn hit_mirror_converts_to_hit() {
        let hit = super::HitMirror {
            path: PathBuf::from("/tmp/atlas"),
            name: "atlas".into(),
            parent: PathBuf::from("/tmp"),
            extension: Some("rs".into()),
            kind: super::DocKindMirror::File,
            size: 10,
            mtime: Some(42),
            score: 1.5,
        };

        let converted: atlas_index::Hit = hit.into();
        assert_eq!(converted.name, "atlas");
        assert_eq!(converted.kind, atlas_index::DocKind::File);
    }
}
