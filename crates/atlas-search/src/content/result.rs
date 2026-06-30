//! Match results, spans, and summary types.

use std::path::PathBuf;

/// A byte-offset span within a matched line.
#[derive(Debug, Clone, Copy)]
pub struct MatchSpan {
    /// Byte offset of the start of the match within `ContentMatch::line`.
    pub start: u32,
    /// Byte offset of the end of the match within `ContentMatch::line` (exclusive).
    pub end: u32,
}

/// A single line (or multi-line block) matching the search pattern.
#[derive(Debug, Clone)]
pub struct ContentMatch {
    /// Path to the file containing this match.
    pub path: PathBuf,
    /// 1-based line number of the first matched line.
    pub line_number: u64,
    /// Byte offset into the file of the first matched byte.
    pub byte_offset: u64,
    /// The matched line content (lossy UTF-8).
    pub line: String,
    /// All match spans within `line` (byte offsets relative to `line`).
    pub spans: Vec<MatchSpan>,
    /// Up to `context_before` lines immediately preceding the match.
    pub before: Vec<String>,
    /// Up to `context_after` lines immediately following the match.
    pub after: Vec<String>,
    /// `true` when the file was detected as binary but `BinaryHandling::BinaryAsBinary` is active.
    pub is_binary: bool,
}

/// Events streamed through `SearchHandle::receiver`.
#[derive(Debug, Clone)]
pub enum SearchEvent {
    /// A matching line was found.
    Match(ContentMatch),
    /// All matches in a file have been reported.
    FileSearched {
        /// Path of the file that was just searched.
        path: PathBuf,
        /// Number of matches found in this file.
        matches: u64,
    },
    /// A non-fatal error occurred (e.g. permission denied on a file).
    Error {
        /// File that caused the error, if applicable.
        path: Option<PathBuf>,
        /// Human-readable error description.
        error: String,
    },
    /// Aggregate statistics (sent just before `Done`).
    Summary(SearchSummary),
    /// The search has finished; the channel is closed immediately after.
    Done,
}

/// Aggregate statistics for a completed (or cancelled) search.
#[derive(Debug, Clone, Default)]
pub struct SearchSummary {
    /// Total number of files examined (attempted to search).
    pub files_searched: u64,
    /// Number of files that contained at least one match.
    pub files_with_matches: u64,
    /// Total number of matches across all files.
    pub matches: u64,
    /// Total bytes read across all searched files.
    pub bytes_searched: u64,
    /// Wall-clock time from `run()` call to search completion, in milliseconds.
    pub elapsed_ms: u64,
    /// `true` if the search stopped because a match limit was exceeded.
    pub stopped_due_to_limit: bool,
    /// `true` if `SearchHandle::cancel()` was called before the search finished.
    pub cancelled: bool,
}
