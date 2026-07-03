//! Benchmark: local copy of a fixed set of files through `OperationQueue`.
//!
//! Exercises `atlas_ops::execute::execute_op` end-to-end for a Copy op —
//! the hot path when the user hits F5. Small files stress the per-file
//! bookkeeping (mutex acquisitions, `to_path_buf()`, progress emit
//! throttling); larger files stress the read/write loop.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use atlas_core::Location;
use atlas_ops::{ConflictPolicy, OpEvent, OpKind, OperationQueue, QueueOptions};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use tempfile::TempDir;

fn build_source_tree(count: usize, size_bytes: usize) -> (TempDir, Vec<PathBuf>) {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path().to_path_buf();
    let payload = vec![0xABu8; size_bytes];
    let mut sources = Vec::with_capacity(count);
    for i in 0..count {
        let p = root.join(format!("src-{i:04}.bin"));
        fs::write(&p, &payload).expect("write source");
        sources.push(p);
    }
    (dir, sources)
}

fn wait_for_complete(events: &crossbeam_channel::Receiver<OpEvent>) {
    while let Ok(ev) = events.recv_timeout(Duration::from_secs(30)) {
        match ev {
            OpEvent::Completed { .. } | OpEvent::Failed { .. } | OpEvent::Cancelled { .. } => {
                return
            }
            _ => {}
        }
    }
}

fn bench_copy_100_small(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_ops::copy");
    group.sample_size(15);
    group.measurement_time(Duration::from_secs(8));
    group.warm_up_time(Duration::from_secs(3));

    group.bench_function("copy_100_files_4kb", |b| {
        b.iter_batched(
            || {
                let (src_dir, sources) = build_source_tree(100, 4 * 1024);
                let dst_dir = TempDir::new().expect("dst tempdir");
                (src_dir, dst_dir, sources)
            },
            |(src_dir, dst_dir, sources)| {
                let (queue, events) = OperationQueue::start(QueueOptions {
                    workers: 1,
                    progress_interval: Duration::from_millis(100),
                });
                let op = OpKind::Copy {
                    sources: sources.into_iter().map(Location::Local).collect(),
                    dest_dir: Location::Local(dst_dir.path().to_path_buf()),
                    policy: ConflictPolicy::Overwrite,
                };
                let _id = queue.submit(op);
                wait_for_complete(&events);
                drop(queue);
                drop(dst_dir);
                drop(src_dir);
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

fn bench_copy_one_big(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_ops::copy");
    group.sample_size(15);
    group.measurement_time(Duration::from_secs(8));
    group.warm_up_time(Duration::from_secs(3));

    group.bench_function("copy_1_file_16mb", |b| {
        b.iter_batched(
            || {
                let (src_dir, sources) = build_source_tree(1, 16 * 1024 * 1024);
                let dst_dir = TempDir::new().expect("dst tempdir");
                (src_dir, dst_dir, sources)
            },
            |(src_dir, dst_dir, sources)| {
                let (queue, events) = OperationQueue::start(QueueOptions {
                    workers: 1,
                    progress_interval: Duration::from_millis(100),
                });
                let op = OpKind::Copy {
                    sources: sources.into_iter().map(Location::Local).collect(),
                    dest_dir: Location::Local(dst_dir.path().to_path_buf()),
                    policy: ConflictPolicy::Overwrite,
                };
                let _id = queue.submit(op);
                wait_for_complete(&events);
                drop(queue);
                drop(dst_dir);
                drop(src_dir);
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_copy_100_small, bench_copy_one_big);
criterion_main!(benches);
