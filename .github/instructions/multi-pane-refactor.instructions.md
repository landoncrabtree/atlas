---
applyTo: "**/*.rs,**/*.slint"
description: "Design doc for Atlas's tmux-style N-pane workspace refactor. Read before touching workspace.rs, shell.rs, atlas.slint, workspace.slint, or pane.slint."
---

# Multi-pane workspace refactor

Atlas's current workspace hard-codes at most two panes (`pane0`, `pane1`), each with a fixed set of `pane0-*` / `pane1-*` Slint properties and callbacks and a `[PaneControllers; 2]` on `AppShell`. The end state is a **tmux-style tiling workspace**: arbitrary N panes arranged in a binary split tree, each pane fully independent (own tabs, own history, own view mode, own selection).

This document is the single source of truth for that refactor. Update it as decisions land.

## Goals

1. **N panes** — any number, arranged in a binary tree of horizontal / vertical splits, each split's ratio user-adjustable via drag.
2. **Independent per-pane state** — tabs, active tab, view mode, back/forward history, selection, focused index, sort spec — all owned by the pane.
3. **First-class keyboard** — split (`Cmd+D` / `Cmd+Shift+D`), navigate between panes (`Ctrl+H/J/K/L`), cycle view modes (`Cmd+Shift+E`), all reachable from `atlas-keymap`'s Dispatcher.
4. **Mouse-driven copy/move** with per-operation progress bar; drag-drop within and across panes.
5. **Zero regressions** — every existing feature (details/grid/miller/tree/gallery, tabs, palette, search, ops, bulk rename, theme, config, keymap dispatch) keeps working.

## Non-goals (explicit v0.3+ scope)

- Floating windows detached from the workspace.
- Drag-to-detach-tab into a new window.
- Panes across multiple Atlas windows.
- Zoom/maximise a single pane (tmux `Ctrl+B z`).

## Model

```rust
/// Stable per-pane identifier. Assigned on split; never reused.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(pub u32);

/// Binary tree of splits. Leaves hold `PaneId`s; each internal node holds
/// a direction and a `ratio` (0.05..=0.95) giving the first child's fraction
/// of the split axis.
#[derive(Debug, Clone)]
pub enum SplitLayout {
    Leaf(PaneId),
    Split {
        direction: SplitDirection,
        ratio: f32,
        first: Box<SplitLayout>,
        second: Box<SplitLayout>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection { Horizontal, Vertical }

/// One pane's full state. Every field a v0.2 pane could reasonably own
/// lives here.
pub struct PaneState {
    pub id: PaneId,
    pub tabs: Vec<TabModel>,           // each tab owns location + history
    pub active_tab: usize,
    pub view_mode: ViewMode,
}

pub struct TabModel {
    pub title: String,
    pub location: atlas_core::Location,   // Local(PathBuf) | Remote(RemoteUri, BackendKind)
    pub history: BackForwardStack,     // moved off NavigationController
    pub sort: SortSpec,                // per-tab overrides of the config default
    pub filter: Filter,
}

pub struct WorkspaceModel {
    pub layout: SplitLayout,
    pub panes: ahash::AHashMap<PaneId, PaneState>,
    pub focused: PaneId,
    pub next_pane_id: u32,
}
```

Operations on `WorkspaceModel`:

- `split(focused_id, dir, at_ratio) -> PaneId` — replace the focused Leaf with a Split; the new PaneId inherits the focused pane's current location + view mode (like duplicating a tmux pane).
- `close(id) -> Option<PaneId>` — remove the leaf. If its sibling was a Leaf, the sibling replaces the Split. Focus moves to the sibling (or the nearest neighbour if the sibling was itself a Split — first descendant). Refuses to remove the last pane.
- `layout_rects(bounds: Rect) -> Vec<(PaneId, Rect)>` — recursively compute screen rectangles for every leaf.
- `focus_direction(dir: Cardinal) -> Option<PaneId>` — vim-style neighbour lookup: from the focused leaf's rect, find the leaf whose rect touches in `dir` with the largest edge overlap.
- `resize_split(node_path, delta_ratio)` — adjust a specific split's ratio; used by drag handles.

Tests must cover: split of an initial single pane, deep nested splits, close-with-sibling-collapse, close-with-sibling-is-split, `layout_rects` for known shapes, `focus_direction` in a 2×2 grid.

## Rust layer refactor

### AppShell

- `panes_ctrl: [PaneControllers; 2]` becomes `panes_ctrl: parking_lot::RwLock<ahash::AHashMap<PaneId, PaneControllers>>`.
- `pane0_vm` / `pane1_vm` collapse into `vms: RwLock<AHashMap<PaneId, Arc<dyn LocationViewModel>>>`.
- Every existing pane-indexed method (`selected_paths(pane)`, `focused_pane()`, `set_view_mode(pane, mode)`, `new_tab(pane)`, `close_tab(pane, tab)`, `select_tab(pane, tab)`, `cycle_tab(pane, delta)`, `pane_location(pane)`, `set_focused_pane(pane)`, `is_dual_pane()`, `set_dual_pane(on)`) takes `PaneId` instead of `usize` or is replaced by a `focused` variant.
- New methods: `split_focused(dir)`, `close_focused()`, `focus_direction(dir)`, `cycle_view_mode()`.
- `PaneControllers` gains a `pane_id: PaneId` field.

### NavigationController

- Delete `stacks: SmallVec<[Mutex<BackForwardStack>; 2]>` — history moves onto `TabModel`.
- `navigate(pane_id, path)` looks up the pane's active tab, pushes to that tab's history, loads a fresh vm, invokes `on_location_changed`.
- `back(pane_id)` / `forward(pane_id)` operate on the active tab's stack.
- `on_location_changed` callback signature becomes `Fn(PaneId, Arc<InMemoryLocationViewModel>) + Send + Sync`.

### DispatcherActions

New default action IDs registered in `atlas-keymap` and in the Dispatcher in `main.rs`:

| ID | Default binding | Handler |
|---|---|---|
| `pane::SplitRight` | `cmd-d` | `shell.split_focused(SplitDirection::Horizontal)` |
| `pane::SplitDown` | `cmd-shift-d` | `shell.split_focused(SplitDirection::Vertical)` |
| `pane::Close` | `cmd-shift-w` | `shell.close_focused()` |
| `pane::FocusLeft` | `ctrl-h` | `shell.focus_direction(Cardinal::Left)` |
| `pane::FocusDown` | `ctrl-j` | `shell.focus_direction(Cardinal::Down)` |
| `pane::FocusUp` | `ctrl-k` | `shell.focus_direction(Cardinal::Up)` |
| `pane::FocusRight` | `ctrl-l` | `shell.focus_direction(Cardinal::Right)` |
| `view::Cycle` | `cmd-shift-e` | Rotates the focused pane's view mode Details→Grid→Gallery→Miller→Tree→Details |

## Slint layer refactor

The current `atlas.slint` has ~30 `pane0-*` properties and ~15 callbacks, mirrored for `pane1-*`. That doesn't scale. New shape:

```slint
struct PaneSlintData {
    id: int,
    x: length,
    y: length,
    width: length,
    height: length,
    is-focused: bool,
    path: string,
    // simple per-pane fields
    view-mode: string,
    active-tab: int,
    tabs: [TabEntry],
    details-selected-anchor: int,
    details-focused-index: int,
    grid-focused-index: int,
    gallery-focused-index: int,
    tree-focused-index: int,
    tree-selected-index: int,
    miller-focused-col: int,
    // metadata as a Slint sub-struct
    gallery-metadata: MetadataFields,
    gallery-preview-fallback-glyph: string,
    gallery-preview-loading: bool,
}
```

**Caveat**: Slint structs support nested primitives but *not* nested models (arrays of images, arrays of rows). Heavy per-pane data — `details-rows`, `details-columns`, `details-selected-mask`, `grid-thumbnails`, `grid-has-thumbs`, `grid-selected-mask`, `gallery-strip-thumbnails`, `gallery-preview-image`, `tree-nodes`, `miller-columns`, `path-segments` — must stay as **parallel top-level arrays** indexed by pane index in a stable, layout-order sequence.

That gives us:

```slint
export component AtlasWindow inherits Window {
    in property <[PaneSlintData]> panes;
    in property <int> focused-pane-id;

    // Parallel heavy-data arrays. Indexed by the pane's position in `panes`.
    in property <[[EntryRowItem]]> panes-details-rows;
    in property <[[ColumnSpec]]> panes-details-columns;
    in property <[[bool]]> panes-details-selected-mask;
    in property <[[image]]> panes-grid-thumbnails;
    in property <[[bool]]> panes-grid-has-thumbs;
    in property <[[bool]]> panes-grid-selected-mask;
    in property <[image]> panes-gallery-preview-image;
    in property <[[image]]> panes-gallery-strip-thumbnails;
    in property <[[TreeNode]]> panes-tree-nodes;
    in property <[[MillerColData]]> panes-miller-columns;
    in property <[[string]]> panes-path-segments;

    // Callbacks all take pane-id as their first argument.
    callback address-submitted(int /*pane-id*/, string);
    callback breadcrumb-clicked(int /*pane-id*/, int /*segment*/);
    callback tab-selected(int /*pane-id*/, int /*tab*/);
    // …and so on for every existing pane* callback.

    for pane[i] in panes: Pane {
        x: pane.x;
        y: pane.y;
        width: pane.width;
        height: pane.height;
        focused: pane.id == focused-pane-id;
        path: pane.path;
        // … reads pane and panes-*[i]
        address-submitted(p) => { root.address-submitted(pane.id, p); }
        // …
    }
}
```

The Rust side pushes `panes` in **layout order** (depth-first through the `SplitLayout` tree), and the parallel heavy arrays in the same order. Callbacks receive the semantic `pane.id`, not the array index, so operations survive layout reshuffles.

Split-handle drag: each internal split node emits a thin 4-px `TouchArea` between its children; drag adjusts `ratio` and fires `resize-split(node-index, delta)`.

## Copy/move UX

Drag-and-drop between panes maps to a `UiAction::FsCopy { source_pane, target_pane }` (F5) or `FsMove` (F6). The existing `OpsController` pipeline already surfaces progress in the ops panel — no new plumbing needed beyond the drag event → dispatch translation.

## Phased execution plan

Each phase lands as a self-contained commit that keeps the app functional.

- **Phase 0** *(landed)* — Immediate keybind wins + design doc. `Ctrl+H/J/K/L` cycle pane focus in the current 2-pane model (Left/Right work today; Up/Down are no-ops until we have a real grid). `Cmd+D` / `Cmd+Shift+D` open the 2nd pane if it isn't open (falls back to `set_dual_pane(true)`) and log a "coming soon" warning for the 3rd+ pane. `Cmd+Shift+E` cycles the focused pane's view mode.
- **Phase 1** *(landed)* — Introduced `PaneId` + `SplitLayout` in `atlas-ui/src/models/split.rs`. Rewrote `WorkspaceModel` to use it. Compat layer projects any `SplitLayout` of ≤2 leaves back onto the existing `pane0-*` / `pane1-*` Slint bindings.
- **Phase 2** *(landed — the "Location + Tab-owned history" refactor)* — Moved `BackForwardStack` and `SortSpec`/`Filter` onto `TabModel`. `TabModel.location` retyped from `PathBuf` to `atlas_core::Location`. `NavigationController` became stateless. Tab switch reuses the vm cache; back/forward operates on the active tab's history. Per-pane status bar with connection chip landed here.
- **Phase 2.5** *(landed — remote filesystems)* — Added `atlas-remote` with per-scheme submodules (`vm/sftp.rs`, `vm/ftp.rs`, `vm/webdav.rs`, `vm/s3.rs`). Introduced the Connect modal (`Cmd+K` / `Ctrl+Alt+K`), saved-servers store (`servers.toml`), connection pool, retry envelope, and OpenSSH-compatible TOFU host-key store (`known_hosts`). OpenDAL removed; each backend owns its own crate stack.
- **Phase 3** *(pending)* — Rewrite `atlas.slint` + `workspace.slint` + `pane.slint` around the `[PaneSlintData]` model. Delete the `pane0-*` / `pane1-*` compat layer. Split-handle drag lands here.
- **Phase 4** *(pending)* — Wire true `split_focused` / `close_focused`, split-handle drag ratio adjustment, and drag-drop copy/move between panes.

Phases 0–2.5 are non-invasive relative to the Slint UI shape (still ≤2 leaves). Phases 3–4 are the deep Slint changes.

## Migration hazards

- `AppShell` currently exposes `pane0`/`pane1` accessors used from `main.rs`, tests, and the Dispatcher. Every callsite needs an audit; add `#[deprecated]` on the old accessors during Phase 1 to catch stragglers.
- `atlas-keymap`'s default keymap lists explicit `cmd-1` … `cmd-9` bindings for tab switching but not for pane switching. Extend once Phase 4 lands.
- Slint struct-of-model nesting is unsupported as of 1.17 — the parallel-arrays trick above is the escape hatch. If a future Slint release adds nested-model support, collapse the parallel arrays into `PaneSlintData`.
- `atlas-ipc` and `atlas-indexd` don't care about panes — those crates need no changes.
- **`Location`, not `PathBuf`.** Every pane operation, callback, or view-model that takes a location must take `atlas_core::Location`. Local-only fast paths (thumbnails, `atlas-watch`, native trash, free-space) call `Location::as_local()` and no-op / early-return for remote. Cross-backend transfers route through `atlas-ops::execute_op` → `atlas_remote::stream::stream_copy`.
- **Cross-pane scroll preservation.** Never replace a per-pane `panes-*` Slint property with a new `VecModel::from(...)` — mutate the existing `Rc<VecModel>` in place. `OuterPaneModels::sync_vec_model` in `crates/atlas-ui/src/shell.rs` is the canonical helper; it iterates and only calls `set_row_data` when values differ, preserving each ListView's scroll offset across tab switches.
- **Modal chord routing.** Any new modal or focused text input must bubble its `input-focused` bool up to the root `FocusScope`'s `keymap-bypass-active` disjunction in `assets/ui/atlas.slint`. Otherwise Pane keymaps intercept `Cmd+A` / `Cmd+C` / `Cmd+V` and native TextInput shortcuts break. See [`ui-composition.instructions.md`](ui-composition.instructions.md) §5.
- **Remote fast-path checks.** Any callsite that used to be "compute from `PathBuf`" must be audited for local-only assumptions. Prefer explicit `match location { Local(p) => …, Remote(_, _) => early_return }` over `unwrap()` on `as_local()`.

## Verification checklist per phase

- `cargo test --workspace` still green.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- Manual smoke: launch, open a folder, switch view modes, split, close, switch tabs, hit palette, run a copy. Nothing should regress.
- Snapshot the UI in single-pane, 2-pane vertical, 2-pane horizontal, and (post-Phase 3) 4-pane 2×2 layouts.
- For any change touching remote panes: run the mock-server integration suite (`cargo test -p atlas-remote`) or verify the reason for `MOCK_SERVERS_SKIP=1`.
- Screenshot the change via `computer-use-*` MCP tools — mandatory on every UI PR.
