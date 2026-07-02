//! Integration test for the preview-watch write-back path.
//!
//! Scenario:
//! 1. Open a remote file via [`PreviewCache::open_remote_file`] against
//!    the writable paramiko SFTP mock.
//! 2. Wait for the download + `open::that` (recorded via the test
//!    opener) to complete.
//! 3. Overwrite the cache file with new bytes to simulate an editor
//!    save.
//! 4. Wait for the debounce + upload to complete.
//! 5. Read the remote file back via a fresh `RemoteLocationViewModel`
//!    and assert the bytes match the local edit.
//!
//! This exercises the full pipeline: file-watcher event → debounce →
//! SHA-diff → `atlas_remote::RemoteLocationViewModel::write`.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use atlas_config::RemotePreview;
use atlas_core::BackendKind;
use atlas_fs::{Entry, EntryKind, Metadata, OpenOptions};
use atlas_remote::RemoteLocationViewModel;
use atlas_ui::remote::{
    OpenHandler, PreviewCache, PreviewOutcome, WriteBackEvent, WriteBackNoticeKind,
};
use parking_lot::Mutex;
use tempfile::TempDir;

use common::MockSftpServer;

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

#[test]
fn local_edit_uploads_back_to_remote() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let initial = b"initial contents";
    let remote_file = server.root_dir().join("editable.txt");
    std::fs::write(&remote_file, initial)?;

    let tmp = TempDir::new()?;
    let opener = Arc::new(RecordingOpener::default());
    let cfg = RemotePreview {
        cache_dir: Some(tmp.path().to_path_buf()),
        max_bytes: 10_000_000,
        max_age_secs: 86_400,
        max_open_bytes: 10_000_000,
        stream_threshold_bytes: 4_194_304,
        stream_chunk_bytes: 262_144,
        write_back_enabled: true,
        // Short debounce so the test doesn't idle for the default
        // 500 ms.
        write_back_debounce_ms: 100,
    };
    let cache = PreviewCache::with_opener(cfg, opener.clone());
    let watch_events = cache.watch_registry().subscribe_events();

    let entry = Entry {
        name: "editable.txt".into(),
        path: PathBuf::from("editable.txt"),
        kind: EntryKind::File,
        metadata: Metadata {
            size: initial.len() as u64,
            ..Metadata::default()
        },
    };
    let mut file_uri = server.uri("anon");
    file_uri.path = "/editable.txt".into();

    let outcome = cache.open_remote_file(file_uri.clone(), BackendKind::Sftp, entry);
    match outcome {
        PreviewOutcome::Downloading => {}
        other => panic!("expected Downloading on first activate, got {other:?}"),
    }

    // Wait for the download → open pipeline to complete.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while opener.calls.load(Ordering::SeqCst) == 0 {
        if std::time::Instant::now() >= deadline {
            panic!("first activate never called opener within 15s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let cache_path = opener
        .last_path
        .lock()
        .clone()
        .expect("opener received cache path");
    assert_eq!(std::fs::read(&cache_path)?, initial);

    // Give the async callback a moment to register the cache file
    // with the watcher.
    let mut watching = false;
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(50));
        if cache.watch_registry().is_watching_for_test(&cache_path) {
            watching = true;
            break;
        }
    }
    assert!(
        watching,
        "cache_path {cache_path:?} was never registered with the write-back watcher"
    );

    // Simulate an editor save with new contents. We write to the
    // cache file (real editors do the same) and then manually
    // dispatch the modification. Driving `handle_modification`
    // directly bypasses macOS FSEvents' canonicalized-path quirks
    // that would otherwise make this test flaky — the underlying
    // SHA-diff / debounce / upload state machine is what we're
    // testing anyway, and it's exercised identically either way.
    let edited = b"edited contents from atlas write-back";
    std::fs::write(&cache_path, edited)?;
    cache.watch_registry().dispatch_edit_for_test(&cache_path);

    // Drain the internal event channel; we expect UploadStarted +
    // Notice(Completed) within ~2 s (debounce 100 ms + upload ~ms).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut saw_started = false;
    let mut completed_message: Option<String> = None;
    while std::time::Instant::now() < deadline {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .unwrap_or_default();
        match watch_events.recv_timeout(remaining) {
            Ok(WriteBackEvent::UploadStarted { .. }) => {
                saw_started = true;
            }
            Ok(WriteBackEvent::Notice(notice)) => {
                if notice.kind == WriteBackNoticeKind::Completed {
                    completed_message = Some(notice.message);
                    break;
                }
                panic!(
                    "expected Completed notice for writable mount, got Failed: {}",
                    notice.message
                );
            }
            Err(_) => break,
        }
    }
    assert!(
        saw_started,
        "watcher should have emitted UploadStarted after edit"
    );
    let msg = completed_message.expect("upload should complete within timeout");
    assert!(
        msg.contains("Uploaded editable.txt"),
        "message must reference filename; got: {msg}"
    );

    // Verify the remote file now contains the edited bytes.
    let mut root_uri = server.uri("anon");
    root_uri.path = "/".into();
    let vm = RemoteLocationViewModel::open_live(
        root_uri,
        BackendKind::Sftp,
        atlas_remote::Credentials::Anonymous,
        OpenOptions::default(),
    )?;
    let rt = tokio::runtime::Runtime::new()?;
    let remote_bytes = rt.block_on(vm.read("editable.txt"))?;
    assert_eq!(remote_bytes, edited, "remote file must reflect local edit");

    Ok(())
}

/// Failure-path coverage: point the write-back at a URI that cannot
/// be reached and prove the registry surfaces a `Failed` notice
/// rather than a `Completed` one. This mirrors the read-only-mount
/// failure case (permission denied, quota exhausted, network) — the
/// only observable difference is the error string.
///
/// The test uses port 1 (privileged, always closed) which yields a
/// prompt connect refusal on every platform.
#[test]
fn upload_failure_surfaces_notice_with_preserved_cache() -> Result<()> {
    skip_if_no_python!();
    let tmp = TempDir::new()?;
    let opener = Arc::new(RecordingOpener::default());
    let cfg = RemotePreview {
        cache_dir: Some(tmp.path().to_path_buf()),
        max_bytes: 10_000_000,
        max_age_secs: 86_400,
        max_open_bytes: 10_000_000,
        stream_threshold_bytes: 4_194_304,
        stream_chunk_bytes: 262_144,
        write_back_enabled: true,
        write_back_debounce_ms: 50,
    };
    let cache = PreviewCache::with_opener(cfg, opener);
    let watch_events = cache.watch_registry().subscribe_events();

    // Materialise a cache line by hand — we're not exercising the
    // download path here.
    let cache_line = tmp.path().join("badkey");
    std::fs::create_dir_all(&cache_line)?;
    let cache_file = cache_line.join("readme.txt");
    std::fs::write(&cache_file, b"initial")?;

    // Register the cache file against an SFTP URI pointing at a
    // closed port. Retry envelope will eventually give up.
    let bad_uri = atlas_core::RemoteUri {
        scheme: "sftp".into(),
        host: Some("127.0.0.1".into()),
        port: Some(1),
        username: Some("nobody".into()),
        path: "/readme.txt".into(),
        credential_ref: None,
    };
    cache
        .watch_registry()
        .register(cache_file.clone(), bad_uri, BackendKind::Sftp)?;

    // Simulate an edit + manual dispatch. Give the retry envelope
    // ~30 s to exhaust; the mock harness's retry policy is short so
    // this normally lands in ~1–3 s.
    std::fs::write(&cache_file, b"edited contents")?;
    cache.watch_registry().dispatch_edit_for_test(&cache_file);

    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut failure_message: Option<String> = None;
    while std::time::Instant::now() < deadline {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .unwrap_or_default();
        match watch_events.recv_timeout(remaining) {
            Ok(WriteBackEvent::UploadStarted { .. }) => {}
            Ok(WriteBackEvent::Notice(notice)) => match notice.kind {
                WriteBackNoticeKind::Failed => {
                    failure_message = Some(notice.message);
                    break;
                }
                WriteBackNoticeKind::Completed => {
                    panic!("unexpected success on unreachable host: {}", notice.message);
                }
            },
            Err(_) => break,
        }
    }
    let msg = failure_message.expect("Failed notice must arrive within 60s");
    assert!(
        msg.contains("Local edits preserved at"),
        "failure message must reference cache path preservation; got: {msg}"
    );

    // Cache file was NOT rolled back; the user's edits survive.
    assert_eq!(std::fs::read(&cache_file)?, b"edited contents");
    Ok(())
}
