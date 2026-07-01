//! Clap CLI definition for atlas-indexd.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Atlas background indexer daemon.
#[derive(Debug, Parser)]
#[command(name = "atlas-indexd", about = "Atlas background file-indexer daemon")]
pub struct Cli {
    /// Override the IPC socket path.
    #[arg(long, global = true)]
    pub socket: Option<PathBuf>,

    /// Log level filter (for example: debug, info, warn).
    #[arg(long, global = true, env = "RUST_LOG")]
    pub log_level: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Supported atlas-indexd subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the daemon.
    Run,
    /// Install and load a macOS LaunchAgent.
    Install {
        /// Install a per-user LaunchAgent.
        #[arg(long)]
        user: bool,
    },
    /// Unload and remove the macOS LaunchAgent.
    Uninstall,
    /// Query the running daemon for index statistics.
    Status,
    /// Ping the running daemon.
    Ping,
}
