//! Benchmark: watcher event burst on a large `InMemoryLocationViewModel`.
//!
//! Exercises [`InMemoryLocationViewModel::handle_created`],
//! [`InMemoryLocationViewModel::handle_modified`], and
//! [`InMemoryLocationViewModel::handle_removed`] on a 10k-entry view
//! with a mix of 1_000 events (create / modify / remove). Before the
//! name-indexed side table, each handler did an O(N) linear scan of
//! `raw` and `view`; total cost per burst was O(M × N). After: each
//! handler is O(log N) via the side-table map + partition_point.
//!
//! We drive the handler paths directly through
//! `handle_*_for_bench` shims (`#[doc(hidden)]`) so the bench does
//! not depend on the real notify/debouncer plumbing.

#[path = "common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use atlas_fs::{InMemoryLocationViewModel, LocationViewModel, OpenOptions, ViewModelEvent};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use tempfile::TempDir;

/// Open a view model over a `flat_dir` fixture of `size` regular files
/// and wait for the initial load to complete.
fn setup(size: usize) -> (TempDir, Arc<InMemoryLocationViewModel>) {
    let dir = common::flat_dir(size);
    let vm = InMemoryLocationViewModel::open(dir.path().to_path_buf(), OpenOptions::default());
    let rx = vm.subscribe();
    let deadline = Instant::now() + Duration::from_secs(30);
    while (vm.len() < size || !vm.is_loaded())
        && deadline.saturating_duration_since(Instant::now()) > Duration::ZERO
    {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(ViewModelEvent::Loaded | ViewModelEvent::EntriesChanged) => {}
            Ok(ViewModelEvent::Error(_)) => {}
            Err(_) => {}
        }
    }
    (dir, vm)
}

fn bench_watcher_burst(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_fs::view_model::watcher");
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(3));
    group.measurement_time(Duration::from_secs(6));

    // 10k-entry pane, 1000 modify events on files that already exist.
    group.bench_function("watcher_burst_1000_modifies_on_10k", |b| {
        b.iter_batched(
            || setup(10_000),
            |(dir, vm)| {
                let root = dir.path().to_path_buf();
                for i in 0..1_000 {
                    let name = format!("file-{:06}.txt", i);
                    let path = root.join(&name);
                    // Touch the file so build_entry re-stats a real path.
                    std::fs::write(&path, b"y").expect("write");
                    vm.handle_modified_for_bench(path);
                }
                criterion::black_box(vm.len());
                drop(vm);
                drop(dir);
            },
            BatchSize::LargeInput,
        );
    });

    // 10k-entry pane, 500 create + 500 remove events for names not in
    // the initial listing. Creates land in the sorted view; removes
    // remove them again. Balanced so `len()` returns to 10k.
    group.bench_function("watcher_burst_500_creates_500_removes_on_10k", |b| {
        b.iter_batched(
            || setup(10_000),
            |(dir, vm)| {
                let root = dir.path().to_path_buf();
                let mut new_paths: Vec<PathBuf> = Vec::with_capacity(500);
                for i in 0..500 {
                    let name = format!("new-{:06}.txt", i);
                    let path = root.join(&name);
                    std::fs::write(&path, b"z").expect("write");
                    vm.handle_created_for_bench(path.clone());
                    new_paths.push(path);
                }
                for path in &new_paths {
                    let _ = std::fs::remove_file(path);
                    vm.handle_removed_for_bench(path.clone());
                }
                criterion::black_box(vm.len());
                drop(vm);
                drop(dir);
            },
            BatchSize::LargeInput,
        );
    });

    // 1k-entry pane sanity check to keep the O(N) vs O(log N) trend
    // visible in one place: the burst size stays 1000 but pane size
    // drops by 10×, so a naive O(N) implementation drops proportionally.
    group.bench_function("watcher_burst_1000_modifies_on_1k", |b| {
        b.iter_batched(
            || setup(1_000),
            |(dir, vm)| {
                let root = dir.path().to_path_buf();
                for i in 0..1_000 {
                    let name = format!("file-{:06}.txt", i);
                    let path = root.join(&name);
                    std::fs::write(&path, b"y").expect("write");
                    vm.handle_modified_for_bench(path);
                }
                criterion::black_box(vm.len());
                drop(vm);
                drop(dir);
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_watcher_burst);
criterion_main!(benches);
