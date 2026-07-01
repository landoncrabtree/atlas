//! Per-root daemon state.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use atlas_index::{IndexReader, IndexWriter};
use atlas_watch::RootId;
use parking_lot::{Mutex, RwLock};

/// State for one indexed directory root.
pub struct IndexRoot {
    /// Canonical root path.
    pub path: PathBuf,
    /// Watcher-assigned root identifier.
    pub root_id: RootId,
    /// On-disk directory containing the index.
    pub index_dir: PathBuf,
    /// Shared index writer.
    pub writer: Mutex<IndexWriter>,
    /// Shared index reader.
    pub reader: RwLock<IndexReader>,
    /// Count of queued mutations since the last commit.
    pub pending_writes: AtomicUsize,
    /// Whether a background full-root ingest is currently running.
    pub indexing: AtomicBool,
}

impl IndexRoot {
    /// Increment the pending-write counter and return the new value.
    #[must_use]
    pub fn mark_pending(&self) -> usize {
        self.pending_writes.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Reset the pending-write counter to zero and return the old value.
    #[must_use]
    pub fn take_pending(&self) -> usize {
        self.pending_writes.swap(0, Ordering::Relaxed)
    }
}
