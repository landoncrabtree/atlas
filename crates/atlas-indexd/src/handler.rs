//! IPC handler implementation for atlas-indexd.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use atlas_index::{DocKind, Hit, Query, SearchOptions, SortBy};
use atlas_ipc::protocol::{ErrorCode, Notification, Request, Response, PROTOCOL_VERSION};
use atlas_ipc::server::Handler;

use crate::daemon::Daemon;

/// IPC request handler backed by an [`Arc<Daemon>`].
pub struct DaemonHandler {
    daemon: Arc<Daemon>,
}

impl DaemonHandler {
    /// Create a new handler for `daemon`.
    #[must_use]
    pub fn new(daemon: Arc<Daemon>) -> Self {
        Self { daemon }
    }
}

#[async_trait]
impl Handler for DaemonHandler {
    async fn handle(&self, req: Request) -> Response {
        match req {
            Request::Ping => Response::Pong,
            Request::Hello {
                client_name: _,
                client_version: _,
            } => Response::Hello {
                server_name: "atlas-indexd".into(),
                server_version: env!("CARGO_PKG_VERSION").into(),
                protocol_version: PROTOCOL_VERSION,
            },
            Request::Search {
                query_json,
                options_json,
            } => self.handle_search(&query_json, &options_json),
            Request::Stats => {
                let stats = self.daemon.stats();
                Response::Stats {
                    docs: stats.docs,
                    on_disk_bytes: stats.on_disk_bytes,
                }
            }
            Request::AddRoot { path } => match self.daemon.add_root(path).await {
                Ok(()) => Response::Ok,
                Err(error) => invalid_request(error),
            },
            Request::RemoveRoot { path } => match self.daemon.remove_root(path).await {
                Ok(()) => Response::Ok,
                Err(error) => invalid_request(error),
            },
            Request::Reindex { path } => match self.daemon.reindex(path).await {
                Ok(()) => Response::Ok,
                Err(error) => invalid_request(error),
            },
            Request::Shutdown => {
                let daemon = Arc::clone(&self.daemon);
                tokio::spawn(async move {
                    daemon.shutdown().await;
                });
                Response::Ok
            }
        }
    }

    fn notifications(&self) -> Option<broadcast::Receiver<Notification>> {
        Some(self.daemon.notifications())
    }
}

impl DaemonHandler {
    fn handle_search(&self, query_json: &str, options_json: &str) -> Response {
        let query = match serde_json::from_str::<QueryWire>(query_json).and_then(Query::try_from) {
            Ok(query) => query,
            Err(error) => return invalid_request(error),
        };
        let options = match serde_json::from_str::<SearchOptionsWire>(options_json)
            .map(SearchOptions::from)
        {
            Ok(options) => options,
            Err(error) => return invalid_request(error),
        };

        match self.daemon.search(&query, &options) {
            Ok(hits) => {
                match serde_json::to_string(&hits.iter().map(HitWire::from).collect::<Vec<_>>()) {
                    Ok(hits_json) => Response::SearchHits { hits_json },
                    Err(error) => internal_error(error),
                }
            }
            Err(error) => internal_error(error),
        }
    }
}

fn invalid_request(error: impl std::fmt::Display) -> Response {
    Response::Error {
        code: ErrorCode::InvalidRequest,
        message: error.to_string(),
    }
}

fn internal_error(error: impl std::fmt::Display) -> Response {
    Response::Error {
        code: ErrorCode::InternalError,
        message: error.to_string(),
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DocKindWire {
    File,
    Dir,
    Symlink,
    Other,
}

impl From<DocKindWire> for DocKind {
    fn from(value: DocKindWire) -> Self {
        match value {
            DocKindWire::File => Self::File,
            DocKindWire::Dir => Self::Dir,
            DocKindWire::Symlink => Self::Symlink,
            DocKindWire::Other => Self::Other,
        }
    }
}

impl From<DocKind> for DocKindWire {
    fn from(value: DocKind) -> Self {
        match value {
            DocKind::File => Self::File,
            DocKind::Dir => Self::Dir,
            DocKind::Symlink => Self::Symlink,
            DocKind::Other => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum SortByWire {
    #[default]
    Score,
    Name,
    Size,
    Mtime,
}

impl From<SortByWire> for SortBy {
    fn from(value: SortByWire) -> Self {
        match value {
            SortByWire::Score => Self::Score,
            SortByWire::Name => Self::Name,
            SortByWire::Size => Self::Size,
            SortByWire::Mtime => Self::Mtime,
        }
    }
}

impl From<SortBy> for SortByWire {
    fn from(value: SortBy) -> Self {
        match value {
            SortBy::Score => Self::Score,
            SortBy::Name => Self::Name,
            SortBy::Size => Self::Size,
            SortBy::Mtime => Self::Mtime,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SearchOptionsWire {
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    include_hidden: bool,
    #[serde(default)]
    sort: SortByWire,
}

impl From<SearchOptionsWire> for SearchOptions {
    fn from(value: SearchOptionsWire) -> Self {
        Self {
            limit: value.limit.max(1),
            include_hidden: value.include_hidden,
            sort: value.sort.into(),
        }
    }
}

const fn default_limit() -> usize {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum QueryWire {
    NamePrefix { value: String },
    NameSubstring { value: String },
    NameFuzzy { term: String, distance: u8 },
    Extension { value: String },
    ExactPath { path: PathBuf },
    All { queries: Vec<QueryWire> },
    Any { queries: Vec<QueryWire> },
    InSubtree { path: PathBuf },
    KindAnyOf { kinds: Vec<DocKindWire> },
}

impl TryFrom<QueryWire> for Query {
    type Error = serde_json::Error;

    fn try_from(value: QueryWire) -> Result<Self, Self::Error> {
        Ok(match value {
            QueryWire::NamePrefix { value } => Self::NamePrefix(value),
            QueryWire::NameSubstring { value } => Self::NameSubstring(value),
            QueryWire::NameFuzzy { term, distance } => Self::NameFuzzy { term, distance },
            QueryWire::Extension { value } => Self::Extension(value),
            QueryWire::ExactPath { path } => Self::ExactPath(path),
            QueryWire::All { queries } => Self::All(
                queries
                    .into_iter()
                    .map(Query::try_from)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            QueryWire::Any { queries } => Self::Any(
                queries
                    .into_iter()
                    .map(Query::try_from)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            QueryWire::InSubtree { path } => Self::InSubtree(path),
            QueryWire::KindAnyOf { kinds } => {
                Self::KindAnyOf(kinds.into_iter().map(DocKind::from).collect())
            }
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HitWire {
    path: PathBuf,
    name: String,
    parent: PathBuf,
    extension: Option<String>,
    kind: DocKindWire,
    size: u64,
    mtime: Option<i64>,
    score: f32,
}

impl From<&Hit> for HitWire {
    fn from(value: &Hit) -> Self {
        Self {
            path: value.path.clone(),
            name: value.name.clone(),
            parent: value.parent.clone(),
            extension: value.extension.clone(),
            kind: value.kind.into(),
            size: value.size,
            mtime: value.mtime,
            score: value.score,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_wire_round_trips() {
        let query = QueryWire::All {
            queries: vec![
                QueryWire::NamePrefix {
                    value: "src".into(),
                },
                QueryWire::KindAnyOf {
                    kinds: vec![DocKindWire::Dir, DocKindWire::Symlink],
                },
            ],
        };

        let parsed = Query::try_from(query).expect("query conversion should succeed");
        assert!(matches!(parsed, Query::All(parts) if parts.len() == 2));
    }
}
