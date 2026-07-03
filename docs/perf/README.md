# Atlas performance benchmarks

Criterion benchmarks live in each crate's `benches/` directory. They exist so
every change on a hot path can be justified with a before/after number.

## Running

```bash
# One crate:
cargo bench -p atlas-fs

# One bench, one filter:
cargo bench -p atlas-fs --bench sort -- sort_10k_extension

# Save named baseline (does not overwrite the previous one):
cargo bench -p atlas-fs --bench sort -- --save-baseline main

# Compare against a saved baseline:
cargo bench -p atlas-fs --bench sort -- --baseline main
```

Reports land under `target/criterion/<group>/<bench>/report/index.html`.

## Current coverage

| Crate | Bench | Group | What it exercises |
|---|---|---|---|
| `atlas-fs` | `walker` | `atlas_fs::walker` | Parallel `walk()` over synthetic trees (10k flat, 100×100). |
| `atlas-fs` | `lister` | `atlas_fs::lister` | Single-directory streaming `list_directory()` at 1k / 10k. |
| `atlas-fs` | `sort` | `atlas_fs::sort` | `sort_in_place` under natural / lexi / extension / size specs. |
| `atlas-fs` | `filter` | `atlas_fs::filter` | `CompiledFilter::matches` with substring, no query, hide-hidden. |
| `atlas-fs` | `view_model` | `atlas_fs::view_model` | `InMemoryLocationViewModel::open` end-to-end at 1k / 5k / 10k. Catches quadratic loader amortization. |
| `atlas-search` | `fuzzy` | `atlas_search::fuzzy` | `fuzzy_rank` over 2k / 10k path corpora. |
| `atlas-ops` | `copy` | `atlas_ops::copy` | `OperationQueue` end-to-end Copy of 100×4 KiB and 1×16 MiB. |

## Methodology

- Each bench uses `sample_size ≥ 15`, `warm_up_time ≥ 3s`, and
  `iter_batched(..., BatchSize::LargeInput)` when the setup itself is
  non-trivial (tempdirs, allocated corpora). That keeps setup out of
  the timed section.
- Benches are `#![allow(dead_code)]` free of `println!`; all workspace
  lints apply. `cargo clippy --workspace --all-targets` must pass.
- Every optimization commit records the baseline median and stddev
  under `docs/perf/results-<date>.md`.
