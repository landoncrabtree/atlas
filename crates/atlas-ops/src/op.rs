//! Operation types and progress/event payloads for the operations queue.

use std::path::PathBuf;
use std::time::Instant;

use atlas_core::Location;

use crate::conflict::ConflictPolicy;
use crate::undo::UndoToken;

/// Unique identifier assigned to an operation.
pub type OpId = u64;

/// Cancellation flag bit.
pub(crate) const FLAG_CANCEL: u8 = 1;
/// Pause flag bit.
pub(crate) const FLAG_PAUSE: u8 = 2;

/// Supported filesystem operation kinds.
///
/// Every op accepts [`Location`] rather than [`PathBuf`] so both local
/// and remote endpoints (SFTP / S3 / WebDAV / FTP) can be driven through
/// the same queue. Local-only callers wrap raw paths via
/// [`Location::local`] at their edge — the ops queue then routes on
/// backend kind.
#[derive(Debug, Clone)]
pub enum OpKind {
    /// Copy one or more sources into a destination directory.
    Copy {
        /// Source locations to copy.
        sources: Vec<Location>,
        /// Destination directory.
        dest_dir: Location,
        /// Conflict policy.
        policy: ConflictPolicy,
    },
    /// Move one or more sources into a destination directory.
    Move {
        /// Source locations to move.
        sources: Vec<Location>,
        /// Destination directory.
        dest_dir: Location,
        /// Conflict policy.
        policy: ConflictPolicy,
    },
    /// Delete one or more locations.
    Delete {
        /// Locations to delete.
        paths: Vec<Location>,
        /// Whether to send items to the OS trash (local-only; remote
        /// deletes are always permanent).
        to_trash: bool,
    },
    /// Rename a location in-place.
    Rename {
        /// Existing location.
        path: Location,
        /// Replacement file or directory name.
        new_name: String,
    },
    /// Create a directory.
    Mkdir {
        /// Directory location to create.
        path: Location,
        /// Whether to create parents (local only; remote backends
        /// synthesise intermediate directories as needed regardless).
        parents: bool,
    },
}

impl OpKind {
    /// Returns a stable descriptor suitable for UI summaries and queue listings.
    #[must_use]
    pub fn descriptor(&self) -> OpKindDescriptor {
        match self {
            Self::Copy {
                sources, dest_dir, ..
            } => OpKindDescriptor {
                kind: "Copy",
                summary: format!("{} items → {}", sources.len(), dest_dir.display_path()),
            },
            Self::Move {
                sources, dest_dir, ..
            } => OpKindDescriptor {
                kind: "Move",
                summary: format!("{} items → {}", sources.len(), dest_dir.display_path()),
            },
            Self::Delete { paths, to_trash } => OpKindDescriptor {
                kind: if *to_trash { "Trash" } else { "Delete" },
                summary: format!("{} items", paths.len()),
            },
            Self::Rename { path, new_name } => OpKindDescriptor {
                kind: "Rename",
                summary: format!("{} → {}", path.display_path(), new_name),
            },
            Self::Mkdir { path, parents } => OpKindDescriptor {
                kind: "Mkdir",
                summary: if *parents {
                    format!("create {} (with parents)", path.display_path())
                } else {
                    format!("create {}", path.display_path())
                },
            },
        }
    }
}

/// Current lifecycle state of an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpStatus {
    /// Waiting in the queue.
    Queued,
    /// Currently executing.
    Running,
    /// Temporarily paused.
    Paused,
    /// Finished successfully.
    Done,
    /// Finished with an error.
    Failed,
    /// Stopped by cancellation.
    Cancelled,
}

/// Snapshot of aggregate progress for an operation.
#[derive(Debug, Clone, Default)]
pub struct ProgressSnapshot {
    /// Total logical items involved in the operation.
    pub items_total: u64,
    /// Completed logical items.
    pub items_done: u64,
    /// Total bytes to process for file content work.
    pub bytes_total: u64,
    /// Completed bytes.
    pub bytes_done: u64,
    /// Current path being processed. For remote locations this is the
    /// URI path portion (no scheme/host), matching how the ops panel
    /// currently displays a single "current file" line — the front-end
    /// only needs the basename.
    pub current_path: Option<PathBuf>,
}

/// Mutable queue state stored for each submitted operation.
#[derive(Debug, Clone)]
pub struct Operation {
    /// Operation identifier.
    pub id: OpId,
    /// Requested operation kind.
    pub kind: OpKind,
    /// Current operation status.
    pub status: OpStatus,
    /// Start instant once work begins.
    pub started_at: Option<Instant>,
    /// Finish instant once work ends.
    pub finished_at: Option<Instant>,
    /// Current progress snapshot.
    pub progress: ProgressSnapshot,
    /// Error string for failed operations.
    pub error: Option<String>,
    /// Undo token assigned when an undoable operation succeeds.
    pub undo_token: Option<UndoToken>,
}

/// Event stream emitted by [`crate::queue::OperationQueue`].
#[derive(Debug, Clone)]
pub enum OpEvent {
    /// Operation was queued.
    Queued { id: OpId, kind: OpKindDescriptor },
    /// Operation started running.
    Started { id: OpId },
    /// Progress update.
    Progress {
        id: OpId,
        snapshot: ProgressSnapshot,
    },
    /// Conflict requires user resolution. Currently only fires for
    /// local-destination copy/move flows; remote destinations always
    /// resolve to Overwrite in the current adapter.
    Conflict {
        id: OpId,
        source: PathBuf,
        dest: PathBuf,
        resolver: crate::conflict::ConflictResponder,
    },
    /// Operation completed successfully.
    Completed { id: OpId },
    /// A retry is about to run for a transient failure. Emitted only
    /// by remote flows that use `atlas_remote::retry::with_retry`.
    Retrying {
        /// Op that is retrying.
        id: OpId,
        /// 1-indexed retry attempt (attempt 1 == first retry).
        attempt: u32,
        /// Delay before the next attempt (milliseconds).
        next_backoff_ms: u64,
    },
    /// A retry loop gave up. The op will surface a subsequent
    /// [`OpEvent::Failed`] carrying the terminal error.
    RetryFailed {
        /// Op that gave up.
        id: OpId,
        /// Total number of attempts made before giving up.
        attempts: u32,
    },
    /// Operation failed.
    Failed {
        id: OpId,
        error: String,
        partial_progress: ProgressSnapshot,
    },
    /// Operation was cancelled.
    Cancelled { id: OpId },
}

/// Human-readable summary of an [`OpKind`].
#[derive(Debug, Clone)]
pub struct OpKindDescriptor {
    /// Stable operation kind label.
    pub kind: &'static str,
    /// Short summary suitable for UIs.
    pub summary: String,
}
