//! Index statistics.
//!
//! [`IndexStats`] is returned by [`IndexReader::stats`] and exposes document
//! count, on-disk byte usage, and the wall-clock time of the last commit.

/// A snapshot of index health metrics.
///
/// Obtain via [`IndexReader::stats`][crate::IndexReader::stats].
#[derive(Debug, Clone, Copy)]
pub struct IndexStats {
    /// Number of documents currently in the index (live, after merges).
    pub num_docs: u64,
    /// Total bytes occupied by index files on disk.
    pub on_disk_bytes: u64,
    /// Unix epoch seconds of the most recent commit, or `None` when the index
    /// has not been committed yet.
    pub last_commit_unix: Option<i64>,
}
