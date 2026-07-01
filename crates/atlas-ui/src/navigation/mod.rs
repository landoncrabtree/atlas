//! Navigation module — back/forward history, bookmarks, and the navigation
//! controller that ties them together.
//!
//! # Overview
//!
//! - [`BackForwardStack`] — bounded per-pane history with back/forward semantics.
//! - [`BookmarkStore`] — thread-safe named bookmark store backed by
//!   `atlas_config::Bookmark`.
//! - [`NavigationController`] — coordinates history, bookmarks, and
//!   [`atlas_fs::InMemoryLocationViewModel`] lifecycle per pane.
//! - [`path_completions`] — lightweight prefix helper for address-bar autocomplete.

pub mod bookmarks;
pub mod controller;
pub mod history;

pub use bookmarks::{Bookmark, BookmarkStore};
pub use controller::NavigationController;
pub use history::BackForwardStack;

use std::path::{Path, PathBuf};

use atlas_core::path::expand_tilde;
use atlas_fs::{list_directory, ListEvent, ListRequest};

/// Return up to `limit` filesystem paths whose final component starts with
/// the tail of `prefix` (case-insensitive).
///
/// Reads the parent directory of `prefix` via the Atlas filesystem layer.
/// Suitable for address-bar autocomplete; the caller must not invoke this on
/// the Slint event-loop thread since it blocks until the listing completes.
#[must_use]
pub fn path_completions(prefix: &str, limit: usize) -> Vec<PathBuf> {
    if limit == 0 || prefix.is_empty() {
        return Vec::new();
    }

    let expanded_prefix = expand_tilde(prefix);
    let p = Path::new(&expanded_prefix);
    let expanded_text = expanded_prefix.to_string_lossy();
    let (parent, tail): (&Path, &str) = if expanded_text.ends_with(std::path::MAIN_SEPARATOR) {
        (p, "")
    } else {
        match (p.parent(), p.file_name().and_then(|f| f.to_str())) {
            (Some(par), Some(fname)) if !par.as_os_str().is_empty() => (par, fname),
            _ => (p, ""),
        }
    };

    let req = ListRequest {
        path: parent.to_path_buf(),
        follow_symlinks: false,
        include_hidden: false,
    };

    let rx = list_directory(req);
    let tail_lower = tail.to_lowercase();
    let mut results: Vec<PathBuf> = Vec::new();

    for event in &rx {
        match event {
            ListEvent::Batch(entries) => {
                for entry in entries {
                    if results.len() >= limit {
                        return results;
                    }
                    if entry.name.to_lowercase().starts_with(tail_lower.as_str()) {
                        results.push(entry.path.clone());
                    }
                }
            }
            ListEvent::Error { path, error } => {
                tracing::debug!(?path, %error, "path_completions: list error");
                break;
            }
            ListEvent::Done => break,
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_returns_at_most_limit() {
        let tmp = tempfile::TempDir::new().expect("temp dir should create");
        for name in ["apple", "apricot", "avocado", "banana"] {
            std::fs::create_dir(tmp.path().join(name)).expect("test dir should create");
        }

        let prefix = format!("{}/a", tmp.path().display());
        let results = path_completions(&prefix, 2);
        assert!(results.len() <= 2);
    }

    #[test]
    fn completions_filters_by_prefix() {
        let tmp = tempfile::TempDir::new().expect("temp dir should create");
        std::fs::create_dir(tmp.path().join("foo")).expect("foo dir should create");
        std::fs::create_dir(tmp.path().join("bar")).expect("bar dir should create");

        let prefix = format!("{}/f", tmp.path().display());
        let results = path_completions(&prefix, 10);
        assert!(results.iter().all(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with('f'))
                .unwrap_or(false)
        }));
    }

    #[test]
    fn completions_empty_limit_returns_empty() {
        let results = path_completions("/", 0);
        assert!(results.is_empty());
    }
}
