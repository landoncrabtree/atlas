//! Benchmark: `atlas_fs::CompiledFilter::matches` across many entries.

#[path = "common/mod.rs"]
mod common;

use std::time::Duration;

use atlas_fs::Filter;
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_filter(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_fs::filter");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(5));
    group.warm_up_time(Duration::from_secs(3));

    let entries = common::synthetic_entries(10_000);

    group.bench_function("matches_substring_10k", |b| {
        let f = Filter {
            query: Some("gamma".into()),
            ..Filter::default()
        };
        let cf = f.compile().expect("compile filter");
        b.iter(|| {
            let mut count = 0usize;
            for e in &entries {
                if cf.matches(e) {
                    count += 1;
                }
            }
            criterion::black_box(count);
        });
    });

    group.bench_function("matches_no_query_10k", |b| {
        // Baseline: filter with no substring — hidden gate + short-circuits only.
        let cf = Filter::default().compile().expect("compile filter");
        b.iter(|| {
            let mut count = 0usize;
            for e in &entries {
                if cf.matches(e) {
                    count += 1;
                }
            }
            criterion::black_box(count);
        });
    });

    group.bench_function("matches_hide_hidden_10k", |b| {
        // Hidden gate active — the common case for `show_hidden = false`.
        let f = Filter {
            include_hidden: false,
            ..Filter::default()
        };
        let cf = f.compile().expect("compile filter");
        b.iter(|| {
            let mut count = 0usize;
            for e in &entries {
                if cf.matches(e) {
                    count += 1;
                }
            }
            criterion::black_box(count);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_filter);
criterion_main!(benches);
