//! Per-pane tab and view state for the N-pane workspace model.

use std::path::Path;

use atlas_core::Location;

use crate::models::{pane::ViewMode, split::PaneId, tab::TabModel};

/// Full state owned by a single pane in the split workspace.
#[derive(Debug, Clone)]
pub struct PaneState {
    /// Stable pane identifier.
    pub id: PaneId,
    /// Ordered tab list.
    pub tabs: Vec<TabModel>,
    /// Index of the currently active tab.
    pub active_tab: usize,
    /// View mode for the pane.
    pub view_mode: ViewMode,
}

impl PaneState {
    /// Construct a new pane with one initial tab.
    #[must_use]
    pub fn new(id: PaneId, initial: TabModel, view_mode: ViewMode) -> Self {
        Self {
            id,
            tabs: vec![initial],
            active_tab: 0,
            view_mode,
        }
    }

    /// Return the active tab.
    #[must_use]
    pub fn active(&self) -> &TabModel {
        &self.tabs[self.active_tab]
    }

    /// Return the active tab mutably.
    pub fn active_mut(&mut self) -> &mut TabModel {
        &mut self.tabs[self.active_tab]
    }

    /// Return the active tab's location.
    ///
    /// Returns a synthetic `Location::Local(".")` when the active tab has
    /// no location yet — matches the historical `PathBuf`-returning API
    /// and keeps callers on the fast path.
    #[must_use]
    pub fn active_location(&self) -> Location {
        self.active()
            .location
            .clone()
            .unwrap_or_else(|| Location::local("."))
    }

    /// Return the active tab's location as a local [`Path`], if it is
    /// [`Location::Local`]. Returns `None` for remote locations.
    ///
    /// TODO(remote): review each caller — some (thumbnails, native trash,
    /// clipboard) will need explicit "not supported on remote" handling
    /// once remote backends are wired end-to-end.
    #[must_use]
    pub fn active_local_path(&self) -> Option<&Path> {
        self.active().location.as_ref().and_then(Location::as_local)
    }

    /// Append `tab` and make it active.
    pub fn add_tab(&mut self, tab: TabModel) {
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
    }

    /// Remove a tab unless it is the last remaining tab.
    pub fn close_tab(&mut self, index: usize) -> Option<TabModel> {
        if self.tabs.len() == 1 || index >= self.tabs.len() {
            return None;
        }

        let removed = self.tabs.remove(index);
        if self.active_tab > index {
            self.active_tab -= 1;
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        Some(removed)
    }

    /// Set the active tab when `index` is valid.
    pub fn set_active(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active_tab = index;
        }
    }
}

#[cfg(test)]
mod tests {
    use atlas_fs::{Filter, SortSpec};

    use super::*;

    #[test]
    fn add_tab_grows_tabs_and_updates_active_index() {
        let mut pane = PaneState::new(
            PaneId(1),
            TabModel::new(
                Location::local("/a"),
                8,
                SortSpec::default(),
                Filter::default(),
            ),
            ViewMode::Details,
        );

        pane.add_tab(TabModel::new(
            Location::local("/b"),
            8,
            SortSpec::default(),
            Filter::default(),
        ));

        assert_eq!(pane.tabs.len(), 2);
        assert_eq!(pane.active_tab, 1);
        assert_eq!(pane.active_location(), Location::local("/b"));
    }

    #[test]
    fn close_tab_refuses_last_tab() {
        let mut pane = PaneState::new(
            PaneId(1),
            TabModel::new(
                Location::local("/a"),
                8,
                SortSpec::default(),
                Filter::default(),
            ),
            ViewMode::Details,
        );

        assert!(pane.close_tab(0).is_none());
        assert_eq!(pane.tabs.len(), 1);
    }

    #[test]
    fn close_tab_adjusts_active_index() {
        let mut pane = PaneState::new(
            PaneId(1),
            TabModel::new(
                Location::local("/a"),
                8,
                SortSpec::default(),
                Filter::default(),
            ),
            ViewMode::Details,
        );
        pane.add_tab(TabModel::new(
            Location::local("/b"),
            8,
            SortSpec::default(),
            Filter::default(),
        ));
        pane.add_tab(TabModel::new(
            Location::local("/c"),
            8,
            SortSpec::default(),
            Filter::default(),
        ));
        pane.set_active(2);

        let removed = pane.close_tab(2);
        assert!(removed.is_some());
        assert_eq!(pane.active_tab, 1);
        assert_eq!(pane.active_location(), Location::local("/b"));
    }
}
