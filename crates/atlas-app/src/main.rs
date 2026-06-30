//! Atlas — application binary.
//!
//! This is an intentionally thin wrapper. All UI types come from `atlas-ui`.
//! The real atlas-keymap and atlas-fs plumbing is wired in follow-up todos;
//! `LoggingActionSink` is a stub that logs every dispatched action so we can
//! verify the callback plumbing works end-to-end.

use std::sync::Arc;

use anyhow::Result;
use slint::ComponentHandle as _;
use tracing_subscriber::EnvFilter;

use atlas_ui::{
    actions::{ActionSink, UiAction},
    models::{PaletteModel, StatusModel, WorkspaceModel},
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
    shell.set_workspace(WorkspaceModel::new_default());
    shell.set_status(StatusModel::default());
    shell.set_palette(PaletteModel::default());

    window.run()?;

    Ok(())
}
