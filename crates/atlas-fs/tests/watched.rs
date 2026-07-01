//! Integration tests for live-watching [`InMemoryLocationViewModel`].
//!
//! Each test uses a "sentinel write-and-poll" pattern: a uniquely named marker
//! file is created and we poll until it appears in the snapshot. This proves
//! the entire watcher pipeline (OS backend → debounce → event thread →
//! snapshot mutation) is operational before the actual assertion file is
//! created or removed. No fixed sleeps are used for event detection.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use atlas_fs::{
    Filter, InMemoryLocationViewModel, LocationViewModel, OpenOptions, SortKey, SortOrder,
    SortSpec, ViewModelEvent,
};
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────────

const TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

fn poll_until(pred: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return pred();
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_event<P>(rx: &crossbeam_channel::Receiver<ViewModelEvent>, mut pred: P) -> bool
where
    P: FnMut(&ViewModelEvent) -> bool,
{
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
            Ok(ev) => {
                if pred(&ev) {
                    return true;
                }
            }
            Err(_) => break,
        }
    }
    false
}

fn wait_for_loaded(rx: &crossbeam_channel::Receiver<ViewModelEvent>) {
    let _ = wait_for_event(rx, |ev| matches!(ev, ViewModelEvent::Loaded));
}

fn snapshot_names(vm: &InMemoryLocationViewModel) -> Vec<String> {
    vm.entries().iter().map(|e| e.name.clone()).collect()
}

/// Create a sentinel file, wait for it to appear in the VM snapshot (proving
/// the watcher pipeline is active), then remove it and wait for that removal
/// to propagate.
fn wait_for_watcher_ready(dir: &Path, vm: &InMemoryLocationViewModel) {
    let name = "__watcher_sentinel__".to_owned();
    let path = dir.join(&name);
    let deadline = Instant::now() + TIMEOUT;
    let mut counter: u64 = 0;

    while Instant::now() < deadline {
        counter += 1;
        // Re-write on every iteration so the watcher catches a create/modify
        // event even if the first write was missed during OS backend startup.
        let _ = fs::write(&path, counter.to_string().as_bytes());

        if snapshot_names(vm).contains(&name) {
            let _ = fs::remove_file(&path);
            // Wait for removal to propagate so it doesn't bleed into the test.
            let _ = poll_until(|| !snapshot_names(vm).contains(&name));
            return;
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    let _ = fs::remove_file(&path);
    panic!("watcher sentinel timed out — watcher pipeline not operational");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn watched_create_appears_in_snapshot() {
    let dir = TempDir::new().unwrap();
    let vm = InMemoryLocationViewModel::open_live(dir.path(), OpenOptions::default());
    let rx = vm.subscribe();
    wait_for_loaded(&rx);
    wait_for_watcher_ready(dir.path(), &vm);

    fs::write(dir.path().join("newfile.txt"), b"hello").unwrap();

    let appeared = poll_until(|| snapshot_names(&vm).contains(&"newfile.txt".to_owned()));
    assert!(
        appeared,
        "newfile.txt should appear in snapshot after creation"
    );
}

#[test]
fn watched_remove_disappears_from_snapshot() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("target.txt"), b"data").unwrap();

    let vm = InMemoryLocationViewModel::open_live(dir.path(), OpenOptions::default());
    let rx = vm.subscribe();
    wait_for_loaded(&rx);

    assert!(
        poll_until(|| snapshot_names(&vm).contains(&"target.txt".to_owned())),
        "target.txt should be in initial snapshot"
    );
    wait_for_watcher_ready(dir.path(), &vm);

    fs::remove_file(dir.path().join("target.txt")).unwrap();

    let gone = poll_until(|| !snapshot_names(&vm).contains(&"target.txt".to_owned()));
    assert!(
        gone,
        "target.txt should disappear from snapshot after removal"
    );
}

#[test]
fn watched_modify_updates_snapshot_metadata() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("watched.txt");
    fs::write(&path, b"v1").unwrap();

    let vm = InMemoryLocationViewModel::open_live(dir.path(), OpenOptions::default());
    let rx = vm.subscribe();
    wait_for_loaded(&rx);

    assert!(
        poll_until(|| snapshot_names(&vm).contains(&"watched.txt".to_owned())),
        "watched.txt should be in initial snapshot"
    );
    wait_for_watcher_ready(dir.path(), &vm);

    let size_before = vm
        .entries()
        .iter()
        .find(|e| e.name == "watched.txt")
        .map(|e| e.metadata.size)
        .unwrap_or(0);

    // Overwrite with substantially different content to ensure size changes.
    // Accept Created | Modified events — macOS FSEvents may deliver either.
    fs::write(&path, b"v2 with substantially more content to change size").unwrap();

    let updated = poll_until(|| {
        vm.entries()
            .iter()
            .find(|e| e.name == "watched.txt")
            .is_some_and(|e| e.metadata.size != size_before)
    });
    assert!(updated, "watched.txt size should update after modification");
}

#[test]
fn watched_filter_blocks_non_matching_creates() {
    let dir = TempDir::new().unwrap();
    let opts = OpenOptions {
        filter: Filter {
            include_globs: vec!["*.txt".to_owned()],
            ..Filter::default()
        },
        ..OpenOptions::default()
    };
    let vm = InMemoryLocationViewModel::open_live(dir.path(), opts);
    let rx = vm.subscribe();
    wait_for_loaded(&rx);

    // Use a *.txt sentinel to verify the watcher is operational.
    let sentinel_name = "__watcher_sentinel__.txt".to_owned();
    let sentinel_path = dir.path().join(&sentinel_name);
    let deadline = Instant::now() + TIMEOUT;
    let mut counter: u64 = 0;
    let sentinel_ok = loop {
        counter += 1;
        let _ = fs::write(&sentinel_path, counter.to_string().as_bytes());
        if snapshot_names(&vm).contains(&sentinel_name) {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(POLL_INTERVAL);
    };
    fs::remove_file(&sentinel_path).ok();
    let _ = poll_until(|| !snapshot_names(&vm).contains(&sentinel_name));
    assert!(sentinel_ok, "*.txt sentinel should appear through filter");

    // .log file should NOT appear (filtered out).
    fs::write(dir.path().join("debug.log"), b"log").unwrap();
    // .txt file SHOULD appear.
    fs::write(dir.path().join("notes.txt"), b"notes").unwrap();

    let txt_appeared = poll_until(|| snapshot_names(&vm).contains(&"notes.txt".to_owned()));
    assert!(
        txt_appeared,
        "notes.txt should appear (matches *.txt filter)"
    );

    // Allow extra time for any .log event to arrive and be incorrectly added.
    let extra_deadline = Instant::now() + Duration::from_millis(600);
    while Instant::now() < extra_deadline {
        std::thread::sleep(POLL_INTERVAL);
    }
    assert!(
        !snapshot_names(&vm).contains(&"debug.log".to_owned()),
        "debug.log should be blocked by the *.txt filter"
    );
}

#[test]
fn watched_sort_inserts_at_correct_position() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("aaa.txt"), b"a").unwrap();
    fs::write(dir.path().join("zzz.txt"), b"z").unwrap();

    let opts = OpenOptions {
        sort: SortSpec {
            key: SortKey::Name,
            order: SortOrder::Asc,
            dirs_first: false,
            natural: true,
            case_insensitive: true,
        },
        ..OpenOptions::default()
    };
    let vm = InMemoryLocationViewModel::open_live(dir.path(), opts);
    let rx = vm.subscribe();
    wait_for_loaded(&rx);

    assert!(poll_until(|| vm.len() >= 2), "initial entries should load");
    wait_for_watcher_ready(dir.path(), &vm);

    fs::write(dir.path().join("mmm.txt"), b"m").unwrap();

    let sorted_correctly = poll_until(|| {
        let entries = vm.entries();
        if entries.len() < 3 {
            return false;
        }
        let pos_aaa = entries.iter().position(|e| e.name == "aaa.txt");
        let pos_mmm = entries.iter().position(|e| e.name == "mmm.txt");
        let pos_zzz = entries.iter().position(|e| e.name == "zzz.txt");
        matches!((pos_aaa, pos_mmm, pos_zzz), (Some(a), Some(m), Some(z)) if a < m && m < z)
    });
    assert!(
        sorted_correctly,
        "mmm.txt should be inserted between aaa and zzz"
    );
}

#[test]
fn watched_no_watch_behaves_like_before() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("existing.txt"), b"data").unwrap();

    let vm = InMemoryLocationViewModel::open(dir.path(), OpenOptions::default());
    let rx = vm.subscribe();
    wait_for_loaded(&rx);

    assert!(poll_until(|| vm.len() >= 1), "existing entry should load");
    assert_eq!(vm.location(), dir.path() as &Path);
    assert!(snapshot_names(&vm).contains(&"existing.txt".to_owned()));
    assert!(!vm.is_watching(), "watch=false should not attach a watcher");
}

#[test]
fn watched_drop_stops_cleanly() {
    let dir = TempDir::new().unwrap();
    let vm = InMemoryLocationViewModel::open_live(dir.path(), OpenOptions::default());
    let rx = vm.subscribe();
    wait_for_loaded(&rx);
    drop(vm);
    // Test finishes without hanging — that proves the watcher thread exited cleanly.
    drop(dir);
}
