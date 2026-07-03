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
    /// Whether this pane currently shows hidden entries (dotfiles on
    /// Unix, HIDDEN-attribute files on Windows). Toggled at runtime via
    /// `pane::ToggleHidden` (Cmd+. on macOS / Ctrl+H on Linux + Windows);
    /// initial value comes from `config.view.show_hidden` at pane
    /// construction time. Runtime toggles do NOT persist — the next
    /// launch reverts to the config default.
    pub show_hidden: bool,
}

impl PaneState {
    /// Construct a new pane with one initial tab.
    ///
    /// `show_hidden` is initialised to `false` — the historical default
    /// matching `config.view.show_hidden = false`. Call
    /// [`Self::new_with_show_hidden`] when the caller has the pane's
    /// initial visibility policy in hand (typically the shell,
    /// threading `config.view.show_hidden` through).
    #[must_use]
    pub fn new(id: PaneId, initial: TabModel, view_mode: ViewMode) -> Self {
        Self::new_with_show_hidden(id, initial, view_mode, false)
    }

    /// Construct a new pane with an explicit initial `show_hidden`
    /// policy. Used by the shell so the first navigation applies the
    /// user's `config.view.show_hidden` default.
    #[must_use]
    pub fn new_with_show_hidden(
        id: PaneId,
        initial: TabModel,
        view_mode: ViewMode,
        show_hidden: bool,
    ) -> Self {
        Self {
            id,
            tabs: vec![initial],
            active_tab: 0,
            view_mode,
            show_hidden,
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

    #[test]
    fn new_defaults_show_hidden_to_false() {
        let pane = PaneState::new(
            PaneId(1),
            TabModel::new(
                Location::local("/a"),
                8,
                SortSpec::default(),
                Filter::default(),
            ),
            ViewMode::Details,
        );
        assert!(!pane.show_hidden);
    }

    #[test]
    fn new_with_show_hidden_carries_through() {
        let pane = PaneState::new_with_show_hidden(
            PaneId(1),
            TabModel::new(
                Location::local("/a"),
                8,
                SortSpec::default(),
                Filter::default(),
            ),
            ViewMode::Details,
            true,
        );
        assert!(pane.show_hidden);
    }
}
