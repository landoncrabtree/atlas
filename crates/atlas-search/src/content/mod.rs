//! Content-search API — ripgrep-powered streaming file search.

pub mod filter;
pub mod pattern;
pub mod result;
pub mod search;

pub use filter::{BinaryHandling, FileFilter};
pub use pattern::{CaseSensitivity, PatternSpec};
pub use result::{ContentMatch, MatchSpan, SearchEvent, SearchSummary};
pub use search::{run, run_blocking, SearchHandle, SearchOptions, SearchRequest};
