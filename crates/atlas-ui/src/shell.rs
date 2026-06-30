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

use std::{path::Path, sync::Arc};

use atlas_core::path::expand_tilde;
use parking_lot::{Mutex, RwLock};
use slint::{ComponentHandle as _, ModelRc, SharedString, VecModel};

use crate::{
    actions::{ActionSink, UiAction},
    models::{PaletteModel, PaletteResult, StatusModel, WorkspaceModel},
    theme::ThemeMode,
    views::details::DetailsController,
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
    details0: Arc<DetailsController>,
}

impl AppShell {
    /// Build the shell, wire all Slint callbacks, and return a shared handle.
    pub fn new(window: &AtlasWindow, actions: impl ActionSink) -> Arc<Self> {
        let actions: Arc<Mutex<Box<dyn ActionSink>>> = Arc::new(Mutex::new(Box::new(actions)));
        let details0 = DetailsController::new(0, window.as_weak(), Arc::clone(&actions));
        let shell = Arc::new(Self {
            window: window.as_weak(),
            workspace: RwLock::new(WorkspaceModel::new_default()),
            palette: RwLock::new(PaletteModel::default()),
            status: RwLock::new(StatusModel::default()),
            actions,
            details0,
        });

        shell.wire_callbacks(window);
        shell
    }

    /// Return the pane-0 details controller.
    #[must_use]
    pub fn details_controller(&self) -> Arc<DetailsController> {
        Arc::clone(&self.details0)
    }

    fn wire_callbacks(self: &Arc<Self>, window: &AtlasWindow) {
        macro_rules! dispatch {
            ($actions:expr, $action:expr) => {{
                let actions = Arc::clone(&$actions);
                move || actions.lock().dispatch($action)
            }};
        }

        {
            let actions = Arc::clone(&self.actions);
            window.on_palette_query_changed(move |query| {
                actions
                    .lock()
                    .dispatch(UiAction::PaletteQueryChanged(query.into()));
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            window.on_palette_confirm(move |action_id| {
                actions
                    .lock()
                    .dispatch(UiAction::PaletteConfirm(action_id.into()));
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            window.on_palette_dismiss(move || {
                actions.lock().dispatch(UiAction::DismissPalette);
            });
        }
        window.on_toggle_palette(dispatch!(self.actions, UiAction::TogglePalette));

        {
            let actions = Arc::clone(&self.actions);
            window.on_pane0_focused(move || {
                actions.lock().dispatch(UiAction::PaneFocusChanged(0));
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            window.on_pane1_focused(move || {
                actions.lock().dispatch(UiAction::PaneFocusChanged(1));
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
                let next = {
                    let workspace = shell.workspace.read();
                    (workspace.focused_pane + 1) % pane_count
                };
                actions.lock().dispatch(UiAction::PaneFocusChanged(next));
            });
        }

        {
            let actions = Arc::clone(&self.actions);
            window.on_pane0_address_submitted(move |path| {
                dispatch_navigation(&actions, 0, path);
            });
        }
        window.on_pane0_address_cancelled(dispatch!(self.actions, UiAction::DismissPalette));
        {
            let actions = Arc::clone(&self.actions);
            window.on_pane0_breadcrumb_clicked(move |segment| {
                actions.lock().dispatch(UiAction::BreadcrumbClicked {
                    pane: 0,
                    segment: segment as usize,
                });
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
            let actions = Arc::clone(&self.actions);
            window.on_pane0_tab_closed(move |tab| {
                actions.lock().dispatch(UiAction::TabClosed {
                    pane: 0,
                    tab: tab as usize,
                });
            });
        }
        window.on_pane0_new_tab(dispatch!(self.actions, UiAction::NewTab { pane: 0 }));

        {
            let details = Arc::clone(&self.details0);
            window.on_pane0_details_row_clicked(move |index, ctrl, shift| {
                details.select_index(index as usize, ctrl, shift);
            });
        }
        {
            let details = Arc::clone(&self.details0);
            window.on_pane0_details_row_double_clicked(move |index| {
                details.select_index(index as usize, false, false);
                details.activate_focused();
            });
        }
        {
            let details = Arc::clone(&self.details0);
            window.on_pane0_details_header_clicked(move |column_index| {
                details.header_clicked(column_index as usize);
            });
        }

        {
            let actions = Arc::clone(&self.actions);
            window.on_pane1_address_submitted(move |path| {
                dispatch_navigation(&actions, 1, path);
            });
        }
        window.on_pane1_address_cancelled(dispatch!(self.actions, UiAction::DismissPalette));
        {
            let actions = Arc::clone(&self.actions);
            window.on_pane1_breadcrumb_clicked(move |segment| {
                actions.lock().dispatch(UiAction::BreadcrumbClicked {
                    pane: 1,
                    segment: segment as usize,
                });
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
            let actions = Arc::clone(&self.actions);
            window.on_pane1_tab_closed(move |tab| {
                actions.lock().dispatch(UiAction::TabClosed {
                    pane: 1,
                    tab: tab as usize,
                });
            });
        }
        window.on_pane1_new_tab(dispatch!(self.actions, UiAction::NewTab { pane: 1 }));
    }

    /// Update the workspace state and schedule a property push on the event loop.
    pub fn set_workspace(self: &Arc<Self>, model: WorkspaceModel) {
        *self.workspace.write() = model;
        let shell = Arc::clone(self);
        let _ = slint::invoke_from_event_loop(move || {
            let workspace = shell.workspace.read();
            let Some(window) = shell.window.upgrade() else {
                return;
            };

            window.set_dual_pane(workspace.dual_pane);
            window.set_focus_index(workspace.focused_pane as i32);

            if let Some(pane0) = workspace.panes.first() {
                let pane0_path = pane0.location.to_string_lossy().into_owned();
                let pane0_view_mode = pane0.view_mode.to_string();
                window.set_pane0_path(pane0_path.into());
                window.set_pane0_segments(to_segments_model(&pane0.path_segments()));
                window.set_pane0_view_mode(pane0_view_mode.into());
                window.set_pane0_tabs(to_tab_model(&pane0.tabs));
                window.set_pane0_active_tab(pane0.active_tab as i32);
            }

            if let Some(pane1) = workspace.panes.get(1) {
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
        *self.palette.write() = model;
        let shell = Arc::clone(self);
        let _ = slint::invoke_from_event_loop(move || {
            let palette = shell.palette.read();
            let Some(window) = shell.window.upgrade() else {
                return;
            };
            window.set_palette_visible(palette.visible);
            window.set_palette_query(SharedString::from(palette.query.as_str()));
            window.set_palette_results(to_palette_model(&palette.results));
            window.set_palette_selected(palette.selected as i32);
        });
    }

    /// Update status bar state.
    pub fn set_status(self: &Arc<Self>, model: StatusModel) {
        *self.status.write() = model;
        let shell = Arc::clone(self);
        let _ = slint::invoke_from_event_loop(move || {
            let status = shell.status.read();
            let Some(window) = shell.window.upgrade() else {
                return;
            };
            let indexer_status = status.indexer_state.to_string();
            window.set_total_entries(status.total_entries as i32);
            window.set_selected_entries(status.selected_entries as i32);
            window.set_indexer_status(indexer_status.into());
        });
    }

    /// Apply a theme mode.
    pub fn set_theme(self: &Arc<Self>, theme: ThemeMode) {
        let is_dark = theme.is_dark();
        let shell = Arc::clone(self);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = shell.window.upgrade() else {
                return;
            };
            window.set_dark(is_dark);
        });
    }
}
