//! Atlas — application binary.
//!
//! This is an intentionally thin wrapper. All UI types come from `atlas-ui`.
//! The file-system-backed Details view is wired here by creating the shell
//! and driving the initial pane location through the navigation controller.
//!
//! # Theme chain
//!
//! 1. `atlas_config::load()` reads `config.ui.theme` (e.g. `"atlas-dark"`).
//! 2. `ThemeLoader` resolves the ID: built-ins first, then the user themes dir.
//! 3. `ThemeWatcher::start` loads the initial tokens, returning an
//!    `Arc<ArcSwap<ThemeTokens>>` and a `Receiver<ThemeEvent>`.
//! 4. `shell.apply_theme(...)` pushes the initial tokens to the Slint `Theme`
//!    global via `invoke_from_event_loop`.
//! 5. A background thread drains the event channel; on each `Reloaded` it
//!    reads the fresh tokens from the `ArcSwap` and calls `apply_theme` again.

use std::{env, path::PathBuf, sync::Arc};

use anyhow::Result;
use arc_swap::ArcSwap;
use slint::ComponentHandle as _;
use tracing_subscriber::EnvFilter;

use atlas_ui::{
    actions::{ActionSink, UiAction},
    models::{PaletteModel, StatusModel},
    search::SearchController,
    shell::AppShell,
    theme::{ThemeLoader, ThemeTokens, ThemeWatcher},
    theming::ThemeEvent,
    AtlasWindow, NavigationController,
};

/// Application-level action sink that routes [`UiAction`]s to the appropriate
/// controller.
///
/// File-system operations (`FsCopy`, `FsMove`, etc.) are handled directly by
/// [`AppShell`]'s Slint callback wiring (see `wire_callbacks` in shell.rs),
/// so the sink only needs to handle the remaining lifecycle actions here.
/// The `Fs*` variants exist in [`UiAction`] for future atlas-keymap
/// integration (when keymap strings like `"fs::Copy"` are translated to typed
/// `UiAction` values by the keymap resolver).
struct AtlasActionSink {
    nav: Arc<NavigationController>,
}

impl AtlasActionSink {
    fn new(nav: Arc<NavigationController>) -> Self {
        Self { nav }
    }
}

impl ActionSink for AtlasActionSink {
    fn dispatch(&mut self, action: UiAction) {
        match action {
            // Navigation — actually drive the controller so the view updates.
            UiAction::Navigate { pane, path } => {
                tracing::debug!(pane, ?path, "navigating");
                self.nav.navigate(pane, path);
            }
            UiAction::BreadcrumbClicked { pane, segment } => {
                tracing::debug!(pane, segment, "breadcrumb clicked");
                self.nav.breadcrumb_clicked(pane, segment);
            }
            // Fs* actions are wired directly in AppShell::wire_callbacks via
            // Slint F-key callbacks; they do not flow through this sink in the
            // current implementation. Log at debug so the path is traceable.
            UiAction::FsCopy { .. }
            | UiAction::FsMove { .. }
            | UiAction::FsDelete { .. }
            | UiAction::FsRename { .. }
            | UiAction::FsMkdir { .. }
            | UiAction::FsCancel { .. }
            | UiAction::FsResolveConflict { .. }
            | UiAction::ToggleOpsPanel => {
                tracing::debug!(?action, "fs op action (handled by AppShell directly)");
            }
            // Bulk-rename actions are wired directly in AppShell::wire_callbacks.
            // The variants exist for future atlas-keymap integration.
            UiAction::OpenBulkRename
            | UiAction::BulkRenameQuery(_)
            | UiAction::BulkRenameConfirm { .. }
            | UiAction::BulkRenameClose => {
                tracing::debug!(?action, "bulk rename action (handled by AppShell directly)");
            }
            UiAction::SetDualPane(_) | UiAction::PaneFocusChanged(_) => {
                tracing::debug!(
                    ?action,
                    "pane focus/layout action (handled by AppShell directly)"
                );
            }
            _ => {
                tracing::info!(?action, "ui action");
            }
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,atlas=debug")),
        )
        .init();

    tracing::info!("starting atlas");

    let config = atlas_config::load().unwrap_or_default();
    let theme_id = config.ui.theme.clone();

    let window = AtlasWindow::new()?;
    let nav = NavigationController::new(&config.bookmarks);
    let search_ctrl = SearchController::new();
    let index_client = search_ctrl
        .runtime_handle()
        .block_on(atlas_search::IndexClient::connect_default())
        .map(Arc::new)
        .map_err(|error| {
            tracing::warn!(%error, "search index not available");
            error
        })
        .ok();
    search_ctrl.set_index_client(index_client);
    search_ctrl.attach_window(window.as_weak());
    let shell: Arc<AppShell> = AppShell::new(
        &window,
        AtlasActionSink::new(Arc::clone(&nav)),
        Arc::clone(&nav),
        Arc::clone(&search_ctrl),
    );

    let loader = ThemeLoader::new();
    let (theme_watcher, themes_arc, theme_events) = ThemeWatcher::start(loader, &theme_id)
        .unwrap_or_else(|e| {
            tracing::warn!("cannot load theme {theme_id:?}: {e}; falling back to atlas-dark");
            ThemeWatcher::start(ThemeLoader::new(), "atlas-dark")
                .expect("built-in atlas-dark must always load")
        });

    shell.apply_theme(&themes_arc.load());
    spawn_theme_event_thread(Arc::clone(&shell), Arc::clone(&themes_arc), theme_events);

    let start_path = config.general.start_path.clone().unwrap_or_else(|| {
        env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/"))
    });
    search_ctrl.set_scope(Some(start_path.clone()));
    nav.navigate(0, start_path);
    shell.set_status(StatusModel::default());
    shell.set_palette(PaletteModel::default());

    window.run()?;
    theme_watcher.stop();

    Ok(())
}

/// Spawn a thread that drains [`ThemeEvent`]s and calls [`AppShell::apply_theme`]
/// on each successful reload.
fn spawn_theme_event_thread(
    shell: Arc<AppShell>,
    themes_arc: Arc<ArcSwap<ThemeTokens>>,
    events: crossbeam_channel::Receiver<ThemeEvent>,
) {
    std::thread::Builder::new()
        .name("atlas-theme-events".to_owned())
        .spawn(move || {
            for event in &events {
                match event {
                    ThemeEvent::Reloaded(ref id) => {
                        tracing::info!("theme reloaded: {id}");
                        shell.apply_theme(&themes_arc.load());
                    }
                    ThemeEvent::LoadError {
                        ref id,
                        ref message,
                    } => {
                        tracing::warn!("theme load error for {id}: {message}");
                    }
                }
            }
        })
        .expect("failed to spawn atlas-theme-events thread");
}
