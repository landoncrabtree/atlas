//! Benchmark: nucleo-backed fuzzy scoring over a candidate list.
//!
//! This is the code path that drives the command palette and goto-anywhere
//! surfaces. `fuzzy_rank` creates a fresh `Matcher` per candidate today,
//! so re-using it should show a large speed-up here.

use std::time::Duration;

use atlas_search::fuzzy_rank;
use criterion::{criterion_group, criterion_main, Criterion};

fn candidates(n: usize) -> Vec<String> {
    // Realistic-ish file-path corpus so the fuzzy matcher does non-trivial
    // work on each candidate.
    let mut out = Vec::with_capacity(n);
    let prefixes = [
        "crates/atlas-fs/src",
        "crates/atlas-ui/src",
        "crates/atlas-search/src",
        "docs/perf",
        "assets/ui/components",
        "assets/ui/views/details",
        "target/debug",
    ];
    let stems = [
        "walker",
        "lister",
        "view_model",
        "sort",
        "filter",
        "shell",
        "controller",
        "palette",
        "search",
        "handle_created",
        "handle_removed",
        "handle_modified",
        "notify",
        "render_cache",
    ];
    let exts = ["rs", "toml", "md", "slint"];
    for i in 0..n {
        let p = prefixes[i % prefixes.len()];
        let s = stems[i % stems.len()];
        let e = exts[i % exts.len()];
        out.push(format!("{p}/{s}-{i:04}.{e}"));
    }
    out
}

fn bench_rank(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_search::fuzzy");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(6));
    group.warm_up_time(Duration::from_secs(3));

    let corpus_2k = candidates(2_000);
    let corpus_10k = candidates(10_000);

    group.bench_function("rank_2k_candidates_ctrl", |b| {
        b.iter(|| {
            let ranked = fuzzy_rank(corpus_2k.clone(), "ctrl", |s| s.as_str());
            criterion::black_box(ranked);
        });
    });

    group.bench_function("rank_2k_candidates_walker", |b| {
        b.iter(|| {
            let ranked = fuzzy_rank(corpus_2k.clone(), "walker", |s| s.as_str());
            criterion::black_box(ranked);
        });
    });

    group.bench_function("rank_10k_candidates_shell", |b| {
        b.iter(|| {
            let ranked = fuzzy_rank(corpus_10k.clone(), "shell", |s| s.as_str());
            criterion::black_box(ranked);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_rank);
criterion_main!(benches);
