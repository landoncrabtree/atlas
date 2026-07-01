//! Integration tests for atlas-indexd.
//!
//! These tests start the daemon in-process, exercise the IPC layer end-to-end,
//! and cover the core lifecycle: ping, stats, incremental indexing, and clean
//! shutdown.
//!
//! Watcher latency can vary; generous timeouts and the ping-readiness pattern
//! are used throughout. Tests that depend on watcher timing are marked
//! `#[ignore]` so they can be opted into explicitly with `cargo test -- --ignored`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tokio::time::timeout;

use atlas_config::{Config, Indexer};
use atlas_indexd::daemon::Daemon;
use atlas_ipc::client::Client;
use atlas_ipc::protocol::{Request, Response};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a minimal config with an optional indexed root.
fn make_config(roots: Vec<PathBuf>) -> Config {
    Config {
        indexer: Indexer {
            enabled: !roots.is_empty(),
            roots,
            respect_gitignore: false,
            max_memory_mb: 16,
        },
        ..Config::default()
    }
}

/// Unique socket path under a TempDir so tests don't collide.
fn temp_socket(dir: &TempDir) -> PathBuf {
    dir.path().join("indexd.sock")
}

/// Start the daemon in the background and return a handle to it.
///
/// The caller is responsible for calling [`Daemon::shutdown`] (or letting the
/// `JoinHandle` drive the task to completion via `Shutdown` IPC).
async fn start_daemon(config: Config, socket: PathBuf) -> Arc<Daemon> {
    let daemon = Daemon::start(config, socket)
        .await
        .expect("daemon::start should succeed");
    let run_daemon = Arc::clone(&daemon);
    tokio::spawn(async move {
        run_daemon.run().await.expect("daemon::run should not fail");
    });
    daemon
}

/// Retry `atlas_ipc::client::Client::connect` until the socket appears.
async fn wait_for_socket(socket: &Path) -> Client {
    let deadline = Duration::from_secs(5);
    timeout(deadline, async {
        loop {
            match Client::connect(socket).await {
                Ok(client) => return client,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .expect("daemon did not bind socket within 5s")
}

// ---------------------------------------------------------------------------
// 1. Empty config — daemon starts, no roots, stats return zeros
// ---------------------------------------------------------------------------

#[tokio::test]
async fn daemon_start_empty_config() {
    let tmp = TempDir::new().unwrap();
    let socket = temp_socket(&tmp);

    let daemon = start_daemon(make_config(vec![]), socket.clone()).await;

    // Wait for the socket to appear, then probe via IPC.
    let client = wait_for_socket(&socket).await;
    client.ping().await.expect("ping should succeed");

    let resp = client
        .request(Request::Stats)
        .await
        .expect("stats request should succeed");
    match resp {
        Response::Stats { docs, .. } => {
            assert_eq!(docs, 0, "empty config should produce zero docs");
        }
        other => panic!("unexpected response: {other:?}"),
    }

    daemon.shutdown().await;
}

// ---------------------------------------------------------------------------
// 2. Add root at runtime — root appears in the daemon state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn add_root_appears_in_stats() {
    let tmp = TempDir::new().unwrap();
    let socket = temp_socket(&tmp);
    let root_dir = TempDir::new().unwrap();

    // Create a few files so there is something to index.
    std::fs::write(root_dir.path().join("a.txt"), "hello").unwrap();
    std::fs::write(root_dir.path().join("b.txt"), "world").unwrap();

    let daemon = start_daemon(make_config(vec![]), socket.clone()).await;
    let client = wait_for_socket(&socket).await;

    // Add a root via IPC.
    let resp = client
        .request(Request::AddRoot {
            path: root_dir.path().to_path_buf(),
        })
        .await
        .expect("AddRoot request should succeed");
    assert!(matches!(resp, Response::Ok), "expected Ok, got {resp:?}");

    // Allow the ingest to run (blocking spawn_blocking task).
    tokio::time::sleep(Duration::from_millis(500)).await;

    let resp = client
        .request(Request::Stats)
        .await
        .expect("stats request should succeed");
    match resp {
        Response::Stats { docs, .. } => {
            assert!(docs >= 2, "expected at least 2 docs, got {docs}");
        }
        other => panic!("unexpected response: {other:?}"),
    }

    daemon.shutdown().await;
}

// ---------------------------------------------------------------------------
// 3. Full lifecycle: ping → stats → shutdown, daemon exits cleanly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ping_stats_shutdown_lifecycle() {
    let tmp = TempDir::new().unwrap();
    let socket = temp_socket(&tmp);
    let root_dir = TempDir::new().unwrap();
    std::fs::write(root_dir.path().join("readme.md"), "# test").unwrap();

    let config = make_config(vec![root_dir.path().to_path_buf()]);
    let daemon = start_daemon(config, socket.clone()).await;
    let client = wait_for_socket(&socket).await;

    // Ping
    client.ping().await.expect("ping should succeed");

    // Hello handshake
    let resp = client
        .request(Request::Hello {
            client_name: "test".into(),
            client_version: "0.0.0".into(),
        })
        .await
        .expect("Hello request should succeed");
    assert!(
        matches!(resp, Response::Hello { .. }),
        "expected Hello, got {resp:?}"
    );

    // Stats — give ingest a moment to run
    tokio::time::sleep(Duration::from_millis(500)).await;
    let resp = client
        .request(Request::Stats)
        .await
        .expect("stats request should succeed");
    assert!(
        matches!(resp, Response::Stats { .. }),
        "expected Stats, got {resp:?}"
    );

    // Shutdown
    let resp = client
        .request(Request::Shutdown)
        .await
        .expect("Shutdown request should succeed");
    assert!(matches!(resp, Response::Ok), "expected Ok, got {resp:?}");

    // Give the daemon a moment to flush and shut down.
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(daemon);
}

// ---------------------------------------------------------------------------
// 4. Incremental: file created after startup raises doc count
//    Marked #[ignore] because watcher latency is non-deterministic.
// ---------------------------------------------------------------------------

/// This test relies on the filesystem watcher debounce + the 5-second periodic
/// commit.  The 10-second timeout is intentionally generous to stay green on
/// slow CI machines.  Run with `cargo test -- --ignored` when verifying watcher
/// behaviour.
#[tokio::test]
#[ignore = "depends on watcher timing; run explicitly with -- --ignored"]
async fn incremental_file_created_increases_docs() {
    let tmp = TempDir::new().unwrap();
    let socket = temp_socket(&tmp);
    let root_dir = TempDir::new().unwrap();

    let config = make_config(vec![root_dir.path().to_path_buf()]);
    let daemon = start_daemon(config, socket.clone()).await;
    let client = wait_for_socket(&socket).await;

    // Wait for initial ingest.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let docs_before = match client.request(Request::Stats).await.expect("stats before") {
        Response::Stats { docs, .. } => docs,
        other => panic!("unexpected: {other:?}"),
    };

    // Create a new file inside the watched root.
    std::fs::write(root_dir.path().join("new_file.rs"), "fn main() {}").unwrap();

    // Poll stats until docs increase or we time out.
    let docs_after = timeout(Duration::from_secs(10), async {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            match client.request(Request::Stats).await {
                Ok(Response::Stats { docs, .. }) if docs > docs_before => return docs,
                _ => continue,
            }
        }
    })
    .await
    .expect("doc count did not increase within 10s");

    assert!(
        docs_after > docs_before,
        "expected docs to increase: before={docs_before}, after={docs_after}"
    );

    daemon.shutdown().await;
}

// ---------------------------------------------------------------------------
// 5. RemoveRoot removes the root from the daemon
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remove_root_removes_from_daemon() {
    let tmp = TempDir::new().unwrap();
    let socket = temp_socket(&tmp);
    let root_dir = TempDir::new().unwrap();
    std::fs::write(root_dir.path().join("f.txt"), "data").unwrap();

    let daemon = start_daemon(make_config(vec![]), socket.clone()).await;
    let client = wait_for_socket(&socket).await;

    // Add root.
    let resp = client
        .request(Request::AddRoot {
            path: root_dir.path().to_path_buf(),
        })
        .await
        .expect("AddRoot");
    assert!(matches!(resp, Response::Ok));
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Remove root.
    let resp = client
        .request(Request::RemoveRoot {
            path: root_dir.path().to_path_buf(),
        })
        .await
        .expect("RemoveRoot");
    assert!(matches!(resp, Response::Ok));

    // Stats should reflect zero roots' worth of docs.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let resp = client
        .request(Request::Stats)
        .await
        .expect("stats after remove");
    match resp {
        Response::Stats { docs, .. } => {
            // After removing the only root the doc count should be zero (index deleted).
            assert_eq!(docs, 0, "expected 0 docs after removing root, got {docs}");
        }
        other => panic!("unexpected: {other:?}"),
    }

    daemon.shutdown().await;
}
