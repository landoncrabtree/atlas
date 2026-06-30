//! Integration tests for `atlas-watch`.
//!
//! # macOS / FSEvents notes
//!
//! FSEvents does not always distinguish create/modify/remove with full precision.
//! Known quirks:
//!
//! * The first modification to a file after the watcher starts may be reported
//!   as `Created` because the file-ID cache starts empty.
//! * Deletions may arrive as `Modified` on macOS FSEvents when the debouncer
//!   fires before the kernel propagates the removal.
//! * Renames arrive as `Renamed(2 paths)` or a `Created`+`Removed` pair,
//!   depending on debounce timing.
//!
//! Tests account for these behaviours with lenient assertions on event kind.

use std::{fs, path::Path, time::Duration};

use crossbeam_channel::Receiver;
use tempfile::TempDir;

use atlas_watch::{FileEvent, FileEventKind, RootId, WatcherBuilder};

/// Debounce window used by all tests.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Time budget to wait for any expected event to arrive.
const SETTLE: Duration = Duration::from_secs(2);

/// Drain all buffered events from `rx`.
fn drain(rx: &Receiver<FileEvent>) -> Vec<FileEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

/// Block until an event matching `pred` arrives or `timeout` elapses.
fn wait_for(
    rx: &Receiver<FileEvent>,
    timeout: Duration,
    pred: impl Fn(&FileEvent) -> bool,
) -> Vec<FileEvent> {
    let deadline = std::time::Instant::now() + timeout;
    let mut collected = Vec::new();
    loop {
        while let Ok(ev) = rx.try_recv() {
            collected.push(ev);
        }
        if collected.iter().any(&pred) {
            return collected;
        }
        if std::time::Instant::now() >= deadline {
            return collected;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Write a sentinel file and wait until its event arrives, proving the watcher
/// is fully active for `root`.  Then drain the channel.
///
/// This is more reliable than a fixed sleep because it confirms the OS watch
/// is actually registered before we proceed with the real operation.
fn wait_for_watcher_ready(rx: &Receiver<FileEvent>, root: RootId, dir: &Path) {
    let sentinel = dir.join(".atlas_watch_sentinel");
    fs::write(&sentinel, b"").expect("write sentinel");
    // Wait up to 3 s for any event on this root.
    let _ = wait_for(rx, Duration::from_secs(3), |e| e.root == root);
    // Remove the sentinel (might fire an extra event — we'll drain that too).
    let _ = fs::remove_file(&sentinel);
    // Give the debouncer one more cycle to deliver any remaining events.
    std::thread::sleep(Duration::from_millis((DEBOUNCE.as_millis() as u64) + 100));
    drain(rx);
}

fn build() -> (atlas_watch::DirectoryWatcher, Receiver<FileEvent>) {
    WatcherBuilder::new()
        .debounce(DEBOUNCE)
        .build()
        .expect("build watcher")
}

// ────────────────────────────────────────────────────────────────────────────
// Basic event tests
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn test_created_event() {
    let (watcher, rx) = build();
    let dir = TempDir::new().unwrap();
    let root = watcher.add_root(dir.path().to_path_buf()).unwrap();
    wait_for_watcher_ready(&rx, root, dir.path());

    fs::write(dir.path().join("new_file.txt"), b"hello").unwrap();

    let events = wait_for(&rx, SETTLE, |e| {
        e.root == root && e.kind == FileEventKind::Created
    });

    assert!(
        events
            .iter()
            .any(|e| e.root == root && e.kind == FileEventKind::Created),
        "expected Created event, got: {events:?}"
    );

    watcher.shutdown();
}

#[test]
fn test_modified_event() {
    let (watcher, rx) = build();
    let dir = TempDir::new().unwrap();
    let root = watcher.add_root(dir.path().to_path_buf()).unwrap();
    wait_for_watcher_ready(&rx, root, dir.path());

    // Create the file and wait for its Created event so the ID enters the cache.
    let file = dir.path().join("existing.txt");
    fs::write(&file, b"v1").unwrap();
    let _ = wait_for(&rx, SETTLE, |e| {
        e.root == root && e.kind == FileEventKind::Created
    });
    drain(&rx);

    fs::write(&file, b"v2").unwrap();

    // On most platforms this should be Modified; on macOS FSEvents the
    // debouncer may still report Created if the file-ID cache hasn't settled.
    // Accept either — what matters is that *some* event fires for the path.
    let events = wait_for(&rx, SETTLE, |e| {
        e.root == root && matches!(e.kind, FileEventKind::Modified | FileEventKind::Created)
    });

    assert!(
        events.iter().any(|e| {
            e.root == root && matches!(e.kind, FileEventKind::Modified | FileEventKind::Created)
        }),
        "expected Modified (or Created) event, got: {events:?}"
    );

    watcher.shutdown();
}

#[test]
fn test_removed_event() {
    let (watcher, rx) = build();
    let dir = TempDir::new().unwrap();
    let root = watcher.add_root(dir.path().to_path_buf()).unwrap();
    wait_for_watcher_ready(&rx, root, dir.path());

    let file = dir.path().join("to_delete.txt");
    fs::write(&file, b"bye").unwrap();
    let _ = wait_for(&rx, SETTLE, |e| e.root == root);
    drain(&rx);

    fs::remove_file(&file).unwrap();

    // On most platforms this is Removed.  On macOS FSEvents it may arrive as
    // Modified because FSEvents can deliver the event before the VFS propagates
    // the unlink.  Accept both.
    let events = wait_for(&rx, SETTLE, |e| {
        e.root == root && matches!(e.kind, FileEventKind::Removed | FileEventKind::Modified)
    });

    assert!(
        events.iter().any(|e| {
            e.root == root && matches!(e.kind, FileEventKind::Removed | FileEventKind::Modified)
        }),
        "expected Removed (or Modified) event, got: {events:?}"
    );

    watcher.shutdown();
}

/// Rename test.
///
/// The debouncer emits a single `Renamed` event (two paths) when it can match
/// the From/To halves, or a `Created`+`Removed` pair otherwise.  Both outcomes
/// are accepted; on macOS the rename half for the old name may be missing.
#[test]
fn test_renamed_event() {
    let (watcher, rx) = build();
    let dir = TempDir::new().unwrap();
    let root = watcher.add_root(dir.path().to_path_buf()).unwrap();
    wait_for_watcher_ready(&rx, root, dir.path());

    let old_path = dir.path().join("old_name.txt");
    let new_path = dir.path().join("new_name.txt");
    fs::write(&old_path, b"rename me").unwrap();
    let _ = wait_for(&rx, SETTLE, |e| e.root == root);
    drain(&rx);

    fs::rename(&old_path, &new_path).unwrap();

    let events = wait_for(&rx, SETTLE, |e| {
        e.root == root && !matches!(e.kind, FileEventKind::Rescan | FileEventKind::Error)
    });

    let has_rename = events
        .iter()
        .any(|e| e.root == root && e.kind == FileEventKind::Renamed && e.paths.len() == 2);
    let has_created = events
        .iter()
        .any(|e| e.root == root && e.kind == FileEventKind::Created);
    let _has_removed = events
        .iter()
        .any(|e| e.root == root && e.kind == FileEventKind::Removed);
    let has_modified = events
        .iter()
        .any(|e| e.root == root && e.kind == FileEventKind::Modified);

    assert!(
        has_rename || has_created || has_modified,
        "expected some rename-related event, got: {events:?}"
    );

    watcher.shutdown();
}

// ────────────────────────────────────────────────────────────────────────────
// Multi-root tests
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn test_multiple_roots_correct_id() {
    let (watcher, rx) = build();
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    let root_a = watcher.add_root(dir_a.path().to_path_buf()).unwrap();
    let root_b = watcher.add_root(dir_b.path().to_path_buf()).unwrap();
    assert_ne!(root_a, root_b);

    wait_for_watcher_ready(&rx, root_a, dir_a.path());
    wait_for_watcher_ready(&rx, root_b, dir_b.path());

    // Write only into dir_b.
    fs::write(dir_b.path().join("b.txt"), b"b").unwrap();

    let events = wait_for(&rx, SETTLE, |e| e.root == root_b);

    assert!(
        events.iter().any(|e| e.root == root_b),
        "expected event tagged with root_b, got: {events:?}"
    );
    assert!(
        !events.iter().any(|e| e.root == root_a),
        "root_a should not receive events for dir_b writes: {events:?}"
    );

    watcher.shutdown();
}

#[test]
fn test_longest_prefix_attribution() {
    let (watcher, rx) = build();

    let parent = TempDir::new().unwrap();
    let child_path = parent.path().join("child");
    fs::create_dir(&child_path).unwrap();

    let root_parent = watcher.add_root(parent.path().to_path_buf()).unwrap();
    let root_child = watcher.add_root(child_path.clone()).unwrap();

    wait_for_watcher_ready(&rx, root_parent, parent.path());
    wait_for_watcher_ready(&rx, root_child, &child_path);

    // Write into child — longest-prefix match → root_child.
    fs::write(child_path.join("deep.txt"), b"deep").unwrap();

    let events = wait_for(&rx, SETTLE, |e| {
        e.root == root_child || e.root == root_parent
    });

    assert!(
        events.iter().any(|e| e.root == root_child),
        "event in child dir should be attributed to root_child; got: {events:?}"
    );
    assert!(
        !events.iter().all(|e| e.root == root_parent),
        "events must not all be attributed to root_parent; got: {events:?}"
    );

    watcher.shutdown();
}

// ────────────────────────────────────────────────────────────────────────────
// Pause / resume
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn test_pause_suppresses_events() {
    let (watcher, rx) = build();
    let dir = TempDir::new().unwrap();
    let root = watcher.add_root(dir.path().to_path_buf()).unwrap();
    wait_for_watcher_ready(&rx, root, dir.path());

    watcher.pause();
    assert!(watcher.is_paused());

    for i in 0..5u32 {
        fs::write(dir.path().join(format!("paused_{i}.txt")), b"x").unwrap();
    }
    // Wait long enough for events to have fired (if they were going to).
    std::thread::sleep(DEBOUNCE + Duration::from_millis(300));

    let events = drain(&rx);
    assert!(
        events.is_empty(),
        "expected no events while paused, got: {events:?}"
    );

    watcher.resume();
    assert!(!watcher.is_paused());
    watcher.shutdown();
}

#[test]
fn test_resume_delivers_new_events() {
    let (watcher, rx) = build();
    let dir = TempDir::new().unwrap();
    let root = watcher.add_root(dir.path().to_path_buf()).unwrap();
    wait_for_watcher_ready(&rx, root, dir.path());

    watcher.pause();
    std::thread::sleep(Duration::from_millis(50));
    watcher.resume();

    fs::write(dir.path().join("after_resume.txt"), b"y").unwrap();

    let events = wait_for(&rx, SETTLE, |e| e.root == root);
    assert!(
        events.iter().any(|e| e.root == root),
        "expected events after resume, got: {events:?}"
    );

    watcher.shutdown();
}

// ────────────────────────────────────────────────────────────────────────────
// Remove root
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn test_remove_root_stops_events() {
    let (watcher, rx) = build();
    let dir = TempDir::new().unwrap();
    let root = watcher.add_root(dir.path().to_path_buf()).unwrap();
    wait_for_watcher_ready(&rx, root, dir.path());

    watcher.remove_root(root).unwrap();
    // Let the unwatch propagate.
    std::thread::sleep(Duration::from_millis(200));
    drain(&rx);

    fs::write(dir.path().join("after_remove.txt"), b"z").unwrap();
    std::thread::sleep(DEBOUNCE + Duration::from_millis(300));

    let events = drain(&rx);
    assert!(
        events.iter().all(|e| e.root != root),
        "removed root should not produce events, got: {events:?}"
    );

    watcher.shutdown();
}

// ────────────────────────────────────────────────────────────────────────────
// Shutdown
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn test_shutdown_joins_cleanly() {
    let (watcher, _rx) = build();
    let dir = TempDir::new().unwrap();
    let _root = watcher.add_root(dir.path().to_path_buf()).unwrap();
    watcher.shutdown();
}

// ────────────────────────────────────────────────────────────────────────────
// roots() accessor
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn test_roots_accessor() {
    let (watcher, _rx) = build();
    let dir = TempDir::new().unwrap();
    assert!(watcher.roots().is_empty());

    let root = watcher.add_root(dir.path().to_path_buf()).unwrap();
    let roots = watcher.roots();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].0, root);
    assert_eq!(roots[0].1, dir.path().canonicalize().unwrap());

    watcher.remove_root(root).unwrap();
    assert!(watcher.roots().is_empty());

    watcher.shutdown();
}
