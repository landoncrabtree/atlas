---
applyTo: "**/*.rs,**/*.slint,**/Cargo.toml"
description: "Atlas performance goals, principles, and anti-patterns. Apply to all Rust, Slint, and dependency-graph changes."
---

# Performance

Performance is a defining feature of Atlas, not an afterthought. This document codifies our **goals**, **principles**, and **anti-patterns**.

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

Thumbnail cache: SQLite WAL with LRU eviction at a byte cap (default 500 MB). Index: on-disk tantivy with periodic merges. Remote connection pool: `atlas_remote::pool::ConnectionPool` keys on `(scheme, host, port, user)` and evicts idle connections after a TTL — never grow the pool unbounded. Don't grow anything unbounded.

### 5. Measure before you optimize

We don't accept micro-optimizations without a benchmark or trace showing they matter. Conversely, we don't accept regressions without a benchmark showing they're acceptable.

### 6. Cross-thread coordination is explicit

`crossbeam-channel` for producer-consumer. `parking_lot::RwLock` / `Mutex` for shared mutable state. `arc-swap::ArcSwap<T>` for read-mostly snapshots (config, theme). No `Arc<Mutex<HashMap>>` everywhere — pick the right tool.

### 7. Local fast paths short-circuit remote

Every operation that has a native-only implementation (thumbnails, `notify` watcher, native trash, free-space queries, memory-mapped reads) must guard with `Location::as_local()` and either short-circuit for `Remote(_)` or hand off to `atlas-remote`. Do **not** simulate local semantics over the network — return an explicit "unsupported for remote" outcome and let the caller decide.

### 8. Remote search stays bounded and lazy

Search over remote panes debounces every input change (`search.debounce_ms`, default 150), enforces a minimum query length (`search.min_query_length`, default 2), and caps visible results (`search.max_visible_results`, default 100). Never issue a PROPFIND / LIST / recursive SFTP walk on every keystroke; batch and de-duplicate. All remote search runs on the `atlas_remote::runtime` handle, never on the UI thread.

## Anti-patterns

| Don't | Why |
|---|---|
| `std::fs::read_dir` outside `atlas-fs` / `atlas-indexd` | Bypasses our streaming + sort + filter pipeline |
| `String` keys in hot maps | Use `&str`, interned strings, or hash-prefixed keys |
| `tokio::main` in library crates | Locks consumers into one runtime; use `atlas_remote::runtime::handle()` |
| Bringing up a fresh SFTP/HTTP/FTP client per operation | Bypasses the connection pool and the retry envelope |
| Assuming `Location` is `Local` without `as_local()` | Panics or blocks for remote paths |
| `.set_panes_details_rows(VecModel::from(rows).into())` on every refresh | Detaches the ListView from its previous model, resets scroll offset; mutate the persistent `Rc<VecModel>` via `OuterPaneModels::sync_vec_model` instead |
| `Vec<T>` shared with `Arc<Mutex<…>>` for read-mostly state | Use `ArcSwap<Arc<[T]>>` or `Arc<RwLock<Vec<T>>>` |
| Synchronous JSON / TOML parse on UI thread | Move to worker, push result via event loop |
| Allocating per item in hot loops | Reuse buffers; pre-size collections |
| `clone()` of large strings to satisfy the borrow checker | Use `&str` or `Cow<'_, str>` |
| Unbounded channels for downstream backpressure-sensitive work | Use `bounded(N)` and let producers slow down |

## Benchmarking and perf reviews

Measure before optimizing hot paths. Benchmark setup, Criterion commands,
flamegraph/profiling commands, result assessment, hot-vs-cold classification,
and perf commit format live in
[`.github/skills/write-benches/SKILL.md`](../skills/write-benches/SKILL.md).
