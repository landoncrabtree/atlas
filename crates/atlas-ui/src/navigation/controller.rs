//! Navigation controller — drives per-pane back/forward history and
//! coordinates [`InMemoryLocationViewModel`] lifecycle.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use ahash::AHashMap;
use atlas_core::{path::expand_tilde, Location};
use atlas_fs::{
    Filter, InMemoryLocationViewModel, LocationViewModel, OpenOptions, SortKey as FsSortKey,
    SortOrder as FsSortOrder, SortSpec,
};
use parking_lot::{Mutex, RwLock};
use smallvec::SmallVec;

use crate::{
    models::split::PaneId,
    navigation::{bookmarks::BookmarkStore, history::BackForwardStack},
};

/// Default history capacity (number of back entries per pane).
const DEFAULT_HISTORY_CAPACITY: usize = 100;

type LocationChangedCallback = dyn Fn(usize, Arc<InMemoryLocationViewModel>) + Send + Sync;
type PaneLocationChangedCallback = dyn Fn(PaneId, Arc<InMemoryLocationViewModel>) + Send + Sync;

/// Per-pane navigation controller.
///
/// Maintains independent back/forward history and the current
/// [`InMemoryLocationViewModel`] for each pane. Registered listeners are
/// notified via `on_location_changed` whenever a pane loads a new directory.
///
/// Construct with [`NavigationController::new`] and share behind an [`Arc`].
///
/// # Remote locations
///
/// The controller accepts [`Location::Remote`] at the API surface for
/// symmetry with local paths — but remote navigation is not this
/// controller's concern. When a remote location arrives here the
/// controller silently no-ops; the shell-level dispatcher
/// [`AppShell::navigate_pane_to_location`] catches [`Location::Remote`]
/// *before* calling this controller and routes it through the remote
/// mount path instead.
pub struct NavigationController {
    /// One back/forward stack per pane.
    stacks: SmallVec<[Mutex<BackForwardStack>; 2]>,
    /// Shared bookmark store.
    bookmarks: Arc<BookmarkStore>,
    /// Current location view model per pane (`None` until first navigation).
    locations: RwLock<SmallVec<[Option<Arc<InMemoryLocationViewModel>>; 2]>>,
    /// Callback invoked when a pane's location changes.
    on_location_changed: RwLock<Option<Box<LocationChangedCallback>>>,
    /// Current location view model per PaneId.
    locations_v2: RwLock<AHashMap<PaneId, Arc<InMemoryLocationViewModel>>>,
    /// Callback invoked when a PaneId-based location changes.
    on_pane_location_changed: RwLock<Option<Box<PaneLocationChangedCallback>>>,
    /// Default [`OpenOptions`] applied to each newly-opened location. Built
    /// from `config.general` and `config.view` so hidden-file visibility,
    /// symlink handling, and sort defaults actually take effect.
    open_options: RwLock<OpenOptions>,
}

impl NavigationController {
    /// Construct a new controller, pre-populating the bookmark store from
    /// `config_bookmarks`. Uses [`OpenOptions::default`] for locations — use
    /// [`Self::with_config`] to thread user config through.
    #[must_use]
    pub fn new(config_bookmarks: &[atlas_config::Bookmark]) -> Arc<Self> {
        Self::build(
            config_bookmarks,
            OpenOptions::default(),
            DEFAULT_HISTORY_CAPACITY,
        )
    }

    /// Construct a new controller with defaults derived from the full
    /// [`atlas_config::Config`]. Reads `general.follow_symlinks`,
    /// `view.natural_sort`, `view.dirs_first`, `view.default_sort_key`,
    /// `view.default_sort_order`, and `navigation.history_size`.
    ///
    /// **`include_hidden` is always `true`** at this layer. Per-pane
    /// hidden-file visibility is controlled by
    /// [`atlas_fs::Filter::include_hidden`] which the shell applies
    /// after the vm loads, seeded from `config.view.show_hidden` and
    /// flipped at runtime by `pane::ToggleHidden` (Cmd+.). Listing
    /// dotfiles unconditionally here means the raw list carries every
    /// entry so the runtime toggle is a cheap filter refresh — no
    /// second listing pass, no I/O — for both local and remote panes.
    #[must_use]
    pub fn with_config(config: &atlas_config::Config) -> Arc<Self> {
        let opts = OpenOptions {
            // See doc comment: raw list always includes hidden entries;
            // the pane-scoped `Filter::include_hidden` selects visibility.
            include_hidden: true,
            follow_symlinks: config.general.follow_symlinks,
            watch: false,
            sort: SortSpec {
                key: match config.view.default_sort_key {
                    atlas_config::SortKey::Name => FsSortKey::Name,
                    atlas_config::SortKey::Size => FsSortKey::Size,
                    atlas_config::SortKey::Modified => FsSortKey::Modified,
                    atlas_config::SortKey::Kind => FsSortKey::Kind,
                    atlas_config::SortKey::Extension => FsSortKey::Extension,
                },
                order: match config.view.default_sort_order {
                    atlas_config::SortOrder::Asc => FsSortOrder::Asc,
                    atlas_config::SortOrder::Desc => FsSortOrder::Desc,
                },
                dirs_first: config.view.dirs_first,
                natural: config.view.natural_sort,
                case_insensitive: true,
            },
            // Seed the filter with the config default; the shell applies
            // per-pane overrides after the vm loads.
            filter: Filter {
                include_hidden: config.view.show_hidden,
                ..Filter::default()
            },
        };
        // config: reads config.navigation.history_size
        let history_cap = config.navigation.history_size.max(1);
        Self::build(&config.bookmarks, opts, history_cap)
    }

    fn build(
        config_bookmarks: &[atlas_config::Bookmark],
        open_options: OpenOptions,
        history_capacity: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            stacks: smallvec::smallvec![
                Mutex::new(BackForwardStack::new(history_capacity)),
                Mutex::new(BackForwardStack::new(history_capacity)),
            ],
            bookmarks: Arc::new(BookmarkStore::from_config(config_bookmarks)),
            locations: RwLock::new(smallvec::smallvec![None, None]),
            on_location_changed: RwLock::new(None),
            locations_v2: RwLock::new(AHashMap::default()),
            on_pane_location_changed: RwLock::new(None),
            open_options: RwLock::new(open_options),
        })
    }

    /// Navigate pane `pane` to `location`.
    ///
    /// Expands a leading `~` for local paths, canonicalizes (best-effort),
    /// skips if already at the same location, pushes to history, opens a
    /// fresh [`InMemoryLocationViewModel`], and fires `on_location_changed`.
    ///
    /// Remote locations are accepted at the API level but not yet routed
    /// through this controller — the caller sees a `warn!` and the pane
    /// is left unchanged. TODO(remote): wire through the atlas-remote
    /// backend registry.
    pub fn navigate(&self, pane: usize, location: impl Into<Location>) {
        let location = location.into();
        let Some(canonical) = self.resolve_local(location) else {
            return;
        };

        if pane < self.stacks.len()
            && self.stacks[pane]
                .lock()
                .current()
                .and_then(Location::as_local)
                == Some(canonical.as_path())
        {
            return;
        }

        if pane < self.stacks.len() {
            self.stacks[pane]
                .lock()
                .push(Location::local(canonical.clone()));
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

        let Some(location) = target else { return };
        let Some(local) = location.into_local() else {
            tracing::warn!("navigation: back/forward to remote location is not yet supported");
            return;
        };
        self.load_location(pane, local);
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

    /// Navigate `pane` to `location` using the PaneId-based API.
    ///
    /// This API does not modify tab history; callers own that state.
    pub fn navigate_pane(&self, pane: PaneId, location: impl Into<Location>) {
        self.navigate_pane_impl(pane, location.into(), true);
    }

    /// Navigate `pane` to `location` without emitting the PaneId callback.
    pub fn navigate_pane_no_push(&self, pane: PaneId, location: impl Into<Location>) {
        self.navigate_pane_impl(pane, location.into(), false);
    }

    /// Register a callback that fires whenever a PaneId-based location changes.
    pub fn on_pane_location_changed(
        &self,
        f: impl Fn(PaneId, Arc<InMemoryLocationViewModel>) + Send + Sync + 'static,
    ) {
        *self.on_pane_location_changed.write() = Some(Box::new(f));
    }

    /// Get the current PaneId-based location view model.
    #[must_use]
    pub fn location_for_pane(&self, pane: PaneId) -> Option<Arc<InMemoryLocationViewModel>> {
        self.locations_v2.read().get(&pane).map(Arc::clone)
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
            self.stacks[pane]
                .lock()
                .back_history()
                .into_iter()
                .filter_map(Location::into_local)
                .collect()
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

    fn navigate_pane_impl(&self, pane: PaneId, location: Location, emit_callback: bool) {
        let Some(canonical) = self.resolve_local(location) else {
            return;
        };

        if self
            .location_for_pane(pane)
            .is_some_and(|vm| vm.location() == canonical.as_path())
        {
            return;
        }

        self.load_location_for_pane(pane, canonical, emit_callback);
    }

    fn current_path(&self, pane: usize) -> Option<PathBuf> {
        if pane >= self.stacks.len() {
            return None;
        }
        self.stacks[pane]
            .lock()
            .current()
            .and_then(Location::as_local)
            .map(Path::to_path_buf)
    }

    /// Resolve a [`Location`] down to a canonical local [`PathBuf`], or
    /// `None` if the location is remote (handled by the shell-level
    /// dispatcher) or cannot be canonicalized.
    fn resolve_local(&self, location: Location) -> Option<PathBuf> {
        let path = match location {
            Location::Local(path) => path,
            Location::Remote(uri, _) => {
                tracing::debug!(
                    uri = %uri,
                    "navigation: remote location dispatched at shell level, controller no-ops"
                );
                return None;
            }
        };
        self.canonicalize_path(path)
    }

    fn canonicalize_path(&self, path: PathBuf) -> Option<PathBuf> {
        let expanded = expand_tilde(path);
        match expanded.canonicalize() {
            Ok(canonical) => Some(canonical),
            Err(error) => {
                tracing::warn!(?expanded, %error, "navigation: cannot canonicalize path; skipping");
                None
            }
        }
    }

    /// Open a new view model for `path`, store it, and fire the callback.
    fn load_location(&self, pane: usize, path: PathBuf) {
        let vm = self.open_location(path);

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

    fn load_location_for_pane(&self, pane: PaneId, path: PathBuf, emit_callback: bool) {
        let vm = self.open_location(path);
        self.locations_v2.write().insert(pane, Arc::clone(&vm));

        if emit_callback {
            if let Some(cb) = self.on_pane_location_changed.read().as_ref() {
                cb(pane, vm);
            }
        }
    }

    fn open_location(&self, path: PathBuf) -> Arc<InMemoryLocationViewModel> {
        let opts = self.open_options.read().clone();
        InMemoryLocationViewModel::open_live(path, opts)
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

    #[test]
    fn navigate_pane_stores_location_by_pane_id() {
        let tmp = tempfile::TempDir::new().expect("temp dir should create");
        let path = tmp
            .path()
            .canonicalize()
            .expect("temp dir path should canonicalize");

        let ctrl = NavigationController::new(&[]);
        ctrl.navigate_pane(PaneId(7), path.clone());

        let vm = ctrl
            .location_for_pane(PaneId(7))
            .expect("pane location should exist");
        assert_eq!(vm.location(), path.as_path());
    }

    #[test]
    fn pane_location_callback_fires_and_no_push_suppresses_callback() {
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
        let seen: StdArc<StdMutex<Vec<(PaneId, PathBuf)>>> = StdArc::new(StdMutex::new(Vec::new()));
        let seen_clone = StdArc::clone(&seen);
        ctrl.on_pane_location_changed(move |pane, vm| {
            seen_clone
                .lock()
                .expect("seen mutex should not poison")
                .push((pane, vm.location().to_path_buf()));
        });

        ctrl.navigate_pane(PaneId(9), a.clone());
        ctrl.navigate_pane_no_push(PaneId(9), b.clone());

        let seen = seen.lock().expect("seen mutex should not poison").clone();
        assert_eq!(seen, vec![(PaneId(9), a)]);
        assert_eq!(
            ctrl.location_for_pane(PaneId(9))
                .map(|vm| vm.location().to_path_buf()),
            Some(b)
        );
    }

    #[test]
    fn navigate_pane_ignores_remote_location_at_controller_layer() {
        // Remote destinations are dispatched at the shell level, not
        // here — the controller silently no-ops so the shell's remote
        // mount path can handle them without duelling with the local
        // canonicalise pipeline.
        let ctrl = NavigationController::new(&[]);
        ctrl.navigate_pane(
            PaneId(42),
            <Location as std::str::FromStr>::from_str("sftp://user@host/tmp").expect("uri parses"),
        );
        assert!(ctrl.location_for_pane(PaneId(42)).is_none());
    }
}
