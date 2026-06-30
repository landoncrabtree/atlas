//! Workspace model — holds one or two panes and tracks the focused pane.

use std::path::PathBuf;

use directories::BaseDirs;
use smallvec::SmallVec;

use crate::models::pane::PaneModel;

/// Top-level workspace state.
#[derive(Debug, Clone)]
pub struct WorkspaceModel {
    /// Pane list (one or two elements).
    pub panes: SmallVec<[PaneModel; 2]>,
    /// Index of the pane that currently has keyboard focus.
    pub focused_pane: usize,
    /// When `true`, both panes are shown side by side.
    pub dual_pane: bool,
}

impl WorkspaceModel {
    /// Construct a sensible default workspace rooted at `$HOME`.
    pub fn new_default() -> Self {
        let home = BaseDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut pane = PaneModel::new(home);
        pane.focused = true;

        let mut panes = SmallVec::new();
        panes.push(pane);

        Self {
            panes,
            focused_pane: 0,
            dual_pane: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_default_has_one_focused_pane() {
        let workspace = WorkspaceModel::new_default();
        assert_eq!(workspace.focused_pane, 0);
        assert!(!workspace.dual_pane);
        assert_eq!(workspace.panes.len(), 1);
        assert!(workspace.panes[0].focused);
    }

    #[test]
    fn workspace_default_points_to_home() {
        let workspace = WorkspaceModel::new_default();
        let home = BaseDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        assert_eq!(workspace.panes[0].location, home);
    }
}
