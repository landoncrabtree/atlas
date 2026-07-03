//! Benchmark: `atlas_fs::sort_in_place` against 10k synthetic entries.

#[path = "common/mod.rs"]
mod common;

use std::time::Duration;

use atlas_fs::{sort_in_place, SortKey, SortOrder, SortSpec};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};

fn spec(key: SortKey, natural: bool, case_insensitive: bool) -> SortSpec {
    SortSpec {
        key,
        order: SortOrder::Asc,
        dirs_first: true,
        natural,
        case_insensitive,
    }
}

fn bench_sort(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_fs::sort");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(6));
    group.warm_up_time(Duration::from_secs(3));

    let entries = common::synthetic_entries(10_000);

    group.bench_function("sort_10k_name_natural_ci", |b| {
        let s = spec(SortKey::Name, true, true);
        b.iter_batched(
            || entries.clone(),
            |mut v| {
                sort_in_place(&mut v, &s);
                criterion::black_box(&v);
            },
            BatchSize::LargeInput,
        );
    });

    group.bench_function("sort_10k_name_lexi_ci", |b| {
        let s = spec(SortKey::Name, false, true);
        b.iter_batched(
            || entries.clone(),
            |mut v| {
                sort_in_place(&mut v, &s);
                criterion::black_box(&v);
            },
            BatchSize::LargeInput,
        );
    });

    group.bench_function("sort_10k_extension", |b| {
        let s = spec(SortKey::Extension, true, true);
        b.iter_batched(
            || entries.clone(),
            |mut v| {
                sort_in_place(&mut v, &s);
                criterion::black_box(&v);
            },
            BatchSize::LargeInput,
        );
    });

    group.bench_function("sort_10k_size", |b| {
        let s = spec(SortKey::Size, false, false);
        b.iter_batched(
            || entries.clone(),
            |mut v| {
                sort_in_place(&mut v, &s);
                criterion::black_box(&v);
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_sort);
criterion_main!(benches);
