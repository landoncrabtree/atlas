//! Navigation controller — drives per-pane back/forward history and
//! coordinates [`InMemoryLocationViewModel`] lifecycle.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use atlas_core::path::expand_tilde;
use atlas_fs::{InMemoryLocationViewModel, OpenOptions};
use parking_lot::{Mutex, RwLock};
use smallvec::SmallVec;

use crate::navigation::{bookmarks::BookmarkStore, history::BackForwardStack};

/// Default history capacity (number of back entries per pane).
const DEFAULT_HISTORY_CAPACITY: usize = 100;

type LocationChangedCallback = dyn Fn(usize, Arc<InMemoryLocationViewModel>) + Send + Sync;

/// Per-pane navigation controller.
///
/// Maintains independent back/forward history and the current
/// [`InMemoryLocationViewModel`] for each pane. Registered listeners are
/// notified via `on_location_changed` whenever a pane loads a new directory.
///
/// Construct with [`NavigationController::new`] and share behind an [`Arc`].
pub struct NavigationController {
    /// One back/forward stack per pane.
    stacks: SmallVec<[Mutex<BackForwardStack>; 2]>,
    /// Shared bookmark store.
    bookmarks: Arc<BookmarkStore>,
    /// Current location view model per pane (`None` until first navigation).
    locations: RwLock<SmallVec<[Option<Arc<InMemoryLocationViewModel>>; 2]>>,
    /// Callback invoked when a pane's location changes.
    on_location_changed: RwLock<Option<Box<LocationChangedCallback>>>,
}

impl NavigationController {
    /// Construct a new controller, pre-populating the bookmark store from
    /// `config_bookmarks`.
    #[must_use]
    pub fn new(config_bookmarks: &[atlas_config::Bookmark]) -> Arc<Self> {
        Arc::new(Self {
            stacks: smallvec::smallvec![
                Mutex::new(BackForwardStack::new(DEFAULT_HISTORY_CAPACITY)),
                Mutex::new(BackForwardStack::new(DEFAULT_HISTORY_CAPACITY)),
            ],
            bookmarks: Arc::new(BookmarkStore::from_config(config_bookmarks)),
            locations: RwLock::new(smallvec::smallvec![None, None]),
            on_location_changed: RwLock::new(None),
        })
    }

    /// Navigate pane `pane` to `path`.
    ///
    /// Expands a leading `~`, canonicalizes (best-effort), skips if already at
    /// the same location, pushes to history, opens a fresh
    /// [`InMemoryLocationViewModel`], and fires `on_location_changed`.
    pub fn navigate(&self, pane: usize, path: impl Into<PathBuf>) {
        let expanded = expand_tilde(path.into());

        let canonical = match expanded.canonicalize() {
            Ok(c) => c,
            Err(error) => {
                tracing::warn!(?expanded, %error, "navigation: cannot canonicalize path; skipping");
                return;
            }
        };

        if pane < self.stacks.len()
            && self.stacks[pane].lock().current() == Some(canonical.as_path())
        {
            return;
        }

        if pane < self.stacks.len() {
            self.stacks[pane].lock().push(canonical.clone());
        }

        self.load_location(pane, canonical);
    }

    /// Navigate back (`back = true`) or forward (`back = false`) for `pane`.
    pub fn navigate_relative(&self, pane: usize, back: bool) {
        if pane >= self.stacks.len() {
            return;
        }

        let target = if back {
            self.stacks[pane].lock().back()
        } else {
            self.stacks[pane].lock().forward()
        };

        let Some(path) = target else { return };
        self.load_location(pane, path);
    }

    /// Navigate to the parent directory of the current location.
    pub fn go_up(&self, pane: usize) {
        if let Some(parent) = self
            .current_path(pane)
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
        {
            self.navigate(pane, parent);
        }
    }

    /// Navigate to the user's home directory.
    pub fn go_home(&self, pane: usize) {
        self.navigate(pane, expand_tilde(Path::new("~")));
    }

    /// Simulate a breadcrumb click at `segment_index`.
    ///
    /// Splits the current path into components and navigates to the ancestor
    /// up to and including `segment_index`.
    pub fn breadcrumb_clicked(&self, pane: usize, segment_index: usize) {
        let Some(current) = self.current_path(pane) else {
            return;
        };

        let components: Vec<_> = current.components().collect();
        if segment_index >= components.len() {
            return;
        }

        let mut target = PathBuf::new();
        for component in &components[..=segment_index] {
            target.push(component);
        }

        self.navigate(pane, target);
    }

    /// Return the shared bookmark store.
    #[must_use]
    pub fn bookmarks(&self) -> Arc<BookmarkStore> {
        Arc::clone(&self.bookmarks)
    }

    /// Return a snapshot of the back history for `pane`, oldest-first.
    #[must_use]
    pub fn history(&self, pane: usize) -> Vec<PathBuf> {
        if pane < self.stacks.len() {
            self.stacks[pane].lock().back_history()
        } else {
            Vec::new()
        }
    }

    /// Return the current location view model for `pane`, if one has been loaded.
    #[must_use]
    pub fn location(&self, pane: usize) -> Option<Arc<InMemoryLocationViewModel>> {
        self.locations
            .read()
            .get(pane)
            .and_then(|location| location.as_ref().map(Arc::clone))
    }

    /// Register a callback that fires whenever a pane's location changes.
    ///
    /// Only one callback is supported; subsequent calls replace the previous
    /// registration.
    pub fn on_location_changed(
        &self,
        f: impl Fn(usize, Arc<InMemoryLocationViewModel>) + Send + Sync + 'static,
    ) {
        *self.on_location_changed.write() = Some(Box::new(f));
    }

    fn current_path(&self, pane: usize) -> Option<PathBuf> {
        if pane >= self.stacks.len() {
            return None;
        }
        self.stacks[pane].lock().current().map(Path::to_path_buf)
    }

    /// Open a new view model for `path`, store it, and fire the callback.
    fn load_location(&self, pane: usize, path: PathBuf) {
        let vm = InMemoryLocationViewModel::open(path, OpenOptions::default());

        {
            let mut locs = self.locations.write();
            if pane < locs.len() {
                locs[pane] = Some(Arc::clone(&vm));
            }
        }

        if let Some(cb) = self.on_location_changed.read().as_ref() {
            cb(pane, vm);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc as StdArc, Mutex as StdMutex};

    use atlas_fs::LocationViewModel;

    use super::*;

    #[test]
    fn navigate_and_on_location_changed_fires() {
        let tmp = tempfile::TempDir::new().expect("temp dir should create");
        let path = tmp
            .path()
            .canonicalize()
            .expect("temp dir path should canonicalize");

        let ctrl = NavigationController::new(&[]);

        let received: StdArc<StdMutex<Vec<PathBuf>>> = StdArc::new(StdMutex::new(Vec::new()));
        let received_clone = StdArc::clone(&received);

        ctrl.on_location_changed(move |_pane, vm| {
            received_clone
                .lock()
                .expect("paths mutex should not poison")
                .push(vm.location().to_path_buf());
        });

        ctrl.navigate(0, path.clone());

        let paths = received
            .lock()
            .expect("paths mutex should not poison")
            .clone();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], path);
    }

    #[test]
    fn navigate_same_path_is_noop() {
        let tmp = tempfile::TempDir::new().expect("temp dir should create");
        let path = tmp
            .path()
            .canonicalize()
            .expect("temp dir path should canonicalize");

        let ctrl = NavigationController::new(&[]);
        let count: StdArc<StdMutex<usize>> = StdArc::new(StdMutex::new(0));
        let count_clone = StdArc::clone(&count);
        ctrl.on_location_changed(move |_, _| {
            *count_clone.lock().expect("count mutex should not poison") += 1;
        });

        ctrl.navigate(0, path.clone());
        ctrl.navigate(0, path);

        assert_eq!(*count.lock().expect("count mutex should not poison"), 1);
    }

    #[test]
    fn navigate_relative_back_and_forward() {
        let dir_a = tempfile::TempDir::new().expect("temp dir should create");
        let dir_b = tempfile::TempDir::new().expect("temp dir should create");
        let a = dir_a
            .path()
            .canonicalize()
            .expect("temp dir path should canonicalize");
        let b = dir_b
            .path()
            .canonicalize()
            .expect("temp dir path should canonicalize");

        let ctrl = NavigationController::new(&[]);
        ctrl.navigate(0, a.clone());
        ctrl.navigate(0, b.clone());

        let last: StdArc<StdMutex<Option<PathBuf>>> = StdArc::new(StdMutex::new(None));
        let last_clone = StdArc::clone(&last);
        ctrl.on_location_changed(move |_, vm| {
            *last_clone.lock().expect("last mutex should not poison") =
                Some(vm.location().to_path_buf());
        });

        ctrl.navigate_relative(0, true);
        assert_eq!(*last.lock().expect("last mutex should not poison"), Some(a));

        ctrl.navigate_relative(0, false);
        assert_eq!(*last.lock().expect("last mutex should not poison"), Some(b));
    }

    #[test]
    fn history_snapshot_grows() {
        let dir_a = tempfile::TempDir::new().expect("temp dir should create");
        let dir_b = tempfile::TempDir::new().expect("temp dir should create");
        let a = dir_a
            .path()
            .canonicalize()
            .expect("temp dir path should canonicalize");
        let b = dir_b
            .path()
            .canonicalize()
            .expect("temp dir path should canonicalize");

        let ctrl = NavigationController::new(&[]);
        ctrl.navigate(0, a.clone());
        ctrl.navigate(0, b);

        let hist = ctrl.history(0);
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0], a);
    }

    #[test]
    fn per_pane_navigation_is_independent() {
        let dir_a = tempfile::TempDir::new().expect("temp dir");
        let dir_b = tempfile::TempDir::new().expect("temp dir");
        let a = dir_a.path().canonicalize().expect("canon");
        let b = dir_b.path().canonicalize().expect("canon");

        let ctrl = NavigationController::new(&[]);
        ctrl.navigate(0, a.clone());
        ctrl.navigate(1, b.clone());

        let dir_c = tempfile::TempDir::new().expect("temp dir");
        let c = dir_c.path().canonicalize().expect("canon");
        ctrl.navigate(0, c.clone());

        let last_pane1: StdArc<StdMutex<Option<PathBuf>>> = StdArc::new(StdMutex::new(None));
        let last_pane1_clone = StdArc::clone(&last_pane1);
        ctrl.on_location_changed(move |pane, vm| {
            if pane == 1 {
                *last_pane1_clone.lock().expect("lock") = Some(vm.location().to_path_buf());
            }
        });

        ctrl.navigate_relative(0, true);
        assert!(
            last_pane1.lock().expect("lock").is_none(),
            "pane 1 history must not change when pane 0 navigates"
        );
    }
}
