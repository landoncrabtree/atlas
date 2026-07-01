//! Pane model — a single file-explorer pane with its own location and tabs.

use std::path::PathBuf;

use crate::models::tab::TabModel;

/// View rendering mode for a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// Traditional list/details columns.
    #[default]
    Details,
    /// Large icon grid.
    Grid,
    /// Photo or media gallery.
    Gallery,
    /// Miller-columns (macOS Finder-style).
    Miller,
    /// Expandable tree view.
    Tree,
}

impl std::fmt::Display for ViewMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Details => f.write_str("Details"),
            Self::Grid => f.write_str("Grid"),
            Self::Gallery => f.write_str("Gallery"),
            Self::Miller => f.write_str("Miller"),
            Self::Tree => f.write_str("Tree"),
        }
    }
}

/// State for a single pane in the workspace.
#[derive(Debug, Clone)]
pub struct PaneModel {
    /// Filesystem path currently shown in this pane.
    pub location: PathBuf,
    /// Active rendering mode.
    pub view_mode: ViewMode,
    /// Ordered tab list.
    pub tabs: Vec<TabModel>,
    /// Index of the currently visible tab.
    pub active_tab: usize,
    /// Whether this pane has keyboard focus.
    pub focused: bool,
}

impl PaneModel {
    /// Construct a pane model pointing to `location`.
    pub fn new(location: impl Into<PathBuf>) -> Self {
        let location = location.into();

        Self {
            location: location.clone(),
            view_mode: ViewMode::default(),
            tabs: vec![TabModel::at(location)],
            active_tab: 0,
            focused: false,
        }
    }

    /// Split `self.location` into path segments for the breadcrumb bar.
    pub fn path_segments(&self) -> Vec<String> {
        let mut segments: Vec<String> = self
            .location
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect();

        if segments.is_empty() {
            segments.push("/".to_owned());
        }

        segments
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_mode_display() {
        assert_eq!(ViewMode::Details.to_string(), "Details");
        assert_eq!(ViewMode::Grid.to_string(), "Grid");
        assert_eq!(ViewMode::Gallery.to_string(), "Gallery");
        assert_eq!(ViewMode::Miller.to_string(), "Miller");
        assert_eq!(ViewMode::Tree.to_string(), "Tree");
    }

    #[test]
    fn pane_model_new_segments() {
        let pane = PaneModel::new("/Users/alice/Downloads");
        let segments = pane.path_segments();
        assert!(segments.contains(&"Downloads".to_owned()));
    }
}
