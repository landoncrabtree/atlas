//! Benchmark: `atlas_fs::walk` over a synthetic tree.
//!
//! Measures wall-clock throughput of the parallel walker on a 10k-file
//! synthetic tree assembled in a `tempfile::TempDir`. The tempdir is
//! constructed once per benchmark iteration in `iter_batched` setup, so
//! filesystem creation cost stays out of the timed section.

#[path = "common/mod.rs"]
mod common;

use std::time::Duration;

use atlas_fs::{walk, ListEvent, WalkRequest};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};

fn bench_walk_10k_flat(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_fs::walker");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(6));
    group.warm_up_time(Duration::from_secs(3));

    group.bench_function("walk_10k_flat", |b| {
        b.iter_batched(
            || common::flat_dir(10_000),
            |dir| {
                let req = WalkRequest {
                    roots: vec![dir.path().to_path_buf()],
                    follow_symlinks: false,
                    include_hidden: true,
                    respect_gitignore: false,
                    max_depth: None,
                };
                let mut count = 0usize;
                let rx = walk(req);
                for ev in &rx {
                    match ev {
                        ListEvent::Batch(entries) => count += entries.len(),
                        ListEvent::Error { .. } => {}
                        ListEvent::Done => break,
                    }
                }
                criterion::black_box(count);
                drop(dir);
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

fn bench_walk_tree(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_fs::walker");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(6));
    group.warm_up_time(Duration::from_secs(3));

    group.bench_function("walk_100dirs_x_100files", |b| {
        b.iter_batched(
            || common::tree(100, 100),
            |dir| {
                let req = WalkRequest {
                    roots: vec![dir.path().to_path_buf()],
                    follow_symlinks: false,
                    include_hidden: true,
                    respect_gitignore: false,
                    max_depth: None,
                };
                let mut count = 0usize;
                let rx = walk(req);
                for ev in &rx {
                    match ev {
                        ListEvent::Batch(entries) => count += entries.len(),
                        ListEvent::Error { .. } => {}
                        ListEvent::Done => break,
                    }
                }
                criterion::black_box(count);
                drop(dir);
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_walk_10k_flat, bench_walk_tree);
criterion_main!(benches);
