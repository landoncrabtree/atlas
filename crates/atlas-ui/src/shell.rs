//! AppShell — the bridge between pure-Rust models and the Slint window.
//!
//! Compilation of the `.slint` files lives in `atlas-ui/build.rs` so that
//! this crate can reference the generated `AtlasWindow` type directly.
//! `atlas-app` therefore does not need its own `slint::include_modules!()`
//! call; it simply re-uses the types re-exported from this crate.
//!
//! Thread-safety: every `set_*` method may be called from any thread. It uses
//! [`slint::invoke_from_event_loop`] to push property changes onto the Slint
//! event loop. The inner `RwLock`s guard the Rust-side model copies.

use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
};

use ahash::AHashMap;
use atlas_core::path::expand_tilde;
use atlas_fs::LocationViewModel;
use atlas_keymap::{defaults::default_actions, ActionRegistry, Keymap};
use directories::UserDirs;
use parking_lot::{Mutex, RwLock};
use slint::{ComponentHandle as _, ModelRc, SharedString, VecModel};

use crate::{
    actions::{ActionSink, UiAction},
    models::{
        split::{Cardinal, PaneId, Rect, SplitDirection},
        PaletteModel, PaletteResult, StatusModel, TabModel, ViewMode, WorkspaceModel,
    },
    navigation::NavigationController,
    ops::OpsController,
    palette::{ActionsSource, GotoPathsSource, PaletteController, WalkerPathIndex},
    rename::BulkRenameController,
    search::SearchController,
    theme::{ThemeMode, ThemeTokens},
    theming::defaults,
    views::details::DetailsController,
    views::gallery::GalleryController,
    views::grid::GridController,
    views::miller::MillerController,
    views::tree::TreeController,
    AtlasWindow, PaletteEntry, TabEntry,
};

fn to_tab_model(tabs: &[crate::models::TabModel]) -> ModelRc<TabEntry> {
    let entries: Vec<TabEntry> = tabs
        .iter()
        .map(|tab| TabEntry {
            title: SharedString::from(tab.title.as_str()),
            dirty: tab.dirty,
        })
        .collect();
    ModelRc::new(VecModel::from(entries))
}

fn to_palette_model(results: &[PaletteResult]) -> ModelRc<PaletteEntry> {
    let entries: Vec<PaletteEntry> = results
        .iter()
        .map(|result| PaletteEntry {
            title: SharedString::from(result.title.as_str()),
            subtitle: SharedString::from(result.subtitle.as_str()),
            action_id: SharedString::from(result.action_id.as_str()),
        })
        .collect();
    ModelRc::new(VecModel::from(entries))
}

fn to_segments_model(segments: &[String]) -> ModelRc<SharedString> {
    let entries: Vec<SharedString> = segments
        .iter()
        .map(|segment| SharedString::from(segment.as_str()))
        .collect();
    ModelRc::new(VecModel::from(entries))
}

/// Split a path into breadcrumb segments (equivalent to the legacy
/// `PaneModel::path_segments`). Always yields at least one segment.
fn path_segments_for(path: &Path) -> Vec<String> {
    let mut segments: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();

    if segments.is_empty() {
        segments.push("/".to_owned());
    }

    segments
}

fn dispatch_navigation(
    actions: &Arc<Mutex<Box<dyn ActionSink>>>,
    pane: usize,
    raw_path: SharedString,
) {
    actions.lock().dispatch(UiAction::Navigate {
        pane,
        path: expand_tilde(Path::new(raw_path.as_str())),
    });
}

fn palette_root() -> PathBuf {
    if let Some(user_dirs) = UserDirs::new() {
        return user_dirs.home_dir().to_path_buf();
    }

    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn dirs_home() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn build_palette_controller(
    window: &AtlasWindow,
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
) -> Arc<PaletteController> {
    let mut registry = ActionRegistry::new();
    for action in default_actions() {
        registry.register(action);
    }

    // Build the keymap starting from defaults, then layer the user's
    // keymap.toml on top if it exists.  Failures are logged as warnings so a
    // malformed user keymap never prevents startup.
    let mut keymap = Keymap::with_defaults();
    if let Ok(km_path) = atlas_config::keymap_file_path() {
        if km_path.exists() {
            match std::fs::read_to_string(&km_path) {
                Ok(text) => {
                    if let Err(e) = keymap.apply_user_toml(&text) {
                        tracing::warn!("ignoring malformed keymap {}: {e}", km_path.display());
                    } else {
                        tracing::info!("loaded user keymap from {}", km_path.display());
                    }
                }
                Err(e) => {
                    tracing::warn!("could not read keymap file {}: {e}", km_path.display());
                }
            }
        }
    }
    let keymap = Arc::new(keymap);
    let actions_source = Arc::new(ActionsSource::new(Arc::new(registry), Arc::clone(&keymap)));
    let path_index = Arc::new(WalkerPathIndex::new(palette_root()));
    let goto_source = Arc::new(GotoPathsSource::new(path_index));

    let controller = PaletteController::new(actions);
    controller.attach_window(window.as_weak());
    controller.register_source(actions_source);
    controller.register_source(goto_source);
    controller.set_on_dispatch(|action_id| {
        tracing::info!(%action_id, "palette action dispatched");
    });
    controller
}

/// Per-pane view controllers.
///
/// Cloning is cheap: every field is an [`Arc`], so a clone shares the
/// underlying controllers.
#[derive(Clone)]
pub struct PaneControllers {
    /// Stable id of the pane these controllers drive.
    pub pane_id: PaneId,
    /// Details view controller for the pane.
    pub details: Arc<crate::views::details::DetailsController>,
    /// Grid view controller for the pane.
    pub grid: Arc<crate::views::grid::GridController>,
    /// Miller columns view controller for the pane.
    pub miller: Arc<crate::views::miller::MillerController>,
    /// Tree view controller for the pane.
    pub tree: Arc<crate::views::tree::TreeController>,
    /// Gallery view controller for the pane.
    pub gallery: Arc<crate::views::gallery::GalleryController>,
}

fn build_pane_controllers(
    pane_id: PaneId,
    slint_index: usize,
    window: &AtlasWindow,
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
    thumb_cache: Arc<atlas_thumbs::SqliteCache>,
) -> PaneControllers {
    let details = DetailsController::new(slint_index, window.as_weak(), Arc::clone(&actions));
    let grid = GridController::new(
        slint_index,
        window.as_weak(),
        Arc::clone(&actions),
        Arc::clone(&thumb_cache),
    );
    let gallery = GalleryController::new(
        slint_index,
        window.as_weak(),
        Arc::clone(&actions),
        Arc::clone(&thumb_cache),
    );
    let tree = TreeController::new(slint_index, Arc::clone(&actions));
    tree.attach_window(window.as_weak());
    let miller = MillerController::new(actions);
    miller.attach_window(window.as_weak());

    PaneControllers {
        pane_id,
        details,
        grid,
        miller,
        tree,
        gallery,
    }
}

/// Owns Rust-side model state and bridges it to the Slint window.
///
/// Construct with [`AppShell::new`], then call
/// [`AppShell::project_workspace_to_slint`], [`AppShell::set_status`], and
/// [`AppShell::set_theme`] to push initial state.
///
/// The workspace is an N-pane [`WorkspaceModel`]; per-pane controllers and
/// view models are keyed by [`PaneId`]. The Slint UI is still 2-pane-capable
/// (Phase 4 rewrites it), so the shell keeps a `pane_slint_index` map from
/// [`PaneId`] to the Slint slot (0 or 1) used by the compat callback layer.
pub struct AppShell {
    window: slint::Weak<AtlasWindow>,
    workspace: RwLock<WorkspaceModel>,
    /// Per-pane view controllers keyed by pane id.
    panes_ctrl: RwLock<AHashMap<PaneId, PaneControllers>>,
    /// Current location view model per pane id.
    vms: RwLock<AHashMap<PaneId, Arc<dyn LocationViewModel>>>,
    /// Maps `PaneId` → Slint slot index (0 or 1) for the compat layer.
    pane_slint_index: RwLock<AHashMap<PaneId, usize>>,
    palette: RwLock<PaletteModel>,
    status: RwLock<StatusModel>,
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
    navigation: Arc<NavigationController>,
    palette_ctrl: Arc<PaletteController>,
    search: Arc<SearchController>,
    /// File-operations queue controller.
    ops: Arc<OpsController>,
    /// Bulk rename modal controller.
    bulk_rename: Arc<BulkRenameController>,
    /// Shared thumbnail cache used when building new pane controllers on split.
    thumb_cache: Arc<atlas_thumbs::SqliteCache>,
}

impl AppShell {
    /// Build the shell, wire all Slint callbacks, and return a shared handle.
    pub fn new(
        window: &AtlasWindow,
        actions: impl ActionSink,
        nav: Arc<NavigationController>,
        search: Arc<SearchController>,
    ) -> Arc<Self> {
        let actions: Arc<Mutex<Box<dyn ActionSink>>> = Arc::new(Mutex::new(Box::new(actions)));
        let thumb_cache = Arc::new(
            atlas_thumbs::SqliteCache::open_default()
                .unwrap_or_else(|error| panic!("failed to open thumbnail cache: {error}")),
        );

        let workspace = WorkspaceModel::new_default();
        let initial_pane_id = workspace.focused;

        let mut panes_ctrl = AHashMap::default();
        panes_ctrl.insert(
            initial_pane_id,
            build_pane_controllers(
                initial_pane_id,
                0,
                window,
                Arc::clone(&actions),
                Arc::clone(&thumb_cache),
            ),
        );

        let mut pane_slint_index = AHashMap::default();
        pane_slint_index.insert(initial_pane_id, 0usize);

        let palette_ctrl = build_palette_controller(window, Arc::clone(&actions));
        search.set_action_sink(Arc::clone(&actions));
        let ops = OpsController::new();
        ops.attach_window(window.as_weak());
        let bulk_rename = BulkRenameController::new(Arc::clone(&ops), Arc::clone(&actions));
        bulk_rename.attach_window(window.as_weak());
        let shell = Arc::new(Self {
            window: window.as_weak(),
            workspace: RwLock::new(workspace),
            panes_ctrl: RwLock::new(panes_ctrl),
            vms: RwLock::new(AHashMap::default()),
            pane_slint_index: RwLock::new(pane_slint_index),
            palette: RwLock::new(PaletteModel::default()),
            status: RwLock::new(StatusModel::default()),
            actions,
            navigation: nav,
            palette_ctrl,
            search,
            ops,
            bulk_rename,
            thumb_cache,
        });

        shell.wire_callbacks(window);
        shell.register_nav_callbacks();
        shell
    }

    /// Return the focused pane's details controller.
    #[must_use]
    pub fn details_controller(&self) -> Arc<DetailsController> {
        Arc::clone(&self.focused_controllers().details)
    }

    /// Return the focused pane's grid controller.
    #[must_use]
    pub fn grid_controller(&self) -> Arc<GridController> {
        Arc::clone(&self.focused_controllers().grid)
    }

    /// Return the focused pane's gallery controller.
    #[must_use]
    pub fn gallery_controller(&self) -> Arc<GalleryController> {
        Arc::clone(&self.focused_controllers().gallery)
    }

    /// Return the focused pane's tree controller.
    #[must_use]
    pub fn tree_controller(&self) -> Arc<TreeController> {
        Arc::clone(&self.focused_controllers().tree)
    }

    /// Return the focused pane's miller columns controller.
    #[must_use]
    pub fn miller_controller(&self) -> Arc<MillerController> {
        Arc::clone(&self.focused_controllers().miller)
    }

    /// Return a clone of the controllers for the currently focused pane,
    /// falling back to any pane if the focused pane has no controllers yet.
    fn focused_controllers(&self) -> PaneControllers {
        let id = self.focused_pane_id();
        let panes = self.panes_ctrl.read();
        panes
            .get(&id)
            .or_else(|| panes.values().next())
            .cloned()
            .expect("at least one pane's controllers must exist")
    }

    /// Get the per-pane view controllers by pane id.
    ///
    /// Returns a clone; controllers live behind [`Arc`], so this is cheap.
    #[must_use]
    pub fn pane_by_id(&self, id: PaneId) -> Option<PaneControllers> {
        self.panes_ctrl.read().get(&id).cloned()
    }

    /// Resolve the controllers currently occupying Slint slot `index`.
    fn ctrl_for_index(&self, index: usize) -> Option<PaneControllers> {
        self.pane_id_for_index(index)
            .and_then(|id| self.pane_by_id(id))
    }

    /// Return the shared navigation controller.
    #[must_use]
    pub fn navigation(&self) -> Arc<NavigationController> {
        Arc::clone(&self.navigation)
    }

    /// Return the shared palette controller.
    #[must_use]
    pub fn palette_controller(&self) -> Arc<PaletteController> {
        Arc::clone(&self.palette_ctrl)
    }

    /// Return the shared search controller.
    #[must_use]
    pub fn search(&self) -> Arc<SearchController> {
        Arc::clone(&self.search)
    }

    /// Return the file-operations controller.
    #[must_use]
    pub fn ops(&self) -> Arc<OpsController> {
        Arc::clone(&self.ops)
    }

    /// Return the bulk-rename modal controller.
    #[must_use]
    pub fn bulk_rename(&self) -> Arc<BulkRenameController> {
        Arc::clone(&self.bulk_rename)
    }

    /// Return the focused pane's [`PaneId`].
    #[must_use]
    pub fn focused_pane_id(&self) -> PaneId {
        self.workspace.read().focused
    }

    /// Set focus to the given pane.
    pub fn set_focused_pane_id(self: &Arc<Self>, id: PaneId) {
        {
            self.workspace.write().set_focused(id);
        }
        self.project_workspace_to_slint();
    }

    /// Resolve a Slint pane index (0 or 1) to a [`PaneId`] via DFS leaf order.
    fn pane_id_for_index(&self, index: usize) -> Option<PaneId> {
        let leaves = self.workspace.read().layout.all_leaves();
        leaves.get(index).copied()
    }

    /// Split the focused pane in `direction`. Returns the new [`PaneId`].
    ///
    /// Compat gate: refuses to produce a 3rd leaf until Phase 4 lands (logs a
    /// warning and returns `None`) because the Slint UI is still 2-pane-only.
    pub fn split_focused(self: &Arc<Self>, direction: SplitDirection) -> Option<PaneId> {
        let leaf_count = self.workspace.read().layout.leaf_count();
        if leaf_count >= 2 {
            tracing::warn!("split_focused: 3rd pane not supported until Phase 4; ignoring");
            return None;
        }

        let (new_id, new_location) = {
            let mut ws = self.workspace.write();
            let new_id = ws.split_focused(direction, None);
            let loc = ws
                .pane(new_id)
                .expect("just created")
                .active_location()
                .to_path_buf();
            (new_id, loc)
        };

        // Assign the new pane to Slint slot 1.
        self.pane_slint_index.write().insert(new_id, 1);

        // Build controllers for the new pane on slot 1.
        let window = self.window.upgrade().expect("window must be alive");
        let new_ctrl = build_pane_controllers(
            new_id,
            1,
            &window,
            Arc::clone(&self.actions),
            Arc::clone(&self.thumb_cache),
        );
        self.panes_ctrl.write().insert(new_id, new_ctrl);

        // Navigate the new pane to the inherited location.
        self.navigation.navigate_pane(new_id, new_location);
        self.project_workspace_to_slint();
        Some(new_id)
    }

    /// Close the focused pane. Refuses to close the last remaining pane.
    pub fn close_focused_pane(self: &Arc<Self>) {
        let outcome = {
            let mut ws = self.workspace.write();
            ws.close_focused()
        };
        let Some(outcome) = outcome else {
            tracing::debug!("close_focused_pane: only one pane; refusing");
            return;
        };

        self.panes_ctrl.write().remove(&outcome.removed);
        self.vms.write().remove(&outcome.removed);

        // Reassign Slint slot indices for the remaining panes in DFS order.
        let leaves = self.workspace.read().layout.all_leaves();
        {
            let mut idx_map = self.pane_slint_index.write();
            idx_map.clear();
            for (i, &leaf) in leaves.iter().enumerate().take(2) {
                idx_map.insert(leaf, i);
            }
        }

        self.project_workspace_to_slint();
    }

    /// Move focus in cardinal direction `dir` using the layout geometry.
    pub fn focus_direction(self: &Arc<Self>, dir: Cardinal) {
        let bounds = self.window_bounds();
        {
            self.workspace.write().focus_direction(dir, bounds);
        }
        self.project_workspace_to_slint();
    }

    /// Cycle the focused pane's view mode Details→Grid→Gallery→Miller→Tree→…
    pub fn cycle_view_mode(self: &Arc<Self>) {
        let id = self.focused_pane_id();
        let cur = self
            .workspace
            .read()
            .pane(id)
            .map(|p| p.view_mode)
            .unwrap_or_default();
        let next = match cur {
            ViewMode::Details => ViewMode::Grid,
            ViewMode::Grid => ViewMode::Gallery,
            ViewMode::Gallery => ViewMode::Miller,
            ViewMode::Miller => ViewMode::Tree,
            ViewMode::Tree => ViewMode::Details,
        };
        self.set_view_mode(id, next);
    }

    fn window_bounds(&self) -> Rect {
        self.window
            .upgrade()
            .map(|w| {
                let size = w.window().size();
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: size.width as f32,
                    height: size.height as f32,
                }
            })
            .unwrap_or(Rect::from_size(1440.0, 900.0))
    }

    /// Return the current directory path for `id`, if available.
    #[must_use]
    pub fn pane_location(&self, id: PaneId) -> Option<PathBuf> {
        self.workspace
            .read()
            .pane(id)
            .map(|p| p.active_location().to_path_buf())
    }

    /// Set the view mode for pane `id` and push the change to the UI.
    pub fn set_view_mode(self: &Arc<Self>, id: PaneId, mode: ViewMode) {
        {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(id) else {
                tracing::debug!(?id, "set_view_mode: pane not found");
                return;
            };
            if p.view_mode == mode {
                return;
            }
            p.view_mode = mode;
        }
        self.project_workspace_to_slint();
    }

    /// Enable / disable vim-mode navigation on the Slint FocusScope.
    ///
    /// When true, `hjkl` navigates. When false, only arrow keys do.
    pub fn set_vim_mode(self: &Arc<Self>, enabled: bool) {
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = weak.upgrade() {
                window.set_vim_mode(enabled);
            }
        });
    }

    /// Set which tab is active in pane `id` and reload its location. No-op if
    /// `id` or `tab` is out of range.
    pub fn select_tab(self: &Arc<Self>, id: PaneId, tab: usize) {
        let target = {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(id) else {
                return;
            };
            if tab >= p.tabs.len() {
                return;
            }
            p.set_active(tab);
            Some(p.active_location().to_path_buf())
        };
        self.project_workspace_to_slint();
        if let Some(loc) = target {
            self.navigation.navigate_pane(id, loc);
        }
    }

    /// Cycle to the next (`delta = 1`) or previous (`delta = -1`) tab in
    /// pane `id`, wrapping around at the ends.
    pub fn cycle_tab(self: &Arc<Self>, id: PaneId, delta: isize) {
        let target = {
            let ws = self.workspace.read();
            let Some(p) = ws.pane(id) else {
                return;
            };
            let len = p.tabs.len() as isize;
            if len == 0 {
                return;
            }
            let cur = p.active_tab as isize;
            let next = ((cur + delta) % len + len) % len;
            Some(next as usize)
        };
        if let Some(t) = target {
            self.select_tab(id, t);
        }
    }

    /// Split the focused pane rightward (horizontal split).
    #[deprecated(since = "0.0.1", note = "Phase 3: use split_focused")]
    pub fn split_focused_or_toggle_dual(self: &Arc<Self>) {
        self.split_focused(SplitDirection::Horizontal);
    }

    /// Append a new tab to pane `id` pointing at the pane's current location.
    /// The new tab becomes active. No-op if `id` is out of range.
    pub fn new_tab(self: &Arc<Self>, id: PaneId) {
        let loc = self.pane_location(id).unwrap_or_else(dirs_home);
        {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(id) else {
                tracing::debug!(?id, "new_tab: pane not found");
                return;
            };
            p.add_tab(TabModel::at(loc.clone()));
        }
        self.project_workspace_to_slint();
        self.navigation.navigate_pane(id, loc);
    }

    /// Remove tab `tab` from pane `id`. Refuses to close the last tab
    /// (the pane must always have at least one). Adjusts the active tab so
    /// that a still-valid tab remains selected, navigating to its location
    /// when the active tab changed.
    pub fn close_tab(self: &Arc<Self>, id: PaneId, tab: usize) {
        let switch_to: Option<PathBuf> = {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(id) else {
                tracing::debug!(?id, tab, "close_tab: pane not found");
                return;
            };
            let was_active = tab == p.active_tab;
            if p.close_tab(tab).is_some() && was_active {
                Some(p.active_location().to_path_buf())
            } else {
                None
            }
        };
        self.project_workspace_to_slint();
        if let Some(dest) = switch_to {
            self.navigation.navigate_pane(id, dest);
        }
    }

    /// Navigate pane `id` to the parent of its current location.
    pub fn go_up(self: &Arc<Self>, id: PaneId) {
        if let Some(parent) = self
            .pane_location(id)
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
        {
            {
                let mut ws = self.workspace.write();
                if let Some(p) = ws.pane_mut(id) {
                    p.active_mut().navigate_to(parent.clone());
                }
            }
            self.navigation.navigate_pane(id, parent);
        }
    }

    /// Navigate pane `id` to the user's home directory.
    pub fn go_home(self: &Arc<Self>, id: PaneId) {
        let home = expand_tilde(Path::new("~"));
        {
            let mut ws = self.workspace.write();
            if let Some(p) = ws.pane_mut(id) {
                p.active_mut().navigate_to(home.clone());
            }
        }
        self.navigation.navigate_pane(id, home);
    }

    /// Navigate pane `id` to the ancestor at breadcrumb `segment_index`.
    pub fn breadcrumb_clicked(self: &Arc<Self>, id: PaneId, segment_index: usize) {
        let Some(current) = self.pane_location(id) else {
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
        {
            let mut ws = self.workspace.write();
            if let Some(p) = ws.pane_mut(id) {
                p.active_mut().navigate_to(target.clone());
            }
        }
        self.navigation.navigate_pane(id, target);
    }

    /// Navigate the focused pane backward in its active tab's history.
    pub fn back_focused(self: &Arc<Self>) {
        let id = self.focused_pane_id();
        let dest = {
            self.workspace
                .write()
                .pane_mut(id)
                .and_then(|p| p.active_mut().back())
        };
        if let Some(path) = dest {
            self.navigation.navigate_pane_no_push(id, path);
        }
    }

    /// Navigate the focused pane forward in its active tab's history.
    pub fn forward_focused(self: &Arc<Self>) {
        let id = self.focused_pane_id();
        let dest = {
            self.workspace
                .write()
                .pane_mut(id)
                .and_then(|p| p.active_mut().forward())
        };
        if let Some(path) = dest {
            self.navigation.navigate_pane_no_push(id, path);
        }
    }

    // ── Deprecated usize-indexed compat shims ────────────────────────────
    // These resolve the Slint slot index to a PaneId via the layout's
    // DFS-ordered leaves. New code should use the PaneId-based methods.

    /// Return the Slint slot index (0 or 1) of the focused pane.
    #[deprecated(since = "0.0.1", note = "Phase 3: use focused_pane_id()")]
    #[must_use]
    pub fn focused_pane(&self) -> usize {
        let focused = self.focused_pane_id();
        let leaves = self.workspace.read().layout.all_leaves();
        leaves.iter().position(|&id| id == focused).unwrap_or(0)
    }

    /// Return whether more than one pane is open.
    #[deprecated(
        since = "0.0.1",
        note = "Phase 3: use split_focused/close_focused_pane"
    )]
    #[must_use]
    pub fn is_dual_pane(&self) -> bool {
        self.workspace.read().layout.leaf_count() > 1
    }

    /// Enable (split) or disable (close) the second pane.
    #[deprecated(
        since = "0.0.1",
        note = "Phase 3: use split_focused/close_focused_pane"
    )]
    pub fn set_dual_pane(self: &Arc<Self>, on: bool) {
        if on {
            if self.workspace.read().layout.leaf_count() < 2 {
                self.split_focused(SplitDirection::Horizontal);
            }
        } else if self.workspace.read().layout.leaf_count() > 1 {
            if let Some(id1) = self.pane_id_for_index(1) {
                self.set_focused_pane_id(id1);
                self.close_focused_pane();
            }
        }
    }

    /// Set the focused pane by Slint slot index (0 or 1).
    #[deprecated(since = "0.0.1", note = "Phase 3: use set_focused_pane_id")]
    pub fn set_focused_pane(self: &Arc<Self>, index: usize) {
        if let Some(id) = self.pane_id_for_index(index) {
            self.set_focused_pane_id(id);
        }
    }

    /// Return the filesystem paths of all selected entries in pane `id`.
    ///
    /// Reads the selection mask from the Slint window and the entry list from
    /// the stored location view model. **Must be called on the Slint
    /// event-loop thread.**
    ///
    /// # Caveats
    ///
    /// Only the Details view selection is read. Grid/Miller/Tree selection
    /// reading is a TODO once those views expose a unified selection API.
    #[must_use]
    pub fn selected_paths(&self, id: PaneId) -> Vec<PathBuf> {
        let Some(slint_idx) = self.pane_slint_index.read().get(&id).copied() else {
            return Vec::new();
        };
        let Some(window) = self.window.upgrade() else {
            return Vec::new();
        };

        let mask_model = if slint_idx == 0 {
            window.get_pane0_details_selected_mask()
        } else {
            window.get_pane1_details_selected_mask()
        };

        let vm_guard = self.vms.read();
        let Some(vm) = vm_guard.get(&id) else {
            return Vec::new();
        };
        let entries = vm.entries();

        use slint::Model as _;
        (0..mask_model.row_count())
            .filter(|&i| mask_model.row_data(i).unwrap_or(false))
            .filter_map(|i| entries.get(i).map(|e| e.path.clone()))
            .collect()
    }

    /// Return the path of the focused (cursor) entry in pane `id`, if any.
    ///
    /// **Must be called on the Slint event-loop thread.**
    ///
    /// # Caveats
    ///
    /// Currently reads from the Details view focused index only. Grid/Miller/Tree
    /// are a TODO.
    #[must_use]
    pub fn focused_entry(&self, id: PaneId) -> Option<PathBuf> {
        let slint_idx = self.pane_slint_index.read().get(&id).copied()?;
        let window = self.window.upgrade()?;

        let focused_idx = if slint_idx == 0 {
            window.get_pane0_details_focused_index()
        } else {
            window.get_pane1_details_focused_index()
        };

        if focused_idx < 0 {
            return None;
        }

        let vm_guard = self.vms.read();
        let vm = vm_guard.get(&id)?;
        vm.entries()
            .get(focused_idx as usize)
            .map(|e| e.path.clone())
    }

    fn register_nav_callbacks(self: &Arc<Self>) {
        // Legacy usize-indexed bridge: map the Slint slot index to a PaneId
        // via DFS leaf order, then forward to the shared handler.
        let shell_weak = Arc::downgrade(self);
        self.navigation.on_location_changed(move |pane_usize, vm| {
            let Some(shell) = shell_weak.upgrade() else {
                return;
            };
            let pane_id = {
                let ws = shell.workspace.read();
                ws.layout
                    .all_leaves()
                    .get(pane_usize)
                    .copied()
                    .unwrap_or(ws.focused)
            };
            shell.on_location_changed_impl(pane_id, vm);
        });

        // New PaneId-based callback.
        let shell_weak2 = Arc::downgrade(self);
        self.navigation
            .on_pane_location_changed(move |pane_id, vm| {
                let Some(shell) = shell_weak2.upgrade() else {
                    return;
                };
                shell.on_location_changed_impl(pane_id, vm);
            });
    }

    /// Shared handler for both the legacy and PaneId navigation callbacks.
    fn on_location_changed_impl(
        self: &Arc<Self>,
        pane_id: PaneId,
        vm: Arc<atlas_fs::InMemoryLocationViewModel>,
    ) {
        let path = vm.location().to_path_buf();
        let vm_dyn: Arc<dyn LocationViewModel> = Arc::clone(&vm) as Arc<dyn LocationViewModel>;
        let vm_for_status = Arc::clone(&vm);

        self.vms.write().insert(pane_id, Arc::clone(&vm_dyn));

        {
            let panes = self.panes_ctrl.read();
            if let Some(ctrl) = panes.get(&pane_id) {
                ctrl.details.set_location(Arc::clone(&vm_dyn));
                ctrl.grid.set_location(Arc::clone(&vm_dyn));
                ctrl.gallery.set_location(Arc::clone(&vm_dyn));
                ctrl.tree.set_root(path.clone());
                ctrl.miller.set_root(path.clone());
            }
        }

        {
            let mut workspace = self.workspace.write();
            if let Some(pane_state) = workspace.pane_mut(pane_id) {
                let tab = pane_state.active_mut();
                tab.location = Some(path.clone());
                tab.title = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
            }
        }

        if pane_id == self.focused_pane_id() {
            self.search.set_scope(Some(path.clone()));
        }

        self.project_workspace_to_slint();
        self.refresh_status();

        // Spawn a lightweight watcher that re-computes status whenever the vm
        // emits an event, then exits when the vm subscription channel closes.
        let shell_bg = Arc::clone(self);
        let events = vm_for_status.subscribe();
        std::thread::Builder::new()
            .name(format!("atlas-status-{pane_id:?}"))
            .spawn(move || {
                while let Ok(_ev) = events.recv() {
                    shell_bg.refresh_status();
                }
            })
            .ok();
    }

    fn wire_callbacks(self: &Arc<Self>, window: &AtlasWindow) {
        macro_rules! dispatch {
            ($actions:expr, $action:expr) => {{
                let actions = Arc::clone(&$actions);
                move || actions.lock().dispatch($action)
            }};
        }

        {
            let palette_ctrl = Arc::clone(&self.palette_ctrl);
            window.on_palette_query_changed(move |query| {
                palette_ctrl.set_query(query.as_str());
            });
        }
        {
            let palette_ctrl = Arc::clone(&self.palette_ctrl);
            window.on_palette_confirm(move |_action_id| {
                palette_ctrl.confirm();
            });
        }
        {
            let palette_ctrl = Arc::clone(&self.palette_ctrl);
            window.on_palette_dismiss(move || {
                palette_ctrl.close();
            });
        }
        {
            let palette_ctrl = Arc::clone(&self.palette_ctrl);
            window.on_toggle_palette(move || {
                if palette_ctrl.is_visible() {
                    palette_ctrl.close();
                } else {
                    palette_ctrl.open(0);
                }
            });
        }
        {
            let palette_ctrl = Arc::clone(&self.palette_ctrl);
            window.on_open_goto(move || {
                palette_ctrl.open(1);
            });
        }
        {
            let palette_ctrl = Arc::clone(&self.palette_ctrl);
            window.on_palette_selection_delta(move |delta| {
                palette_ctrl.move_selection(delta as isize);
            });
        }
        {
            let shell = self.clone();
            window.on_select_tab(move |pane, tab| {
                if pane >= 0 && tab >= 0 {
                    if let Some(id) = shell.pane_id_for_index(pane as usize) {
                        shell.select_tab(id, tab as usize);
                    }
                }
            });
        }
        {
            let shell = self.clone();
            window.on_cycle_tab(move |pane, delta| {
                if pane >= 0 {
                    if let Some(id) = shell.pane_id_for_index(pane as usize) {
                        shell.cycle_tab(id, delta as isize);
                    }
                }
            });
        }

        // ── Multi-pane workspace commands ──
        //
        // split-right / split-down split the focused pane; the compat gate in
        // `split_focused` refuses a 3rd leaf until Phase 4 rewrites the Slint UI.
        {
            let shell = self.clone();
            window.on_pane_split_right(move || {
                shell.split_focused(SplitDirection::Horizontal);
            });
        }
        {
            let shell = self.clone();
            window.on_pane_split_down(move || {
                shell.split_focused(SplitDirection::Vertical);
            });
        }
        {
            let shell = self.clone();
            window.on_pane_close(move || shell.close_focused_pane());
        }
        {
            let shell = self.clone();
            window.on_pane_cycle_view_mode(move || shell.cycle_view_mode());
        }
        {
            let shell = self.clone();
            window.on_pane_focus_direction(move |dir| {
                let cardinal = match dir.as_str() {
                    "left" => Cardinal::Left,
                    "right" => Cardinal::Right,
                    "up" => Cardinal::Up,
                    "down" => Cardinal::Down,
                    _ => return,
                };
                shell.focus_direction(cardinal);
            });
        }

        // ── Focused-pane navigation callbacks ────────────────────────────
        // The FocusScope in atlas.slint dispatches these when no modal or
        // text input has focus (arrow keys / Enter / Backspace / vim hjkl).
        // Route to the focused pane's *current view* — details/grid/etc.
        {
            let shell = self.clone();
            window.on_pane_move_focus(move |delta| {
                let id = shell.focused_pane_id();
                let mode = shell
                    .workspace
                    .read()
                    .pane(id)
                    .map(|p| p.view_mode)
                    .unwrap_or_default();
                let Some(ctrl) = shell.pane_by_id(id) else {
                    return;
                };
                match mode {
                    ViewMode::Details => ctrl.details.move_focus(delta as i64),
                    ViewMode::Grid => {
                        ctrl.grid.move_focus(delta as isize, 0);
                    }
                    ViewMode::Gallery => ctrl.gallery.move_focus(delta as isize),
                    ViewMode::Miller => ctrl.miller.move_focus(delta as isize),
                    ViewMode::Tree => ctrl.tree.move_focus(delta as isize),
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane_activate_focused(move || {
                let id = shell.focused_pane_id();
                let mode = shell
                    .workspace
                    .read()
                    .pane(id)
                    .map(|p| p.view_mode)
                    .unwrap_or_default();
                let Some(ctrl) = shell.pane_by_id(id) else {
                    return;
                };
                match mode {
                    ViewMode::Details => ctrl.details.activate_focused(),
                    ViewMode::Grid => ctrl.grid.activate_focused(),
                    ViewMode::Gallery => ctrl.gallery.activate_focused(),
                    ViewMode::Miller => ctrl.miller.activate_focused(),
                    ViewMode::Tree => ctrl.tree.activate_focused(),
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane_go_up(move || {
                shell.go_up(shell.focused_pane_id());
            });
        }

        {
            let search_ctrl = Arc::clone(&self.search);
            let actions = Arc::clone(&self.actions);
            window.on_search_query_changed(move |query| {
                actions
                    .lock()
                    .dispatch(UiAction::SearchQueryChanged(query.to_string()));
                search_ctrl.set_query(query.to_string());
            });
        }
        {
            let search_ctrl = Arc::clone(&self.search);
            let actions = Arc::clone(&self.actions);
            window.on_search_confirm(move |index| {
                actions
                    .lock()
                    .dispatch(UiAction::SearchConfirm(index as usize));
                search_ctrl.confirm(index as usize);
            });
        }
        {
            let search_ctrl = Arc::clone(&self.search);
            let actions = Arc::clone(&self.actions);
            window.on_search_close(move || {
                actions.lock().dispatch(UiAction::SearchClose);
                search_ctrl.close();
            });
        }
        {
            let search_ctrl = Arc::clone(&self.search);
            let actions = Arc::clone(&self.actions);
            window.on_toggle_search_panel(move || {
                actions.lock().dispatch(UiAction::ToggleSearchPanel);
                if search_ctrl.is_open() {
                    search_ctrl.close();
                } else {
                    search_ctrl.open();
                }
            });
        }
        {
            let search_ctrl = Arc::clone(&self.search);
            let actions = Arc::clone(&self.actions);
            window.on_open_search_panel(move || {
                actions.lock().dispatch(UiAction::OpenSearchPanel);
                search_ctrl.open();
            });
        }

        {
            let actions = Arc::clone(&self.actions);
            let shell = Arc::clone(self);
            window.on_pane0_focused(move || {
                if let Some(id) = shell.pane_id_for_index(0) {
                    actions.lock().dispatch(UiAction::PaneFocusChanged(0));
                    shell.set_focused_pane_id(id);
                }
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            let shell = Arc::clone(self);
            window.on_pane1_focused(move || {
                if let Some(id) = shell.pane_id_for_index(1) {
                    actions.lock().dispatch(UiAction::PaneFocusChanged(1));
                    shell.set_focused_pane_id(id);
                }
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            let shell = Arc::clone(self);
            window.on_cycle_pane_focus(move || {
                let leaves = shell.workspace.read().layout.all_leaves();
                if leaves.is_empty() {
                    return;
                }
                let focused = shell.focused_pane_id();
                let cur = leaves.iter().position(|&id| id == focused).unwrap_or(0);
                let next = (cur + 1) % leaves.len();
                actions.lock().dispatch(UiAction::PaneFocusChanged(next));
                shell.set_focused_pane_id(leaves[next]);
            });
        }

        {
            let actions = Arc::clone(&self.actions);
            let nav = Arc::clone(&self.navigation);
            let shell = Arc::clone(self);
            window.on_pane0_address_submitted(move |path| {
                dispatch_navigation(&actions, 0, path.clone());
                if let Some(id) = shell.pane_id_for_index(0) {
                    let expanded = expand_tilde(Path::new(path.as_str()));
                    nav.navigate_pane(id, expanded);
                }
            });
        }
        window.on_pane0_address_cancelled(dispatch!(self.actions, UiAction::DismissPalette));
        {
            let actions = Arc::clone(&self.actions);
            let shell = Arc::clone(self);
            window.on_pane0_breadcrumb_clicked(move |segment| {
                let seg = segment as usize;
                actions.lock().dispatch(UiAction::BreadcrumbClicked {
                    pane: 0,
                    segment: seg,
                });
                if let Some(id) = shell.pane_id_for_index(0) {
                    shell.breadcrumb_clicked(id, seg);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_tab_selected(move |tab| {
                if let Some(id) = shell.pane_id_for_index(0) {
                    shell.select_tab(id, tab as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_tab_closed(move |tab| {
                if let Some(id) = shell.pane_id_for_index(0) {
                    shell.close_tab(id, tab as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_new_tab(move || {
                if let Some(id) = shell.pane_id_for_index(0) {
                    shell.new_tab(id);
                }
            });
        }

        {
            let shell = self.clone();
            window.on_pane0_details_row_clicked(move |index, ctrl, shift| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.details.select_index(index as usize, ctrl, shift);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_details_row_double_clicked(move |index| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.details.select_index(index as usize, false, false);
                    c.details.activate_focused();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_details_header_clicked(move |column_index| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.details.header_clicked(column_index as usize);
                }
            });
        }

        // Grid callbacks — pane 0
        {
            let shell = self.clone();
            window.on_pane0_grid_entry_clicked(move |index, ctrl, shift| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.grid.select_index(index as usize, ctrl, shift);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_grid_entry_double_clicked(move |index| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.grid.select_index(index as usize, false, false);
                    c.grid.activate_focused();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_grid_thumbnail_visible(move |index| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.grid.thumbnail_visible(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_grid_columns_changed(move |cols| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.grid.set_columns(cols as usize);
                }
            });
        }

        // Gallery callbacks — pane 0
        {
            let shell = self.clone();
            window.on_pane0_gallery_entry_clicked(move |index| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.gallery.entry_clicked(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_gallery_strip_visible(move |index| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.gallery.strip_visible(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_gallery_preview_visible(move |index| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.gallery.preview_visible(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_gallery_prev_image(move || {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.gallery.prev_image();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_gallery_next_image(move || {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.gallery.next_image();
                }
            });
        }

        // Tree callbacks — pane 0
        {
            let shell = self.clone();
            window.on_pane0_tree_row_clicked(move |index, ctrl, shift| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.tree.select_index(index as usize, ctrl, shift);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_tree_row_double_clicked(move |index| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.tree.select_index(index as usize, false, false);
                    c.tree.activate_focused();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_tree_chevron_clicked(move |index| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    let visible = c.tree.build_visible_nodes();
                    if let Some(row) = visible.get(index as usize) {
                        let path = std::path::PathBuf::from(row.node_id.as_str());
                        c.tree.toggle(&path);
                    }
                }
            });
        }

        // Miller callbacks — pane 0
        {
            let shell = self.clone();
            window.on_pane0_miller_row_clicked(move |col, row| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.miller.select_row(col as usize, row as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_miller_row_double_clicked(move |col, row| {
                if let Some(c) = shell.ctrl_for_index(0) {
                    c.miller.select_row(col as usize, row as usize);
                    c.miller.activate_focused();
                }
            });
        }

        {
            let actions = Arc::clone(&self.actions);
            let nav = Arc::clone(&self.navigation);
            let shell = Arc::clone(self);
            window.on_pane1_address_submitted(move |path| {
                dispatch_navigation(&actions, 1, path.clone());
                if let Some(id) = shell.pane_id_for_index(1) {
                    let expanded = expand_tilde(Path::new(path.as_str()));
                    nav.navigate_pane(id, expanded);
                }
            });
        }
        window.on_pane1_address_cancelled(dispatch!(self.actions, UiAction::DismissPalette));
        {
            let actions = Arc::clone(&self.actions);
            let shell = Arc::clone(self);
            window.on_pane1_breadcrumb_clicked(move |segment| {
                let seg = segment as usize;
                actions.lock().dispatch(UiAction::BreadcrumbClicked {
                    pane: 1,
                    segment: seg,
                });
                if let Some(id) = shell.pane_id_for_index(1) {
                    shell.breadcrumb_clicked(id, seg);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_tab_selected(move |tab| {
                if let Some(id) = shell.pane_id_for_index(1) {
                    shell.select_tab(id, tab as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_tab_closed(move |tab| {
                if let Some(id) = shell.pane_id_for_index(1) {
                    shell.close_tab(id, tab as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_new_tab(move || {
                if let Some(id) = shell.pane_id_for_index(1) {
                    shell.new_tab(id);
                }
            });
        }

        // Details callbacks — pane 1
        {
            let shell = self.clone();
            window.on_pane1_details_row_clicked(move |index, ctrl, shift| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.details.select_index(index as usize, ctrl, shift);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_details_row_double_clicked(move |index| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.details.select_index(index as usize, false, false);
                    c.details.activate_focused();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_details_header_clicked(move |column_index| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.details.header_clicked(column_index as usize);
                }
            });
        }

        // Grid callbacks — pane 1
        {
            let shell = self.clone();
            window.on_pane1_grid_entry_clicked(move |index, ctrl, shift| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.grid.select_index(index as usize, ctrl, shift);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_grid_entry_double_clicked(move |index| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.grid.select_index(index as usize, false, false);
                    c.grid.activate_focused();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_grid_thumbnail_visible(move |index| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.grid.thumbnail_visible(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_grid_columns_changed(move |cols| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.grid.set_columns(cols as usize);
                }
            });
        }

        // Gallery callbacks — pane 1
        {
            let shell = self.clone();
            window.on_pane1_gallery_entry_clicked(move |index| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.gallery.entry_clicked(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_gallery_strip_visible(move |index| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.gallery.strip_visible(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_gallery_preview_visible(move |index| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.gallery.preview_visible(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_gallery_prev_image(move || {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.gallery.prev_image();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_gallery_next_image(move || {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.gallery.next_image();
                }
            });
        }

        // Tree callbacks — pane 1
        {
            let shell = self.clone();
            window.on_pane1_tree_row_clicked(move |index, ctrl, shift| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.tree.select_index(index as usize, ctrl, shift);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_tree_row_double_clicked(move |index| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.tree.select_index(index as usize, false, false);
                    c.tree.activate_focused();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_tree_chevron_clicked(move |index| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    let visible = c.tree.build_visible_nodes();
                    if let Some(row) = visible.get(index as usize) {
                        let path = std::path::PathBuf::from(row.node_id.as_str());
                        c.tree.toggle(&path);
                    }
                }
            });
        }

        // Miller callbacks — pane 1
        {
            let shell = self.clone();
            window.on_pane1_miller_row_clicked(move |col, row| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.miller.select_row(col as usize, row as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_miller_row_double_clicked(move |col, row| {
                if let Some(c) = shell.ctrl_for_index(1) {
                    c.miller.select_row(col as usize, row as usize);
                    c.miller.activate_focused();
                }
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            let shell = Arc::clone(self);
            window.on_toggle_dual_pane(move || {
                let dual = shell.workspace.read().layout.leaf_count() > 1;
                actions.lock().dispatch(UiAction::SetDualPane(!dual));
                if dual {
                    if let Some(id1) = shell.pane_id_for_index(1) {
                        shell.set_focused_pane_id(id1);
                        shell.close_focused_pane();
                    }
                } else {
                    shell.split_focused(SplitDirection::Horizontal);
                }
            });
        }

        // ── Ops-panel callbacks ───────────────────────────────────────────────

        {
            let ops = Arc::clone(&self.ops);
            window.on_ops_cancel(move |index| {
                tracing::debug!(index, "ops-cancel from UI");
                ops.cancel_by_index(index as usize);
            });
        }
        {
            let ops = Arc::clone(&self.ops);
            window.on_ops_dismiss(move |index| {
                tracing::debug!(index, "ops-dismiss from UI");
                ops.dismiss_by_index(index as usize);
            });
        }
        {
            let ops = Arc::clone(&self.ops);
            window.on_ops_close(move || {
                ops.set_visible(false);
            });
        }
        {
            let ops = Arc::clone(&self.ops);
            window.on_toggle_ops_panel(move || {
                ops.toggle_visible();
            });
        }

        // ── F-key file-operation callbacks ────────────────────────────────────
        // These callbacks are triggered from the atlas.slint FocusScope key
        // handlers (F2, F5, F6, F7, F8) and routed directly to OpsController
        // rather than through the ActionSink, matching the pattern used by
        // PaletteController and SearchController.

        {
            let shell = Arc::clone(self);
            window.on_fs_copy(move || {
                let focused = shell.focused_pane_id();
                let sources = shell.selected_paths(focused);
                if sources.is_empty() {
                    tracing::warn!(?focused, "fs::Copy (F5): no selection");
                    return;
                }
                // The other pane (second DFS leaf) is the destination.
                // Single-pane: destination dialog is a post-MVP follow-up.
                let leaves = shell.workspace.read().layout.all_leaves();
                let other = leaves.iter().find(|&&id| id != focused).copied();
                let dest = other.and_then(|id| shell.pane_location(id));
                match dest {
                    Some(dest_dir) => {
                        tracing::info!(
                            sources = sources.len(),
                            dest = %dest_dir.display(),
                            "fs::Copy (F5)"
                        );
                        shell.ops.submit_copy(sources, dest_dir);
                    }
                    None => {
                        tracing::warn!(
                            "fs::Copy (F5): no destination pane; \
                             a destination-path dialog is a post-MVP follow-up"
                        );
                    }
                }
            });
        }
        {
            let shell = Arc::clone(self);
            window.on_fs_move(move || {
                let focused = shell.focused_pane_id();
                let sources = shell.selected_paths(focused);
                if sources.is_empty() {
                    tracing::warn!(?focused, "fs::Move (F6): no selection");
                    return;
                }
                let leaves = shell.workspace.read().layout.all_leaves();
                let other = leaves.iter().find(|&&id| id != focused).copied();
                let dest = other.and_then(|id| shell.pane_location(id));
                match dest {
                    Some(dest_dir) => {
                        tracing::info!(
                            sources = sources.len(),
                            dest = %dest_dir.display(),
                            "fs::Move (F6)"
                        );
                        shell.ops.submit_move(sources, dest_dir);
                    }
                    None => {
                        tracing::warn!(
                            "fs::Move (F6): no destination pane; \
                             a destination-path dialog is a post-MVP follow-up"
                        );
                    }
                }
            });
        }
        {
            let shell = Arc::clone(self);
            window.on_fs_delete(move || {
                let focused = shell.focused_pane_id();
                let paths = shell.selected_paths(focused);
                if paths.is_empty() {
                    tracing::warn!(?focused, "fs::Delete (F8): no selection");
                    return;
                }
                tracing::info!(count = paths.len(), "fs::Delete (F8) → trash");
                // F8 always sends to trash (non-destructive default).
                // Shift+F8 for permanent delete is a post-MVP binding.
                shell.ops.submit_delete(paths, true);
            });
        }
        {
            let shell = Arc::clone(self);
            window.on_fs_rename(move || {
                let focused = shell.focused_pane_id();
                // TODO(post-MVP): show an inline rename text-input or modal dialog.
                // For now we log the focused entry and skip the operation.
                match shell.focused_entry(focused) {
                    Some(path) => {
                        tracing::info!(
                            path = %path.display(),
                            "fs::Rename (F2): rename dialog not yet implemented (post-MVP)"
                        );
                    }
                    None => {
                        tracing::warn!(?focused, "fs::Rename (F2): no focused entry");
                    }
                }
            });
        }
        {
            let shell = Arc::clone(self);
            window.on_fs_mkdir(move || {
                let focused = shell.focused_pane_id();
                let Some(location) = shell.pane_location(focused) else {
                    tracing::warn!(?focused, "fs::Mkdir (F7): no pane location");
                    return;
                };
                // Choose a unique "New Folder" name within the current location.
                let name = unique_new_folder_name(&location);
                let path = location.join(&name);
                tracing::info!(path = %path.display(), "fs::Mkdir (F7)");
                shell.ops.submit_mkdir(path);
            });
        }

        // ── Bulk-rename callbacks ─────────────────────────────────────────────
        // Cmd/Ctrl+Shift+F2 → open-bulk-rename → open with current selection.
        {
            let shell = Arc::clone(self);
            window.on_open_bulk_rename(move || {
                let focused = shell.focused_pane_id();
                let paths = shell.selected_paths(focused);
                tracing::info!(
                    ?focused,
                    count = paths.len(),
                    "bulk rename: opening modal (Cmd/Ctrl+Shift+F2)"
                );
                shell.bulk_rename.open(paths);
            });
        }
        {
            let bulk_rename = Arc::clone(&self.bulk_rename);
            window.on_bulk_rename_pattern_changed(move |q| {
                bulk_rename.set_pattern(q.to_string());
            });
        }
        {
            let bulk_rename = Arc::clone(&self.bulk_rename);
            window.on_bulk_rename_replacement_changed(move |q| {
                bulk_rename.set_replacement(q.to_string());
            });
        }
        {
            let bulk_rename = Arc::clone(&self.bulk_rename);
            window.on_bulk_rename_toggle_regex(move || {
                bulk_rename.toggle_regex();
            });
        }
        {
            let bulk_rename = Arc::clone(&self.bulk_rename);
            window.on_bulk_rename_toggle_case(move || {
                bulk_rename.toggle_case_insensitive();
            });
        }
        {
            let bulk_rename = Arc::clone(&self.bulk_rename);
            window.on_bulk_rename_confirm(move || {
                bulk_rename.confirm();
            });
        }
        {
            let bulk_rename = Arc::clone(&self.bulk_rename);
            window.on_bulk_rename_cancel(move || {
                bulk_rename.close();
            });
        }
    }

    /// Project the workspace's first ≤2 layout leaves (in DFS order) onto the
    /// existing `pane0-*` / `pane1-*` Slint properties.
    ///
    /// The Slint UI is still 2-pane-capable (Phase 4 rewrites it), so any
    /// leaves beyond the first two are dropped with a warning.
    pub fn project_workspace_to_slint(self: &Arc<Self>) {
        struct PaneSnapshot {
            path: String,
            segments: Vec<String>,
            view_mode: String,
            tabs: Vec<TabModel>,
            active_tab: i32,
        }

        let (focus_idx, leaf_count, pane0, pane1) = {
            let ws = self.workspace.read();
            let leaves = ws.layout.all_leaves();
            let leaf_count = leaves.len();
            let focus_idx = leaves.iter().position(|&id| id == ws.focused).unwrap_or(0);

            let snapshot_for = |id: PaneId| -> PaneSnapshot {
                let pane = ws.pane(id).expect("leaf must have pane state");
                let location = pane.active_location();
                PaneSnapshot {
                    path: location.to_string_lossy().into_owned(),
                    segments: path_segments_for(location),
                    view_mode: pane.view_mode.to_string(),
                    tabs: pane.tabs.clone(),
                    active_tab: pane.active_tab as i32,
                }
            };

            let pane0 = leaves.first().map(|&id| snapshot_for(id));
            let pane1 = leaves.get(1).map(|&id| snapshot_for(id));
            (focus_idx, leaf_count, pane0, pane1)
        };

        if leaf_count > 2 {
            tracing::warn!(
                leaf_count,
                "project_workspace_to_slint: only first 2 panes projected to Slint (Phase 4 pending)"
            );
        }

        let is_dual = leaf_count > 1;
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };

            window.set_dual_pane(is_dual);
            window.set_focus_index(focus_idx as i32);

            if let Some(pane0) = pane0 {
                window.set_pane0_path(pane0.path.into());
                window.set_pane0_segments(to_segments_model(&pane0.segments));
                window.set_pane0_view_mode(pane0.view_mode.into());
                window.set_pane0_tabs(to_tab_model(&pane0.tabs));
                window.set_pane0_active_tab(pane0.active_tab);
            }

            if let Some(pane1) = pane1 {
                window.set_pane1_path(pane1.path.into());
                window.set_pane1_segments(to_segments_model(&pane1.segments));
                window.set_pane1_view_mode(pane1.view_mode.into());
                window.set_pane1_tabs(to_tab_model(&pane1.tabs));
                window.set_pane1_active_tab(pane1.active_tab);
            } else {
                window.set_pane1_path(SharedString::default());
                window.set_pane1_segments(ModelRc::new(VecModel::from(Vec::<SharedString>::new())));
                window.set_pane1_view_mode(SharedString::from("Details"));
                window.set_pane1_tabs(ModelRc::new(VecModel::from(Vec::<TabEntry>::new())));
                window.set_pane1_active_tab(0);
            }
        });
    }

    /// Update palette state.
    pub fn set_palette(self: &Arc<Self>, model: PaletteModel) {
        *self.palette.write() = model.clone();
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            window.set_palette_visible(model.visible);
            window.set_palette_query(SharedString::from(model.query.as_str()));
            window.set_palette_results(to_palette_model(&model.results));
            window.set_palette_selected(model.selected as i32);
        });
    }

    /// Update status bar state.
    pub fn set_status(self: &Arc<Self>, model: StatusModel) {
        *self.status.write() = model.clone();
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            let indexer_status = model.indexer_state.to_string();
            window.set_total_entries(model.total_entries as i32);
            window.set_folder_count(model.folder_count as i32);
            window.set_file_count(model.file_count as i32);
            window.set_total_size_text(crate::format_size(model.total_bytes).into());
            window.set_selected_entries(model.selected_entries as i32);
            window.set_selected_size_text(crate::format_size(model.selected_bytes).into());
            window.set_indexer_status(indexer_status.into());
        });
    }

    /// Recompute status stats from the focused pane's current entries and push.
    ///
    /// Cheap to call — walks the in-memory entry snapshot. Invoked whenever
    /// the location changes or the entry list updates.
    pub fn refresh_status(self: &Arc<Self>) {
        let id = self.focused_pane_id();
        let vm = self.vms.read().get(&id).cloned();
        let Some(vm) = vm else {
            return;
        };
        let entries = vm.entries();
        let mut folders = 0usize;
        let mut files = 0usize;
        let mut total_bytes: u64 = 0;
        for e in &entries {
            match e.kind {
                atlas_fs::EntryKind::Dir => folders += 1,
                _ => {
                    files += 1;
                    total_bytes += e.metadata.size;
                }
            }
        }
        let existing = self.status.read().clone();
        let model = StatusModel {
            total_entries: entries.len(),
            folder_count: folders,
            file_count: files,
            total_bytes,
            selected_entries: existing.selected_entries,
            selected_bytes: existing.selected_bytes,
            indexer_state: existing.indexer_state,
        };
        self.set_status(model);
    }

    /// Apply a theme mode (convenience wrapper over [`Self::apply_theme`]).
    ///
    /// Loads the built-in tokens for `theme` and delegates to `apply_theme`.
    pub fn set_theme(self: &Arc<Self>, theme: ThemeMode) {
        let tokens = if theme.is_dark() {
            defaults::default_dark()
        } else {
            defaults::default_light()
        };
        self.apply_theme(&tokens);
    }

    /// Push all [`ThemeTokens`] into the Slint `Theme` global.
    ///
    /// Color, typography, and chrome values are forwarded through the
    /// `theme-*` bridge properties on `AtlasWindow` (defined in
    /// `assets/ui/atlas.slint`), which propagate them to the `Theme` global
    /// via `changed` callbacks.
    ///
    /// May be called from any thread; updates are marshalled onto the Slint
    /// event loop via [`slint::invoke_from_event_loop`].
    pub fn apply_theme(self: &Arc<Self>, tokens: &ThemeTokens) {
        let tokens = tokens.clone();
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };

            let c = &tokens.colors;
            window.set_theme_bg(c.bg.to_slint_color());
            window.set_theme_panel_bg(c.panel_bg.to_slint_color());
            window.set_theme_panel_bg_elevated(c.panel_bg_elevated.to_slint_color());
            window.set_theme_fg(c.fg.to_slint_color());
            window.set_theme_fg_muted(c.fg_muted.to_slint_color());
            window.set_theme_fg_faint(c.fg_faint.to_slint_color());
            window.set_theme_border(c.border.to_slint_color());
            window.set_theme_border_strong(c.border_strong.to_slint_color());
            window.set_theme_accent(c.accent.to_slint_color());
            window.set_theme_accent_fg(c.accent_fg.to_slint_color());
            window.set_theme_accent_soft(c.accent_soft.to_slint_color());
            window.set_theme_selection_bg(c.selection_bg.to_slint_color());
            window.set_theme_selection_fg(c.selection_fg.to_slint_color());
            window.set_theme_hover_bg(c.hover_bg.to_slint_color());
            window.set_theme_error(c.error.to_slint_color());
            window.set_theme_success(c.success.to_slint_color());
            window.set_theme_warning(c.warning.to_slint_color());

            let t = &tokens.typography;
            window.set_theme_font_family(t.font_family.as_str().into());
            window.set_theme_monospace(t.monospace_family.as_str().into());
            window.set_theme_font_size(t.font_size_pt);

            let ch = &tokens.chrome;
            window.set_theme_titlebar_h(ch.titlebar_h_px);
            window.set_theme_statusbar_h(ch.statusbar_h_px);
            window.set_theme_tab_h(ch.tab_h_px);
            window.set_theme_addressbar_h(ch.addressbar_h_px);
            window.set_theme_row_h_default(ch.row_h_default_px);
            window.set_theme_row_h_compact(ch.row_h_compact_px);
            window.set_theme_row_h_spacious(ch.row_h_spacious_px);
            window.set_theme_radius_xs(ch.radius_xs_px);
            window.set_theme_radius_sm(ch.radius_sm_px);
            window.set_theme_radius_md(ch.radius_md_px);
            window.set_theme_radius_lg(ch.radius_lg_px);
            window.set_theme_radius_xl(ch.radius_xl_px);
            window.set_theme_space_1(ch.space_1_px);
            window.set_theme_space_2(ch.space_2_px);
            window.set_theme_space_3(ch.space_3_px);
            window.set_theme_space_4(ch.space_4_px);
            window.set_theme_space_5(ch.space_5_px);
            window.set_theme_space_6(ch.space_6_px);
            window.set_theme_space_8(ch.space_8_px);
            window.set_theme_space_10(ch.space_10_px);
            window.set_theme_spacing_xs(ch.space_1_px);
            window.set_theme_spacing_sm(ch.space_2_px);
            window.set_theme_spacing_md(ch.space_3_px);
            window.set_theme_spacing_lg(ch.space_4_px);

            window.set_dark(tokens.mode.is_dark());
        });
    }
}

/// Return a "New Folder" name that does not yet exist in `parent_dir`.
///
/// Tries `"New Folder"`, then `"New Folder 2"`, `"New Folder 3"`, … up to 99.
fn unique_new_folder_name(parent_dir: &Path) -> String {
    let base = "New Folder";
    if !parent_dir.join(base).exists() {
        return base.to_owned();
    }
    for n in 2u32..=99 {
        let candidate = format!("{base} {n}");
        if !parent_dir.join(&candidate).exists() {
            return candidate;
        }
    }
    // Fallback: very unlikely in practice.
    base.to_owned()
}

#[cfg(test)]
mod tests {
    //! Tests for the pane-index ↔ `PaneId` DFS mapping and split-tree
    //! mutations that back [`AppShell`]'s Slint-slot compatibility layer.
    //!
    //! These operate on [`WorkspaceModel`] directly (no Slint window), since
    //! `AppShell` construction requires a live event loop.

    use crate::models::{
        pane_state::PaneState,
        split::{Cardinal, PaneId, Rect, SplitDirection},
        tab::TabModel,
        ViewMode, WorkspaceModel,
    };

    fn workspace_at(path: &str) -> WorkspaceModel {
        let id = PaneId(1);
        WorkspaceModel::new(PaneState::new(id, TabModel::at(path), ViewMode::Details))
    }

    /// Resolve a Slint slot index (0/1) to a `PaneId` via DFS leaf order —
    /// mirrors `AppShell::pane_id_for_index`.
    fn index_to_id(ws: &WorkspaceModel, index: usize) -> Option<PaneId> {
        ws.layout.all_leaves().get(index).copied()
    }

    #[test]
    fn pane_id_for_index_single_pane() {
        let ws = workspace_at("/a");
        assert_eq!(index_to_id(&ws, 0), Some(PaneId(1)));
        assert_eq!(index_to_id(&ws, 1), None);
    }

    #[test]
    fn split_and_both_indices_resolve() {
        let mut ws = workspace_at("/a");
        let new_id = ws.split_focused(SplitDirection::Horizontal, None);
        assert_eq!(index_to_id(&ws, 0), Some(PaneId(1)));
        assert_eq!(index_to_id(&ws, 1), Some(new_id));
        assert_eq!(ws.layout.leaf_count(), 2);
    }

    #[test]
    fn close_focused_leaves_one_pane() {
        let mut ws = workspace_at("/a");
        let new_id = ws.split_focused(SplitDirection::Horizontal, None);
        assert_eq!(ws.focused, new_id);
        let outcome = ws.close_focused().expect("two panes → close succeeds");
        assert_eq!(outcome.removed, new_id);
        assert_eq!(ws.focused, PaneId(1));
        assert_eq!(ws.layout.leaf_count(), 1);
        assert_eq!(index_to_id(&ws, 1), None);
    }

    #[test]
    fn focus_direction_in_two_pane_horizontal_split() {
        let mut ws = workspace_at("/a");
        let right = ws.split_focused(SplitDirection::Horizontal, None);
        let bounds = Rect::from_size(200.0, 200.0);
        // Focus is on the right pane after split; move left → pane 0.
        assert_eq!(ws.focus_direction(Cardinal::Left, bounds), Some(PaneId(1)));
        assert_eq!(ws.focused, PaneId(1));
        // Move right → back to the new pane.
        assert_eq!(ws.focus_direction(Cardinal::Right, bounds), Some(right));
        assert_eq!(ws.focused, right);
    }

    #[test]
    fn dfs_ordering_stable_across_splits() {
        let mut ws = workspace_at("/a");
        let right = ws.split_focused(SplitDirection::Horizontal, None);
        assert!(ws.set_focused(PaneId(1)));
        let down = ws.split_focused(SplitDirection::Vertical, None);
        // DFS order: pane 1's subtree (1, down) then the right sibling.
        assert_eq!(ws.leaves_in_order(), vec![PaneId(1), down, right]);
    }
}
