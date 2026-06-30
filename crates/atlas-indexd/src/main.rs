//! `atlas-indexd` — background indexer daemon.

use anyhow::Result;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,atlas_indexd=debug")),
        )
        .init();

    tracing::info!("atlas-indexd starting (skeleton)");

    // TODO(indexd): set up tantivy index roots, notify watchers, and IPC server.
    // For now the daemon is a noop so the workspace compiles end-to-end.
    Ok(())
}
