//! Per-column state for the Miller columns view.
//!
//! Each [`Column`] corresponds to one visible column in the Miller stack and
//! owns a live [`LocationViewModel`] that streams directory contents into
//! [`Column::entries`].  The concrete backing view model (local
//! [`atlas_fs::InMemoryLocationViewModel`], remote
//! [`atlas_remote::RemoteLocationViewModel`], …) is chosen by the controller's
//! [`super::controller::LocationOpener`] so a Miller pane can descend into
//! either a local path or a remote SFTP/S3/… tree without duplicating logic.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

use atlas_fs::{Entry, LocationViewModel};
use parking_lot::RwLock;

/// Maximum number of concurrently visible Miller columns.
///
/// Matches the number of column slots defined in `miller-state.slint`.
pub const MAX_COLUMNS: usize = 8;

/// State for a single Miller column.
///
/// Shared between the main thread (reads entries for Slint pushes) and the
/// per-column subscription thread (writes entries when the location updates).
pub struct Column {
    /// Filesystem or remote path this column is rooted at.
    pub path: PathBuf,
    /// Live-updating view model for the directory.
    pub location: Arc<dyn LocationViewModel>,
    /// Sorted, filtered entry snapshot — updated by the subscription thread.
    pub entries: RwLock<Vec<Entry>>,
    /// Focused row index within this column (0-based; `usize::MAX` = none).
    pub focused: AtomicUsize,
    /// Set to `true` once the first batch of entries has arrived.
    pub loaded: AtomicBool,
}

impl Column {
    /// Construct a new column for `path` backed by `location`.
    #[must_use]
    pub fn new(path: PathBuf, location: Arc<dyn LocationViewModel>) -> Arc<Self> {
        Arc::new(Self {
            path,
            location,
            entries: RwLock::new(Vec::new()),
            focused: AtomicUsize::new(0),
            loaded: AtomicBool::new(false),
        })
    }

    /// Focused row index, or `None` if no row is focused.
    #[must_use]
    pub fn focused_index(&self) -> Option<usize> {
        let v = self.focused.load(Ordering::Relaxed);
        if v == usize::MAX {
            None
        } else {
            Some(v)
        }
    }
}
