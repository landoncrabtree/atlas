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
    collections::VecDeque,
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
        split::{Cardinal, PaneId, Rect, SplitDirection, SplitLayout},
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
    AtlasWindow, PaletteEntry, PaneSlintData, SplitHandle, TabEntry,
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

/// Raw (non-Slint) descriptor for a split-handle grab area.
struct SplitHandleData {
    node_index: i32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    horizontal: bool,
}

/// Walk the split tree in DFS order and collect one grab-area descriptor per
/// internal `Split` node.  `node_index` is the DFS visit order (0, 1, …).
fn collect_split_handles(layout: &SplitLayout, bounds: Rect) -> Vec<SplitHandleData> {
    let mut handles = Vec::new();
    collect_handles_recurse(layout, bounds, &mut handles, &mut 0i32);
    handles
}

fn collect_handles_recurse(
    layout: &SplitLayout,
    bounds: Rect,
    handles: &mut Vec<SplitHandleData>,
    node_idx: &mut i32,
) {
    let SplitLayout::Split {
        direction,
        ratio,
        first,
        second,
    } = layout
    else {
        return;
    };

    let idx = *node_idx;
    *node_idx += 1;
    let ratio = ratio.clamp(0.05, 0.95);

    let (first_bounds, second_bounds, handle) = match direction {
        SplitDirection::Horizontal => {
            let split_x = bounds.x + bounds.width * ratio;
            (
                Rect {
                    x: bounds.x,
                    y: bounds.y,
                    width: bounds.width * ratio,
                    height: bounds.height,
                },
                Rect {
                    x: split_x,
                    y: bounds.y,
                    width: bounds.width * (1.0 - ratio),
                    height: bounds.height,
                },
                SplitHandleData {
                    node_index: idx,
                    x: split_x - 2.0,
                    y: bounds.y,
                    width: 4.0,
                    height: bounds.height,
                    horizontal: false,
                },
            )
        }
        SplitDirection::Vertical => {
            let split_y = bounds.y + bounds.height * ratio;
            (
                Rect {
                    x: bounds.x,
                    y: bounds.y,
                    width: bounds.width,
                    height: bounds.height * ratio,
                },
                Rect {
                    x: bounds.x,
                    y: split_y,
                    width: bounds.width,
                    height: bounds.height * (1.0 - ratio),
                },
                SplitHandleData {
                    node_index: idx,
                    x: bounds.x,
                    y: split_y - 2.0,
                    width: bounds.width,
                    height: 4.0,
                    horizontal: true,
                },
            )
        }
    };

    handles.push(handle);
    collect_handles_recurse(first, first_bounds, handles, node_idx);
    collect_handles_recurse(second, second_bounds, handles, node_idx);
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
/// view models are keyed by [`PaneId`]. The Slint UI renders panes via a
/// `for pane[i] in panes` loop driven by [`PaneSlintData`] pushed by
/// `project_workspace_to_slint`. The `pane_slint_index` map tracks each
/// pane's DFS slot index so per-slot heavy-data properties remain routed
/// correctly while view controllers are migrated to an N-pane model.
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
    /// Recently-closed tabs per pane, newest first. Bounded to 20 entries per pane.
    closed_tabs: RwLock<AHashMap<PaneId, VecDeque<TabModel>>>,
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
            closed_tabs: RwLock::new(AHashMap::default()),
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
    ///
    /// Used by the `toggle-dual-pane` callback and Phase 4.1 migration code.
    #[allow(dead_code)]
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
    /// Creates a new pane by splitting the currently focused leaf. The new pane
    /// inherits the focused pane's current location. After Phase 4 the Slint UI
    /// renders N panes, so any number of splits is supported.
    pub fn split_focused(self: &Arc<Self>, direction: SplitDirection) -> Option<PaneId> {
        let leaf_count = self.workspace.read().layout.leaf_count();
        // Determine which DFS slot the new pane will occupy (= current count).
        let new_slot = leaf_count;

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

        // Assign the new pane to its DFS slot.
        self.pane_slint_index.write().insert(new_id, new_slot);

        // Build controllers for the new pane (slot index used for push routing).
        let window = self.window.upgrade().expect("window must be alive");
        let new_ctrl = build_pane_controllers(
            new_id,
            new_slot,
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

    /// The workspace content area: window bounds minus the titlebar, toolbar,
    /// status bar, and shortcut footer.  This is the rectangle that the pane
    /// container fills and that `layout_rects` must be called against.
    ///
    /// The constant offsets below match the fixed pixel heights used in
    /// `atlas.slint` (toolbar 32 px, status 24 px, footer 24 px).  Adjust
    /// if those values ever change.
    fn workspace_content_bounds(&self) -> Rect {
        let wb = self.window_bounds();
        // Toolbar (32 px) + status bar (24 px) + shortcut footer (24 px).
        // Titlebar is drawn by the OS and already excluded from logical pixels.
        const TOP_CHROME: f32 = 32.0;
        const BOTTOM_CHROME: f32 = 24.0 + 24.0;
        let height = (wb.height - TOP_CHROME - BOTTOM_CHROME).max(1.0);
        Rect {
            x: 0.0,
            y: 0.0,
            width: wb.width.max(1.0),
            height,
        }
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
    /// when the active tab changed. Pushes the removed tab onto the
    /// per-pane closed-tab history (bounded at 20) for `reopen_closed_tab`.
    pub fn close_tab(self: &Arc<Self>, id: PaneId, tab: usize) {
        // Release the workspace lock before acquiring closed_tabs to avoid
        // any potential lock-ordering issues.
        let result: Option<(TabModel, bool)> = {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(id) else {
                tracing::debug!(?id, tab, "close_tab: pane not found");
                return;
            };
            let was_active = tab == p.active_tab;
            p.close_tab(tab).map(|removed| (removed, was_active))
        };
        let switch_to = if let Some((removed, was_active)) = result {
            {
                let mut ct = self.closed_tabs.write();
                let deque = ct.entry(id).or_default();
                deque.push_front(removed);
                if deque.len() > 20 {
                    deque.pop_back();
                }
            }
            if was_active {
                self.workspace
                    .read()
                    .pane(id)
                    .map(|p| p.active_location().to_path_buf())
            } else {
                None
            }
        } else {
            None
        };
        self.project_workspace_to_slint();
        if let Some(dest) = switch_to {
            self.navigation.navigate_pane(id, dest);
        }
    }

    /// Move the tab at `from` to `to` within pane `pane`. Tabs between the
    /// two positions shift by one to fill the gap. The active tab tracks the
    /// moved tab so it stays selected. No-op when `from == to` or either
    /// index is out of range.
    pub fn reorder_tab(self: &Arc<Self>, pane: PaneId, from: usize, to: usize) {
        if from == to {
            return;
        }
        {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(pane) else { return };
            let len = p.tabs.len();
            if from >= len || to >= len {
                return;
            }
            let tab = p.tabs.remove(from);
            p.tabs.insert(to, tab);
            // Adjust the active-tab index so selection follows the moved tab.
            if p.active_tab == from {
                p.active_tab = to;
            } else if from < to {
                // Tabs in (from, to] shifted left by one.
                if p.active_tab > from && p.active_tab <= to {
                    p.active_tab -= 1;
                }
            } else {
                // from > to; tabs in [to, from) shifted right by one.
                if p.active_tab >= to && p.active_tab < from {
                    p.active_tab += 1;
                }
            }
        }
        self.project_workspace_to_slint();
    }

    /// Duplicate the tab at `tab` in pane `pane`, inserting the copy
    /// immediately after and activating it. The copy starts with fresh
    /// history containing only the current location, but inherits the
    /// source tab's sort specification and filter.
    pub fn duplicate_tab(self: &Arc<Self>, pane: PaneId, tab: usize) {
        let (new_loc, src_sort, src_filter) = {
            let ws = self.workspace.read();
            let Some(p) = ws.pane(pane) else { return };
            if tab >= p.tabs.len() {
                return;
            }
            let src = &p.tabs[tab];
            let loc = src.location.clone().unwrap_or_else(dirs_home);
            (loc, src.sort.clone(), src.filter.clone())
        };
        // Build the duplicate: fresh history, inherited sort + filter.
        let mut new_tab = TabModel::at(new_loc.clone());
        new_tab.sort = src_sort;
        new_tab.filter = src_filter;
        let insert_at = tab + 1;
        {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(pane) else { return };
            p.tabs.insert(insert_at, new_tab);
            p.active_tab = insert_at;
        }
        self.project_workspace_to_slint();
        self.navigation.navigate_pane(pane, new_loc);
    }

    /// Close every tab in pane `pane` except the one at index `keep`.
    /// Refuses when the pane has only one tab or `keep` is out of range.
    /// All removed tabs are pushed onto the closed-tab history.
    pub fn close_other_tabs(self: &Arc<Self>, pane: PaneId, keep: usize) {
        let (switch_to, closed) = {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(pane) else { return };
            if p.tabs.len() <= 1 || keep >= p.tabs.len() {
                return;
            }
            let kept = p.tabs[keep].clone();
            let all: Vec<TabModel> = std::mem::replace(&mut p.tabs, vec![kept]);
            let closed: Vec<TabModel> = all
                .into_iter()
                .enumerate()
                .filter_map(|(i, t)| (i != keep).then_some(t))
                .collect();
            p.active_tab = 0;
            let dest = p.active_location().to_path_buf();
            (dest, closed)
        };
        {
            let mut ct = self.closed_tabs.write();
            let deque = ct.entry(pane).or_default();
            for t in closed.into_iter().rev() {
                deque.push_front(t);
                if deque.len() > 20 {
                    deque.pop_back();
                }
            }
        }
        self.project_workspace_to_slint();
        self.navigation.navigate_pane(pane, switch_to);
    }

    /// Close every tab in pane `pane` at an index strictly greater than
    /// `from`. No-op when `from` is the last tab. All removed tabs are
    /// pushed onto the closed-tab history.
    pub fn close_tabs_to_right_of(self: &Arc<Self>, pane: PaneId, from: usize) {
        let (switch_to, closed) = {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(pane) else { return };
            if from + 1 >= p.tabs.len() {
                return;
            }
            let closed: Vec<TabModel> = p.tabs.drain(from + 1..).collect();
            let navigated = if p.active_tab > from {
                p.active_tab = from;
                Some(p.active_location().to_path_buf())
            } else {
                None
            };
            (navigated, closed)
        };
        {
            let mut ct = self.closed_tabs.write();
            let deque = ct.entry(pane).or_default();
            for t in closed.into_iter().rev() {
                deque.push_front(t);
                if deque.len() > 20 {
                    deque.pop_back();
                }
            }
        }
        self.project_workspace_to_slint();
        if let Some(dest) = switch_to {
            self.navigation.navigate_pane(pane, dest);
        }
    }

    /// Pop the most-recently-closed tab off the pane's history stack and
    /// append it at the end of the tab list, making it active. No-op when
    /// the history is empty.
    pub fn reopen_closed_tab(self: &Arc<Self>, pane: PaneId) {
        let reopened = {
            let mut ct = self.closed_tabs.write();
            ct.get_mut(&pane).and_then(VecDeque::pop_front)
        };
        let Some(tab) = reopened else { return };
        let loc = tab.location.clone().unwrap_or_else(dirs_home);
        {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(pane) else { return };
            p.tabs.push(tab);
            p.active_tab = p.tabs.len() - 1;
        }
        self.project_workspace_to_slint();
        self.navigation.navigate_pane(pane, loc);
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
            window.on_select_tab(move |pane_id, tab| {
                if tab >= 0 {
                    let id = PaneId(pane_id as u32);
                    shell.select_tab(id, tab as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_cycle_tab(move |pane_id, delta| {
                let id = PaneId(pane_id as u32);
                shell.cycle_tab(id, delta as isize);
            });
        }

        // ── Multi-pane workspace commands ──
        //
        // split-right / split-down split the focused pane. Phase 4 removed the
        // 2-pane compat gate — the Slint UI now renders N panes.
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
            window.on_pane_focused(move |pane_id| {
                let id = PaneId(pane_id as u32);
                let slot = shell.pane_slint_index.read().get(&id).copied().unwrap_or(0);
                actions.lock().dispatch(UiAction::PaneFocusChanged(slot));
                shell.set_focused_pane_id(id);
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
            window.on_address_submitted(move |pane_id, path| {
                let id = PaneId(pane_id as u32);
                let slot = shell.pane_slint_index.read().get(&id).copied().unwrap_or(0);
                dispatch_navigation(&actions, slot, path.clone());
                let expanded = expand_tilde(Path::new(path.as_str()));
                nav.navigate_pane(id, expanded);
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            window.on_address_cancelled(move |_pane_id| {
                actions.lock().dispatch(UiAction::DismissPalette);
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            let shell = Arc::clone(self);
            window.on_breadcrumb_clicked(move |pane_id, segment| {
                let id = PaneId(pane_id as u32);
                let slot = shell.pane_slint_index.read().get(&id).copied().unwrap_or(0);
                let seg = segment as usize;
                actions.lock().dispatch(UiAction::BreadcrumbClicked {
                    pane: slot,
                    segment: seg,
                });
                shell.breadcrumb_clicked(id, seg);
            });
        }
        {
            let shell = self.clone();
            window.on_tab_selected(move |pane_id, tab| {
                let id = PaneId(pane_id as u32);
                shell.select_tab(id, tab as usize);
            });
        }
        {
            let shell = self.clone();
            window.on_tab_closed(move |pane_id, tab| {
                let id = PaneId(pane_id as u32);
                shell.close_tab(id, tab as usize);
            });
        }
        {
            let shell = self.clone();
            window.on_new_tab(move |pane_id| {
                let id = PaneId(pane_id as u32);
                shell.new_tab(id);
            });
        }

        // ── Details callbacks ─────────────────────────────────────────────────
        {
            let shell = self.clone();
            window.on_details_row_clicked(move |pane_id, index, ctrl, shift| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.details.select_index(index as usize, ctrl, shift);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_details_row_double_clicked(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.details.select_index(index as usize, false, false);
                    c.details.activate_focused();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_details_header_clicked(move |pane_id, column_index| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.details.header_clicked(column_index as usize);
                }
            });
        }

        // ── Grid callbacks ────────────────────────────────────────────────────
        {
            let shell = self.clone();
            window.on_grid_entry_clicked(move |pane_id, index, ctrl, shift| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.grid.select_index(index as usize, ctrl, shift);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_grid_entry_double_clicked(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.grid.select_index(index as usize, false, false);
                    c.grid.activate_focused();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_grid_thumbnail_visible(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.grid.thumbnail_visible(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_grid_columns_changed(move |pane_id, cols| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.grid.set_columns(cols as usize);
                }
            });
        }

        // ── Gallery callbacks ─────────────────────────────────────────────────
        {
            let shell = self.clone();
            window.on_gallery_entry_clicked(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.gallery.entry_clicked(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_gallery_strip_visible(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.gallery.strip_visible(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_gallery_preview_visible(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.gallery.preview_visible(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_gallery_prev_image(move |pane_id| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.gallery.prev_image();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_gallery_next_image(move |pane_id| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.gallery.next_image();
                }
            });
        }

        // ── Tree callbacks ────────────────────────────────────────────────────
        {
            let shell = self.clone();
            window.on_tree_row_clicked(move |pane_id, index, ctrl, shift| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.tree.select_index(index as usize, ctrl, shift);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_tree_row_double_clicked(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.tree.select_index(index as usize, false, false);
                    c.tree.activate_focused();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_tree_chevron_clicked(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    let visible = c.tree.build_visible_nodes();
                    if let Some(row) = visible.get(index as usize) {
                        let path = std::path::PathBuf::from(row.node_id.as_str());
                        c.tree.toggle(&path);
                    }
                }
            });
        }

        // ── Miller callbacks ──────────────────────────────────────────────────
        {
            let shell = self.clone();
            window.on_miller_row_clicked(move |pane_id, col, row| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.miller.select_row(col as usize, row as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_miller_row_double_clicked(move |pane_id, col, row| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
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

    /// Project the N-pane workspace layout onto Slint.
    ///
    /// Builds a [`PaneSlintData`] entry for every leaf pane in DFS order and
    /// pushes it via `set_panes`.  Also pushes split-handle descriptors, the
    /// focused-pane id, and the per-slot light data (`pane0-*` / `pane1-*`
    /// tabs/segments/active-tab) used by the per-slot conditional routing in
    /// the `for pane[i] in panes` loop until view controllers are migrated to
    /// full N-pane parallel arrays in Phase 4.1.
    pub fn project_workspace_to_slint(self: &Arc<Self>) {
        /// Cheap-to-clone snapshot of one pane's light data.
        struct PaneData {
            id: i32,
            x: f32,
            y: f32,
            width: f32,
            height: f32,
            path: String,
            view_mode: String,
            tabs: Vec<TabModel>,
            active_tab: i32,
            segments: Vec<String>,
        }

        let (focused_id, focus_idx, pane_data, handle_data) = {
            let ws = self.workspace.read();
            let bounds = self.workspace_content_bounds();
            let rects = ws.layout.layout_rects(bounds);

            // Rebuild the DFS-position map so callbacks continue routing correctly.
            {
                let mut idx_map = self.pane_slint_index.write();
                idx_map.clear();
                for (i, (id, _)) in rects.iter().enumerate() {
                    idx_map.insert(*id, i);
                }
            }

            let focused = ws.focused;
            let focus_idx = rects.iter().position(|(id, _)| *id == focused).unwrap_or(0) as i32;

            let pane_data: Vec<PaneData> = rects
                .iter()
                .map(|(id, rect)| {
                    let pane = ws.pane(*id).expect("leaf in layout must have pane state");
                    let location = pane.active_location();
                    PaneData {
                        id: id.0 as i32,
                        x: rect.x,
                        y: rect.y,
                        width: rect.width,
                        height: rect.height,
                        path: location.to_string_lossy().into_owned(),
                        view_mode: pane.view_mode.to_string(),
                        tabs: pane.tabs.clone(),
                        active_tab: pane.active_tab as i32,
                        segments: path_segments_for(location),
                    }
                })
                .collect();

            let handle_data = collect_split_handles(&ws.layout, bounds);

            (focused.0 as i32, focus_idx, pane_data, handle_data)
        };

        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };

            // Build and push the PaneSlintData array.
            // `x`, `y`, `width`, `height` are `sp::Coord` = `f32` in the generated struct.
            let slint_panes: Vec<PaneSlintData> = pane_data
                .iter()
                .map(|p| PaneSlintData {
                    id: p.id,
                    x: p.x,
                    y: p.y,
                    width: p.width,
                    height: p.height,
                    path: SharedString::from(p.path.as_str()),
                    view_mode: SharedString::from(p.view_mode.as_str()),
                    active_tab: p.active_tab,
                })
                .collect();
            window.set_panes(ModelRc::new(VecModel::from(slint_panes)));
            window.set_focused_pane_id(focused_id);
            window.set_focus_index(focus_idx);

            // Per-slot light data: tabs and path-segments for slots 0 and 1.
            // Phase 4.1: generalise this once controllers emit to N-pane arrays.
            if let Some(p) = pane_data.first() {
                window.set_pane0_tabs(to_tab_model(&p.tabs));
                window.set_pane0_active_tab(p.active_tab);
                window.set_pane0_segments(to_segments_model(&p.segments));
            }
            if let Some(p) = pane_data.get(1) {
                window.set_pane1_tabs(to_tab_model(&p.tabs));
                window.set_pane1_active_tab(p.active_tab);
                window.set_pane1_segments(to_segments_model(&p.segments));
            } else {
                window.set_pane1_tabs(ModelRc::new(VecModel::from(Vec::<TabEntry>::new())));
                window.set_pane1_active_tab(0);
                window.set_pane1_segments(ModelRc::new(VecModel::from(Vec::<SharedString>::new())));
            }

            // Split-handle descriptors.
            let slint_handles: Vec<SplitHandle> = handle_data
                .iter()
                .map(|h| SplitHandle {
                    node_index: h.node_index,
                    x: h.x,
                    y: h.y,
                    width: h.width,
                    height: h.height,
                    horizontal: h.horizontal,
                })
                .collect();
            window.set_split_handles(ModelRc::new(VecModel::from(slint_handles)));
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

// ── Phase-5 tab-operation tests ───────────────────────────────────────────────
//
// These tests exercise the algorithms that `AppShell`'s new tab methods
// delegate to.  They operate directly on `WorkspaceModel` / `PaneState`
// because `AppShell::new` requires a live Slint event loop.
#[cfg(test)]
mod tab_ops_tests {
    use std::collections::VecDeque;

    use crate::models::{
        pane_state::PaneState, split::PaneId, tab::TabModel, ViewMode, WorkspaceModel,
    };

    /// Build a workspace with `n` tabs in pane `id`.  Tab titles are
    /// `"tab-0"`, `"tab-1"`, … so order can be verified by title.
    fn workspace_with_n_tabs(id: PaneId, n: usize) -> WorkspaceModel {
        assert!(n >= 1);
        let mut ws = WorkspaceModel::new(PaneState::new(
            id,
            TabModel::at("/root/tab-0"),
            ViewMode::Details,
        ));
        {
            let p = ws.pane_mut(id).unwrap();
            for i in 1..n {
                p.add_tab(TabModel::at(format!("/root/tab-{i}")));
            }
            p.set_active(0);
        }
        ws
    }

    // ── reorder_tab ───────────────────────────────────────────────────────

    #[test]
    fn reorder_from_0_to_2_in_4_tab_pane() {
        let id = PaneId(1);
        let mut ws = workspace_with_n_tabs(id, 4);
        // Make tab-0 active to verify it follows the move.
        ws.pane_mut(id).unwrap().set_active(0);

        // Simulate AppShell::reorder_tab(pane, from=0, to=2).
        let p = ws.pane_mut(id).unwrap();
        let from = 0usize;
        let to = 2usize;
        let tab = p.tabs.remove(from);
        p.tabs.insert(to, tab);
        if p.active_tab == from {
            p.active_tab = to;
        } else if from < to && p.active_tab > from && p.active_tab <= to {
            p.active_tab -= 1;
        }

        assert_eq!(p.tabs[0].title, "tab-1");
        assert_eq!(p.tabs[1].title, "tab-2");
        assert_eq!(p.tabs[2].title, "tab-0"); // moved tab
        assert_eq!(p.tabs[3].title, "tab-3");
        assert_eq!(p.active_tab, 2); // follows the moved tab
    }

    #[test]
    fn reorder_non_active_tab_preserves_selection() {
        let id = PaneId(1);
        let mut ws = workspace_with_n_tabs(id, 4);
        ws.pane_mut(id).unwrap().set_active(3); // active is tab-3

        // Move tab-1 → tab-3 (active tab shifts left).
        let p = ws.pane_mut(id).unwrap();
        let from = 1usize;
        let to = 3usize;
        let tab = p.tabs.remove(from);
        p.tabs.insert(to, tab);
        // from < to and active (3) > from (1) and <= to (3) → shift left.
        if p.active_tab == from {
            p.active_tab = to;
        } else if from < to && p.active_tab > from && p.active_tab <= to {
            p.active_tab -= 1;
        }

        assert_eq!(p.tabs[3].title, "tab-1");
        assert_eq!(p.active_tab, 2); // was 3, shifted left by 1
        assert_eq!(p.tabs[p.active_tab].title, "tab-3");
    }

    // ── duplicate_tab ─────────────────────────────────────────────────────

    #[test]
    fn duplicate_inserts_copy_after_and_activates() {
        let id = PaneId(1);
        let mut ws = workspace_with_n_tabs(id, 3);

        // Simulate AppShell::duplicate_tab(pane, tab=1).
        let src = ws.pane(id).unwrap().tabs[1].clone();
        let insert_at = 2;
        let mut dup = TabModel::at(src.location.clone().unwrap());
        dup.sort = src.sort.clone();
        dup.filter = src.filter.clone();
        {
            let p = ws.pane_mut(id).unwrap();
            p.tabs.insert(insert_at, dup);
            p.active_tab = insert_at;
        }

        let p = ws.pane(id).unwrap();
        assert_eq!(p.tabs.len(), 4);
        assert_eq!(p.active_tab, 2);
        assert_eq!(p.tabs[1].title, "tab-1");
        assert_eq!(p.tabs[2].title, "tab-1"); // copy has same path → same title
        assert_eq!(p.tabs[3].title, "tab-2");
    }

    // ── close_other_tabs ──────────────────────────────────────────────────

    #[test]
    fn close_other_tabs_leaves_only_kept() {
        let id = PaneId(1);
        let mut ws = workspace_with_n_tabs(id, 5);

        // Simulate AppShell::close_other_tabs(pane, keep=2).
        let keep = 2usize;
        let p = ws.pane_mut(id).unwrap();
        let kept = p.tabs[keep].clone();
        let all: Vec<TabModel> = std::mem::replace(&mut p.tabs, vec![kept]);
        let _closed: Vec<TabModel> = all
            .into_iter()
            .enumerate()
            .filter_map(|(i, t)| (i != keep).then_some(t))
            .collect();
        p.active_tab = 0;

        let p = ws.pane(id).unwrap();
        assert_eq!(p.tabs.len(), 1);
        assert_eq!(p.active_tab, 0);
        assert_eq!(p.tabs[0].title, "tab-2");
    }

    // ── close_tabs_to_right_of ────────────────────────────────────────────

    #[test]
    fn close_tabs_to_right_leaves_correct_count() {
        let id = PaneId(1);
        let mut ws = workspace_with_n_tabs(id, 5);

        // Simulate AppShell::close_tabs_to_right_of(pane, from=1).
        let from = 1usize;
        let p = ws.pane_mut(id).unwrap();
        let _closed: Vec<TabModel> = p.tabs.drain(from + 1..).collect();
        if p.active_tab > from {
            p.active_tab = from;
        }

        let p = ws.pane(id).unwrap();
        assert_eq!(p.tabs.len(), 2);
        assert_eq!(p.tabs[0].title, "tab-0");
        assert_eq!(p.tabs[1].title, "tab-1");
    }

    // ── closed-tab history ────────────────────────────────────────────────

    #[test]
    fn close_tab_pushes_to_closed_deque_and_reopen_pops() {
        let id = PaneId(1);
        let mut ws = workspace_with_n_tabs(id, 3);
        let mut closed: AHashMap<PaneId, VecDeque<TabModel>> = ahash::AHashMap::default();

        // Simulate closing tab 0.
        let p = ws.pane_mut(id).unwrap();
        if let Some(removed) = p.close_tab(0) {
            let deque = closed.entry(id).or_default();
            deque.push_front(removed);
        }
        assert_eq!(closed[&id].front().unwrap().title, "tab-0");
        assert_eq!(ws.pane(id).unwrap().tabs.len(), 2);

        // Simulate reopen_closed_tab.
        let reopened = closed.get_mut(&id).and_then(VecDeque::pop_front);
        assert!(reopened.is_some());
        assert_eq!(reopened.unwrap().title, "tab-0");
        assert!(closed[&id].is_empty());
    }

    #[test]
    fn closed_tab_deque_caps_at_twenty() {
        let id = PaneId(1);
        let mut closed: AHashMap<PaneId, VecDeque<TabModel>> = ahash::AHashMap::default();

        for i in 0..25usize {
            let deque = closed.entry(id).or_default();
            deque.push_front(TabModel::at(format!("/root/tab-{i}")));
            if deque.len() > 20 {
                deque.pop_back();
            }
        }

        let deque = &closed[&id];
        assert_eq!(deque.len(), 20);
        // Most recently pushed (tab-24) is at the front.
        assert_eq!(deque.front().unwrap().title, "tab-24");
        // Oldest surviving entry (tab-5) is at the back; 0–4 were evicted.
        assert_eq!(deque.back().unwrap().title, "tab-5");
    }

    // Need AHashMap in scope for the tests above.
    use ahash::AHashMap;
}
