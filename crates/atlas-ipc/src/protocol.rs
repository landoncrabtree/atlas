//! Wire protocol types: Request, Response, Notification, Envelope.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Protocol version. Bump only on breaking framing changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// Top-level envelope wrapping every message on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Must equal [`PROTOCOL_VERSION`].
    pub version: u32,
    /// Correlation ID: matches Request to Response. 0 for Notifications.
    pub correlation: u64,
    /// The payload.
    pub payload: Frame,
}

/// The three message kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Frame {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

/// Requests sent from the client (app) to the server (daemon).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Health check.
    Ping,
    /// Initial handshake.
    Hello {
        client_name: String,
        client_version: String,
    },
    /// Submit a search query. Both fields are JSON-serialized to avoid a hard
    /// dependency on atlas-index/atlas-search types.
    Search {
        query_json: String,
        options_json: String,
    },
    /// Request indexer statistics.
    Stats,
    /// Add an indexed root directory.
    AddRoot { path: PathBuf },
    /// Remove an indexed root directory.
    RemoveRoot { path: PathBuf },
    /// Trigger a re-index. `None` means all roots.
    Reindex { path: Option<PathBuf> },
    /// Ask the daemon to shut down.
    Shutdown,
}

/// Responses sent from the server (daemon) to the client (app).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// Reply to [`Request::Ping`].
    Pong,
    /// Reply to [`Request::Hello`].
    Hello {
        server_name: String,
        server_version: String,
        protocol_version: u32,
    },
    /// Reply to [`Request::Search`]. JSON-serialized `Vec<Hit>`.
    SearchHits { hits_json: String },
    /// Reply to [`Request::Stats`].
    Stats { docs: u64, on_disk_bytes: u64 },
    /// Generic success.
    Ok,
    /// Generic error.
    Error { code: ErrorCode, message: String },
}

/// Error codes carried in [`Response::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    InvalidRequest,
    ProtocolMismatch,
    NotReady,
    InternalError,
    NotImplemented,
}

/// Server-pushed notifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Notification {
    /// Incremental progress while indexing a root.
    IndexProgress {
        root: PathBuf,
        files: u64,
        bytes: u64,
    },
    /// Indexing complete for a root.
    IndexComplete { root: PathBuf, took_ms: u64 },
    /// Indexing failed for a root.
    IndexError { root: PathBuf, message: String },
}
