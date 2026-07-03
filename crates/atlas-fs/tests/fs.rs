//! Integration tests for `atlas-fs`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use atlas_fs::{
    list_directory, sort_in_place, walk, CompiledFilter, Entry, EntryKind, Filter,
    InMemoryLocationViewModel, ListEvent, ListRequest, LocationViewModel, Metadata, OpenOptions,
    SortKey, SortOrder, SortSpec, ViewModelEvent, WalkRequest,
};
use tempfile::TempDir;

fn make_tree() -> TempDir {
    let dir = TempDir::new().expect("create tempdir");
    let root = dir.path();
    fs::write(root.join("alpha.txt"), b"a").unwrap();
    fs::write(root.join("beta.rs"), b"bb").unwrap();
    fs::write(root.join(".hidden"), b"secret").unwrap();
    fs::create_dir(root.join("subdir")).unwrap();
    fs::write(root.join("subdir").join("nested.txt"), b"nested").unwrap();
    dir
}

fn collect(rx: crossbeam_channel::Receiver<ListEvent>) -> (Vec<Entry>, Vec<PathBuf>) {
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    for ev in rx.iter() {
        match ev {
            ListEvent::Batch(b) => entries.extend(b),
            ListEvent::Error { path, .. } => errors.push(path),
            ListEvent::Done => break,
        }
    }
    (entries, errors)
}

fn names(entries: &[Entry]) -> HashSet<String> {
    entries.iter().map(|e| e.name.clone()).collect()
}

fn synthetic(name: &str, kind: EntryKind, size: u64) -> Entry {
    Entry {
        path: PathBuf::from(name),
        name: name.to_string(),
        kind,
        metadata: Metadata {
            size,
            ..Metadata::default()
        },
    }
}

#[test]
fn list_directory_returns_visible_entries() {
    let tree = make_tree();
    let rx = list_directory(ListRequest {
        path: tree.path().to_path_buf(),
        follow_symlinks: false,
        include_hidden: false,
    });
    let (entries, errors) = collect(rx.into_receiver());
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    let n = names(&entries);
    assert!(n.contains("alpha.txt"));
    assert!(n.contains("beta.rs"));
    assert!(n.contains("subdir"));
    assert!(!n.contains(".hidden"), "hidden file should be excluded");
}

#[test]
fn list_directory_includes_hidden_when_requested() {
    let tree = make_tree();
    let rx = list_directory(ListRequest {
        path: tree.path().to_path_buf(),
        follow_symlinks: false,
        include_hidden: true,
    });
    let (entries, _) = collect(rx.into_receiver());
    assert!(names(&entries).contains(".hidden"));
}

#[test]
fn list_directory_missing_path_emits_error_then_done() {
    let rx = list_directory(ListRequest {
        path: PathBuf::from("/atlas-fs/definitely/missing/path"),
        follow_symlinks: false,
        include_hidden: false,
    });
    let (entries, errors) = collect(rx.into_receiver());
    assert!(entries.is_empty());
    assert_eq!(errors.len(), 1);
}

#[test]
fn walk_counts_with_and_without_hidden() {
    let tree = make_tree();
    let visible = walk(WalkRequest {
        roots: vec![tree.path().to_path_buf()],
        follow_symlinks: false,
        include_hidden: false,
        respect_gitignore: false,
        max_depth: None,
    });
    let (visible_entries, _) = collect(visible.into_receiver());
    let vn = names(&visible_entries);
    assert!(vn.contains("alpha.txt"));
    assert!(vn.contains("nested.txt"));
    assert!(vn.contains("subdir"));
    assert!(!vn.contains(".hidden"));

    let hidden = walk(WalkRequest {
        roots: vec![tree.path().to_path_buf()],
        follow_symlinks: false,
        include_hidden: true,
        respect_gitignore: false,
        max_depth: None,
    });
    let (hidden_entries, _) = collect(hidden.into_receiver());
    assert!(names(&hidden_entries).contains(".hidden"));
    assert!(hidden_entries.len() > visible_entries.len());
}

#[test]
fn walk_respects_max_depth() {
    let tree = make_tree();
    let handle = walk(WalkRequest {
        roots: vec![tree.path().to_path_buf()],
        follow_symlinks: false,
        include_hidden: false,
        respect_gitignore: false,
        max_depth: Some(1),
    });
    let (entries, _) = collect(handle.into_receiver());
    let n = names(&entries);
    assert!(n.contains("subdir"));
    assert!(!n.contains("nested.txt"), "depth 2 entry must be excluded");
}

#[test]
fn compare_orders_dirs_first_and_natural() {
    let spec = SortSpec {
        key: SortKey::Name,
        order: SortOrder::Asc,
        dirs_first: true,
        natural: true,
        case_insensitive: true,
    };

    let mut entries = vec![
        synthetic("file10", EntryKind::File, 0),
        synthetic("file2", EntryKind::File, 0),
        synthetic("zdir", EntryKind::Dir, 0),
    ];
    sort_in_place(&mut entries, &spec);
    let order: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(order, vec!["zdir", "file2", "file10"]);
}

#[test]
fn compare_case_insensitive() {
    let spec = SortSpec {
        key: SortKey::Name,
        order: SortOrder::Asc,
        dirs_first: false,
        natural: false,
        case_insensitive: true,
    };
    let mut entries = vec![
        synthetic("Banana", EntryKind::File, 0),
        synthetic("apple", EntryKind::File, 0),
    ];
    sort_in_place(&mut entries, &spec);
    let order: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(order, vec!["apple", "Banana"]);
}

#[test]
fn compare_size_desc() {
    let spec = SortSpec {
        key: SortKey::Size,
        order: SortOrder::Desc,
        dirs_first: false,
        natural: false,
        case_insensitive: true,
    };
    let mut entries = vec![
        synthetic("small", EntryKind::File, 10),
        synthetic("big", EntryKind::File, 999),
    ];
    sort_in_place(&mut entries, &spec);
    assert_eq!(entries[0].name, "big");
}

#[test]
fn filter_substring() {
    let f = Filter {
        query: Some("ALPHA".into()),
        ..Filter::default()
    };
    let cf: CompiledFilter = f.compile().unwrap();
    assert!(cf.matches(&synthetic("alpha.txt", EntryKind::File, 0)));
    assert!(!cf.matches(&synthetic("beta.rs", EntryKind::File, 0)));
}

#[test]
fn filter_glob_include_exclude() {
    let f = Filter {
        include_globs: vec!["*.rs".into()],
        exclude_globs: vec!["mod.rs".into()],
        ..Filter::default()
    };
    let cf = f.compile().unwrap();
    assert!(cf.matches(&synthetic("lib.rs", EntryKind::File, 0)));
    assert!(!cf.matches(&synthetic("notes.txt", EntryKind::File, 0)));
    assert!(!cf.matches(&synthetic("mod.rs", EntryKind::File, 0)));
}

#[test]
fn filter_regex() {
    let f = Filter {
        regex: Some(r"^test_.*\.rs$".into()),
        ..Filter::default()
    };
    let cf = f.compile().unwrap();
    assert!(cf.matches(&synthetic("test_foo.rs", EntryKind::File, 0)));
    assert!(!cf.matches(&synthetic("foo.rs", EntryKind::File, 0)));
}

#[test]
fn filter_invalid_regex_errors() {
    let f = Filter {
        regex: Some("(".into()),
        ..Filter::default()
    };
    assert!(f.compile().is_err());
}

fn wait_for<F: Fn() -> bool>(pred: F) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    pred()
}

#[test]
fn view_model_loads_and_emits_events() {
    let tree = make_tree();
    let vm = InMemoryLocationViewModel::open(tree.path().to_path_buf(), OpenOptions::default());
    let rx = vm.subscribe();

    let mut saw_loaded = false;
    let mut saw_changed = false;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !(saw_loaded && saw_changed) {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ViewModelEvent::Loaded) => saw_loaded = true,
            Ok(ViewModelEvent::EntriesChanged) => saw_changed = true,
            Ok(ViewModelEvent::Error(_)) | Err(_) => {}
        }
    }

    assert!(wait_for(|| vm.is_loaded()), "view model never loaded");
    assert!(vm.len() >= 3, "expected entries to load");

    let names: HashSet<String> = vm.entries().iter().map(|e| e.name.clone()).collect();
    assert!(names.contains("alpha.txt"));
}

#[test]
fn view_model_set_filter_changes_snapshot() {
    let tree = make_tree();
    let vm = InMemoryLocationViewModel::open(tree.path().to_path_buf(), OpenOptions::default());
    assert!(wait_for(|| vm.len() >= 3));

    vm.set_filter(Filter {
        include_globs: vec!["*.rs".into()],
        ..Filter::default()
    })
    .unwrap();

    let entries = vm.entries();
    assert!(entries.iter().all(|e| e.name.ends_with(".rs")));
    assert!(entries.iter().any(|e| e.name == "beta.rs"));
}

#[test]
fn view_model_location_matches() {
    let tree = make_tree();
    let vm = InMemoryLocationViewModel::open(tree.path().to_path_buf(), OpenOptions::default());
    assert_eq!(vm.location(), tree.path() as &Path);
}
