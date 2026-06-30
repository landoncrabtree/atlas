//! Atlas — application binary.
//!
//! This is an intentionally thin wrapper. All UI types come from `atlas-ui`.
//! The file-system-backed Details view is wired here by creating the initial
//! location view model and attaching it to the pane-0 controller.
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
use atlas_fs::{InMemoryLocationViewModel, OpenOptions};
use slint::ComponentHandle as _;
use tracing_subscriber::EnvFilter;

use atlas_ui::{
    actions::{ActionSink, UiAction},
    models::{PaletteModel, PaneModel, StatusModel, WorkspaceModel},
    shell::AppShell,
    theme::{ThemeLoader, ThemeTokens, ThemeWatcher},
    theming::ThemeEvent,
    AtlasWindow,
};

/// Stub action sink that logs every UI action.
struct LoggingActionSink;

impl ActionSink for LoggingActionSink {
    fn dispatch(&mut self, action: UiAction) {
        tracing::info!(?action, "ui action");
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

    // Load configuration; fall back to defaults on any parse/IO error.
    let config = atlas_config::load().unwrap_or_default();
    let theme_id = config.ui.theme.clone();

    let window = AtlasWindow::new()?;
    let shell: Arc<AppShell> = AppShell::new(&window, LoggingActionSink);

    // ── Theme chain ─────────────────────────────────────────────────────────
    // Resolve the theme ID from config, fall back to "atlas-dark" if missing.
    let loader = ThemeLoader::new();
    let (theme_watcher, themes_arc, theme_events) = ThemeWatcher::start(loader, &theme_id)
        .unwrap_or_else(|e| {
            tracing::warn!("cannot load theme {theme_id:?}: {e}; falling back to atlas-dark");
            ThemeWatcher::start(ThemeLoader::new(), "atlas-dark")
                .expect("built-in atlas-dark must always load")
        });

    // Push initial tokens before the window is shown.
    shell.apply_theme(&themes_arc.load());

    // Drain hot-reload events and re-apply the swapped tokens.
    spawn_theme_event_thread(Arc::clone(&shell), Arc::clone(&themes_arc), theme_events);
    // ────────────────────────────────────────────────────────────────────────

    let home = env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"));
    let location = InMemoryLocationViewModel::open(home.clone(), OpenOptions::default());
    shell.details_controller().set_location(location);

    let mut workspace = WorkspaceModel::new_default();
    if let Some(pane0) = workspace.panes.first_mut() {
        *pane0 = PaneModel::new(home);
        pane0.focused = true;
    }
    shell.set_workspace(workspace);
    shell.set_status(StatusModel::default());
    shell.set_palette(PaletteModel::default());

    window.run()?;

    // Best-effort cleanup — stop the FS watcher on normal exit.
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
