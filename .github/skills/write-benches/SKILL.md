---
name: write-benches
description: Guide for adding, running, and interpreting Atlas Criterion benchmarks. Use when a change touches a hot path, claims a performance improvement, or needs benchmark evidence before optimization.
---

Atlas treats performance claims as evidence-backed engineering work. Use this
skill before adding an optimization, a new benchmark, or a perf-focused commit.

## When to add a bench

Add or update a benchmark when a change touches a hot path or claims a measurable
win. Hot paths include filesystem walking/listing/sorting/filtering, fuzzy or
content search, thumbnail generation, view-model loading, IPC, remote streaming,
file operations, and UI model updates that run per frame or per entry.

Red flags that usually need a bench:

- `clone()` or allocation inside loops over entries, frames, matches, chunks, or
  candidates.
- Lowercasing, formatting, path conversion, or `to_string_lossy().into_owned()`
  per item.
- Rebuilding matchers, regexes, clients, view models, or Slint models per event.
- Sorting/filtering whole collections after every small batch.
- Unbounded queues, per-byte/per-chunk locks, or extra filesystem walks.
- Any change that trades median latency for tail latency, memory, or throughput.

Do not optimize first and look for justification later. Measure the baseline,
make the smallest change, then measure again.

## Hot path vs cold path

Classify every perf change explicitly:

- **Hot path**: user-visible latency, per-frame/per-entry work, or a repeated
  background loop. Requires a benchmark plus a flamegraph or trace when the
  change is non-trivial.
- **Cold path**: setup, one-shot validation, rare error handling, or code that
  only runs during configuration/migration. Reasoning is acceptable; write
  `cold path — no measurement` in the commit body with the rationale.

If you are unsure, treat it as hot until measurement proves otherwise.

## Setting up a bench

Criterion benches live under the crate they measure:

```text
crates/<crate>/benches/<scenario>.rs
```

Match the existing harness style:

- Group names use the Rust path shape, e.g. `atlas_fs::sort`.
- Bench IDs name the input size and scenario, e.g. `sort_10k_extension`.
- Build deterministic fixtures outside the timed section.
- Use `iter_batched(..., BatchSize::LargeInput)` when setup allocates tempdirs,
  corpora, trees, or queues.
- Keep benches free of `println!` and dead-code allowances; workspace lints
  still apply.
- Prefer realistic sizes: 1k/10k entries, 2k/10k search candidates, or file sizes
  that match the user-facing scenario.

Boilerplate shape:

```rust
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};

fn bench_scenario(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas_crate::scenario");
    group.sample_size(15);
    group.warm_up_time(std::time::Duration::from_secs(3));
    group.bench_function("case_name", |b| {
        b.iter_batched(
            build_fixture,
            |fixture| exercise_hot_path(fixture),
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

criterion_group!(benches, bench_scenario);
criterion_main!(benches);
```

Use the existing baseline harnesses as examples: `atlas-fs` covers walker,
lister, sort, filter, and view-model loading; `atlas-search` covers fuzzy
ranking; `atlas-ops` covers copy throughput.

## Running benches

Common commands:

```bash
# One crate
cargo bench -p atlas-fs

# One bench target and one filter
cargo bench -p atlas-fs --bench sort -- sort_10k_extension

# Whole workspace
cargo bench --workspace

# Save a named baseline, then compare a later run against it
cargo bench -p atlas-fs --bench sort -- --save-baseline main
cargo bench -p atlas-fs --bench sort -- --baseline main
```

Default harness settings for Atlas benches are `sample_size >= 15` and
`warm_up_time >= 3s`. CI may use a smaller sample size for informational runs,
but local before/after numbers in a PR should use the bench's checked-in
defaults unless you explain otherwise.

Reports land under `target/criterion/<group>/<bench>/report/index.html`. Save
relevant medians, confidence intervals, and raw report paths in the PR body or a
`docs/perf/results-<date>.md` historical note when the work is part of a perf
audit.

## Assessing results

A meaningful improvement is usually a median change greater than **5%** with the
confidence intervals not overlapping enough to look like runner noise.

Use this decision rule:

- Median improves by >5% and the tail does not regress: keep the change.
- Median is noise but p95/p99 improves: keep only if the user-facing scenario is
  tail-latency-sensitive, and say so in the rationale.
- p99 improves but median regresses: keep only with an explicit trade-off, e.g.
  interactive stalls disappear at the cost of background throughput. Otherwise
  revise or revert.
- Median regresses by >5%: revert unless there is a documented product reason.
- No measurable change: report `no change` and revert the optimization unless it
  is a cold-path cleanup or readability/correctness improvement.

Always compare the same hardware, same target profile, same fixture, and same
command. Runner-to-runner comparisons are informational only.

## Commit format for perf changes

Use a `perf(<crate>):` subject that states the measured result:

```text
perf(<crate>): <what changed> — <X%|Xms> improvement
```

Commit body template:

```text
Baseline: <bench name> median <before> (<95% CI>)
After: <bench name> median <after> (<95% CI>)
Improvement: <delta>, outside noise because <reason>
Rationale: <why this path matters to users>
```

For cold-path cleanup, replace the baseline block with:

```text
cold path — no measurement
Rationale: <why this cannot affect hot-path latency or throughput>
```

Never claim a performance win from intuition alone.
