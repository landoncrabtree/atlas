//! Tab model — represents a single browser-like tab within a pane.

use std::path::PathBuf;

/// A single tab inside a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabModel {
    /// Display title shown in the tab strip.
    pub title: String,
    /// Whether the tab has unsaved or in-progress state.
    pub dirty: bool,
    /// The location this tab is currently viewing. `None` means the tab
    /// has never been navigated (freshly opened) and should adopt the
    /// pane's current location the first time it's activated.
    pub location: Option<PathBuf>,
}

impl TabModel {
    /// Create a clean (non-dirty) tab with the given title, unbound to a location.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            dirty: false,
            location: None,
        }
    }

    /// Create a tab bound to the given location, with the title derived from
    /// its file name.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self {
            title,
            dirty: false,
            location: Some(path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_model_new() {
        let tab = TabModel::new("Downloads");
        assert_eq!(tab.title, "Downloads");
        assert!(!tab.dirty);
        assert!(tab.location.is_none());
    }

    #[test]
    fn tab_model_at() {
        let tab = TabModel::at("/Users/alice/Downloads");
        assert_eq!(tab.title, "Downloads");
        assert_eq!(tab.location.as_deref().unwrap(), std::path::Path::new("/Users/alice/Downloads"));
    }
}
