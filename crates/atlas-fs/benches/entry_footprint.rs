//! Benchmark: [`Entry`] footprint on a 100k-entry synthetic pane.
//!
//! Reports:
//! - the total heap-owned buffer size across the pane (`path` + `name`
//!   byte counts summed, plus the `Vec<Entry>` capacity itself).
//! - the Arc-clone cost of grabbing an [`Arc<[Entry]>`] snapshot and
//!   dropping it 1_000 times, so a future migration of `Entry.name`
//!   to `Arc<str>` or `Entry.path` to `Arc<Path>` (Scope 4) can be
//!   compared against this baseline.
//!
//! Constructs the entry set synthetically via `common::synthetic_entries`
//! so the measurement isolates the `Entry` shape cost, not the
//! filesystem stat cost.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;
use std::time::Duration;

use atlas_fs::Entry;
use criterion::{criterion_group, criterion_main, Criterion};

/// Rough heap footprint for an [`Entry`]: sum of `path` byte length +
/// `name` byte length. Does not include the `Vec` capacity or the
/// per-allocation slop; captures the O(N) growth term that dominates
/// large panes.
fn entry_heap_bytes(entries: &[Entry]) -> usize {
    let mut total: usize = 0;
    for e in entries {
        total += e.path.as_os_str().len();
        total += e.name.len();
    }
    total
}

fn bench_footprint(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_fs::view_model::entry_footprint");
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(3));
    group.measurement_time(Duration::from_secs(5));

    // Emit a size marker as a synthetic bench so the criterion report
    // captures the current Entry shape. `size_of::<Entry>()` is the
    // stack-side footprint; adding it to the string-heap total gives
    // a first-order estimate of total pane RSS.
    let entry_stack_bytes = std::mem::size_of::<Entry>();
    let entries = common::synthetic_entries(100_000);
    let heap_bytes = entry_heap_bytes(&entries);
    let total_bytes = entries.len() * entry_stack_bytes + heap_bytes;
    // Report footprint via a dedicated bench_function so criterion
    // includes it in the report without violating the workspace's
    // print_stderr lint. The measurement is essentially instantaneous
    // (three field reads) so it does not skew adjacent bench timings.
    group.bench_function(
        format!("footprint_marker_size={entry_stack_bytes}_heap={heap_bytes}_total={total_bytes}"),
        |b| {
            b.iter(|| {
                criterion::black_box((entry_stack_bytes, heap_bytes, total_bytes));
            });
        },
    );

    // Bench 1: `Arc<[Entry]>` clone cost across a 100k-entry snapshot.
    // The construction (Arc::from(slice)) happens once outside the timed
    // section; each iteration then clones the outer Arc 1_000 times.
    let snapshot: Arc<[Entry]> = Arc::from(entries.as_slice());
    group.bench_function("arc_slice_clone_100k_x1000", |b| {
        b.iter(|| {
            let mut acc = 0usize;
            for _ in 0..1_000 {
                let c = Arc::clone(&snapshot);
                acc = acc.wrapping_add(c.len());
                criterion::black_box(&c);
            }
            criterion::black_box(acc);
        });
    });

    // Bench 2: reconstruct an Arc<[Entry]> from a Vec via `Arc::from`,
    // which is the cost the lazy publish path pays on the first read
    // after any mutation. Isolated per-Entry Clone cost is the
    // dominant term.
    group.bench_function("arc_slice_from_vec_10k", |b| {
        let small = common::synthetic_entries(10_000);
        b.iter(|| {
            let snap: Arc<[Entry]> = Arc::from(small.as_slice());
            criterion::black_box(&snap);
        });
    });

    // Bench 3: clone every Entry in a 10k-entry Vec. Isolates the
    // per-Entry clone cost that a future Arc<str>/Arc<Path> migration
    // would collapse into two atomic increments.
    group.bench_function("entry_vec_clone_10k", |b| {
        let src = common::synthetic_entries(10_000);
        b.iter(|| {
            let cloned: Vec<Entry> = src.clone();
            criterion::black_box(&cloned);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_footprint);
criterion_main!(benches);
