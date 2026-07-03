//! Benchmark: opening a directory through `InMemoryLocationViewModel` end-to-end.
//!
//! This is the metric that catches the run-loader's recompute-per-batch
//! amortization behaviour: the loader accumulates entries in 64-item
//! batches and calls `recompute` (which filter-clones + sorts the full
//! raw set) on every batch. A quadratic implementation shows up as a
//! large blow-up between 1k and 10k entries.

#[path = "common/mod.rs"]
mod common;

use std::time::{Duration, Instant};

use atlas_fs::{InMemoryLocationViewModel, LocationViewModel, OpenOptions, ViewModelEvent};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};

fn wait_loaded(vm: &InMemoryLocationViewModel, expected_min: usize) {
    let rx = vm.subscribe();
    let deadline = Instant::now() + Duration::from_secs(30);
    // Drain existing state check + subscribe-and-wait pattern.
    loop {
        if vm.len() >= expected_min && vm.is_loaded() {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(ViewModelEvent::Loaded) | Ok(ViewModelEvent::EntriesChanged) => {}
            Ok(ViewModelEvent::Error(_)) => {}
            Err(_) => {
                if vm.len() >= expected_min && vm.is_loaded() {
                    return;
                }
            }
        }
    }
}

fn bench_open(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_fs::view_model");
    group.sample_size(15);
    group.measurement_time(Duration::from_secs(8));
    group.warm_up_time(Duration::from_secs(3));

    for &size in &[1_000usize, 5_000, 10_000] {
        group.bench_function(format!("open_and_load_{size}"), |b| {
            b.iter_batched(
                || common::flat_dir(size),
                |dir| {
                    let vm = InMemoryLocationViewModel::open(
                        dir.path().to_path_buf(),
                        OpenOptions::default(),
                    );
                    wait_loaded(&vm, size);
                    criterion::black_box(vm.len());
                    drop(vm);
                    drop(dir);
                },
                BatchSize::LargeInput,
            );
        });
    }

    group.finish();
}

criterion_group!(benches, bench_open);
criterion_main!(benches);
