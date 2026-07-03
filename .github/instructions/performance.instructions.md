---
applyTo: "**/*.rs"
description: "Atlas performance philosophy + Rust best practices. Applies to every Rust source file."
---

# Performance

Atlas is a cross-platform file explorer. Every recommendation optimizes for:

- Near-instant navigation
- Low-latency filesystem operations
- Smooth 60–144 FPS UI rendering
- Fast search and filtering
- Minimal input latency
- Efficient async execution
- Low memory usage
- Excellent battery efficiency
- Predictable frame times (no stutters)

Treat every unnecessary allocation, copy, lock, wakeup, syscall, context
switch, and UI redraw as a performance bug unless justified.

**Validation lives in [`.github/skills/write-benches/SKILL.md`](../skills/write-benches/SKILL.md)**
— that skill covers Criterion harnesses, when to add benches, and how to
interpret results. This file is the *rules*; the skill is the *validation
protocol*.

## Overall philosophy

Performance comes before abstraction.

Prefer: simplicity, cache locality, zero-cost abstractions, static dispatch,
predictable execution, explicit ownership.

Avoid abstraction layers that obscure allocations or synchronization.

**Measure before assuming.**

## Rust best practices

### Minimize allocations

- Avoid allocating in hot loops, per-frame work, per-entry transforms, and search
  candidate scoring.
- Prefer borrowing (`&str`, `&Path`, slices) over constructing owned `String`,
  `PathBuf`, or `Vec` values.
- Reuse buffers with `clear()` when ownership stays local and capacity is useful.
- Pre-allocate with `Vec::with_capacity`, `String::with_capacity`, or map/set
  capacity when the approximate size is known.
- Prefer `SmallVec`, arrays, or stack storage for tiny collections with a known
  upper bound.
- Avoid `format!` in hot paths; write into an existing buffer or defer formatting
  until UI/log presentation.
- Do not allocate just to satisfy an API. Change the API to accept a borrowed
  value when ownership is not required.

### Eliminate unnecessary cloning

- Treat every `clone()` on `String`, `PathBuf`, `Vec`, `Arc`, and model rows as a
  cost that needs a reason.
- Prefer passing `&T`, `Arc<T>` snapshots, `Cow<'_, str>`, or indexes into stable
  storage.
- Clone cheap identifiers only when it reduces contention or lifetime coupling.
- Avoid cloning whole row/view-model collections to update one field; mutate the
  persistent model or publish an immutable snapshot.
- Watch for hidden clones from `to_string()`, `to_owned()`,
  `to_string_lossy().into_owned()`, iterator `collect()`, and serde round-trips.

### Prefer static dispatch

- Prefer generics and concrete types over `dyn Trait` on hot paths.
- Keep trait objects at subsystem boundaries where dynamic dispatch buys
  isolation or plugin-like behavior.
- Avoid boxed closures/futures in tight loops; use concrete closure types or enum
  dispatch when practical.
- Do not hide allocations behind abstraction layers such as `Box<dyn Iterator>`
  unless the path is cold or measured acceptable.

### Avoid `Arc<Mutex<T>>` by default

- `Arc<Mutex<T>>` is a last resort, not the default shared-state shape.
- For read-mostly state, prefer `ArcSwap<Arc<T>>`, `Arc<[T]>`, immutable snapshots,
  or `RwLock`.
- For producer/consumer flow, prefer bounded channels.
- For sharded mutable maps, prefer `DashMap` or explicit sharding when contention
  matters.
- Keep locks out of callbacks that can be re-entered from Slint or async tasks.
- Never hold a lock across filesystem I/O, network I/O, `.await`, logging-heavy
  code, or UI updates.

### Use `parking_lot`

- Prefer `parking_lot::{Mutex, RwLock}` over `std::sync` locks for short critical
  sections in Atlas code.
- Keep critical sections small and obvious.
- Use `try_lock` only when skipping work is a valid result.
- Document lock ordering when more than one lock can be acquired in the same
  flow.

### Atomics over locks

- Use atomics for simple counters, flags, cancellation state, and generation
  tokens.
- Use the weakest ordering that is correct; defaulting everything to `SeqCst`
  should be deliberate.
- Prefer `AtomicBool`, `AtomicUsize`, and `AtomicU64` for UI-safe dirty flags and
  monotonic versions.
- Do not build complex protocols from atomics unless the invariant is simple and
  documented.

### Reduce cache misses

- Store hot fields together and cold fields separately.
- Prefer compact structs and contiguous arrays for per-entry/per-row data.
- Avoid pointer-heavy structures on large directory listings and search result
  sets.
- Use indexes into stable arrays instead of duplicating strings and paths across
  related structures.
- Prefer `Arc<[T]>` or `Vec<T>` snapshots over linked or tree structures unless
  the operation genuinely needs a tree.

### Iterator vs loops

- Use iterators when they compile to clear, allocation-free code.
- Use explicit loops when they avoid adapters, branching, temporary allocations,
  or hard-to-read borrow workarounds.
- Avoid `collect()` unless the collection is required by the next operation.
- Prefer early exits over building intermediate filtered collections.
- Measure when changing a hot loop for style.

### Inline small hot functions

- Use `#[inline]` for tiny functions on hot paths where call overhead or generic
  specialization matters.
- Avoid blanket `#[inline(always)]`; it can bloat code and hurt instruction-cache
  locality.
- Keep hot helpers small enough that inlining improves clarity and performance.

### Avoid recursion

- Prefer iterative traversal for filesystem trees, split layouts, and large view
  models.
- Use explicit stacks to avoid stack overflows and to make cancellation points
  obvious.
- Recursion is acceptable for shallow UI/layout trees when depth is bounded and
  documented.

### Minimize syscalls

- Batch filesystem metadata reads where possible.
- Reuse directory handles, network clients, and buffers.
- Avoid polling loops; use watchers, channels, debouncing, or backoff.
- Do not stat the same path repeatedly in one operation.
- Keep file-open/read/write/close cycles out of per-row rendering and search
  scoring.

## Async guidelines

### Do not block executors

- Never perform blocking filesystem, network, SQLite, JSON/TOML parsing, image
  decoding, or compression work on an async executor thread unless it is wrapped
  in the appropriate blocking mechanism.
- Atlas library crates should expose channel-based or future-based APIs and let
  the owner pick the executor.
- Slint event-loop callbacks must hand work to a worker and return quickly.

### Avoid unnecessary futures

- Do not make a function `async` when it does not `.await`.
- Avoid boxing futures on hot paths; prefer concrete futures or channels.
- Keep async state machines small by moving synchronous preparation outside the
  async block.
- Avoid spawning a task for work that can be batched with an existing worker.

### Use bounded concurrency

- Bound task counts, channel sizes, and in-flight filesystem/network operations.
- Use backpressure instead of unbounded queues.
- Make cancellation cheap and cooperative.
- Prefer worker pools for repeated homogeneous work such as thumbnails, listings,
  and transfers.

### Keep runtime ownership explicit

- Do not introduce `tokio::main` in library crates.
- All remote I/O uses the shared `atlas_remote::runtime::handle()` path.
- Daemon-owned async work belongs to `atlas-indexd`; UI-owned background work
  should cross back through channels and `slint::invoke_from_event_loop`.

### Debounce and coalesce

- Debounce search, config reloads, filesystem watcher bursts, and UI refreshes.
- Coalesce many small updates into bounded batches.
- Prefer generation tokens over trying to cancel every stale unit individually.
- Drop stale results before they reach the UI.

## Filesystem performance

### Stream directory listings

- Send entries as they are discovered in small batches instead of waiting for a
  complete `Vec`.
- Show useful partial results quickly, then refine sorting/filtering as more data
  arrives.
- Preserve cancellation so navigating away stops wasted work.

### Keep local and remote semantics separate

- Guard local-only fast paths with `Location::as_local()`.
- Do not simulate local-only APIs over a remote backend.
- Route backend-agnostic mutations through `atlas-ops` and remote streaming
  through `atlas_remote::stream`.
- Reuse pooled remote clients; never create a fresh client per row or operation.

### Avoid repeated metadata work

- Capture metadata once per entry when listing.
- Carry the metadata needed by sorting, display, filtering, and operations in the
  view model.
- Avoid repeated `metadata()`, `canonicalize()`, `read_link()`, and MIME/type
  detection calls.

### Be symlink-aware

- Avoid following symlinks by accident during recursive traversal.
- Capture symlink targets once when needed.
- Detect loops with inode/device or equivalent platform identity where recursive
  traversal can encounter cycles.

### Batch expensive updates

- Batch watcher events before refreshing a pane.
- Batch UI row changes before crossing into Slint.
- Batch remote LIST/PROPFIND responses and deduplicate repeated requests.

## UI performance

### The UI thread is sacred

- No blocking I/O, network calls, SQLite, image decoding, TOML/JSON parsing, or
  large allocations on the Slint event loop.
- UI callbacks should validate, enqueue work, and return.
- Worker results cross into the UI with `slint::invoke_from_event_loop`.

### Virtualize visible data

- Render only visible rows, cells, columns, and thumbnails.
- Keep per-row components lightweight and token-driven.
- Avoid constructing full UI models for 100k-entry listings when the viewport
  needs a tiny subset.

### Preserve persistent Slint models

- Do not replace per-pane `VecModel`s on every refresh.
- Mutate persistent models through the established synchronization helpers so
  scroll offset, focus, and selection stay stable.
- Update only changed rows when possible.

### Minimize redraws

- Avoid setting Slint properties to the same value repeatedly.
- Coalesce multiple model updates into one UI pass.
- Keep animations short and avoid animating cursor, scroll, or focus movement.
- Do not trigger full-pane updates for status text or progress changes.

### Keep image work off the event loop

- Decode, resize, and cache thumbnails on workers.
- Bound thumbnail generation threads and request queues.
- Skip or defer thumbnails for huge files and remote paths unless a bounded
  preview path exists.

## Search performance

- Debounce queries and enforce minimum query lengths.
- Cap visible results even when the backend finds many matches.
- Reuse matchers, regexes, buffers, and candidate storage.
- Avoid lowercasing or normalizing every candidate on every keystroke; precompute
  reusable forms when memory allows.
- Stream content-search results and stop promptly on cancellation.
- Keep fuzzy scoring allocation-free for each candidate.
- Do not search remote trees recursively on every input change.

## Sorting

- Prefer stable, deterministic sort keys.
- Precompute expensive keys such as lowercase names, extensions, or natural-sort
  chunks when sorting large collections repeatedly.
- Avoid allocating inside comparators.
- Keep comparators branch-light and total.
- Sort batches only when that preserves user-visible correctness; otherwise
  merge incrementally or defer until enough data arrives.

## String performance

- Prefer `&str`, `OsStr`, `Path`, and `Cow<'_, str>` over owned strings.
- Avoid lossy path conversion unless the UI needs display text.
- Use `to_string_lossy()` as late as possible, and avoid `.into_owned()` unless
  ownership is required.
- Reuse formatting buffers for repeated labels.
- Keep action IDs, backend scheme names, and config keys as borrowed/static
  strings where practical.

## Memory

- Bound every cache and queue.
- Prefer immutable snapshots for large read-mostly state.
- Release large buffers when the peak capacity is unlikely to be reused.
- Avoid retaining per-entry data that can be recomputed cheaply off the hot path.
- Track cache keys and eviction policy alongside the cache implementation.
- Watch `Arc` graphs for accidental retention of stale panes, tabs, or models.

## Data structures

- Choose `Vec` for ordered contiguous data and iteration-heavy workloads.
- Choose `HashMap`/`HashSet` for lookup-heavy workloads; pre-size when possible.
- Choose `BTreeMap`/`BTreeSet` only when sorted iteration or range queries matter.
- Use `IndexMap`-style semantics only when insertion order is required and the
  dependency/cost is justified.
- Prefer enums over stringly-typed state in Rust internals.
- Keep keys compact; avoid large `String` or `PathBuf` keys in hot maps when an
  interned ID, hash, or index can represent the same identity.

## Parallelism

- Parallelize CPU-bound work that is large enough to amortize scheduling cost.
- Do not parallelize tiny loops or UI-bound work.
- Bound worker pools to avoid starving the UI, indexer, and remote runtime.
- Prefer Rayon for CPU-bound batch work already isolated from async runtimes.
- Preserve deterministic output order when the user can see the result.
- Make cancellation and backpressure part of the design before adding workers.

## Logging

- Keep logs out of per-row, per-candidate, and per-frame hot paths unless guarded
  by level checks and sampled/aggregated.
- Prefer structured `tracing` fields over formatted strings.
- Avoid logging while holding locks.
- Do not log secrets, credentials, private paths from remote auth flows, or large
  payloads.
- Use debug/trace logs for diagnostics that can be disabled in normal runs.

## Error handling

- Use typed errors in library crates and carry operation/path context.
- Avoid allocating large error strings on success paths.
- Do not hide errors by retrying forever; retry envelopes must be bounded.
- Classify transient versus permanent remote errors so failed operations stop
  quickly.
- Keep error formatting at presentation/logging boundaries.

## Unsafe

- Avoid `unsafe` unless the safe alternative is measurably insufficient or cannot
  express the required platform/API contract.
- Every unsafe block needs a `SAFETY:` comment explaining the invariant.
- Keep unsafe blocks tiny and wrapped in safe APIs.
- Add tests for boundary conditions around unsafe code.
- Do not use unsafe to paper over ownership or lifetime issues in ordinary Atlas
  data flow.

## Code review checklist

For every function ask:

- Does this allocate?
- Does this clone?
- Does this lock?
- Does this block?
- Does this perform filesystem I/O?
- Does this wake another task?
- Does this allocate a future unnecessarily?
- Does this trigger unnecessary UI updates?
- Can this borrow instead of own?
- Can this be incremental?
- Can this be cached?
- Can this be parallelized?
- Can this be deferred until actually needed?
- Is this cache-friendly?
- Is there a simpler implementation?
- Is this on a hot path?
- Has this been measured?
