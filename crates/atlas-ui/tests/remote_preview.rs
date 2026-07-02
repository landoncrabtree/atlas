//! Integration tests for the remote-file preview cache.
//!
//! These spawn the paramiko SFTP mock server (via
//! `crates/atlas-remote/tests/common/mock.rs`, shared through `#[path]`)
//! and exercise the actual file-open → cache-line → subsequent-cache-hit
//! flow that the fs::View bug fix depends on.
//!
//! Regression coverage:
//!
//! * `remote_file_activate_downloads_and_opens` — activating a remote
//!   file materialises its bytes to the platform cache dir and invokes
//!   the [`OpenHandler`]. The old behaviour was `open::that("readme.txt")`
//!   → shell exit 256; the new behaviour is `open::that(cache_dir/…/readme.txt)`
//!   → success.
//! * `remote_file_activate_uses_cache_on_second_open` — the second
//!   activate must not re-download; `PreviewCache::download_count` stays
//!   at `1` and the [`OpenHandler`] is called on the same cache path.
//! * `remote_directory_activate_resolves_to_remote_location` — the
//!   canonical [`resolve_entry_location`] returns a `Location::Remote`
//!   whose URI path is joined with the entry name (not a bare basename
//!   that the local OS handler would then reject).

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use atlas_config::RemotePreview;
use atlas_core::{BackendKind, Location};
use atlas_fs::{Entry, EntryKind, Metadata};
use atlas_ui::remote::{resolve_entry_location, OpenHandler, PreviewCache, PreviewOutcome};
use parking_lot::Mutex;
use tempfile::TempDir;

use common::MockSftpServer;

/// Test opener that records every `open` call. We use `Arc<Self>` in
/// `PreviewCache::with_opener` so the test can inspect calls after the
/// download completes.
#[derive(Default)]
struct RecordingOpener {
    calls: AtomicU64,
    last_path: Mutex<Option<PathBuf>>,
}

impl OpenHandler for RecordingOpener {
    fn open(&self, path: &std::path::Path) -> std::io::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_path.lock() = Some(path.to_path_buf());
        Ok(())
    }
}

/// Skip the enclosing `#[test]` when the mock-server harness is
/// disabled by the environment. Returns from the surrounding `Result`.
macro_rules! skip_if_no_python {
    () => {{
        if common::mock::should_skip() {
            eprintln!("skipped: MOCK_SERVERS_SKIP=1");
            return Ok(());
        }
        if std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| !s.success())
            .unwrap_or(true)
            && std::process::Command::new("uv")
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| !s.success())
                .unwrap_or(true)
        {
            eprintln!("skipped: neither python3 nor uv on PATH");
            return Ok(());
        }
    }};
}

fn make_cache(dir: &std::path::Path, opener: Arc<RecordingOpener>) -> PreviewCache {
    PreviewCache::with_opener(
        RemotePreview {
            cache_dir: Some(dir.to_path_buf()),
            max_bytes: 10_000_000,
            max_age_secs: 86_400,
            max_open_bytes: 10_000_000,
        },
        opener,
    )
}

#[test]
fn remote_file_activate_downloads_and_opens() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let file_bytes = b"hello over sftp from atlas fs::View";
    std::fs::write(server.root_dir().join("readme.txt"), file_bytes)?;

    let tmp = TempDir::new()?;
    let opener = Arc::new(RecordingOpener::default());
    let cache = make_cache(tmp.path(), opener.clone());

    // Fabricate the entry the way the SFTP backend surfaces it: name is
    // the child basename, `path` is a basename-relative `PathBuf`.
    let entry = Entry {
        name: "readme.txt".into(),
        path: PathBuf::from("readme.txt"),
        kind: EntryKind::File,
        metadata: Metadata {
            size: file_bytes.len() as u64,
            ..Metadata::default()
        },
    };

    // Mount the pane at the server root; entry activation must resolve
    // to `<uri>/readme.txt`.
    let mut file_uri = server.uri("anon");
    file_uri.path = "/readme.txt".into();

    let outcome = cache.open_remote_file(file_uri.clone(), BackendKind::Sftp, entry.clone());
    match outcome {
        PreviewOutcome::Downloading => {}
        other => panic!("expected Downloading on first activate, got {other:?}"),
    }

    // Wait for the background task to hit the opener.
    let start = std::time::Instant::now();
    while opener.calls.load(Ordering::SeqCst) == 0 {
        if start.elapsed() > Duration::from_secs(10) {
            panic!("preview download never called opener within 10s");
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    assert_eq!(cache.download_count(), 1, "expected exactly one download");
    let cached_path = opener
        .last_path
        .lock()
        .clone()
        .expect("opener must have been called with a cache path");
    assert!(
        cached_path.starts_with(tmp.path()),
        "cache path {cached_path:?} should live under the injected cache dir {:?}",
        tmp.path(),
    );
    assert_eq!(std::fs::read(&cached_path)?, file_bytes);

    Ok(())
}

#[test]
fn remote_file_activate_uses_cache_on_second_open() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let file_bytes = b"cache me exactly once";
    std::fs::write(server.root_dir().join("cached.txt"), file_bytes)?;

    let tmp = TempDir::new()?;
    let opener = Arc::new(RecordingOpener::default());
    let cache = make_cache(tmp.path(), opener.clone());

    let entry = Entry {
        name: "cached.txt".into(),
        path: PathBuf::from("cached.txt"),
        kind: EntryKind::File,
        metadata: Metadata {
            size: file_bytes.len() as u64,
            ..Metadata::default()
        },
    };
    let mut file_uri = server.uri("anon");
    file_uri.path = "/cached.txt".into();

    // First activate: download.
    let _ = cache.open_remote_file(file_uri.clone(), BackendKind::Sftp, entry.clone());
    let start = std::time::Instant::now();
    while opener.calls.load(Ordering::SeqCst) == 0 {
        if start.elapsed() > Duration::from_secs(10) {
            panic!("first activate never called opener");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(cache.download_count(), 1, "first open increments counter");
    let first_calls = opener.calls.load(Ordering::SeqCst);

    // Second activate: expected to be a synchronous cache hit — no new
    // download, opener called against the same path.
    let outcome = cache.open_remote_file(file_uri, BackendKind::Sftp, entry);
    match outcome {
        PreviewOutcome::CachedOpen(_) => {}
        other => panic!("expected CachedOpen on second activate, got {other:?}"),
    }
    assert_eq!(
        cache.download_count(),
        1,
        "cache hit must not increment download counter (was 1, still 1)",
    );
    assert!(
        opener.calls.load(Ordering::SeqCst) > first_calls,
        "opener must be called again on cache-hit path"
    );

    Ok(())
}

#[test]
fn remote_directory_activate_resolves_to_remote_location() {
    // Regression guard for the reported bug: `entry.path` on SFTP is a
    // bare basename. The resolver must join it onto the pane URI so the
    // navigation controller gets a well-formed `Location::Remote`,
    // instead of leaking `PathBuf::from("pub")` to `open::that`.
    let pane_loc = Location::Remote(
        atlas_core::RemoteUri {
            scheme: "sftp".into(),
            host: Some("demo.test".into()),
            port: Some(22),
            username: Some("demo".into()),
            path: "/".into(),
            credential_ref: None,
        },
        BackendKind::Sftp,
    );
    let entry = Entry {
        name: "pub".into(),
        path: PathBuf::from("pub"),
        kind: EntryKind::Dir,
        metadata: Metadata::default(),
    };

    let dest = resolve_entry_location(&pane_loc, &entry);
    match dest {
        Location::Remote(uri, kind) => {
            assert_eq!(kind, BackendKind::Sftp);
            assert_eq!(uri.path, "/pub");
            assert_eq!(uri.host.as_deref(), Some("demo.test"));
        }
        Location::Local(p) => {
            panic!("resolver leaked to Location::Local({p:?}) — this is exactly the reported bug")
        }
    }
}
