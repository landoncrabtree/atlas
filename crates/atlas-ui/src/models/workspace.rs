//! Workspace model for the N-pane split-tree workspace.
//!
//! A [`WorkspaceModel`] owns a binary [`SplitLayout`] of panes, the full
//! [`PaneState`] for each pane keyed by [`PaneId`], and the id of the pane
//! that currently holds keyboard focus. Every mutation (split, close, focus
//! move) operates on stable [`PaneId`]s so operations survive layout
//! reshuffles.

use std::path::PathBuf;

use ahash::AHashMap;
use directories::BaseDirs;

use crate::models::{
    pane::ViewMode,
    pane_state::PaneState,
    split::{Cardinal, CloseOutcome, PaneId, Rect, SplitDirection, SplitLayout},
    tab::TabModel,
};

/// Top-level workspace state: a split tree of independent panes.
#[derive(Debug)]
pub struct WorkspaceModel {
    /// Binary split tree describing pane geometry.
    pub layout: SplitLayout,
    /// All pane state keyed by stable pane id.
    pub panes: AHashMap<PaneId, PaneState>,
    /// Currently focused pane id.
    pub focused: PaneId,
    next_id: u32,
}

impl WorkspaceModel {
    /// Fresh workspace with a single pane.
    #[must_use]
    pub fn new(initial: PaneState) -> Self {
        let id = initial.id;
        let mut panes = AHashMap::default();
        panes.insert(id, initial);
        Self {
            layout: SplitLayout::single(id),
            panes,
            focused: id,
            next_id: id.0 + 1,
        }
    }

    /// Construct a sensible default workspace rooted at `$HOME`.
    #[must_use]
    pub fn new_default() -> Self {
        let home = BaseDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        let id = PaneId(1);
        let initial = PaneState::new(id, TabModel::at(home), ViewMode::default());
        Self::new(initial)
    }

    /// Return the currently focused pane.
    #[must_use]
    pub fn focused_pane(&self) -> &PaneState {
        &self.panes[&self.focused]
    }

    /// Return the currently focused pane mutably.
    pub fn focused_pane_mut(&mut self) -> &mut PaneState {
        match self.panes.get_mut(&self.focused) {
            Some(pane) => pane,
            None => unreachable!("focused pane must exist in workspace"),
        }
    }

    /// Set focus to `id` when it exists.
    pub fn set_focused(&mut self, id: PaneId) -> bool {
        if self.panes.contains_key(&id) {
            self.focused = id;
            true
        } else {
            false
        }
    }

    /// Get a pane by id.
    #[must_use]
    pub fn pane(&self, id: PaneId) -> Option<&PaneState> {
        self.panes.get(&id)
    }

    /// Get a pane by id mutably.
    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut PaneState> {
        self.panes.get_mut(&id)
    }

    /// Return leaf pane ids in depth-first layout order.
    #[must_use]
    pub fn leaves_in_order(&self) -> Vec<PaneId> {
        self.layout.all_leaves()
    }

    /// Split the focused pane and focus the new sibling pane.
    pub fn split_focused(
        &mut self,
        direction: SplitDirection,
        initial_view_mode: Option<ViewMode>,
    ) -> PaneId {
        let focused = self.focused;
        let active_location = self.focused_pane().active_location().to_path_buf();
        let view_mode = initial_view_mode.unwrap_or(self.focused_pane().view_mode);
        let new_id = self.allocate_id();
        let new_pane = PaneState::new(new_id, TabModel::at(active_location), view_mode);

        let split_result = self.layout.split_leaf(focused, direction, new_id);
        debug_assert!(split_result.is_ok(), "focused pane must exist in layout");
        self.panes.insert(new_id, new_pane);
        self.focused = new_id;
        new_id
    }

    /// Close the focused pane unless it is the last remaining pane.
    pub fn close_focused(&mut self) -> Option<CloseOutcome> {
        let outcome = self.layout.close_leaf(self.focused)?;
        self.panes.remove(&outcome.removed);
        self.focused = outcome.new_focused;
        Some(outcome)
    }

    /// Move focus in `dir` using layout geometry.
    pub fn focus_direction(&mut self, dir: Cardinal, bounds: Rect) -> Option<PaneId> {
        let next = self.layout.focus_direction(self.focused, dir, bounds)?;
        self.focused = next;
        Some(next)
    }

    fn allocate_id(&mut self) -> PaneId {
        let next = PaneId(self.next_id);
        self.next_id += 1;
        next
    }
}

#[cfg(test)]
mod tests {
    use atlas_fs::{Filter, SortSpec};

    use super::*;

    fn initial_pane(id: PaneId, path: &str, view_mode: ViewMode) -> PaneState {
        PaneState::new(
            id,
            TabModel::new(path, 16, SortSpec::default(), Filter::default()),
            view_mode,
        )
    }

    #[test]
    fn new_creates_single_focused_pane() {
        let workspace = WorkspaceModel::new(initial_pane(PaneId(1), "/a", ViewMode::Details));
        assert_eq!(workspace.focused, PaneId(1));
        assert_eq!(workspace.leaves_in_order(), vec![PaneId(1)]);
        assert_eq!(workspace.panes.len(), 1);
    }

    #[test]
    fn new_default_points_to_home() {
        let workspace = WorkspaceModel::new_default();
        let home = BaseDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        assert_eq!(workspace.focused, PaneId(1));
        assert_eq!(workspace.focused_pane().active_location(), home.as_path());
    }

    #[test]
    fn split_focused_grows_layout_and_inherits_location() {
        let mut workspace = WorkspaceModel::new(initial_pane(PaneId(1), "/a", ViewMode::Gallery));
        let new_id = workspace.split_focused(SplitDirection::Horizontal, None);

        assert_eq!(workspace.leaves_in_order(), vec![PaneId(1), new_id]);
        assert_eq!(workspace.panes.len(), 2);
        assert_eq!(workspace.focused, new_id);
        assert_eq!(
            workspace.pane(new_id).map(PaneState::active_location),
            Some(std::path::Path::new("/a"))
        );
        assert_eq!(
            workspace.pane(new_id).map(|pane| pane.view_mode),
            Some(ViewMode::Gallery)
        );
    }

    #[test]
    fn close_focused_collapses_and_refuses_single_pane() {
        let mut workspace = WorkspaceModel::new(initial_pane(PaneId(1), "/a", ViewMode::Details));
        assert_eq!(workspace.close_focused(), None);

        let new_id = workspace.split_focused(SplitDirection::Horizontal, None);
        let outcome = workspace.close_focused();
        assert_eq!(
            outcome,
            Some(CloseOutcome {
                removed: new_id,
                new_focused: PaneId(1),
            })
        );
        assert_eq!(workspace.leaves_in_order(), vec![PaneId(1)]);
        assert_eq!(workspace.focused, PaneId(1));
        assert_eq!(workspace.panes.len(), 1);
    }

    #[test]
    fn focus_direction_moves_across_two_by_two_grid() {
        let mut workspace = WorkspaceModel::new(initial_pane(PaneId(1), "/a", ViewMode::Details));
        let right = workspace.split_focused(SplitDirection::Horizontal, None);
        assert!(workspace.set_focused(PaneId(1)));
        let down = workspace.split_focused(SplitDirection::Vertical, None);
        assert!(workspace.set_focused(right));
        let down_right = workspace.split_focused(SplitDirection::Vertical, None);

        assert_eq!(
            workspace.leaves_in_order(),
            vec![PaneId(1), down, right, down_right]
        );
        assert!(workspace.set_focused(PaneId(1)));
        assert_eq!(
            workspace.focus_direction(Cardinal::Right, Rect::from_size(200.0, 200.0)),
            Some(right)
        );
        assert!(workspace.set_focused(PaneId(1)));
        assert_eq!(
            workspace.focus_direction(Cardinal::Down, Rect::from_size(200.0, 200.0)),
            Some(down)
        );
    }
}
