//! Filesystem event types.

use std::{path::PathBuf, time::Instant};

use smallvec::SmallVec;

use crate::RootId;

/// The kind of change that triggered a [`FileEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEventKind {
    /// A new file or directory appeared.
    Created,

    /// A file or directory was deleted.
    Removed,

    /// The content or metadata of a file changed.
    Modified,

    /// A file or directory was renamed or moved within the watched tree.
    ///
    /// When this variant is present, `paths[0]` is the old path and `paths[1]`
    /// is the new path.  On some platforms the underlying backend may instead
    /// emit a [`Removed`](FileEventKind::Removed) / [`Created`](FileEventKind::Created)
    /// pair; see [`DirectoryWatcher`](crate::DirectoryWatcher) documentation for
    /// details.
    Renamed,

    /// The backend signalled that events may have been lost and the caller
    /// should re-scan the root.  This is typically triggered by kernel buffer
    /// overflow (inotify `IN_Q_OVERFLOW`, macOS FSEvents rescan).
    Rescan,

    /// The backend reported a watch error.  If the error was scoped to a
    /// particular path, `paths[0]` contains it.
    Error,
}

/// A coalesced, debounced filesystem event for a single watched root.
///
/// For most event kinds `paths` contains exactly one entry.  The exception is
/// [`FileEventKind::Renamed`], where `paths[0]` is the old path and `paths[1]`
/// is the new path.
#[derive(Debug, Clone)]
pub struct FileEvent {
    /// The watched root that produced this event.
    pub root: RootId,

    /// The kind of change.
    pub kind: FileEventKind,

    /// Affected paths.
    ///
    /// * `Created` / `Removed` / `Modified` / `Rescan` / `Error`: 0–1 paths.
    /// * `Renamed`: exactly 2 paths — `[old, new]`.
    pub paths: SmallVec<[PathBuf; 2]>,

    /// Wall-clock instant at which the underlying OS event was recorded.
    pub instant: Instant,
}
