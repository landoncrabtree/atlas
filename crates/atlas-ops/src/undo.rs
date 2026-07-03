//! Undo stack support for reversible filesystem operations.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::anyhow;
use parking_lot::Mutex;
#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
use tracing::warn;

/// Token returned when an undoable operation is pushed onto the stack.
#[derive(Debug, Clone)]
pub struct UndoToken(pub u64);

/// Reversible operation payload.
#[derive(Debug, Clone)]
pub enum UndoEntry {
    /// Reverse a rename by moving `from` back to `to`.
    Rename { from: PathBuf, to: PathBuf },
    /// Restore items that were previously moved to trash.
    Trash { paths: Vec<PathBuf> },
}

/// Fixed-capacity LIFO undo stack.
pub struct UndoStack {
    entries: Mutex<Vec<(u64, UndoEntry)>>,
    next_token: AtomicU64,
    capacity: usize,
}

impl UndoStack {
    /// Creates a new stack with the given capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(Vec::with_capacity(capacity)),
            next_token: AtomicU64::new(1),
            capacity,
        }
    }

    /// Pushes an undo entry, returning its token.
    pub fn push(&self, entry: UndoEntry) -> UndoToken {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let mut entries = self.entries.lock();
        entries.push((token, entry));
        if entries.len() > self.capacity {
            let _removed = entries.remove(0);
        }
        UndoToken(token)
    }

    /// Pops the most recent entry and applies its reverse action.
    pub fn undo(&self) -> atlas_core::Result<()> {
        let entry = self.entries.lock().pop().map(|(_, entry)| entry);
        let Some(entry) = entry else {
            return Ok(());
        };

        match entry {
            UndoEntry::Rename { from, to } => std::fs::rename(&from, &to)
                .map_err(|source| atlas_core::AtlasError::io(Some(from), source)),
            UndoEntry::Trash { paths } => restore_trashed_paths(&paths),
        }
    }

    /// Returns the current stack length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    /// Returns whether the stack is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }
}

#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
fn restore_trashed_paths(paths: &[PathBuf]) -> atlas_core::Result<()> {
    let items =
        trash::os_limited::list().map_err(|error| atlas_core::AtlasError::Other(anyhow!(error)))?;
    let matched = paths
        .iter()
        .filter_map(|path| {
            items
                .iter()
                .find(|item| item.original_path() == *path)
                .cloned()
        })
        .collect::<Vec<_>>();

    if matched.is_empty() {
        warn!(
            count = paths.len(),
            "no trash entries matched requested restore paths"
        );
        return Ok(());
    }

    trash::os_limited::restore_all(matched)
        .map_err(|error| atlas_core::AtlasError::Other(anyhow!(error)))
}

#[cfg(not(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
)))]
fn restore_trashed_paths(_paths: &[PathBuf]) -> atlas_core::Result<()> {
    Err(atlas_core::AtlasError::Other(anyhow!(
        "trash undo is unavailable on this platform"
    )))
}
