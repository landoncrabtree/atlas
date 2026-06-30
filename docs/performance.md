# Performance

Performance is a defining feature of Atlas, not an afterthought. This document codifies our **goals**, **principles**, and the **benchmark methodology** we use to defend them.

> Status: targets are aspirational and tracked toward MVP. Benchmark numbers will be filled in as the harness lands.

## Goals (MVP)

| Scenario | Target | Measured on |
|---|---|---|
| Cold launch → first interactive frame | < 200 ms | M-series MacBook (Pro / Air), warm OS caches |
| Open a 100k-file directory and scroll | smooth 60+ fps (avg frame ≤ 16 ms) | M-series, default theme |
| Fuzzy-find a path against a 1M-doc index | < 50 ms p99 | local SSD, daemon warm |
| Content search across a 1 GB tree (text) | within 1.2× of `ripgrep` | same hardware as `rg`, same flags |
| Memory after 1 hour of typical use | < 250 MB resident | dual-pane, 5 tabs, no large galleries |
| Single-binary `.app` bundle | < 30 MB compressed | macOS release build, stripped |
| Thumbnail generation throughput | ≥ 8 thumbs/sec/core for 1080p JPEGs at 256px | rough decode + resize + WebP encode |

Stretch goals (post-MVP): zero-copy directory enumeration via OS-specific batched APIs (`getdirentries64`, `ReadDirectoryChangesW`), explicit shader pipelines for grid rendering, content-addressable cross-volume dedup for thumbnails.

## Principles

These are non-negotiable. PRs that violate them get pushback.

### 1. The UI thread is sacred

No I/O, no SQLite calls, no JSON parsing, no allocations larger than a few kilobytes on the Slint main thread. All such work goes to a worker thread and the result is pushed back via `slint::invoke_from_event_loop`.

### 2. Streaming over batching

Don't collect a `Vec<Entry>` and then send it. Send entries as you discover them, in small batches of ~64. The user sees the first 200 entries before the last 100,000 finish enumerating.

### 3. Virtualize everything

Lists, grids, trees — only the visible rows mount. We do not render a 100k-entry table; we render the dozen rows the viewport shows.

### 4. Cache, but with eviction

Thumbnail cache: SQLite WAL with LRU eviction at a byte cap (default 500 MB). Index: on-disk tantivy with periodic merges. Don't grow unbounded.

### 5. Measure before you optimize

We don't accept micro-optimizations without a benchmark or trace showing they matter. Conversely, we don't accept regressions without a benchmark showing they're acceptable.

### 6. Cross-thread coordination is explicit

`crossbeam-channel` for producer-consumer. `parking_lot::RwLock` / `Mutex` for shared mutable state. `arc-swap::ArcSwap<T>` for read-mostly snapshots (config, theme). No `Arc<Mutex<HashMap>>` everywhere — pick the right tool.

## Anti-patterns

| Don't | Why |
|---|---|
| `std::fs::read_dir` outside `atlas-fs` / `atlas-indexd` | Bypasses our streaming + sort + filter pipeline |
| `String` keys in hot maps | Use `&str`, interned strings, or hash-prefixed keys |
| `tokio::main` in library crates | Locks consumers into one runtime |
| `Vec<T>` shared with `Arc<Mutex<…>>` for read-mostly state | Use `ArcSwap<Arc<[T]>>` or `Arc<RwLock<Vec<T>>>` |
| Synchronous JSON / TOML parse on UI thread | Move to worker, push result via event loop |
| Allocating per item in hot loops | Reuse buffers; pre-size collections |
| `clone()` of large strings to satisfy the borrow checker | Use `&str` or `Cow<'_, str>` |
| Unbounded channels for downstream backpressure-sensitive work | Use `bounded(N)` and let producers slow down |

## Benchmark methodology

When the harness lands, expect:

- **`cargo bench`** suites in each crate (`crates/<crate>/benches/`) using `criterion`.
- A top-level `bench/` workspace member with end-to-end scenarios:
  - `bench/cold-launch`: spawns the binary, measures time to a known "ready" marker.
  - `bench/large-dir`: opens a tempfile-built tree of N files, measures time-to-first-batch and time-to-fully-loaded.
  - `bench/index-build`: walks a 1M-file generated tree into the daemon's index, measures throughput and disk size.
  - `bench/content-search`: runs ripgrep-equivalent searches, compares against `rg` on the same fixture.
- Tracing-based **flame charts** via `tracing-flame` for ad-hoc profiling.
- Regression tracking: results saved to a CSV/JSON, plotted in CI.

Hardware baseline: M-series MacBook Air (the slowest of our targets) and a recent x86_64 Linux laptop. Numbers reported on both.

## Profiling commands

```bash
# CPU sampling with samply (cross-platform)
cargo install samply
samply record cargo run --release -p atlas-app

# Tracing with tokio-console (daemon only)
RUSTFLAGS="--cfg tokio_unstable" cargo run --release -p atlas-indexd

# Flame chart via tracing-flame
ATLAS_FLAME=on cargo run --release -p atlas-app
# Then convert: inferno-flamegraph < tracing.folded > flame.svg

# Allocation profiling (Linux)
heaptrack cargo run --release -p atlas-app
```

## Performance reviews

Any PR touching a hot path (FS walker, content search, thumbnail pipeline, view virtualization, IPC) should include:

1. The benchmark scenario most affected.
2. Numbers before and after.
3. A flame chart screenshot if the change is non-trivial.

If a PR regresses a benchmark by more than 5% without explicit justification, it's blocked.
