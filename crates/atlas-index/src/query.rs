//! High-level query types for the Atlas index.
//!
//! [`Query`] is the public, composable query language. The reader translates
//! it into a tantivy query tree at search time.

use std::path::PathBuf;

use crate::doc::DocKind;

/// A composable query over the Atlas index.
///
/// Queries are built by the caller and passed to [`IndexReader::search`].
///
/// [`IndexReader::search`]: crate::IndexReader::search
#[derive(Debug, Clone)]
pub enum Query {
    /// Case-insensitive prefix match on the entry name.
    ///
    /// Uses a [`RegexQuery`][tantivy::query::RegexQuery] on the `name_lc`
    /// (raw-tokenized, lowercase) field, so short queries (< 3 chars) are
    /// still fast. The tradeoff vs. `PhraseQuery` is that a regex engine
    /// traverses the FST for every term, which is O(|alphabet| × |regex|)
    /// rather than O(1); for typical filename prefixes this is negligible.
    NamePrefix(String),

    /// Case-insensitive substring match on the entry name via the
    /// ngram(2, 3) tokenizer.
    ///
    /// The query string is itself tokenized with the same ngram analyzer;
    /// the resulting terms are AND-ed so that only documents containing
    /// **all** ngrams are returned. Queries shorter than 2 characters return
    /// no results.
    NameSubstring(String),

    /// Fuzzy (edit-distance) match on the lowercased entry name.
    ///
    /// `distance` is clamped to `0..=2`. Uses
    /// [`FuzzyTermQuery`][tantivy::query::FuzzyTermQuery] on `name_lc`.
    NameFuzzy {
        /// The target term (lowercased before querying).
        term: String,
        /// Maximum allowed edit distance (0 = exact, 1 = one transposition/insertion/deletion, 2 = max).
        distance: u8,
    },

    /// Match entries whose extension equals the given string (case-insensitive).
    Extension(String),

    /// Match the single document whose absolute path is exactly `path`.
    ExactPath(PathBuf),

    /// AND-combine sub-queries: only documents matching **all** are returned.
    All(Vec<Query>),

    /// OR-combine sub-queries: documents matching **any** are returned.
    Any(Vec<Query>),

    /// Restrict results to documents whose `parent` path is or is under
    /// `root`.
    ///
    /// Uses a [`RegexQuery`][tantivy::query::RegexQuery] on the `parent`
    /// field: `^<root>(/.*)?$` (implicit FST anchoring).
    InSubtree(PathBuf),

    /// Restrict results to entries of one of the given kinds.
    KindAnyOf(Vec<DocKind>),
}

/// Options controlling how [`IndexReader::search`] is executed.
///
/// [`IndexReader::search`]: crate::IndexReader::search
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Maximum number of [`Hit`]s to return.
    pub limit: usize,
    /// When `false`, entries with `is_hidden = 1` are silently excluded.
    pub include_hidden: bool,
    /// The ordering of results.
    pub sort: SortBy,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 50,
            include_hidden: false,
            sort: SortBy::Score,
        }
    }
}

/// The field by which search results are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    /// BM25 relevance score (default; highest first).
    Score,
    /// Entry name, lexicographic ascending.
    Name,
    /// File size, descending.
    Size,
    /// Last-modified time, descending (most recent first).
    Mtime,
}

/// A single search result.
#[derive(Debug, Clone)]
pub struct Hit {
    /// Full absolute path of the entry.
    pub path: PathBuf,
    /// Last path segment.
    pub name: String,
    /// Parent directory.
    pub parent: PathBuf,
    /// Lowercased extension without dot, or `None`.
    pub extension: Option<String>,
    /// Entry kind.
    pub kind: DocKind,
    /// Size in bytes.
    pub size: u64,
    /// Last-modified time (Unix seconds), or `None` when unavailable.
    pub mtime: Option<i64>,
    /// BM25 score (1.0 when sorted by a fast field).
    pub score: f32,
}
