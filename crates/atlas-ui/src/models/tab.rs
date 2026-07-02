//! Tab model — represents a single browser-like tab within a pane.

use std::path::Path;

use atlas_core::Location;
use atlas_fs::{Filter, SortSpec};

use crate::navigation::BackForwardStack;

/// A single tab inside a pane.
#[derive(Debug, Clone)]
pub struct TabModel {
    /// Display title shown in the tab strip.
    pub title: String,
    /// The location this tab is currently viewing. `None` before the tab
    /// has ever navigated anywhere (rare — construction always seeds a
    /// location, but explicit reset flows can clear it).
    pub location: Option<Location>,
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
        location: impl Into<Location>,
        history_capacity: usize,
        sort: SortSpec,
        filter: Filter,
    ) -> Self {
        let location = location.into();
        let mut history = BackForwardStack::new(history_capacity);
        history.push(location.clone());
        Self {
            title: title_for_location(&location),
            location: Some(location),
            dirty: false,
            history,
            sort,
            filter,
        }
    }

    /// Create a tab at `path` using default navigation and listing settings.
    #[must_use]
    pub fn at(location: impl Into<Location>) -> Self {
        Self::new(location, 100, SortSpec::default(), Filter::default())
    }

    /// Navigate the tab to `location`, updating its history and title.
    pub fn navigate_to(&mut self, location: impl Into<Location>) {
        let location = location.into();
        self.history.push(location.clone());
        self.set_location(location);
    }

    /// Navigate backward within the tab history.
    pub fn back(&mut self) -> Option<Location> {
        let location = self.history.back()?;
        self.set_location(location.clone());
        Some(location)
    }

    /// Navigate forward within the tab history.
    pub fn forward(&mut self) -> Option<Location> {
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

    fn set_location(&mut self, location: Location) {
        self.title = title_for_location(&location);
        self.location = Some(location);
    }
}

fn title_for_location(location: &Location) -> String {
    match location {
        Location::Local(path) => title_for_path(path),
        Location::Remote(uri, _) => {
            // Prefer the last path segment; fall back to host, then to the
            // full URI so we always have something visible in the tab strip.
            let last = uri
                .path
                .rsplit('/')
                .find(|s| !s.is_empty())
                .map(str::to_owned);
            last.or_else(|| uri.host.clone())
                .unwrap_or_else(|| location.display_path())
        }
    }
}

fn title_for_path(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn tab_model_new() {
        let tab = TabModel::new(
            Location::local("/Users/alice/Downloads"),
            16,
            SortSpec::default(),
            Filter::default(),
        );
        assert_eq!(tab.title, "Downloads");
        assert!(!tab.dirty);
        assert_eq!(
            tab.location.as_ref().and_then(Location::as_local),
            Some(Path::new("/Users/alice/Downloads"))
        );
        assert_eq!(
            tab.history.current(),
            Some(&Location::local("/Users/alice/Downloads"))
        );
    }

    #[test]
    fn tab_model_at() {
        let tab = TabModel::at(Location::local("/Users/alice/Downloads"));
        assert_eq!(tab.title, "Downloads");
        assert_eq!(
            tab.location.as_ref().and_then(Location::as_local),
            Some(Path::new("/Users/alice/Downloads"))
        );
    }

    #[test]
    fn navigate_to_updates_location_and_history() {
        let mut tab = TabModel::at(Location::local("/Users/alice/Downloads"));
        tab.navigate_to(Location::local("/Users/alice/Documents"));
        assert_eq!(
            tab.location.as_ref().and_then(Location::as_local),
            Some(Path::new("/Users/alice/Documents"))
        );
        assert_eq!(tab.title, "Documents");
        assert!(tab.can_back());
    }

    #[test]
    fn back_and_forward_update_location() {
        let mut tab = TabModel::at(Location::local("/Users/alice/Downloads"));
        tab.navigate_to(Location::local("/Users/alice/Documents"));

        let back = tab.back();
        assert_eq!(back, Some(Location::local("/Users/alice/Downloads")));
        assert_eq!(
            tab.location.as_ref().and_then(Location::as_local),
            Some(Path::new("/Users/alice/Downloads"))
        );
        assert!(tab.can_forward());

        let forward = tab.forward();
        assert_eq!(forward, Some(Location::local("/Users/alice/Documents")));
        assert_eq!(
            tab.location.as_ref().and_then(Location::as_local),
            Some(Path::new("/Users/alice/Documents"))
        );
    }

    #[test]
    fn can_back_and_forward_reflect_history_state() {
        let mut tab = TabModel::at(Location::local("/Users/alice/Downloads"));
        assert!(!tab.can_back());
        assert!(!tab.can_forward());

        tab.navigate_to(Location::local("/Users/alice/Documents"));
        assert!(tab.can_back());
        assert!(!tab.can_forward());

        let _ = tab.back();
        assert!(!tab.can_back());
        assert!(tab.can_forward());
    }

    #[test]
    fn remote_tab_title_uses_last_path_segment() {
        let loc = Location::from_str("sftp://user@host/var/log/nginx").unwrap();
        let tab = TabModel::at(loc);
        assert_eq!(tab.title, "nginx");
    }

    #[test]
    fn remote_tab_title_falls_back_to_host() {
        let loc = Location::from_str("sftp://user@host/").unwrap();
        let tab = TabModel::at(loc);
        assert_eq!(tab.title, "host");
    }

    /// Regression test for the "Cmd+[ / Cmd+] on a remote pane is a
    /// no-op" bug. The tab's back/forward stack MUST return
    /// [`Location::Remote`] values verbatim — the shell-level
    /// dispatcher [`AppShell::navigate_pane_to_location_no_push`] then
    /// remounts a fresh remote VM at that URI, unlike the old code
    /// that shunted back/forward through
    /// [`NavigationController::navigate_pane_no_push`], which
    /// early-returned on remote.
    #[test]
    fn back_and_forward_on_remote_history_return_remote_locations() {
        let a = Location::from_str("sftp://user@host/a").unwrap();
        let b = Location::from_str("sftp://user@host/a/b").unwrap();
        let c = Location::from_str("sftp://user@host/a/b/c").unwrap();

        let mut tab = TabModel::at(a.clone());
        tab.navigate_to(b.clone());
        tab.navigate_to(c.clone());
        assert!(tab.can_back());
        assert!(!tab.can_forward());

        // Cmd+[ twice → back at a.
        let back1 = tab.back().expect("first back should return b");
        assert_eq!(back1, b, "first back must land on b");
        assert!(matches!(back1, Location::Remote(_, _)));

        let back2 = tab.back().expect("second back should return a");
        assert_eq!(back2, a, "second back must land on a");
        assert!(matches!(back2, Location::Remote(_, _)));
        assert!(!tab.can_back());
        assert!(tab.can_forward());

        // Cmd+] once → forward to b.
        let fwd = tab.forward().expect("forward should return b");
        assert_eq!(fwd, b, "forward must land on b");
        assert!(matches!(fwd, Location::Remote(_, _)));
        assert!(tab.can_back());
        assert!(tab.can_forward());
    }
}
