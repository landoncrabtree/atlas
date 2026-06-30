//! Atlas — application binary.
//!
//! This is an intentionally thin wrapper. All UI types come from `atlas-ui`.
//! The file-system-backed Details view is wired here by creating the initial
//! location view model and attaching it to the pane-0 controller.

use std::{env, path::PathBuf, sync::Arc};

use anyhow::Result;
use atlas_fs::{InMemoryLocationViewModel, OpenOptions};
use slint::ComponentHandle as _;
use tracing_subscriber::EnvFilter;

use atlas_ui::{
    actions::{ActionSink, UiAction},
    models::{PaletteModel, PaneModel, StatusModel, WorkspaceModel},
    shell::AppShell,
    theme::ThemeMode,
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

    let window = AtlasWindow::new()?;
    let shell: Arc<AppShell> = AppShell::new(&window, LoggingActionSink);
    shell.set_theme(ThemeMode::Dark);

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

    Ok(())
}
