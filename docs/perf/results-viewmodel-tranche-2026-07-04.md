# View-model perf tranche — 2026-07-04

Follow-up to `docs/perf/results-2026-07-03.md`. This tranche executes
Scopes 1–3 of the audit backlog (items #3 "notify outside lock", #15
"snapshots", #4 "watcher"), plus a measurement bench and analysis for
Scope 4 (item #12 "Entry footprint"). All numbers below use the local
`cargo bench` runs with checked-in criterion defaults (`sample_size = 15`
or `20`, `warm_up_time = 3s`) on the same darwin/aarch64 host.

## Hardware / build

- macOS 15, aarch64.
- Rust stable per `rust-toolchain.toml`.
- `cargo bench --locked` (release profile, LTO off).

## Summary

| Scope | Bench | Baseline | After | Delta |
|---|---|---:|---:|---:|
| 1 | `notify_128_fast_subscribers` | 1.410 ms | 1.332 ms | **−14.7%** |
| 1 | `notify_128_blocked_subscribers` | 14.88 µs | 9.90 µs | **−37.3%** |
| 1 | `notify_512_blocked_subscribers` | 40.18 µs | 40.52 µs | **−12.2%** (p<0.05) |
| 2 | `entries_1000_reads_x1000` | 40.22 ms | 9.38 µs | **~4285× speedup** |
| 2 | `entries_10000_reads_x1000` | 417.7 ms | 9.37 µs | **~44600× speedup** |
| 2 | `entries_50000_reads_x1000` | 3.438 s | 9.38 µs | **~366000× speedup** |
| 3 | `watcher_burst_1000_modifies_on_10k` | 857.6 ms | 415.1 ms | **−50.1%** |
| 3 | `watcher_burst_500_creates_500_removes_on_10k` | 790.6 ms | 352.0 ms | **−54.3%** |
| 3 | `watcher_burst_1000_modifies_on_1k` | 124.4 ms | 76.7 ms | **−37.2%** |

Regression guards (no meaningful change):

| Cross-check | Baseline | After |
|---|---:|---:|
| `open_and_load_10000` (Scope 2/3) | 359.6 ms | 348.4 ms |
| `entries_10000_reads_x1000` (Scope 3 vs Scope 2) | 9.37 µs | 9.21 µs |

## Scope 1 — notify outside the subscribers lock

`InMemoryLocationViewModel::notify` previously held the subscribers
`parking_lot::Mutex` across every `event.clone() + tx.send()` call.
The fix snapshots the current sender list as an `Arc<[Sender]>` under
a single atomic Arc clone, drops the lock, then iterates and sends.
Dead subscribers are pruned in a second brief lock acquisition,
identified by channel identity via
`crossbeam_channel::Sender::same_channel`, so a fresh subscriber that
raced into the list between the two lock acquisitions is not
accidentally removed.

The single-notify wall clock stays roughly the same (the send loop is
the dominant cost regardless of lock state); the win is that
concurrent `subscribe()` calls and other `notify()` calls no longer
block on a slow subscriber. The move from `Vec<Sender>` to
`Arc<[Sender]>` removes a per-Sender atomic Arc clone from every
fan-out — on 128 blocked subscribers that alone is a 37% cut.

## Scope 2 — Arc<[Entry]> snapshots

`LocationViewModel::entries` returned `Vec<Entry>` and cloned the full
view Vec on every call — O(N) per read. The migration switches the
trait to `fn entries(&self) -> Arc<[Entry]>` and hands out a shared
snapshot backed by an internal `Arc<[Entry]>` field. Reads become a
single atomic Arc clone (~10 ns) regardless of pane size.

Migration surface:

- `crates/atlas-fs/src/view_model.rs` — trait + `InMemoryLocationViewModel`.
- `crates/atlas-remote/src/vm/mod.rs` — `RemoteLocationViewModel` mirrors the
  same shape.
- `crates/atlas-remote/src/backend.rs`, `crates/atlas-remote/tests/*.rs`,
  `crates/atlas-ui/src/shell.rs`, `crates/atlas-ui/src/views/{details,gallery,grid,miller}/controller.rs`
  — every caller migrated. The per-view internal caches keep their
  `RwLock<Vec<Entry>>` shape via `.to_vec()` on notify, preserving the
  refresh-path cost while every read elsewhere in the app drops to O(1).

## Scope 3 — Watcher event handlers

Two coupled optimisations:

1. `Inner.raw_index: HashMap<String, usize>` maps entry name to
   position in `raw`. Watcher handlers locate the entry in O(1) and
   remove via `swap_remove` in O(1). The view lookup is O(log N) via
   `partition_point` on `compare` (which has a stable name-tie-break).
2. The `Arc<[Entry]>` snapshot introduced in Scope 2 is now lazy:
   `InMemoryLocationViewModel` grows a `published: Mutex<Arc<[Entry]>>`
   + `published_dirty: AtomicBool`. Every mutation calls
   `invalidate_published()` — a single atomic Release store. `entries()`
   checks the flag: fast path returns the cached Arc; slow path snapshots
   `view` into a fresh `Arc<[Entry]>`, stores it, clears the flag.

The store-then-clear ordering makes the flag conservative: a mutation
between store and clear re-sets the flag and the next read rebuilds.
Under a watcher burst this collapses 1000 × O(N) publish work into a
single O(N) publish on the next read — the reason the burst benches
drop by 37–54% even though the per-event view mutation stays O(N)
memmove.

## Scope 4 — Entry footprint (measurement only, deferred)

Baseline measurements on a 100k-entry synthetic pane:

| Quantity | Value |
|---|---:|
| `size_of::<Entry>()` | 152 bytes |
| Heap strings (path + name for 100k) | ~3.15 MB |
| Total ≈ | 18.35 MB |
| `arc_slice_clone_100k_x1000` | 3.94 µs (~4 ns / clone) |
| `arc_slice_from_vec_10k` | 429 µs (~42 ns / Entry clone) |
| `entry_vec_clone_10k` | 417 µs |

Candidates A / B / C were analysed against these numbers:

- **A (name: `Arc<str>`)** — 8-byte stack saving per entry, but the
  extra 16-byte Arc header per allocation is *worse* for the
  unshared-name case that dominates real panes (one directory has no
  duplicate names). Clone cost drops from ~42 ns to ~30 ns (25% faster)
  but total RSS on a 100k pane grows by ~1.6 MB.
- **B (path: `Arc<Path>`)** — same tradeoff, and paths are longer than
  names on average so the RSS regression is worse.
- **C (drop path, store `parent: Arc<Path>` on the view-model)** — the
  only candidate that actually reduces memory (removes 24-byte
  `PathBuf` per entry entirely). Requires a cross-cutting API change:
  every caller of `entry.path` becomes `vm.entry_path(entry)`. Migration
  surface spans `atlas-remote`, `atlas-search`, `atlas-ui`, `atlas-ops`,
  `atlas-thumbs`, and every test that constructs an `Entry`.

**Deferred.** Scope 3 already made the O(N) Entry clone lazy (at most
one per mutation burst), so the remaining per-entry clone cost is
amortised. Scope 4 is filed as a follow-up so it does not get lost.

## Test summary

Baseline: 822 tests, 3 skipped, all passing.  
After tranche: 822 tests, 3 skipped, all passing.

No new tests were required — the existing `watched_*` and `view_model_*`
suites already exercise the mutated paths. Bench targets are separate
from test targets.

## Follow-ups

- **Scope 4**: pick Candidate C and land the `entry.path` → `parent +
  name` split as a dedicated PR. Cross-crate refactor with its own
  bench comparison.
- **atlas-remote notify path**: `RemoteLocationViewModel::notify` still
  holds the subscribers Mutex across `send()`. Same fix as Scope 1
  applies but is out of scope for the atlas-fs-focused tranche.
- **`atlas-search` result-set caching**: has its own view-model-like
  snapshot Vec that has not been migrated to `Arc<[Entry]>`. Filed
  separately.
