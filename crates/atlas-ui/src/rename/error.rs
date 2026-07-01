//! Error types for the bulk-rename module.

/// Errors that can occur during a bulk rename operation.
#[derive(Debug, thiserror::Error)]
pub enum RenameError {
    /// The user-supplied regex pattern failed to compile.
    #[error("invalid regex pattern: {0}")]
    InvalidRegex(#[from] regex::Error),

    /// A proposed name is empty (would rename to an empty string).
    #[error("proposed name is empty for path: {0}")]
    EmptyProposedName(std::path::PathBuf),
}
