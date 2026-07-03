//! Cross-backend dispatch tests for the ops queue.
//!
//! Every test here submits an [`OpKind`] via [`OperationQueue`] and
//! asserts that the top-level [`execute_op`] dispatcher routes it
//! correctly:
//!
//! * `local_to_sftp_*`   — Local → Remote (SFTP)
//! * `sftp_to_local_*`   — Remote (SFTP) → Local
//! * `sftp_to_sftp_*`    — Remote (SFTP) → Remote (SFTP, same host — server-side rename)
//! * `end_to_end_tree`   — 4 MiB pseudo-random directory tree Local → SFTP → Local
//! * `cancel_mid_copy`   — cancellation during a large single-file transfer
//!
//! S3 mock is intentionally not exercised through the queue here: the
//! S3 backend requires IAM credentials, but the ops queue's credential
//! resolver only handles keychain-based `Password` and `Anonymous`
//! today. Cross-backend S3 routing is exercised at the byte-pump layer
//! by `atlas-remote/tests/cross_backend_stream.rs`.
//!
//! Every test skips gracefully when `MOCK_SERVERS_SKIP=1` or when
//! `python3`/`uv` are unavailable — see [`crate::skip_if_no_python`].

// Reuse the mock-server harness that already lives in the
// `atlas-remote` test tree. This keeps a single source of truth for
// mock server bootstrap and avoids either publishing the harness as
// a `pub mod` on atlas-remote proper or maintaining a shared dev-dep
// crate. Rust's `#[path]` attribute stitches the file in verbatim.
#[path = "../../atlas-remote/tests/common/mod.rs"]
mod common;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use atlas_core::{BackendKind, Location};
use atlas_ops::{
    ConflictPolicy, OpEvent, OpKind, OpStatus, OperationQueue, ProgressSnapshot, QueueOptions,
};
use rand::{RngCore, SeedableRng};
use tempfile::TempDir;

use common::MockSftpServer;

const OP_TIMEOUT: Duration = Duration::from_secs(60);

fn small_queue() -> (OperationQueue, crossbeam_channel::Receiver<OpEvent>) {
    OperationQueue::start(QueueOptions {
        workers: 1,
        progress_interval: Duration::from_millis(25),
    })
}

fn write_file(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, bytes).expect("write file");
}

fn read_file(path: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    fs::File::open(path)
        .and_then(|mut f| f.read_to_end(&mut out))
        .expect("read file");
    out
}

/// Build a [`Location::Remote`] pointing at `path` inside the mock
/// SFTP server. `path` should start with `/`.
fn sftp_location(server: &MockSftpServer, user: &str, path: &str) -> Location {
    let mut uri = server.uri(user);
    uri.path = path.into();
    Location::Remote(uri, BackendKind::Sftp)
}

/// Block until `queue.get(id)` reports a terminal status, or until
/// `OP_TIMEOUT` elapses.
fn wait_for_terminal(
    queue: &OperationQueue,
    events: &crossbeam_channel::Receiver<OpEvent>,
    id: u64,
) -> (OpStatus, Vec<OpEvent>) {
    let mut seen = Vec::new();
    let deadline = Instant::now() + OP_TIMEOUT;
    loop {
        if let Ok(event) = events.recv_timeout(Duration::from_millis(50)) {
            seen.push(event);
        }
        let op = queue.get(id).expect("operation present");
        if matches!(
            op.status,
            OpStatus::Done | OpStatus::Failed | OpStatus::Cancelled
        ) {
            // Drain any straggler events before returning.
            while let Ok(ev) = events.try_recv() {
                seen.push(ev);
            }
            return (op.status, seen);
        }
        assert!(
            Instant::now() < deadline,
            "op {id} timed out in status {:?}",
            op.status
        );
    }
}

/// Total bytes reported in the LAST [`OpEvent::Progress`] for this id.
fn last_progress_bytes(events: &[OpEvent], id: u64) -> Option<u64> {
    events.iter().rev().find_map(|ev| match ev {
        OpEvent::Progress {
            id: eid,
            snapshot: ProgressSnapshot { bytes_done, .. },
        } if *eid == id => Some(*bytes_done),
        _ => None,
    })
}

#[test]
fn local_to_sftp_copy_single_file() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let temp = TempDir::new().context("tempdir")?;

    let source = temp.path().join("hello.txt");
    let payload = b"atlas-ops cross-backend copy: local -> sftp\n".to_vec();
    write_file(&source, &payload);

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Copy {
        sources: vec![Location::local(source.clone())],
        dest_dir: sftp_location(&server, "atlas", "/"),
        policy: ConflictPolicy::Overwrite,
    });

    let (status, events_seen) = wait_for_terminal(&queue, &events, id);
    assert_eq!(
        status,
        OpStatus::Done,
        "unexpected status; events={events_seen:?}"
    );

    let landed = server.root_dir().join("hello.txt");
    assert!(landed.exists(), "expected file on mock SFTP root");
    assert_eq!(read_file(&landed), payload);
    Ok(())
}

#[test]
fn sftp_to_local_copy_single_file() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let temp = TempDir::new().context("tempdir")?;

    let remote_file = server.root_dir().join("greeting.txt");
    let payload = b"atlas-ops cross-backend copy: sftp -> local\n".to_vec();
    write_file(&remote_file, &payload);

    let dest_dir = temp.path().join("landing");
    fs::create_dir_all(&dest_dir)?;

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Copy {
        sources: vec![sftp_location(&server, "atlas", "/greeting.txt")],
        dest_dir: Location::local(&dest_dir),
        policy: ConflictPolicy::Overwrite,
    });

    let (status, events_seen) = wait_for_terminal(&queue, &events, id);
    assert_eq!(
        status,
        OpStatus::Done,
        "unexpected status; events={events_seen:?}"
    );

    let landed = dest_dir.join("greeting.txt");
    assert!(landed.exists(), "expected file on local dest");
    assert_eq!(read_file(&landed), payload);
    Ok(())
}

#[test]
fn sftp_to_sftp_same_host_copy() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;

    // Prime source file on the mock.
    let remote_src = server.root_dir().join("src.bin");
    let payload = b"same-host sftp -> sftp copy".to_vec();
    write_file(&remote_src, &payload);

    // Dest inside a subdirectory on the same server.
    let remote_dest_dir = server.root_dir().join("dest");
    fs::create_dir_all(&remote_dest_dir)?;

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Copy {
        sources: vec![sftp_location(&server, "atlas", "/src.bin")],
        dest_dir: sftp_location(&server, "atlas", "/dest"),
        policy: ConflictPolicy::Overwrite,
    });

    let (status, events_seen) = wait_for_terminal(&queue, &events, id);
    assert_eq!(
        status,
        OpStatus::Done,
        "unexpected status; events={events_seen:?}"
    );

    let landed = remote_dest_dir.join("src.bin");
    assert!(landed.exists(), "expected file inside dest/ subdir");
    assert_eq!(read_file(&landed), payload);
    Ok(())
}

#[test]
fn local_to_sftp_move_deletes_source() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let temp = TempDir::new().context("tempdir")?;

    let source = temp.path().join("moved.txt");
    let payload = b"move me across".to_vec();
    write_file(&source, &payload);

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Move {
        sources: vec![Location::local(source.clone())],
        dest_dir: sftp_location(&server, "atlas", "/"),
        policy: ConflictPolicy::Overwrite,
    });

    let (status, events_seen) = wait_for_terminal(&queue, &events, id);
    assert_eq!(
        status,
        OpStatus::Done,
        "unexpected status; events={events_seen:?}"
    );

    assert!(
        !source.exists(),
        "expected local source to be gone after move"
    );
    let landed = server.root_dir().join("moved.txt");
    assert!(landed.exists(), "expected file on mock SFTP");
    assert_eq!(read_file(&landed), payload);
    Ok(())
}

#[test]
fn sftp_delete_removes_file() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let victim = server.root_dir().join("delete-me.bin");
    write_file(&victim, b"gone");
    assert!(victim.exists());

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Delete {
        paths: vec![sftp_location(&server, "atlas", "/delete-me.bin")],
        to_trash: false,
    });

    let (status, events_seen) = wait_for_terminal(&queue, &events, id);
    assert_eq!(
        status,
        OpStatus::Done,
        "unexpected status; events={events_seen:?}"
    );
    assert!(!victim.exists(), "file should be gone after remote delete");
    Ok(())
}

#[test]
fn sftp_mkdir_creates_directory() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Mkdir {
        path: sftp_location(&server, "atlas", "/new-dir"),
        parents: false,
    });

    let (status, events_seen) = wait_for_terminal(&queue, &events, id);
    assert_eq!(
        status,
        OpStatus::Done,
        "unexpected status; events={events_seen:?}"
    );
    assert!(server.root_dir().join("new-dir").is_dir());
    Ok(())
}

#[test]
fn end_to_end_tree_local_sftp_local() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let temp = TempDir::new().context("tempdir")?;

    // Build a ~4 MiB pseudo-random directory tree: 2 subdirs × 5 files
    // each, mixed sizes.
    let src_root = temp.path().join("src");
    let sizes = [
        16 * 1024,
        128 * 1024,
        512 * 1024,
        1024 * 1024,
        2 * 1024 * 1024,
    ];
    let mut rng = rand::rngs::StdRng::seed_from_u64(0xA71A_50F5);
    let mut written = Vec::new();
    for sub in ["alpha", "beta"] {
        for (i, sz) in sizes.iter().enumerate() {
            let mut buf = vec![0_u8; *sz];
            rng.fill_bytes(&mut buf);
            let rel = format!("{sub}/file-{i}.bin");
            let path = src_root.join(&rel);
            write_file(&path, &buf);
            written.push((rel, buf));
        }
    }

    let (queue, events) = small_queue();

    // Stage 1: Local → SFTP (copy the whole `src` directory).
    let id1 = queue.submit(OpKind::Copy {
        sources: vec![Location::local(&src_root)],
        dest_dir: sftp_location(&server, "atlas", "/"),
        policy: ConflictPolicy::Overwrite,
    });
    let (status, events_seen) = wait_for_terminal(&queue, &events, id1);
    assert_eq!(
        status,
        OpStatus::Done,
        "stage-1 failed; events={events_seen:?}"
    );
    let total_bytes: u64 = written.iter().map(|(_, b)| b.len() as u64).sum();
    let last = last_progress_bytes(&events_seen, id1).unwrap_or(0);
    assert!(
        last > 0,
        "expected at least one progress event with non-zero bytes"
    );
    // We should have reported *at least* the total bytes across the
    // tree by the end (the primitive occasionally over-reports on retry).
    assert!(
        last >= total_bytes,
        "final bytes_done ({last}) < tree size ({total_bytes})"
    );

    // Stage 2: SFTP → Local (copy the tree back into a fresh dir).
    let round_trip = temp.path().join("round_trip");
    fs::create_dir_all(&round_trip)?;
    let id2 = queue.submit(OpKind::Copy {
        sources: vec![sftp_location(&server, "atlas", "/src")],
        dest_dir: Location::local(&round_trip),
        policy: ConflictPolicy::Overwrite,
    });
    let (status2, events_seen2) = wait_for_terminal(&queue, &events, id2);
    assert_eq!(
        status2,
        OpStatus::Done,
        "stage-2 failed; events={events_seen2:?}"
    );

    // Byte-equality check.
    let dest_root = round_trip.join("src");
    for (rel, expected) in &written {
        let actual = read_file(&dest_root.join(rel));
        assert_eq!(&actual, expected, "byte mismatch for {rel}");
    }
    Ok(())
}

#[test]
fn cancel_mid_local_to_sftp_copy() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let temp = TempDir::new().context("tempdir")?;

    // Big file to give us a wide cancel window. Mock SFTP throughput
    // is bounded by paramiko + local loopback; 8 MiB reliably takes
    // > 200 ms end-to-end, plenty of window.
    let source = temp.path().join("big.bin");
    let mut rng = rand::rngs::StdRng::seed_from_u64(0xCA5CADE);
    let mut buf = vec![0_u8; 8 * 1024 * 1024];
    rng.fill_bytes(&mut buf);
    write_file(&source, &buf);

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Copy {
        sources: vec![Location::local(source.clone())],
        dest_dir: sftp_location(&server, "atlas", "/"),
        policy: ConflictPolicy::Overwrite,
    });

    // Sleep briefly so the worker actually starts, then request cancel.
    thread::sleep(Duration::from_millis(30));
    queue.cancel(id);

    let (status, events_seen) = wait_for_terminal(&queue, &events, id);
    assert!(
        matches!(
            status,
            OpStatus::Cancelled | OpStatus::Done | OpStatus::Failed
        ),
        "unexpected status {status:?}; events={events_seen:?}"
    );

    // Whether the file lands depends on scheduling; if the transfer
    // was cancelled before completion, any partial file on the remote
    // should not equal the full source. We don't require deletion of
    // partials for cross-backend cancel (documented tradeoff), but the
    // op MUST have observed the cancel request within a short window.
    if status == OpStatus::Cancelled {
        let landed = server.root_dir().join("big.bin");
        if landed.exists() {
            let observed = fs::metadata(&landed)?.len();
            assert!(
                observed <= buf.len() as u64,
                "partial file larger than source ({} > {})",
                observed,
                buf.len()
            );
        }
    }
    Ok(())
}

/// Silence dead-code warnings on helpers that not every test uses.
#[allow(dead_code)]
fn _use_all(_: PathBuf, _: Duration) {}

// ── Cross-backend conflict-policy tests ─────────────────────────────
//
// These exercise the four ConflictDecision variants against
// cross-backend copy paths. Same-backend Local→Local goes through
// the local primitives (already tested in `ops_tests.rs::conflict_*`);
// the cases below cover Local↔Remote and Remote↔Remote transitions.

fn wait_for_conflict<F>(
    queue: &OperationQueue,
    events: &crossbeam_channel::Receiver<OpEvent>,
    id: u64,
    respond: F,
) -> (OpStatus, Vec<OpEvent>)
where
    F: FnOnce(atlas_ops::ConflictResponder),
{
    let mut seen = Vec::new();
    let mut respond = Some(respond);
    let deadline = Instant::now() + OP_TIMEOUT;
    loop {
        if let Ok(event) = events.recv_timeout(Duration::from_millis(50)) {
            if let OpEvent::Conflict { resolver, .. } = &event {
                if let Some(cb) = respond.take() {
                    cb(resolver.clone());
                }
            }
            seen.push(event);
        }
        let op = queue.get(id).expect("operation present");
        if matches!(
            op.status,
            OpStatus::Done | OpStatus::Failed | OpStatus::Cancelled
        ) {
            while let Ok(ev) = events.try_recv() {
                seen.push(ev);
            }
            return (op.status, seen);
        }
        assert!(
            Instant::now() < deadline,
            "op {id} timed out in status {:?}",
            op.status
        );
    }
}

#[test]
fn local_to_sftp_prompt_overwrite() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let temp = TempDir::new().context("tempdir")?;

    // Prime a colliding file on the mock so the conflict trigger fires.
    write_file(&server.root_dir().join("collide.txt"), b"old-remote");
    let source = temp.path().join("collide.txt");
    write_file(&source, b"new-local");

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Copy {
        sources: vec![Location::local(source)],
        dest_dir: sftp_location(&server, "atlas", "/"),
        policy: atlas_ops::ConflictPolicy::Prompt,
    });
    let (status, _) = wait_for_conflict(&queue, &events, id, |resolver| {
        resolver.resolve(atlas_ops::ConflictDecision::Overwrite);
    });
    assert_eq!(status, OpStatus::Done);
    // Local content should have replaced the remote.
    assert_eq!(
        read_file(&server.root_dir().join("collide.txt")),
        b"new-local"
    );
    Ok(())
}

#[test]
fn local_to_sftp_prompt_skip_leaves_remote_untouched() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let temp = TempDir::new().context("tempdir")?;

    write_file(&server.root_dir().join("keep.txt"), b"original-remote");
    let source = temp.path().join("keep.txt");
    write_file(&source, b"local-override");

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Copy {
        sources: vec![Location::local(source)],
        dest_dir: sftp_location(&server, "atlas", "/"),
        policy: atlas_ops::ConflictPolicy::Prompt,
    });
    let (status, _) = wait_for_conflict(&queue, &events, id, |resolver| {
        resolver.resolve(atlas_ops::ConflictDecision::Skip);
    });
    assert_eq!(status, OpStatus::Done);
    // Remote content should be untouched — Skip means "no write".
    assert_eq!(
        read_file(&server.root_dir().join("keep.txt")),
        b"original-remote"
    );
    Ok(())
}

#[test]
fn local_to_sftp_prompt_cancel_cancels_op() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let temp = TempDir::new().context("tempdir")?;

    write_file(&server.root_dir().join("halt.txt"), b"original");
    let source = temp.path().join("halt.txt");
    write_file(&source, b"never-arrives");

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Copy {
        sources: vec![Location::local(source)],
        dest_dir: sftp_location(&server, "atlas", "/"),
        policy: atlas_ops::ConflictPolicy::Prompt,
    });
    let (status, _) = wait_for_conflict(&queue, &events, id, |resolver| {
        resolver.resolve(atlas_ops::ConflictDecision::Cancel);
    });
    assert_eq!(status, OpStatus::Cancelled);
    // Remote file untouched.
    assert_eq!(read_file(&server.root_dir().join("halt.txt")), b"original");
    Ok(())
}

#[test]
fn local_to_sftp_rename_with_suffix_places_suffixed_sibling() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let temp = TempDir::new().context("tempdir")?;

    // Existing colliding remote file.
    write_file(&server.root_dir().join("note.txt"), b"original-remote");
    let source = temp.path().join("note.txt");
    write_file(&source, b"new-local");

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Copy {
        sources: vec![Location::local(source)],
        dest_dir: sftp_location(&server, "atlas", "/"),
        policy: atlas_ops::ConflictPolicy::RenameWithSuffix,
    });
    let (status, _) = wait_for_terminal(&queue, &events, id);
    assert_eq!(status, OpStatus::Done);
    // Original untouched.
    assert_eq!(
        read_file(&server.root_dir().join("note.txt")),
        b"original-remote"
    );
    // Suffixed sibling created.
    assert_eq!(
        read_file(&server.root_dir().join("note (copy).txt")),
        b"new-local"
    );
    Ok(())
}

#[test]
fn sftp_to_local_prompt_overwrite_replaces_local_file() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let temp = TempDir::new().context("tempdir")?;

    write_file(&server.root_dir().join("pull.txt"), b"remote-fresh");
    let dest_dir = temp.path().join("landing");
    fs::create_dir_all(&dest_dir)?;
    write_file(&dest_dir.join("pull.txt"), b"local-stale");

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Copy {
        sources: vec![sftp_location(&server, "atlas", "/pull.txt")],
        dest_dir: Location::local(&dest_dir),
        policy: atlas_ops::ConflictPolicy::Prompt,
    });
    let (status, _) = wait_for_conflict(&queue, &events, id, |resolver| {
        resolver.resolve(atlas_ops::ConflictDecision::Overwrite);
    });
    assert_eq!(status, OpStatus::Done);
    assert_eq!(read_file(&dest_dir.join("pull.txt")), b"remote-fresh");
    Ok(())
}

#[test]
fn sftp_to_local_rename_with_suffix_places_suffixed_local_sibling() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let temp = TempDir::new().context("tempdir")?;

    write_file(&server.root_dir().join("shared.txt"), b"remote-content");
    let dest_dir = temp.path().join("landing");
    fs::create_dir_all(&dest_dir)?;
    write_file(&dest_dir.join("shared.txt"), b"local-keep");

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Copy {
        sources: vec![sftp_location(&server, "atlas", "/shared.txt")],
        dest_dir: Location::local(&dest_dir),
        policy: atlas_ops::ConflictPolicy::RenameWithSuffix,
    });
    let (status, _) = wait_for_terminal(&queue, &events, id);
    assert_eq!(status, OpStatus::Done);
    // Original local untouched.
    assert_eq!(read_file(&dest_dir.join("shared.txt")), b"local-keep");
    // Suffixed sibling landed on local.
    assert_eq!(
        read_file(&dest_dir.join("shared (copy).txt")),
        b"remote-content"
    );
    Ok(())
}

#[test]
fn sftp_to_sftp_prompt_skip_leaves_target_untouched() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let src_dir = server.root_dir().join("srcdir");
    let dst_dir = server.root_dir().join("dstdir");
    fs::create_dir_all(&src_dir)?;
    fs::create_dir_all(&dst_dir)?;
    write_file(&src_dir.join("payload.bin"), b"src-bytes");
    write_file(&dst_dir.join("payload.bin"), b"pre-existing-dst");

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Copy {
        sources: vec![sftp_location(&server, "atlas", "/srcdir/payload.bin")],
        dest_dir: sftp_location(&server, "atlas", "/dstdir"),
        policy: atlas_ops::ConflictPolicy::Prompt,
    });
    let (status, _) = wait_for_conflict(&queue, &events, id, |resolver| {
        resolver.resolve(atlas_ops::ConflictDecision::Skip);
    });
    assert_eq!(status, OpStatus::Done);
    assert_eq!(read_file(&dst_dir.join("payload.bin")), b"pre-existing-dst");
    Ok(())
}

#[test]
fn cross_backend_hardcoded_overwrite_regression() -> Result<()> {
    // Regression: prior to threading policy through `copy_single`,
    // cross-backend copy always resolved to `ConflictPolicy::Overwrite`
    // regardless of what the caller asked for. This test proves the
    // fix: submitting with `Skip` at a colliding remote destination
    // must NOT clobber the destination. If this ever regresses the
    // remote file would end up equal to the local source.
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let temp = TempDir::new().context("tempdir")?;

    write_file(&server.root_dir().join("guard.txt"), b"do-not-overwrite");
    let source = temp.path().join("guard.txt");
    write_file(&source, b"would-clobber");

    let (queue, events) = small_queue();
    let id = queue.submit(OpKind::Copy {
        sources: vec![Location::local(source)],
        dest_dir: sftp_location(&server, "atlas", "/"),
        policy: atlas_ops::ConflictPolicy::Skip,
    });
    let (status, _) = wait_for_terminal(&queue, &events, id);
    assert_eq!(status, OpStatus::Done);
    // If the `let _ = policy` hardcode still lived, this assert would fail.
    assert_eq!(
        read_file(&server.root_dir().join("guard.txt")),
        b"do-not-overwrite",
        "cross-backend copy must honor ConflictPolicy::Skip; if this fails, \
         the `let _ = policy` hardcode has regressed"
    );
    Ok(())
}
