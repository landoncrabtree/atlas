# Perf follow-ups (post-audit-2026-07-03)

Known bottlenecks and hardening tasks intentionally deferred from the
July 2026 audit (see `docs/perf/results-2026-07-03.md`) because they
either need wider scope than a focused perf PR should carry, need
a dedicated benchmark to justify, or fall outside a hot path.

Ordered by expected impact.

## 1. `InMemoryLocationViewModel` snapshot API allocates per read

- **Where**: `crates/atlas-fs/src/view_model.rs::entries`.
- **Symptom**: `fn entries(&self) -> Vec<Entry>` clones the full
  view Vec on every call. On a 100k-entry pane this is meaningful
  churn during rapid resort / refilter cycles.
- **Fix shape**: switch to `fn snapshot(&self) -> Arc<[Entry]>` with
  `ArcSwap<Arc<[Entry]>>` behind the scenes — read-mostly usage
  makes this a natural fit (principle 6). Public trait signature
  change; will ripple through `atlas-ui` shell projection code.
- **Why deferred**: crosses the public `LocationViewModel` boundary
  and touches every caller in `shell.rs`.

## 2. Watcher event handlers do linear scans on `raw` / `view`

- **Where**: `view_model.rs::handle_{created,removed,modified}`.
- **Symptom**: `raw.iter_mut().find(...)`, `view.iter().any(...)`,
  and `retain(|e| e.name != name)` all cost O(N) per event. Under
  a busy build (thousands of `notify` events / sec on a large tree)
  this becomes a scaling wall.
- **Fix shape**: index by name into a `Vec` slot map — e.g.
  `HashMap<String, usize>` pointing into `raw`, and a small
  incremental view maintenance routine. Needs its own bench
  (`view_model::watcher_burst`) to defend the change.
- **Why deferred**: correctness surface is large (concurrent
  removal + create race), warrants an isolated PR + tests.

## 3. `notify` inside `subscribers.lock()`

- **Where**: `view_model.rs::notify` (line ~267) holds the
  subscribers Mutex while calling `event.clone()` and `tx.send()`
  on each subscriber.
- **Symptom**: any slow subscriber blocks new subscription joins
  and the loader's notify calls. Low priority in the current
  single-subscriber-per-pane deployment but worth cleaning up.
- **Fix shape**: snapshot the subscriber list under the lock, drop
  the lock, then iterate. Alternatively, `parking_lot::RwLock` and
  read-only sends.

## 4. `Entry` shape carries unused ownership

- **Where**: `crates/atlas-fs/src/entry.rs`.
- **Symptom**: each `Entry` owns a `PathBuf` and a `String` (name).
  Every view snapshot clones both. On a 100k pane the memory
  footprint is dominated by these two owned buffers.
- **Fix shape**: intern names via `Arc<str>`; make `path` a `Arc<Path>`
  or a slice into the parent path. Requires benchmarking to pick
  between interning and slicing.
- **Why deferred**: cross-cutting change to a widely-cloned struct.

## 5. `atlas_ops::execute::execute_copy` counts twice

- **Where**: `crates/atlas-ops/src/primitives/copy.rs::count_paths`
  (called once at the start of every Copy) followed by
  `copy_path`'s own `WalkDir` traversal.
- **Symptom**: the source tree is walked twice — once for totals,
  once for the actual copy — doubling the stat syscall count.
- **Fix shape**: interleave count-and-copy so totals stream in while
  the copy runs. UI shows `n / ~m so far` until enumeration
  completes; totals firm up part-way through. Requires progress
  event shape change.
- **Why deferred**: touches the event protocol and needs UX signoff.

## 6. Bench coverage gaps

- No end-to-end bench for the full pane-navigation cycle (open
  directory → scroll → sort → filter) — the closest we have is
  `view_model::open_and_load_*`. Follow-up would live in a
  workspace-level `bench/` crate per the perf instructions doc.
- No bench for `atlas-search::content` (ripgrep-facade). Follow-up
  should compare against `rg` directly on a shared fixture.
- No bench for `atlas-thumbs::Generator` throughput. The performance target
  is ≥ 8 thumbs/sec/core for 1080p JPEG @ 256 px; a criterion
  bench would defend that.
- No bench for `atlas-remote` primitives (would need mock servers).
- No CI bench artifact upload. Instrumented in this PR as a
  follow-up idea but not enabled; benches can vary too much across
  runners to gate merges.

## 7. `walker` / `lister` are IO-bound, not CPU-bound

- **Observation**: `walk_10k_flat` = 323 ms, `list_10k_flat` = 313 ms.
  The `ignore` crate's parallel walker and `std::fs::read_dir`'s
  serial walker take within 10 ms of each other on the same
  workload — because on APFS the dominant cost is
  `symlink_metadata` per entry, not directory traversal.
- **Fix shape**: batch stat via `getdirentries64` (macOS) /
  `GetFileInformationByHandleEx` (Windows) / `getdents64` (Linux)
  where possible. This is the "stretch goal — zero-copy directory
  enumeration via OS-specific batched APIs" already called out in
  `performance.instructions.md`. Longer-term.
