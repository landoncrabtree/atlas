//! Tab model — represents a single browser-like tab within a pane.

use std::path::{Path, PathBuf};

use atlas_fs::{Filter, SortSpec};

use crate::navigation::BackForwardStack;

/// A single tab inside a pane.
#[derive(Debug, Clone)]
pub struct TabModel {
    /// Display title shown in the tab strip.
    pub title: String,
    /// The location this tab is currently viewing.
    pub location: Option<PathBuf>,
    /// Whether the tab has unsaved or in-progress state.
    pub dirty: bool,
    /// Per-tab back/forward history.
    pub history: BackForwardStack,
    /// Active sort specification for the tab.
    pub sort: SortSpec,
    /// Active filter for the tab.
    pub filter: Filter,
}

impl TabModel {
    /// Create a clean tab rooted at `location` with a fresh history stack.
    #[must_use]
    pub fn new(
        location: impl Into<PathBuf>,
        history_capacity: usize,
        sort: SortSpec,
        filter: Filter,
    ) -> Self {
        let location = location.into();
        let mut history = BackForwardStack::new(history_capacity);
        history.push(location.clone());
        Self {
            title: title_for_path(&location),
            location: Some(location),
            dirty: false,
            history,
            sort,
            filter,
        }
    }

    /// Create a tab at `path` using default navigation and listing settings.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self::new(path, 100, SortSpec::default(), Filter::default())
    }

    /// Navigate the tab to `location`, updating its history and title.
    pub fn navigate_to(&mut self, location: PathBuf) {
        self.history.push(location.clone());
        self.set_location(location);
    }

    /// Navigate backward within the tab history.
    pub fn back(&mut self) -> Option<PathBuf> {
        let location = self.history.back()?;
        self.set_location(location.clone());
        Some(location)
    }

    /// Navigate forward within the tab history.
    pub fn forward(&mut self) -> Option<PathBuf> {
        let location = self.history.forward()?;
        self.set_location(location.clone());
        Some(location)
    }

    /// Returns `true` when back navigation is available.
    #[must_use]
    pub fn can_back(&self) -> bool {
        self.history.can_go_back()
    }

    /// Returns `true` when forward navigation is available.
    #[must_use]
    pub fn can_forward(&self) -> bool {
        self.history.can_go_forward()
    }

    fn set_location(&mut self, location: PathBuf) {
        self.title = title_for_path(&location);
        self.location = Some(location);
    }
}

fn title_for_path(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_model_new() {
        let tab = TabModel::new(
            "/Users/alice/Downloads",
            16,
            SortSpec::default(),
            Filter::default(),
        );
        assert_eq!(tab.title, "Downloads");
        assert!(!tab.dirty);
        assert_eq!(
            tab.location.as_deref(),
            Some(Path::new("/Users/alice/Downloads"))
        );
        assert_eq!(
            tab.history.current(),
            Some(Path::new("/Users/alice/Downloads"))
        );
    }

    #[test]
    fn tab_model_at() {
        let tab = TabModel::at("/Users/alice/Downloads");
        assert_eq!(tab.title, "Downloads");
        assert_eq!(
            tab.location.as_deref().unwrap(),
            std::path::Path::new("/Users/alice/Downloads")
        );
    }

    #[test]
    fn navigate_to_updates_location_and_history() {
        let mut tab = TabModel::at("/Users/alice/Downloads");
        tab.navigate_to(PathBuf::from("/Users/alice/Documents"));
        assert_eq!(
            tab.location.as_deref(),
            Some(Path::new("/Users/alice/Documents"))
        );
        assert_eq!(tab.title, "Documents");
        assert!(tab.can_back());
    }

    #[test]
    fn back_and_forward_update_location() {
        let mut tab = TabModel::at("/Users/alice/Downloads");
        tab.navigate_to(PathBuf::from("/Users/alice/Documents"));

        let back = tab.back();
        assert_eq!(back.as_deref(), Some(Path::new("/Users/alice/Downloads")));
        assert_eq!(
            tab.location.as_deref(),
            Some(Path::new("/Users/alice/Downloads"))
        );
        assert!(tab.can_forward());

        let forward = tab.forward();
        assert_eq!(
            forward.as_deref(),
            Some(Path::new("/Users/alice/Documents"))
        );
        assert_eq!(
            tab.location.as_deref(),
            Some(Path::new("/Users/alice/Documents"))
        );
    }

    #[test]
    fn can_back_and_forward_reflect_history_state() {
        let mut tab = TabModel::at("/Users/alice/Downloads");
        assert!(!tab.can_back());
        assert!(!tab.can_forward());

        tab.navigate_to(PathBuf::from("/Users/alice/Documents"));
        assert!(tab.can_back());
        assert!(!tab.can_forward());

        let _ = tab.back();
        assert!(!tab.can_back());
        assert!(tab.can_forward());
    }
}
