//! [`SearchRow`] — UI model for a single search result.

use std::path::PathBuf;

use atlas_search::UnifiedResult;

/// A row displayed in the search panel.
#[derive(Debug, Clone)]
pub struct SearchRow {
    /// Path for the underlying result.
    pub path: PathBuf,
    /// Row label shown in the list.
    pub label: String,
    /// Snippet preview shown in the list.
    pub snippet: String,
    /// Row kind string (`"path"` or `"content"`).
    pub kind: String,
}

impl SearchRow {
    /// Build a UI row from a unified search result.
    #[must_use]
    pub fn from_result(result: &UnifiedResult) -> Self {
        match result {
            UnifiedResult::Path { path, .. } => Self {
                path: path.clone(),
                label: path.to_string_lossy().into_owned(),
                snippet: String::new(),
                kind: "path".to_owned(),
            },
            UnifiedResult::Content {
                path,
                line,
                snippet,
                ..
            } => Self {
                path: path.clone(),
                label: format!("{}:{line}", path.to_string_lossy()),
                snippet: truncate_snippet(snippet),
                kind: "content".to_owned(),
            },
        }
    }
}

fn truncate_snippet(snippet: &str) -> String {
    let mut chars = snippet.chars();
    let truncated: String = chars.by_ref().take(200).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}
