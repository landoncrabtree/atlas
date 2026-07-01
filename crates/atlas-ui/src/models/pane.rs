//! Pane model — a single file-explorer pane with its own location and tabs.

use atlas_core::Location;

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
    /// Location currently shown in this pane. Local paths use
    /// [`Location::Local`]; remote backends (SFTP, S3, WebDAV, FTP) use
    /// [`Location::Remote`].
    pub location: Location,
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
    pub fn new(location: impl Into<Location>) -> Self {
        let location = location.into();

        Self {
            location: location.clone(),
            view_mode: ViewMode::default(),
            tabs: vec![TabModel::at(location)],
            active_tab: 0,
            focused: false,
        }
    }

    /// Split the pane location into breadcrumb segments.
    ///
    /// For local locations this is the historical path component list.
    /// For remote locations the first segment is the URI root
    /// (`sftp://user@host:port`) and subsequent segments are path
    /// components. See [`Location::breadcrumb_segments`].
    pub fn path_segments(&self) -> Vec<String> {
        self.location.breadcrumb_segments()
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
    fn pane_model_new_local_segments() {
        let pane = PaneModel::new(Location::local("/Users/alice/Downloads"));
        let segments = pane.path_segments();
        assert!(segments.contains(&"Downloads".to_owned()));
    }

    #[test]
    fn pane_model_new_remote_segments_start_with_uri_root() {
        use std::str::FromStr;
        let loc = Location::from_str("sftp://alice@host/var/log").unwrap();
        let pane = PaneModel::new(loc);
        let segments = pane.path_segments();
        assert_eq!(
            segments.first().map(String::as_str),
            Some("sftp://alice@host")
        );
    }
}
