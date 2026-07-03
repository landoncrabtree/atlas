# Multi-pane workspace

Atlas is a **tiling file explorer**: the window is a workspace that hosts
any number of panes, each pane fully independent. This document explains
the concepts, the layout model, and the state ownership rules that make
that work.

**Keybinds are documented in a single place: [`docs/keymap.md`](keymap.md).**
Every chord referenced here — split, focus, cycle, new tab — has its
authoritative default binding in that file (and its source-of-truth
definition in `crates/atlas-keymap/src/defaults.rs`).

## The workspace at a glance

```
┌───────────────────────────────────────────────────────────┐
│                       AtlasWindow                         │
│  ┌───────────────┬───────────────┬────────────────────┐   │
│  │  Pane 0       │  Pane 1       │  Pane 2            │   │
│  │  ┌─┬─┬─┐      │  ┌─┬─┐        │  ┌─┐               │   │
│  │  │A│B│+│  tabs│  │X│+│  tabs  │  │+│  tab bar      │   │
│  │  └─┴─┴─┘      │  └─┴─┘        │  └─┘               │   │
│  │  ~/repos      │  ~/Downloads  │  sftp://prod/logs  │   │
│  │  [ Details ]  │  [ Miller ]   │  [ Details ]       │   │
│  │  file listing │  file listing │  file listing      │   │
│  │  status bar   │  status bar   │  status bar (🟢)   │   │
│  └───────────────┴───────────────┴────────────────────┘   │
│                       ops panel                           │
└───────────────────────────────────────────────────────────┘
```

The window hosts a **workspace**. The workspace hosts an arbitrary tree
of **panes**. Each pane hosts one or more **tabs**. Each tab owns a
**location** (local or remote), a back/forward history, and its own
sort/filter overrides. Each pane picks its own **view mode** (Details,
Grid, Gallery, Miller) independently of every other pane.

---

## Concepts

### Workspace

The root container. It owns exactly one **split layout tree** (see
below), a set of **panes** keyed by stable `PaneId`, and a single
**focused pane** at any moment. Everything else — the ops queue, the
address bar, the palette — is scoped to the whole workspace rather than
to any one pane.

The workspace does **not** own tabs, location, view mode, selection,
history, or sort. Those all live on panes and tabs (see the state
ownership matrix at the end of this document).

### Pane

A visible tile in the workspace. Every pane has:

- A stable `PaneId` (`u32`) that never gets reused.
- One or more **tabs** (a `Vec<TabModel>`).
- An **active tab index**.
- A **view mode** — one of Details, Grid, Gallery, Miller.
- A **selection** — set of currently marked entries. Independent per
  pane; a drag from one pane to another triggers a copy or move.
- A **focus index** — the row / cell the arrow keys are pointing at.
  Independent from selection.
- A **status bar** — a per-pane strip at the bottom of the tile showing
  entry count, free-space (local) or connection chip (remote), view
  mode, and sort. There is no shared window-level status bar.

Panes do **not** own location, history, sort, or filter. Those live on
tabs.

### Tab

A location within a pane. Every tab remembers:

- Its **location** — an `atlas_core::Location`, which is either
  `Local(PathBuf)` or `Remote(RemoteUri, BackendKind)`.
- Its **back/forward history** — a `BackForwardStack` bounded by
  `navigation.history_size`.
- Its **sort** — column + direction, may differ from the config
  default.
- Its **filter** — hidden-file visibility override.

Switching tabs within a pane re-uses the pane's view mode and selection
model (though selection is naturally per-directory, so switching to a
different location clears the visible selection).

### Split

An internal node in the layout tree that composes two children —
either two panes, or a pane and another split, or two other splits —
along one axis. Each split has a direction (`Horizontal` = side-by-side,
`Vertical` = stacked) and a `ratio` in `[0.05, 0.95]` giving the first
child's fraction of the available axis. Users can drag the divider
between children to adjust the ratio.

### View mode

The rendering strategy for a pane's active location. Currently one of
Details, Grid, Gallery, or Miller. Every view consumes the same
`atlas_fs::LocationViewModel`, so switching modes never re-reads the
directory. View mode is a per-pane property; two panes displaying the
same directory can show it in different modes.

---

## Layout model — tmux-style binary split tree

The workspace's layout is a **binary tree of splits with panes at the
leaves**, closely inspired by tmux:

```
Workspace root
├─ Split(Horizontal, ratio=0.5)          # two side-by-side halves
│  ├─ Leaf(Pane 0)                       # left half is one pane
│  └─ Split(Vertical, ratio=0.6)         # right half is stacked
│     ├─ Leaf(Pane 1)                    #   top: one pane
│     └─ Leaf(Pane 2)                    #   bottom: one pane
```

Renders as:

```
┌────────────┬────────────┐
│            │  Pane 1    │
│  Pane 0    ├────────────┤
│            │  Pane 2    │
└────────────┴────────────┘
```

The layout model is implemented in `crates/atlas-ui/src/models/split.rs`
(the `SplitLayout` enum). Consumers of the model — the Slint renderer,
the focus-direction lookup, the drag-handle system — walk the tree
recursively.

### Layout operations

- **Split** replaces the focused leaf with a `Split { direction, ratio,
  first: <old leaf>, second: <new leaf> }`. The new pane inherits the
  focused pane's current location and view mode, like duplicating a
  tmux pane. Direction and ratio come from the requesting action
  (`pane::SplitRight` → horizontal, `pane::SplitDown` → vertical).
- **Close** removes a leaf. If its sibling is also a Leaf, the sibling
  collapses upward and replaces the enclosing Split; if the sibling is
  a Split, focus moves to that sibling's first descendant leaf. The
  workspace refuses to close the last remaining pane.
- **Layout to rectangles** walks the tree once and produces a
  `Vec<(PaneId, Rect)>` in depth-first, layout order. That order drives
  both the Slint `for` loop and the parallel per-pane arrays for heavy
  data (row lists, thumbnails, miller columns). Callbacks always
  identify their target by the semantic `PaneId`, not by position in
  the array — so operations survive layout reshuffles.

### Non-goals

- Floating panes detached from the workspace.
- Panes shared across multiple Atlas windows.
- Zoom / maximise a single pane (tmux's `Ctrl+B z`).
- Drag-to-detach a tab into its own window.

These are explicitly out of scope for the current design; a future v0.3+
may revisit.

---

## Focus model

Exactly one pane is **focused** at any moment. Focus determines which
pane keybinds like `j` / `k` / `Enter` / `Space` / `F2` operate on, and
which pane is targeted when the palette dispatches an action.

### Which pane owns which state

Focus is a workspace property (the `focused: PaneId` field on the
workspace model), not a pane property. When focus moves:

- Keyboard input stays with the new pane's rendered view.
- The address bar and breadcrumbs reflect the new pane's active tab
  location.
- The status bar's summary follows the newly focused pane (per-pane
  status bars stay visible for un-focused panes; the focused pane's
  bar is highlighted).
- **Selection does not move.** Each pane keeps its own selection set.

### How focus moves

Two orthogonal mechanisms:

- **Click a pane** — the pane's root `TouchArea` fires a focus request,
  and the workspace updates `focused` to that pane's `PaneId`.
- **Directional focus (`pane::FocusLeft` / `Right` / `Up` / `Down`)** —
  a vim-style neighbour lookup. From the focused leaf's rectangle,
  Atlas finds the leaf whose rect touches in the requested direction
  with the largest edge overlap, and focuses that leaf.

The address-bar `TextInput` inside each pane has its own focus state
independent of pane focus. When a text input owns focus, the dispatcher
runs a keymap-bypass gate so `Cmd+A` / `Cmd+C` / `Cmd+V` and arrow keys
operate on the input rather than the pane's row list. See
[`docs/keymap.md`](keymap.md#chord-routing-while-modals-or-text-inputs-are-focused)
for the routing details.

---

## Tabs

Each pane owns an independent tab stack — a `Vec<TabModel>` plus an
`active_tab: usize`. Tabs cannot migrate between panes.

- **Per-tab state**: location, back/forward history, sort spec, filter.
- **Per-pane state (shared across tabs)**: view mode, selection model,
  focus index, status-bar readout.

Because view mode is per-pane and not per-tab, cycling view modes on a
pane affects every tab in that pane. This matches the mental model of
"a pane is a lens; a tab is a place" — you carry your lens with you
when you switch tabs.

Reopening the most recently closed tab is a global action (it applies
to the focused pane); tab cycling and direct tab-selection chords also
target the focused pane. All of these are documented in
[`docs/keymap.md`](keymap.md#tabs).

---

## Local vs remote panes

A pane's tabs each hold an `atlas_core::Location`:

```rust
pub enum Location {
    Local(PathBuf),
    Remote(RemoteUri, BackendKind),
}
```

Any pane can host any backend. In a three-pane layout you might have
one local pane on `~/repos`, one SFTP pane on `sftp://prod/logs`, and
one S3 pane on `s3://my-bucket/`, all at once. The workspace, the ops
queue, and the drag-drop routing treat those panes uniformly.

### Supported backends

| Backend | URL scheme | Implementation |
|---|---|---|
| Local | (no scheme — native paths) | OS filesystem |
| SFTP | `sftp://user@host[:port]/path` | `atlas_remote::vm::sftp` (`russh` + `russh-sftp`) |
| FTP / FTPS | `ftp://user@host[:port]/path` | `atlas_remote::vm::ftp` (`suppaftp`) |
| WebDAV | `webdav://user@host[:port]/path` | `atlas_remote::vm::webdav` (`reqwest` + `quick-xml`) |
| S3 (and compatibles) | `s3://bucket[/prefix]` | `atlas_remote::vm::s3` (`object_store`) |

Every backend owns its full stack (connection, listing, streaming,
retry, error mapping). **OpenDAL was removed in Phase 2.3.5** — there
is no unified remote-fs abstraction. See
[`.github/instructions/remote-backend-authoring.instructions.md`](../.github/instructions/remote-backend-authoring.instructions.md)
for the workflow to add a new backend.

### Local-only fast paths

Some operations only make sense against a native `PathBuf`:

- **Thumbnails** (`atlas-thumbs`) — image decode + SQLite cache keyed
  on inode + mtime.
- **`notify` watcher** (`atlas-watch`) — kernel-level filesystem
  events; there is no equivalent for a remote backend.
- **Native trash** — moves a file into the OS-integrated recycle bin
  via the `trash` crate.
- **Free-space queries** — `statvfs` / `GetDiskFreeSpaceEx`.
- **Memory-mapped reads** — `mmap` for large preview loads.

Each of these callsites guards with `Location::as_local()` and
short-circuits for `Remote(_)`. The pane's status bar hides the free-
space chip for remote tabs and instead shows the connection chip; the
context menu hides "Move to Trash" for remote panes (a real delete is
offered instead); the thumbnail generator skips remote paths and lets
the view render an icon glyph.

### Cross-backend copy and move

Copy and move operations route through `atlas_ops::execute_op`, which
picks the right transfer strategy based on the source and destination
`BackendKind`:

- **Local → Local**: native `rename` when the paths are on the same
  volume; buffered `std::fs::copy` otherwise; native trash for delete.
- **Local ↔ Remote or Remote ↔ Remote**: chunked streaming through
  `atlas_remote::stream::stream_copy`, which reads from the source
  reader and writes into the destination writer on the shared tokio
  runtime (`atlas_remote::runtime::handle()`). Progress and cancellation
  cross backend boundaries transparently.

Drag-and-drop from one pane onto another emits a copy or move action
(same idiom as file managers everywhere). The clipboard-based
`fs::Copy` / `fs::Cut` / `fs::Paste` chords work identically across
backends: cut a file from an SFTP pane, paste into a local pane, and
the ops queue streams the transfer with progress.

### Remote pane lifecycle: pool, retry, TOFU

Multiple panes / tabs pointing at the same remote host share a single
backend client via `atlas_remote::pool::ConnectionPool`. Idle clients
evict after a TTL, so a quick navigate-back reuses the still-
authenticated session, but a truly dormant tab does not hold a socket
open indefinitely.

Every network round-trip is wrapped by `atlas_remote::retry::Retry` —
transient network errors retry with exponential backoff + jitter; auth
/ not-found / already-exists errors propagate immediately.

The first time an SFTP tab reaches a host whose key is not in
`~/.config/atlas/known_hosts`, Atlas raises a TOFU banner in the
Connect modal showing the offered fingerprint. On Accept, the key is
persisted in OpenSSH format; subsequent tabs to the same host verify
against that store. A fingerprint change raises a red warning banner
and refuses the connection until the user explicitly re-accepts.

Credentials never touch `~/.config/atlas/servers.toml` — only opaque
`credential_ref` handles. The actual secrets live in the OS keychain
(macOS Keychain, `libsecret`, Windows Credential Manager) under the
`com.atlas.credentials` namespace.

### Per-pane connection chip

Each remote pane's status bar renders a **connection chip**:

- 🟢 **connected** — the backend client is healthy.
- 🟡 **reconnecting…** — the retry envelope is backing off between
  attempts on a transient network failure.
- 🔴 **disconnected** — auth failed, TLS/host key rejected, or all
  retries exhausted.

Clicking the chip opens the Connect modal pre-filled with the pane's
connection settings so the user can adjust credentials or endpoint.

---

## Interaction with the ops panel

The **ops panel** is a single workspace-level tray docked at the bottom
of the window. Every filesystem operation — from every pane — funnels
into one shared ops queue in `atlas-ops`. Operations are processed by a
bounded worker pool; the panel shows one row per active operation with
progress, cancel button, and source / destination.

Consequences:

- A cross-pane copy from Pane 0 into Pane 1 shows up in exactly one ops
  row, not one per pane.
- Foreground operations that exceed ~250 ms (`FOREGROUND_DEFER` in
  `atlas-ui::ops::controller`) also raise a centered operation-
  progress modal. Hitting **Background** dismisses the modal but keeps
  the op running under the panel.
- Cancellation is cooperative: the `CancellationToken` associated with
  the op is fired, and workers unwind at the next await point.
  Partially-transferred data is left in a documented "partial" state.

There is intentionally no per-pane ops queue; splitting the queue by
pane would double the accounting cost with no user-facing benefit.

---

## Cross-pane scroll preservation

Scrolling one pane never scrolls the other. Under the hood, every
`panes-*` Slint property (the parallel per-pane arrays: details rows,
grid thumbnails, miller columns, path segments, etc.) is backed by a
persistent `Rc<VecModel>` bound **once** at startup. On subsequent
refreshes the `OuterPaneModels::sync_vec_model` helper in
`crates/atlas-ui/src/shell.rs` iterates through the current entries and
only calls `set_row_data` on rows whose value actually changed.

Replacing the outer model (`VecModel::from(new_rows)`) would silently
detach the `ListView` from its previous model and reset the scroll
offset to 0 — users would lose their place every time a directory
refreshed or a tab was switched. The `sync_vec_model` helper is the
single-source-of-truth workaround; any new per-pane property must
follow the same pattern.

---

## Configuration recipes

All settings below go in `~/.config/atlas/config.toml`. Every option is
optional; `[serde(default)]` fills in reasonable defaults for anything
you don't specify.

### Start with a single pane

```toml
[general]
dual_pane = false
```

### Start with a side-by-side split (default)

```toml
[general]
dual_pane = true
```

### Increase back/forward history depth

```toml
[navigation]
history_size = 200   # default: 100
```

### Set default view mode

```toml
[view]
default_mode = "miller"   # details | grid | gallery | miller
```

### Show hidden files by default

```toml
[view]
show_hidden = true
```

### Tune search-panel responsiveness

Atlas debounces the search-panel query, requires a minimum length before
it hits the index, and caps the number of rendered rows to keep the
`ListView` responsive on very common queries. All three apply to remote
panes too (see [`.github/instructions/performance.instructions.md`](../.github/instructions/performance.instructions.md)
§8).

```toml
[search]
min_query_length    = 2     # default: 2  — chars before the query fires
max_visible_results = 100   # default: 100 — hard cap on rendered rows
debounce_ms         = 150   # default: 150 — quiet time before submit
```

### Tune thumbnail generation

```toml
[thumbnails]
cache_max_size_mb  = 1000   # default: 500
generation_threads = 4      # default: num_cpus clamped to 4
```

---

## State ownership matrix

Which layer owns which piece of state? This is the reference table.

| State | Owner | Notes |
|---|---|---|
| Split layout tree | Workspace | Binary `SplitLayout` in `crates/atlas-ui/src/models/split.rs` |
| Focused `PaneId` | Workspace | Exactly one focused pane |
| Ops queue | Workspace | Single shared queue across every pane |
| Address bar visible text | Workspace | Mirrors the focused pane's active-tab location |
| Palette state | Workspace | Global palette |
| Search panel state | Workspace | Applies to the focused pane |
| Tab stack | Pane | `Vec<TabModel>` per pane |
| Active tab index | Pane | `usize` per pane |
| View mode | Pane | Details / Grid / Gallery / Miller |
| Selection set | Pane | Independent per pane |
| Focus row / cell | Pane | Cursor position within the current listing |
| Status bar chip | Pane | Local free-space or remote connection state |
| Location | Tab | `atlas_core::Location` — Local or Remote |
| Back/forward history | Tab | Per-tab `BackForwardStack` |
| Sort spec | Tab | Column + direction, overrides `[view].default_sort` |
| Filter | Tab | Hidden-file visibility override |

When adding a new stateful field, pick its owner from this table's
grain: is it a workspace concept, a pane concept, or a tab concept? A
new field's owner is often the same as an existing similar field —
grep for the closest analog before inventing a new level.

---

## Where to look in the code

- `crates/atlas-ui/src/models/split.rs` — `PaneId`, `SplitLayout`,
  `SplitDirection`, layout algebra.
- `crates/atlas-ui/src/models/workspace.rs` — `WorkspaceModel`,
  `PaneState`, `TabModel`.
- `crates/atlas-ui/src/shell.rs` — `AppShell` adapter, per-pane
  callback wiring, `OuterPaneModels::sync_vec_model`.
- `crates/atlas-ui/src/navigation/controller.rs` — stateless navigator
  that pushes into the active tab's `BackForwardStack`.
- `crates/atlas-remote/src/vm/{sftp,ftp,webdav,s3}.rs` — per-scheme
  backend implementations.
- `assets/ui/atlas.slint` + `assets/ui/pane-data.slint` — Slint-side
  `PaneSlintData` model and the parallel per-pane arrays.

See also:

- [`docs/keymap.md`](keymap.md) — all default keybinds, DSL, chord
  routing.
- [`docs/developer-setup.md`](developer-setup.md) — toolchain,
  nextest, MCP tooling.
- [`.github/instructions/architecture.instructions.md`](../.github/instructions/architecture.instructions.md)
  — crate boundaries, process model, threading, storage.
- [`.github/instructions/ui-composition.instructions.md`](../.github/instructions/ui-composition.instructions.md)
  — canonical flow for adding a modal, panel, view mode, or context
  menu.
- [`.github/instructions/remote-backend-authoring.instructions.md`](../.github/instructions/remote-backend-authoring.instructions.md)
  — end-to-end workflow for adding a new remote backend.
