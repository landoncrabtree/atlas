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

use atlas_core::path::expand_tilde;
use atlas_fs::LocationViewModel;
use atlas_keymap::{defaults::default_actions, ActionRegistry, Keymap};
use directories::UserDirs;
use parking_lot::{Mutex, RwLock};
use slint::{ComponentHandle as _, ModelRc, SharedString, VecModel};

use crate::{
    actions::{ActionSink, UiAction},
    models::{PaletteModel, PaletteResult, PaneModel, StatusModel, WorkspaceModel},
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
pub struct PaneControllers {
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
    pane: usize,
    window: &AtlasWindow,
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
    thumb_cache: Arc<atlas_thumbs::SqliteCache>,
) -> PaneControllers {
    let details = DetailsController::new(pane, window.as_weak(), Arc::clone(&actions));
    let grid = GridController::new(
        pane,
        window.as_weak(),
        Arc::clone(&actions),
        Arc::clone(&thumb_cache),
    );
    let gallery = GalleryController::new(
        pane,
        window.as_weak(),
        Arc::clone(&actions),
        Arc::clone(&thumb_cache),
    );
    let tree = TreeController::new(pane, Arc::clone(&actions));
    tree.attach_window(window.as_weak());
    let miller = MillerController::new(actions);
    miller.attach_window(window.as_weak());

    PaneControllers {
        details,
        grid,
        miller,
        tree,
        gallery,
    }
}

/// Owns Rust-side model state and bridges it to the Slint window.
///
/// Construct with [`AppShell::new`], then call [`AppShell::set_workspace`],
/// [`AppShell::set_status`], and [`AppShell::set_theme`] to push initial state.
/// The real atlas-keymap and atlas-fs wiring happens in a follow-up todo;
/// for now `atlas-app` installs a `LoggingActionSink` stub.
pub struct AppShell {
    window: slint::Weak<AtlasWindow>,
    workspace: RwLock<WorkspaceModel>,
    palette: RwLock<PaletteModel>,
    status: RwLock<StatusModel>,
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
    navigation: Arc<NavigationController>,
    palette_ctrl: Arc<PaletteController>,
    panes_ctrl: Box<[PaneControllers; 2]>,
    search: Arc<SearchController>,
    /// File-operations queue controller.
    ops: Arc<OpsController>,
    /// Bulk rename modal controller.
    bulk_rename: Arc<BulkRenameController>,
    /// Current location view model for pane 0 (updated on navigation).
    ///
    /// Stored so that [`AppShell::selected_paths`] and
    /// [`AppShell::focused_entry`] can read entry paths on the UI thread
    /// without reaching into the view controllers.
    pane0_vm: RwLock<Option<Arc<dyn LocationViewModel>>>,
    /// Current location view model for pane 1.
    pane1_vm: RwLock<Option<Arc<dyn LocationViewModel>>>,
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
        let panes_ctrl = Box::new([
            build_pane_controllers(0, window, Arc::clone(&actions), Arc::clone(&thumb_cache)),
            build_pane_controllers(1, window, Arc::clone(&actions), thumb_cache),
        ]);
        let palette_ctrl = build_palette_controller(window, Arc::clone(&actions));
        search.set_action_sink(Arc::clone(&actions));
        let ops = OpsController::new();
        ops.attach_window(window.as_weak());
        let bulk_rename = BulkRenameController::new(Arc::clone(&ops), Arc::clone(&actions));
        bulk_rename.attach_window(window.as_weak());
        let shell = Arc::new(Self {
            window: window.as_weak(),
            workspace: RwLock::new(WorkspaceModel::new_default()),
            palette: RwLock::new(PaletteModel::default()),
            status: RwLock::new(StatusModel::default()),
            actions,
            navigation: nav,
            palette_ctrl,
            panes_ctrl,
            search,
            ops,
            bulk_rename,
            pane0_vm: RwLock::new(None),
            pane1_vm: RwLock::new(None),
        });

        shell.wire_callbacks(window);
        shell.register_nav_callbacks();
        shell
    }

    /// Return the pane-0 details controller.
    #[must_use]
    pub fn details_controller(&self) -> Arc<DetailsController> {
        Arc::clone(&self.panes_ctrl[0].details)
    }

    /// Return the pane-0 grid controller.
    #[must_use]
    pub fn grid_controller(&self) -> Arc<GridController> {
        Arc::clone(&self.panes_ctrl[0].grid)
    }

    /// Return the pane-0 gallery controller.
    #[must_use]
    pub fn gallery_controller(&self) -> Arc<GalleryController> {
        Arc::clone(&self.panes_ctrl[0].gallery)
    }

    /// Return the pane-0 tree controller.
    #[must_use]
    pub fn tree_controller(&self) -> Arc<TreeController> {
        Arc::clone(&self.panes_ctrl[0].tree)
    }

    /// Return the pane-0 miller columns controller.
    #[must_use]
    pub fn miller_controller(&self) -> Arc<MillerController> {
        Arc::clone(&self.panes_ctrl[0].miller)
    }

    /// Return the per-pane view controllers for `index`.
    #[must_use]
    pub fn pane(&self, index: usize) -> &PaneControllers {
        &self.panes_ctrl[index.min(1)]
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

    /// Return the index of the pane that currently has keyboard focus.
    #[must_use]
    pub fn focused_pane(&self) -> usize {
        self.workspace.read().focused_pane
    }

    /// Return whether dual-pane mode is active.
    #[must_use]
    pub fn is_dual_pane(&self) -> bool {
        self.workspace.read().dual_pane
    }

    /// Set focused pane, updating the workspace model and pushing to Slint.
    pub fn set_focused_pane(self: &Arc<Self>, index: usize) {
        {
            let mut ws = self.workspace.write();
            if ws.focused_pane == index {
                return;
            }
            ws.focused_pane = index;
            for (i, pane) in ws.panes.iter_mut().enumerate() {
                pane.focused = i == index;
            }
        }
        let snapshot = self.workspace.read().clone();
        self.set_workspace(snapshot);
    }

    /// Enable or disable dual-pane mode.
    ///
    /// When enabling and pane 1 has no loaded location, navigates pane 1 to
    /// pane 0's current path (falling back to `$HOME`).
    pub fn set_dual_pane(self: &Arc<Self>, on: bool) {
        let needs_navigate = if on {
            let mut ws = self.workspace.write();
            ws.dual_pane = true;
            if ws.panes.len() < 2 {
                let home = dirs_home();
                ws.panes.push(PaneModel::new(home));
            }
            self.navigation.location(1).is_none()
        } else {
            let mut ws = self.workspace.write();
            ws.dual_pane = false;
            ws.focused_pane = 0;
            for (i, pane) in ws.panes.iter_mut().enumerate() {
                pane.focused = i == 0;
            }
            false
        };
        if needs_navigate {
            let start = self.pane_location(0).unwrap_or_else(dirs_home);
            self.navigation.navigate(1, start);
        }
        let snapshot = self.workspace.read().clone();
        self.set_workspace(snapshot);
    }

    /// Return the current directory path for `pane`, if available.
    #[must_use]
    pub fn pane_location(&self, pane: usize) -> Option<PathBuf> {
        self.workspace
            .read()
            .panes
            .get(pane)
            .map(|p| p.location.clone())
    }

    /// Set the view mode for `pane` and push the change to the UI.
    ///
    /// If `pane` is out of range this is a no-op (with a debug log).
    pub fn set_view_mode(self: &Arc<Self>, pane: usize, mode: crate::models::ViewMode) {
        {
            let mut ws = self.workspace.write();
            let Some(p) = ws.panes.get_mut(pane) else {
                tracing::debug!(pane, "set_view_mode: pane out of range");
                return;
            };
            if p.view_mode == mode {
                return;
            }
            p.view_mode = mode;
        }
        let snapshot = self.workspace.read().clone();
        self.set_workspace(snapshot);
    }

    /// Append a new tab to `pane` pointing at the pane's current location.
    /// The new tab becomes active. No-op if `pane` is out of range.
    pub fn new_tab(self: &Arc<Self>, pane: usize) {
        {
            let mut ws = self.workspace.write();
            let Some(p) = ws.panes.get_mut(pane) else {
                tracing::debug!(pane, "new_tab: pane out of range");
                return;
            };
            let title = p
                .location
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.location.to_string_lossy().into_owned());
            p.tabs.push(crate::models::TabModel::new(title));
            p.active_tab = p.tabs.len() - 1;
        }
        let snapshot = self.workspace.read().clone();
        self.set_workspace(snapshot);
    }

    /// Remove tab `tab` from `pane`. Refuses to close the last tab
    /// (the pane must always have at least one). Adjusts active_tab
    /// so that a still-valid tab remains selected.
    pub fn close_tab(self: &Arc<Self>, pane: usize, tab: usize) {
        {
            let mut ws = self.workspace.write();
            let Some(p) = ws.panes.get_mut(pane) else {
                tracing::debug!(pane, tab, "close_tab: pane out of range");
                return;
            };
            if p.tabs.len() <= 1 || tab >= p.tabs.len() {
                return;
            }
            p.tabs.remove(tab);
            if p.active_tab >= p.tabs.len() {
                p.active_tab = p.tabs.len() - 1;
            } else if tab < p.active_tab {
                p.active_tab -= 1;
            }
        }
        let snapshot = self.workspace.read().clone();
        self.set_workspace(snapshot);
    }

    /// Return the filesystem paths of all selected entries in `pane`.
    ///
    /// Reads the selection mask from the Slint window and the entry list from
    /// the stored location view model. **Must be called on the Slint
    /// event-loop thread.**
    ///
    /// # Caveats
    ///
    /// For pane 0, only the Details view selection is read. Grid/Miller/Tree
    /// selection reading is a TODO once those views expose a unified
    /// selection API.
    #[must_use]
    pub fn selected_paths(&self, pane: usize) -> Vec<PathBuf> {
        let Some(window) = self.window.upgrade() else {
            return Vec::new();
        };

        let mask_model = if pane == 0 {
            window.get_pane0_details_selected_mask()
        } else {
            window.get_pane1_details_selected_mask()
        };

        let vm_guard = if pane == 0 {
            self.pane0_vm.read()
        } else {
            self.pane1_vm.read()
        };
        let Some(ref vm) = *vm_guard else {
            return Vec::new();
        };
        let entries = vm.entries();

        use slint::Model as _;
        (0..mask_model.row_count())
            .filter(|&i| mask_model.row_data(i).unwrap_or(false))
            .filter_map(|i| entries.get(i).map(|e| e.path.clone()))
            .collect()
    }

    /// Return the path of the focused (cursor) entry in `pane`, if any.
    ///
    /// **Must be called on the Slint event-loop thread.**
    ///
    /// # Caveats
    ///
    /// Currently reads from the Details view focused index only. Grid/Miller/Tree
    /// are a TODO.
    #[must_use]
    pub fn focused_entry(&self, pane: usize) -> Option<PathBuf> {
        let window = self.window.upgrade()?;

        let focused_idx = if pane == 0 {
            window.get_pane0_details_focused_index()
        } else {
            window.get_pane1_details_focused_index()
        };

        if focused_idx < 0 {
            return None;
        }

        let vm_guard = if pane == 0 {
            self.pane0_vm.read()
        } else {
            self.pane1_vm.read()
        };
        let vm = vm_guard.as_ref()?;
        vm.entries()
            .get(focused_idx as usize)
            .map(|e| e.path.clone())
    }

    fn register_nav_callbacks(self: &Arc<Self>) {
        let shell_weak = Arc::downgrade(self);
        self.navigation.on_location_changed(move |pane, vm| {
            let Some(shell) = shell_weak.upgrade() else {
                return;
            };
            let path = vm.location().to_path_buf();
            // Coerce to trait object so selected_paths / focused_entry can use it.
            let vm_dyn: Arc<dyn LocationViewModel> = vm.clone();
            if pane == 0 {
                *shell.pane0_vm.write() = Some(vm_dyn);
                let vm_typed: Arc<dyn LocationViewModel> = vm;
                shell.panes_ctrl[0]
                    .details
                    .set_location(Arc::clone(&vm_typed));
                shell.panes_ctrl[0].grid.set_location(Arc::clone(&vm_typed));
                shell.panes_ctrl[0].gallery.set_location(vm_typed);
                shell.panes_ctrl[0].tree.set_root(path.clone());
                shell.panes_ctrl[0].miller.set_root(path.clone());
                shell.search.set_scope(Some(path.clone()));
            } else if pane == 1 {
                *shell.pane1_vm.write() = Some(vm_dyn);
                let vm_typed: Arc<dyn LocationViewModel> = vm;
                shell.panes_ctrl[1]
                    .details
                    .set_location(Arc::clone(&vm_typed));
                shell.panes_ctrl[1].grid.set_location(Arc::clone(&vm_typed));
                shell.panes_ctrl[1].gallery.set_location(vm_typed);
                shell.panes_ctrl[1].tree.set_root(path.clone());
                shell.panes_ctrl[1].miller.set_root(path.clone());
            }
            let new_pane = PaneModel::new(path);
            {
                let mut workspace = shell.workspace.write();
                if pane < workspace.panes.len() {
                    workspace.panes[pane] = new_pane;
                } else if pane == workspace.panes.len() {
                    workspace.panes.push(new_pane);
                }
            }
            let snapshot = shell.workspace.read().clone();
            shell.set_workspace(snapshot);
        });
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

        // ── Focused-pane navigation callbacks ────────────────────────────
        // The FocusScope in atlas.slint dispatches these when no modal or
        // text input has focus (arrow keys / Enter / Backspace / vim hjkl).
        {
            let shell = self.clone();
            window.on_pane_move_focus(move |delta| {
                let pane = shell.focused_pane();
                shell.panes_ctrl[pane].details.move_focus(delta as i64);
            });
        }
        {
            let shell = self.clone();
            window.on_pane_activate_focused(move || {
                let pane = shell.focused_pane();
                shell.panes_ctrl[pane].details.activate_focused();
            });
        }
        {
            let nav = Arc::clone(&self.navigation);
            let shell = self.clone();
            window.on_pane_go_up(move || {
                nav.go_up(shell.focused_pane());
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
                actions.lock().dispatch(UiAction::PaneFocusChanged(0));
                shell.set_focused_pane(0);
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            let shell = Arc::clone(self);
            window.on_pane1_focused(move || {
                actions.lock().dispatch(UiAction::PaneFocusChanged(1));
                shell.set_focused_pane(1);
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            let shell = Arc::clone(self);
            window.on_cycle_pane_focus(move || {
                let pane_count = if shell.workspace.read().dual_pane {
                    2
                } else {
                    1
                };
                let next = (shell.workspace.read().focused_pane + 1) % pane_count;
                actions.lock().dispatch(UiAction::PaneFocusChanged(next));
                shell.set_focused_pane(next);
            });
        }

        {
            let actions = Arc::clone(&self.actions);
            let nav = Arc::clone(&self.navigation);
            window.on_pane0_address_submitted(move |path| {
                dispatch_navigation(&actions, 0, path.clone());
                let expanded = expand_tilde(Path::new(path.as_str()));
                nav.navigate(0, expanded);
            });
        }
        window.on_pane0_address_cancelled(dispatch!(self.actions, UiAction::DismissPalette));
        {
            let actions = Arc::clone(&self.actions);
            let nav = Arc::clone(&self.navigation);
            window.on_pane0_breadcrumb_clicked(move |segment| {
                let seg = segment as usize;
                actions.lock().dispatch(UiAction::BreadcrumbClicked {
                    pane: 0,
                    segment: seg,
                });
                nav.breadcrumb_clicked(0, seg);
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            window.on_pane0_tab_selected(move |tab| {
                actions.lock().dispatch(UiAction::TabSelected {
                    pane: 0,
                    tab: tab as usize,
                });
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_tab_closed(move |tab| {
                shell.close_tab(0, tab as usize);
            });
        }
        {
            let shell = self.clone();
            window.on_pane0_new_tab(move || {
                shell.new_tab(0);
            });
        }

        {
            let details = Arc::clone(&self.panes_ctrl[0].details);
            window.on_pane0_details_row_clicked(move |index, ctrl, shift| {
                details.select_index(index as usize, ctrl, shift);
            });
        }
        {
            let details = Arc::clone(&self.panes_ctrl[0].details);
            window.on_pane0_details_row_double_clicked(move |index| {
                details.select_index(index as usize, false, false);
                details.activate_focused();
            });
        }
        {
            let details = Arc::clone(&self.panes_ctrl[0].details);
            window.on_pane0_details_header_clicked(move |column_index| {
                details.header_clicked(column_index as usize);
            });
        }

        // Grid callbacks — pane 0
        {
            let grid = Arc::clone(&self.panes_ctrl[0].grid);
            window.on_pane0_grid_entry_clicked(move |index, ctrl, shift| {
                grid.select_index(index as usize, ctrl, shift);
            });
        }
        {
            let grid = Arc::clone(&self.panes_ctrl[0].grid);
            window.on_pane0_grid_entry_double_clicked(move |index| {
                grid.select_index(index as usize, false, false);
                grid.activate_focused();
            });
        }
        {
            let grid = Arc::clone(&self.panes_ctrl[0].grid);
            window.on_pane0_grid_thumbnail_visible(move |index| {
                grid.thumbnail_visible(index as usize);
            });
        }
        {
            let grid = Arc::clone(&self.panes_ctrl[0].grid);
            window.on_pane0_grid_columns_changed(move |cols| {
                grid.set_columns(cols as usize);
            });
        }

        // Gallery callbacks — pane 0
        {
            let gallery = Arc::clone(&self.panes_ctrl[0].gallery);
            window.on_pane0_gallery_entry_clicked(move |index| {
                gallery.entry_clicked(index as usize);
            });
        }
        {
            let gallery = Arc::clone(&self.panes_ctrl[0].gallery);
            window.on_pane0_gallery_strip_visible(move |index| {
                gallery.strip_visible(index as usize);
            });
        }
        {
            let gallery = Arc::clone(&self.panes_ctrl[0].gallery);
            window.on_pane0_gallery_preview_visible(move |index| {
                gallery.preview_visible(index as usize);
            });
        }
        {
            let gallery = Arc::clone(&self.panes_ctrl[0].gallery);
            window.on_pane0_gallery_prev_image(move || {
                gallery.prev_image();
            });
        }
        {
            let gallery = Arc::clone(&self.panes_ctrl[0].gallery);
            window.on_pane0_gallery_next_image(move || {
                gallery.next_image();
            });
        }

        // Tree callbacks — pane 0
        {
            let tree = Arc::clone(&self.panes_ctrl[0].tree);
            window.on_pane0_tree_row_clicked(move |index, ctrl, shift| {
                tree.select_index(index as usize, ctrl, shift);
            });
        }
        {
            let tree = Arc::clone(&self.panes_ctrl[0].tree);
            window.on_pane0_tree_row_double_clicked(move |index| {
                tree.select_index(index as usize, false, false);
                tree.activate_focused();
            });
        }
        {
            let tree = Arc::clone(&self.panes_ctrl[0].tree);
            window.on_pane0_tree_chevron_clicked(move |index| {
                let visible = tree.build_visible_nodes();
                if let Some(row) = visible.get(index as usize) {
                    let path = std::path::PathBuf::from(row.node_id.as_str());
                    tree.toggle(&path);
                }
            });
        }

        // Miller callbacks — pane 0
        {
            let miller = Arc::clone(&self.panes_ctrl[0].miller);
            window.on_pane0_miller_row_clicked(move |col, row| {
                miller.select_row(col as usize, row as usize);
            });
        }
        {
            let miller = Arc::clone(&self.panes_ctrl[0].miller);
            window.on_pane0_miller_row_double_clicked(move |col, row| {
                miller.select_row(col as usize, row as usize);
                miller.activate_focused();
            });
        }

        {
            let actions = Arc::clone(&self.actions);
            let nav = Arc::clone(&self.navigation);
            window.on_pane1_address_submitted(move |path| {
                dispatch_navigation(&actions, 1, path.clone());
                let expanded = expand_tilde(Path::new(path.as_str()));
                nav.navigate(1, expanded);
            });
        }
        window.on_pane1_address_cancelled(dispatch!(self.actions, UiAction::DismissPalette));
        {
            let actions = Arc::clone(&self.actions);
            let nav = Arc::clone(&self.navigation);
            window.on_pane1_breadcrumb_clicked(move |segment| {
                let seg = segment as usize;
                actions.lock().dispatch(UiAction::BreadcrumbClicked {
                    pane: 1,
                    segment: seg,
                });
                nav.breadcrumb_clicked(1, seg);
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            window.on_pane1_tab_selected(move |tab| {
                actions.lock().dispatch(UiAction::TabSelected {
                    pane: 1,
                    tab: tab as usize,
                });
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_tab_closed(move |tab| {
                shell.close_tab(1, tab as usize);
            });
        }
        {
            let shell = self.clone();
            window.on_pane1_new_tab(move || {
                shell.new_tab(1);
            });
        }

        // Details callbacks — pane 1
        {
            let details = Arc::clone(&self.panes_ctrl[1].details);
            window.on_pane1_details_row_clicked(move |index, ctrl, shift| {
                details.select_index(index as usize, ctrl, shift);
            });
        }
        {
            let details = Arc::clone(&self.panes_ctrl[1].details);
            window.on_pane1_details_row_double_clicked(move |index| {
                details.select_index(index as usize, false, false);
                details.activate_focused();
            });
        }
        {
            let details = Arc::clone(&self.panes_ctrl[1].details);
            window.on_pane1_details_header_clicked(move |column_index| {
                details.header_clicked(column_index as usize);
            });
        }

        // Grid callbacks — pane 1
        {
            let grid = Arc::clone(&self.panes_ctrl[1].grid);
            window.on_pane1_grid_entry_clicked(move |index, ctrl, shift| {
                grid.select_index(index as usize, ctrl, shift);
            });
        }
        {
            let grid = Arc::clone(&self.panes_ctrl[1].grid);
            window.on_pane1_grid_entry_double_clicked(move |index| {
                grid.select_index(index as usize, false, false);
                grid.activate_focused();
            });
        }
        {
            let grid = Arc::clone(&self.panes_ctrl[1].grid);
            window.on_pane1_grid_thumbnail_visible(move |index| {
                grid.thumbnail_visible(index as usize);
            });
        }
        {
            let grid = Arc::clone(&self.panes_ctrl[1].grid);
            window.on_pane1_grid_columns_changed(move |cols| {
                grid.set_columns(cols as usize);
            });
        }

        // Gallery callbacks — pane 1
        {
            let gallery = Arc::clone(&self.panes_ctrl[1].gallery);
            window.on_pane1_gallery_entry_clicked(move |index| {
                gallery.entry_clicked(index as usize);
            });
        }
        {
            let gallery = Arc::clone(&self.panes_ctrl[1].gallery);
            window.on_pane1_gallery_strip_visible(move |index| {
                gallery.strip_visible(index as usize);
            });
        }
        {
            let gallery = Arc::clone(&self.panes_ctrl[1].gallery);
            window.on_pane1_gallery_preview_visible(move |index| {
                gallery.preview_visible(index as usize);
            });
        }
        {
            let gallery = Arc::clone(&self.panes_ctrl[1].gallery);
            window.on_pane1_gallery_prev_image(move || {
                gallery.prev_image();
            });
        }
        {
            let gallery = Arc::clone(&self.panes_ctrl[1].gallery);
            window.on_pane1_gallery_next_image(move || {
                gallery.next_image();
            });
        }

        // Tree callbacks — pane 1
        {
            let tree = Arc::clone(&self.panes_ctrl[1].tree);
            window.on_pane1_tree_row_clicked(move |index, ctrl, shift| {
                tree.select_index(index as usize, ctrl, shift);
            });
        }
        {
            let tree = Arc::clone(&self.panes_ctrl[1].tree);
            window.on_pane1_tree_row_double_clicked(move |index| {
                tree.select_index(index as usize, false, false);
                tree.activate_focused();
            });
        }
        {
            let tree = Arc::clone(&self.panes_ctrl[1].tree);
            window.on_pane1_tree_chevron_clicked(move |index| {
                let visible = tree.build_visible_nodes();
                if let Some(row) = visible.get(index as usize) {
                    let path = std::path::PathBuf::from(row.node_id.as_str());
                    tree.toggle(&path);
                }
            });
        }

        // Miller callbacks — pane 1
        {
            let miller = Arc::clone(&self.panes_ctrl[1].miller);
            window.on_pane1_miller_row_clicked(move |col, row| {
                miller.select_row(col as usize, row as usize);
            });
        }
        {
            let miller = Arc::clone(&self.panes_ctrl[1].miller);
            window.on_pane1_miller_row_double_clicked(move |col, row| {
                miller.select_row(col as usize, row as usize);
                miller.activate_focused();
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            let shell = Arc::clone(self);
            window.on_toggle_dual_pane(move || {
                let on = !shell.is_dual_pane();
                actions.lock().dispatch(UiAction::SetDualPane(on));
                shell.set_dual_pane(on);
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
                let focused = shell.focused_pane();
                let sources = shell.selected_paths(focused);
                if sources.is_empty() {
                    tracing::warn!(pane = focused, "fs::Copy (F5): no selection");
                    return;
                }
                // In dual-pane mode the other pane is the destination.
                // Single-pane: destination dialog is a post-MVP follow-up.
                let dual = shell.workspace.read().dual_pane;
                let other = if dual { Some(1 - focused) } else { None };
                let dest = other.and_then(|p| shell.pane_location(p));
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
                let focused = shell.focused_pane();
                let sources = shell.selected_paths(focused);
                if sources.is_empty() {
                    tracing::warn!(pane = focused, "fs::Move (F6): no selection");
                    return;
                }
                let dual = shell.workspace.read().dual_pane;
                let other = if dual { Some(1 - focused) } else { None };
                let dest = other.and_then(|p| shell.pane_location(p));
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
                let focused = shell.focused_pane();
                let paths = shell.selected_paths(focused);
                if paths.is_empty() {
                    tracing::warn!(pane = focused, "fs::Delete (F8): no selection");
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
                let focused = shell.focused_pane();
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
                        tracing::warn!(pane = focused, "fs::Rename (F2): no focused entry");
                    }
                }
            });
        }
        {
            let shell = Arc::clone(self);
            window.on_fs_mkdir(move || {
                let focused = shell.focused_pane();
                let Some(location) = shell.pane_location(focused) else {
                    tracing::warn!(pane = focused, "fs::Mkdir (F7): no pane location");
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
                let focused = shell.focused_pane();
                let paths = shell.selected_paths(focused);
                tracing::info!(
                    pane = focused,
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

    /// Update the workspace state and schedule a property push on the event loop.
    pub fn set_workspace(self: &Arc<Self>, model: WorkspaceModel) {
        *self.workspace.write() = model.clone();
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };

            window.set_dual_pane(model.dual_pane);
            window.set_focus_index(model.focused_pane as i32);

            if let Some(pane0) = model.panes.first() {
                let pane0_path = pane0.location.to_string_lossy().into_owned();
                let pane0_view_mode = pane0.view_mode.to_string();
                window.set_pane0_path(pane0_path.into());
                window.set_pane0_segments(to_segments_model(&pane0.path_segments()));
                window.set_pane0_view_mode(pane0_view_mode.into());
                window.set_pane0_tabs(to_tab_model(&pane0.tabs));
                window.set_pane0_active_tab(pane0.active_tab as i32);
            }

            if let Some(pane1) = model.panes.get(1) {
                let pane1_path = pane1.location.to_string_lossy().into_owned();
                let pane1_view_mode = pane1.view_mode.to_string();
                window.set_pane1_path(pane1_path.into());
                window.set_pane1_segments(to_segments_model(&pane1.path_segments()));
                window.set_pane1_view_mode(pane1_view_mode.into());
                window.set_pane1_tabs(to_tab_model(&pane1.tabs));
                window.set_pane1_active_tab(pane1.active_tab as i32);
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
            window.set_selected_entries(model.selected_entries as i32);
            window.set_indexer_status(indexer_status.into());
        });
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
