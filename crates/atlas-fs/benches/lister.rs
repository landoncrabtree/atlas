//! Benchmark: `atlas_fs::list_directory` streaming a single flat directory.

#[path = "common/mod.rs"]
mod common;

use std::time::Duration;

use atlas_fs::{list_directory, ListEvent, ListRequest};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};

fn bench_list_1k(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_fs::lister");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(6));
    group.warm_up_time(Duration::from_secs(3));

    group.bench_function("list_1k_flat", |b| {
        b.iter_batched(
            || common::flat_dir(1_000),
            |dir| {
                let req = ListRequest {
                    path: dir.path().to_path_buf(),
                    follow_symlinks: false,
                    include_hidden: true,
                };
                let rx = list_directory(req);
                let mut count = 0usize;
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

fn bench_list_10k(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_fs::lister");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(6));
    group.warm_up_time(Duration::from_secs(3));

    group.bench_function("list_10k_flat", |b| {
        b.iter_batched(
            || common::flat_dir(10_000),
            |dir| {
                let req = ListRequest {
                    path: dir.path().to_path_buf(),
                    follow_symlinks: false,
                    include_hidden: true,
                };
                let rx = list_directory(req);
                let mut count = 0usize;
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

criterion_group!(benches, bench_list_1k, bench_list_10k);
criterion_main!(benches);
