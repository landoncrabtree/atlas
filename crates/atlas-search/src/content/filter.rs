//! File-traversal filters: globs, gitignore, hidden files, size limits, binary handling.

/// How to handle files that appear to contain binary (NUL-byte) data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BinaryHandling {
    /// Skip binary files entirely (like `rg -I`).
    #[default]
    Skip,
    /// Treat binary files as UTF-8 text.
    AsText,
    /// Search binary files but mark the match result as binary.
    BinaryAsBinary,
}

/// Controls which files are walked and searched.
#[derive(Debug, Clone)]
pub struct FileFilter {
    /// Only search files matching at least one of these glob patterns.
    /// An empty list means "no restriction" (all files pass).
    pub include_globs: Vec<String>,
    /// Skip files matching any of these glob patterns.
    pub exclude_globs: Vec<String>,
    /// Respect `.gitignore`, `.ignore`, and the global git ignore file.
    pub respect_gitignore: bool,
    /// Include hidden files and directories (names starting with `.`).
    pub include_hidden: bool,
    /// Skip files larger than this many bytes. `None` means unlimited.
    pub max_filesize_bytes: Option<u64>,
    /// How to handle binary files.
    pub binary: BinaryHandling,
}

impl Default for FileFilter {
    fn default() -> Self {
        Self {
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            respect_gitignore: true,
            include_hidden: false,
            max_filesize_bytes: None,
            binary: BinaryHandling::Skip,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BinaryHandling, FileFilter};

    #[test]
    fn default_filter_matches_repository_defaults() {
        let filter = FileFilter::default();

        assert!(filter.include_globs.is_empty());
        assert!(filter.exclude_globs.is_empty());
        assert!(filter.respect_gitignore);
        assert!(!filter.include_hidden);
        assert_eq!(filter.max_filesize_bytes, None);
        assert_eq!(filter.binary, BinaryHandling::Skip);
    }
}
