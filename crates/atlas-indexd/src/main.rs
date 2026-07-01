//! `atlas-indexd` — background indexer daemon binary entry point.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use atlas_ipc::client::Client;
use atlas_ipc::protocol::{Request, Response};

use atlas_indexd::cli::{Cli, Command};
use atlas_indexd::daemon::Daemon;
use atlas_indexd::launchd;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.log_level.as_deref())?;

    let socket = match cli.socket {
        Some(path) => path,
        None => atlas_ipc::transport::default_socket_path()?,
    };

    match cli.command.unwrap_or(Command::Run) {
        Command::Run => run_daemon(socket).await,
        Command::Install { user: _ } => {
            launchd::install(Some(socket))?;
            write_line("installed atlas-indexd LaunchAgent")
        }
        Command::Uninstall => {
            launchd::uninstall()?;
            write_line("uninstalled atlas-indexd LaunchAgent")
        }
        Command::Status => status(socket).await,
        Command::Ping => ping(socket).await,
    }
}

fn init_tracing(log_level: Option<&str>) -> Result<()> {
    let filter = match log_level {
        Some(level) => EnvFilter::try_new(level).context("invalid --log-level value")?,
        None => EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,atlas_indexd=debug")),
    };

    tracing_subscriber::fmt().with_env_filter(filter).init();
    Ok(())
}

async fn run_daemon(socket: PathBuf) -> Result<()> {
    let config = atlas_config::load().unwrap_or_default();
    let daemon = Daemon::start(config, socket).await?;
    let signal_daemon = daemon.clone();
    let signal_task = tokio::spawn(async move {
        if shutdown_signal().await.is_ok() {
            signal_daemon.shutdown().await;
        }
    });

    let result = daemon.run().await;
    signal_task.abort();
    result
}

async fn status(socket: PathBuf) -> Result<()> {
    let client = Client::connect(&socket)
        .await
        .with_context(|| format!("connect to {}", socket.display()))?;
    match client.request(Request::Stats).await? {
        Response::Stats {
            docs,
            on_disk_bytes,
        } => write_line(format!("docs={docs} on_disk_bytes={on_disk_bytes}")),
        other => bail!("unexpected response: {other:?}"),
    }
}

async fn ping(socket: PathBuf) -> Result<()> {
    let client = Client::connect(&socket)
        .await
        .with_context(|| format!("connect to {}", socket.display()))?;
    let started = Instant::now();
    client.ping().await?;
    write_line(format!("pong {}ms", started.elapsed().as_millis()))
}

fn write_line(line: impl AsRef<str>) -> Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", line.as_ref())?;
    Ok(())
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<()> {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            result.context("wait for ctrl-c")?;
        }
        _ = sigterm.recv() => {}
    }
    Ok(())
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<()> {
    tokio::signal::ctrl_c().await.context("wait for ctrl-c")?;
    Ok(())
}
