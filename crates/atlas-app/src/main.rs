//! Atlas — application binary.

use anyhow::Result;
use tracing_subscriber::EnvFilter;

slint::include_modules!();

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,atlas=debug")),
        )
        .init();

    tracing::info!("starting atlas");

    let window = AtlasWindow::new()?;
    window.run()?;

    Ok(())
}
