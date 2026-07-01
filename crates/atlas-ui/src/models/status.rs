//! Status-bar model — entry counts and background indexer state.

/// Indexer service state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IndexerState {
    /// The indexer is not running.
    #[default]
    Stopped,
    /// The indexer is actively scanning.
    Indexing {
        /// Files encountered so far.
        files: u64,
    },
    /// The index is up to date.
    Ready {
        /// Total indexed documents.
        docs: u64,
    },
    /// The indexer encountered a fatal error.
    Error,
}

impl std::fmt::Display for IndexerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped => f.write_str("Stopped"),
            Self::Indexing { files } => write!(f, "Indexing {files}"),
            Self::Ready { docs } => write!(f, "Ready {docs}"),
            Self::Error => f.write_str("Error"),
        }
    }
}

/// Status bar data model.
#[derive(Debug, Clone, Default)]
pub struct StatusModel {
    /// Total visible entries in the active pane.
    pub total_entries: usize,
    /// Number of directories among the visible entries.
    pub folder_count: usize,
    /// Number of files (non-directories) among the visible entries.
    pub file_count: usize,
    /// Cumulative byte size of the visible non-directory entries.
    pub total_bytes: u64,
    /// Number of currently selected entries.
    pub selected_entries: usize,
    /// Cumulative byte size of the current selection (files only).
    pub selected_bytes: u64,
    /// Background indexer status.
    pub indexer_state: IndexerState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexer_state_display() {
        assert_eq!(IndexerState::Stopped.to_string(), "Stopped");
        assert_eq!(
            IndexerState::Indexing { files: 1_234 }.to_string(),
            "Indexing 1234"
        );
        assert_eq!(
            IndexerState::Ready { docs: 56_789 }.to_string(),
            "Ready 56789"
        );
        assert_eq!(IndexerState::Error.to_string(), "Error");
    }

    #[test]
    fn status_default_zeroed() {
        let status = StatusModel::default();
        assert_eq!(status.total_entries, 0);
        assert_eq!(status.selected_entries, 0);
        assert_eq!(status.folder_count, 0);
        assert_eq!(status.file_count, 0);
        assert_eq!(status.total_bytes, 0);
        assert_eq!(status.selected_bytes, 0);
        assert_eq!(status.indexer_state, IndexerState::Stopped);
    }
}
