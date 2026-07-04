//! Benchmark: [`LocationViewModel::entries`] read cost after the initial
//! load. Measures the cost of grabbing the current entry snapshot 1_000
//! times — the exact hot path status projections, selection lookups, and
//! per-frame focus updates exercise.
//!
//! Before the `Arc<[Entry]>` snapshot conversion, `entries()` cloned the
//! full view `Vec` on every call — O(N) per read. After the conversion
//! reads are a single atomic Arc-clone. On a 10k-entry pane the win
//! shows up as an ~O(N) → O(1) transition.

#[path = "common/mod.rs"]
mod common;

use std::time::{Duration, Instant};

use atlas_fs::{InMemoryLocationViewModel, LocationViewModel, OpenOptions, ViewModelEvent};
use criterion::{criterion_group, criterion_main, Criterion};

fn wait_loaded(vm: &InMemoryLocationViewModel, expected_min: usize) {
    let rx = vm.subscribe();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if vm.len() >= expected_min && vm.is_loaded() {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(ViewModelEvent::Loaded | ViewModelEvent::EntriesChanged) => {}
            Ok(ViewModelEvent::Error(_)) => {}
            Err(_) => {
                if vm.len() >= expected_min && vm.is_loaded() {
                    return;
                }
            }
        }
    }
}

fn bench_reads(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_fs::view_model::entries");
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(3));
    group.measurement_time(Duration::from_secs(6));

    for &size in &[1_000usize, 10_000, 50_000] {
        // Persistent VM: setup once, share across every iteration so the
        // measurement isolates the actual entries() read cost from the
        // per-iteration open/close/drop overhead.
        let dir = common::flat_dir(size);
        let vm = InMemoryLocationViewModel::open(dir.path().to_path_buf(), OpenOptions::default());
        wait_loaded(&vm, size);
        let vm_ref: &InMemoryLocationViewModel = &vm;

        group.bench_function(format!("entries_{size}_reads_x1000"), |b| {
            b.iter(|| {
                let mut acc = 0usize;
                for _ in 0..1_000 {
                    let snap = vm_ref.entries();
                    acc = acc.wrapping_add(snap.len());
                    criterion::black_box(&snap);
                }
                criterion::black_box(acc);
            });
        });

        drop(vm);
        drop(dir);
    }

    group.finish();
}

criterion_group!(benches, bench_reads);
criterion_main!(benches);
