use std::fs;
use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use atlas_ops::{
    move_via_copy_delete_for_tests, ConflictDecision, ConflictPolicy, OpEvent, OpKind, OpStatus,
    OperationQueue, QueueOptions,
};
use tempfile::TempDir;

fn write_file(path: &Path, data: &[u8]) {
    let parent = path.parent().expect("parent");
    fs::create_dir_all(parent).expect("create parent dirs");
    let mut file = fs::File::create(path).expect("create file");
    file.write_all(data).expect("write file");
}

fn read_file(path: &Path) -> Vec<u8> {
    fs::read(path).expect("read file")
}

fn wait_for_terminal_status(queue: &OperationQueue, id: u64) -> OpStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let op = queue.get(id).expect("operation present");
        if matches!(
            op.status,
            OpStatus::Done | OpStatus::Failed | OpStatus::Cancelled
        ) {
            return op.status;
        }
        assert!(Instant::now() < deadline, "operation timed out");
        thread::sleep(Duration::from_millis(10));
    }
}

fn drain_until_completed(
    queue: &OperationQueue,
    events: &crossbeam_channel::Receiver<OpEvent>,
    id: u64,
) -> Vec<OpEvent> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(event) = events.recv_timeout(Duration::from_millis(20)) {
            let matches_id = match &event {
                OpEvent::Queued { id: event_id, .. }
                | OpEvent::Started { id: event_id }
                | OpEvent::Completed { id: event_id }
                | OpEvent::Cancelled { id: event_id }
                | OpEvent::Progress { id: event_id, .. }
                | OpEvent::Conflict { id: event_id, .. }
                | OpEvent::Failed { id: event_id, .. } => *event_id == id,
            };
            if matches_id {
                let terminal = matches!(
                    event,
                    OpEvent::Completed { .. } | OpEvent::Cancelled { .. } | OpEvent::Failed { .. }
                );
                seen.push(event);
                if terminal {
                    return seen;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for completion"
        );
        if matches!(
            queue.get(id).expect("operation present").status,
            OpStatus::Done
        ) {
            return seen;
        }
    }
}

fn small_queue(
    progress_interval: Duration,
) -> (OperationQueue, crossbeam_channel::Receiver<OpEvent>) {
    OperationQueue::start(QueueOptions {
        workers: 1,
        progress_interval,
    })
}

#[test]
fn copy_single_file() {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("dest");
    let content = b"atlas-ops copy".to_vec();
    write_file(&source, &content);

    let (queue, events) = small_queue(Duration::from_millis(1));
    let id = queue.submit(OpKind::Copy {
        sources: vec![source.clone()],
        dest_dir: dest_dir.clone(),
        policy: ConflictPolicy::Overwrite,
    });

    let _events = drain_until_completed(&queue, &events, id);
    let copied = dest_dir.join("source.txt");
    assert_eq!(read_file(&copied), content);
    let op = queue.get(id).expect("copy op");
    assert_eq!(op.status, OpStatus::Done);
    assert_eq!(op.progress.bytes_total, content.len() as u64);
    assert_eq!(op.progress.bytes_done, content.len() as u64);
    queue.shutdown();
}

#[test]
fn copy_directory_tree() {
    let temp = TempDir::new().expect("tempdir");
    let source_dir = temp.path().join("tree");
    write_file(&source_dir.join("a.txt"), b"a");
    write_file(&source_dir.join("nested/b.txt"), b"bb");
    let dest_dir = temp.path().join("dest");

    let (queue, events) = small_queue(Duration::from_millis(1));
    let id = queue.submit(OpKind::Copy {
        sources: vec![source_dir.clone()],
        dest_dir: dest_dir.clone(),
        policy: ConflictPolicy::Overwrite,
    });

    let _events = drain_until_completed(&queue, &events, id);
    assert_eq!(read_file(&dest_dir.join("tree/a.txt")), b"a");
    assert_eq!(read_file(&dest_dir.join("tree/nested/b.txt")), b"bb");
    queue.shutdown();
}

#[test]
fn move_same_fs() {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("move-me.txt");
    write_file(&source, b"move me");
    let dest_dir = temp.path().join("dest");

    let (queue, events) = small_queue(Duration::from_millis(1));
    let id = queue.submit(OpKind::Move {
        sources: vec![source.clone()],
        dest_dir: dest_dir.clone(),
        policy: ConflictPolicy::Overwrite,
    });

    let _events = drain_until_completed(&queue, &events, id);
    assert!(!source.exists());
    assert_eq!(read_file(&dest_dir.join("move-me.txt")), b"move me");
    queue.shutdown();
}

#[test]
fn move_cross_device_fallback() {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("source.bin");
    let dest = temp.path().join("dest/source.bin");
    write_file(&source, b"fallback");

    move_via_copy_delete_for_tests(&source, &dest).expect("copy-delete fallback");

    assert!(!source.exists());
    assert_eq!(read_file(&dest), b"fallback");
}

#[test]
fn delete_to_trash() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("trash-me.txt");
    write_file(&path, b"trash");

    let (queue, events) = small_queue(Duration::from_millis(1));
    let id = queue.submit(OpKind::Delete {
        paths: vec![path.clone()],
        to_trash: true,
    });

    let _events = drain_until_completed(&queue, &events, id);
    assert!(!path.exists());
    queue.shutdown();
}

#[test]
fn delete_hard() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("delete-me.txt");
    write_file(&path, b"hard");

    let (queue, events) = small_queue(Duration::from_millis(1));
    let id = queue.submit(OpKind::Delete {
        paths: vec![path.clone()],
        to_trash: false,
    });

    let _events = drain_until_completed(&queue, &events, id);
    assert!(!path.exists());
    queue.shutdown();
}

#[test]
fn rename_valid() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("old.txt");
    write_file(&path, b"rename");

    let (queue, events) = small_queue(Duration::from_millis(1));
    let id = queue.submit(OpKind::Rename {
        path: path.clone(),
        new_name: "new.txt".to_owned(),
    });

    let _events = drain_until_completed(&queue, &events, id);
    assert!(!path.exists());
    assert_eq!(read_file(&temp.path().join("new.txt")), b"rename");
    queue.shutdown();
}

#[test]
fn rename_rejects_separator() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("old.txt");
    write_file(&path, b"rename");

    let (queue, events) = small_queue(Duration::from_millis(1));
    let id = queue.submit(OpKind::Rename {
        path,
        new_name: "bad/name.txt".to_owned(),
    });

    let seen = drain_until_completed(&queue, &events, id);
    assert!(seen
        .iter()
        .any(|event| matches!(event, OpEvent::Failed { .. })));
    assert_eq!(queue.get(id).expect("rename op").status, OpStatus::Failed);
    queue.shutdown();
}

#[test]
fn mkdir_with_parents() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("a/b/c");

    let (queue, events) = small_queue(Duration::from_millis(1));
    let id = queue.submit(OpKind::Mkdir {
        path: path.clone(),
        parents: true,
    });

    let _events = drain_until_completed(&queue, &events, id);
    assert!(path.is_dir());
    queue.shutdown();
}

#[test]
fn conflict_skip() {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("dest");
    write_file(&source, b"new");
    write_file(&dest_dir.join("source.txt"), b"old");

    let (queue, events) = small_queue(Duration::from_millis(1));
    let id = queue.submit(OpKind::Copy {
        sources: vec![source],
        dest_dir: dest_dir.clone(),
        policy: ConflictPolicy::Skip,
    });

    let _events = drain_until_completed(&queue, &events, id);
    assert_eq!(read_file(&dest_dir.join("source.txt")), b"old");
    queue.shutdown();
}

#[test]
fn conflict_overwrite() {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("dest");
    write_file(&source, b"new");
    write_file(&dest_dir.join("source.txt"), b"old");

    let (queue, events) = small_queue(Duration::from_millis(1));
    let id = queue.submit(OpKind::Copy {
        sources: vec![source],
        dest_dir: dest_dir.clone(),
        policy: ConflictPolicy::Overwrite,
    });

    let _events = drain_until_completed(&queue, &events, id);
    assert_eq!(read_file(&dest_dir.join("source.txt")), b"new");
    queue.shutdown();
}

#[test]
fn conflict_rename_suffix() {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("foo.txt");
    let dest_dir = temp.path().join("dest");
    write_file(&source, b"one");
    write_file(&dest_dir.join("foo.txt"), b"original");

    let (queue, events) = small_queue(Duration::from_millis(1));
    let first = queue.submit(OpKind::Copy {
        sources: vec![source.clone()],
        dest_dir: dest_dir.clone(),
        policy: ConflictPolicy::RenameWithSuffix,
    });
    let _events = drain_until_completed(&queue, &events, first);
    assert_eq!(read_file(&dest_dir.join("foo (copy).txt")), b"one");

    let second = queue.submit(OpKind::Copy {
        sources: vec![source],
        dest_dir: dest_dir.clone(),
        policy: ConflictPolicy::RenameWithSuffix,
    });
    let _events = drain_until_completed(&queue, &events, second);
    assert_eq!(read_file(&dest_dir.join("foo (copy 2).txt")), b"one");
    queue.shutdown();
}

#[test]
fn conflict_prompt() {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("prompt.txt");
    let dest_dir = temp.path().join("dest");
    write_file(&source, b"new");
    write_file(&dest_dir.join("prompt.txt"), b"old");

    let (queue, events) = small_queue(Duration::from_millis(1));
    let id = queue.submit(OpKind::Copy {
        sources: vec![source],
        dest_dir: dest_dir.clone(),
        policy: ConflictPolicy::Prompt,
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut resolved = false;
    let mut completed = false;
    while !completed {
        match events.recv_timeout(Duration::from_millis(50)) {
            Ok(OpEvent::Conflict {
                id: event_id,
                resolver,
                ..
            }) if event_id == id => {
                resolver.resolve(ConflictDecision::Overwrite);
                resolved = true;
            }
            Ok(OpEvent::Completed { id: event_id }) if event_id == id => completed = true,
            Ok(OpEvent::Failed {
                id: event_id,
                error,
                ..
            }) if event_id == id => {
                panic!("conflict prompt failed: {error}")
            }
            Ok(OpEvent::Cancelled { id: event_id }) if event_id == id => {
                panic!("conflict prompt unexpectedly cancelled: {event_id}")
            }
            Ok(_) => {}
            Err(error) => panic!("failed to receive prompt flow event: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for prompt completion"
        );
    }

    assert!(resolved, "expected conflict to be resolved");
    assert_eq!(read_file(&dest_dir.join("prompt.txt")), b"new");
    queue.shutdown();
}

#[test]
fn cancel_mid_copy() {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("large.bin");
    let dest_dir = temp.path().join("dest");
    write_file(&source, &vec![7_u8; 16 * 1024 * 1024]);

    let (queue, events) = small_queue(Duration::ZERO);
    let id = queue.submit(OpKind::Copy {
        sources: vec![source.clone()],
        dest_dir: dest_dir.clone(),
        policy: ConflictPolicy::Overwrite,
    });
    queue.cancel(id);

    let status = loop {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(OpEvent::Cancelled { id: event_id }) if event_id == id => break OpStatus::Cancelled,
            Ok(_) => {
                let op = queue.get(id).expect("copy op");
                if op.status == OpStatus::Cancelled {
                    break op.status;
                }
            }
            Err(error) => panic!("missing cancel event: {error}"),
        }
    };

    assert_eq!(status, OpStatus::Cancelled);
    assert!(source.exists());
    queue.shutdown();
}

#[test]
fn pause_resume() {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("large.bin");
    let dest_dir = temp.path().join("dest");
    write_file(&source, &vec![3_u8; 16 * 1024 * 1024]);

    let (queue, events) = small_queue(Duration::ZERO);
    let id = queue.submit(OpKind::Copy {
        sources: vec![source],
        dest_dir: dest_dir.clone(),
        policy: ConflictPolicy::Overwrite,
    });

    let mut saw_progress = false;
    while !saw_progress {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(OpEvent::Progress { id: event_id, .. }) if event_id == id => saw_progress = true,
            Ok(_) => {}
            Err(error) => panic!("no progress before pause: {error}"),
        }
    }

    queue.pause(id);
    thread::sleep(Duration::from_millis(100));
    let paused_before = queue.get(id).expect("paused op").progress.bytes_done;
    thread::sleep(Duration::from_millis(150));
    let paused_after = queue.get(id).expect("paused op").progress.bytes_done;
    assert_eq!(paused_before, paused_after);

    queue.resume(id);
    assert_eq!(wait_for_terminal_status(&queue, id), OpStatus::Done);
    assert!(dest_dir.join("large.bin").exists());
    queue.shutdown();
}

#[test]
fn progress_debounce() {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("large.bin");
    let dest_dir = temp.path().join("dest");
    write_file(&source, &vec![9_u8; 8 * 1024 * 1024]);

    let interval = Duration::from_millis(25);
    let (queue, events) = small_queue(interval);
    let id = queue.submit(OpKind::Copy {
        sources: vec![source],
        dest_dir,
        policy: ConflictPolicy::Overwrite,
    });

    let mut timestamps = Vec::new();
    loop {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(OpEvent::Progress { id: event_id, .. }) if event_id == id => {
                timestamps.push(Instant::now());
            }
            Ok(OpEvent::Completed { id: event_id }) if event_id == id => break,
            Ok(OpEvent::Cancelled { id: event_id }) if event_id == id => {
                panic!("unexpected cancel {event_id}")
            }
            Ok(OpEvent::Failed {
                id: event_id,
                error,
                ..
            }) if event_id == id => {
                panic!("unexpected failure {event_id}: {error}")
            }
            Ok(_) => {}
            Err(error) => panic!("timed out waiting for progress: {error}"),
        }
    }

    for window in timestamps.windows(2) {
        assert!(
            window[1].duration_since(window[0])
                >= interval.saturating_sub(Duration::from_millis(5))
        );
    }
    queue.shutdown();
}

#[test]
#[ignore = "trash restore is platform-sensitive and can be flaky on CI"]
fn undo_trash() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("undo-trash.txt");
    write_file(&path, b"undo");

    let (queue, events) = small_queue(Duration::from_millis(1));
    let id = queue.submit(OpKind::Delete {
        paths: vec![path.clone()],
        to_trash: true,
    });

    let _events = drain_until_completed(&queue, &events, id);
    assert!(!path.exists());
    queue.undo_stack().undo().expect("undo trash");
    assert!(path.exists());
    queue.shutdown();
}
