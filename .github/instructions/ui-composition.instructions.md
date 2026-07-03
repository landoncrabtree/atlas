---
applyTo: "**/*.slint,crates/atlas-ui/**/*.rs,crates/atlas-app/**/*.rs,assets/ui/**"
description: "Canonical flow for adding a new modal, panel, view mode, context menu, or keybind in Atlas. Ensure new UI surfaces are consistent with existing ones. Read before any UI PR."
---

# UI composition

Every new UI surface in Atlas — modal, panel, view mode, context menu, palette entry, chip — must follow the same shape as the existing ones. The rule is **converge, don't diverge**: search for an existing helper before writing a new one, and match the tokens, chord routing, and controller patterns already in the tree.

This file is the source of truth for that flow. It replaces improvised approaches; if reality drifts from what's here, fix the code or fix this file — never both diverge silently.

## 1. Read-first checklist

Before writing a single Slint or Rust line for a new surface, read:

- `assets/ui/theme.slint` — every token you will consume (`Theme.bg`, `Theme.accent`, `Theme.space_4`, `Theme.radius_lg`, …).
- `assets/ui/components/` — reusable widgets (`atlas-controls.slint`, `address-bar.slint`, `bulk-rename.slint`, `command-palette.slint`, `connect-server.slint`, `operation-progress.slint`, `ops-panel.slint`, `right-dock-panel.slint`, `pane.slint`, `search-panel.slint`, `shortcut-footer.slint`, `tab-bar.slint`, `titlebar.slint`, `breadcrumbs.slint`).
- The **closest existing surface** to what you are adding — a modal that is 80% like yours already exists in most cases.
- `.github/instructions/design.instructions.md` — HIG-derived tokens and component grammar. **Every visible property comes from `Theme.*`; no hex, no pixel literals.**

Convergence rule, stated as a directive:

> **Before adding a new helper, component, or state channel, grep the codebase for one that already does the job.** If two components share behaviour, share a sub-component. If two states carry the same meaning, share a property.

## 2. File layout convention

Slint components live under `assets/ui/`:

- `assets/ui/atlas.slint` — the root window. **Minimise touches here** to reduce merge friction; wire callbacks in and out, but keep component internals inside the imported files.
- `assets/ui/theme.slint` — the global `Theme` singleton.
- `assets/ui/pane-data.slint` — shared per-pane structs.
- `assets/ui/components/` — reusable widgets **and modals**. Modals live here (bulk-rename, command-palette, connect-server, operation-progress, search-panel are all in `components/`). Despite historical prose that talked about an `assets/ui/panels/` directory, that directory does not exist — everything goes under `components/`.
- `assets/ui/views/{details,grid,gallery,miller}/` — per-view-mode rendering; each has its own subdirectory.

Rust-side controllers live under `crates/atlas-ui/src/<feature>/`:

- One directory per feature (e.g. `palette/`, `search/`, `remote/`, `ops/`, `rename/`, `navigation/`).
- Split `mod.rs` (types + facade) from `controller.rs` (per-session state, event handling).
- The controller owns any `parking_lot::RwLock`, background senders, and lifecycle. `mod.rs` reexports only what `shell.rs` needs.

`crates/atlas-ui/src/shell.rs` is where callbacks are wired via `wire_callbacks`. Keep the wiring dense — one `on_*` closure per Slint callback — and push logic into the feature controller.

## 2.5 Shared modal/menu control library

Before adding or styling a modal/menu control, import from `assets/ui/components/atlas-controls.slint`:

```slint
import {
    AtlasFieldGroup, AtlasList, AtlasListRow, AtlasModal, AtlasPrimaryButton,
    AtlasProgressBar, AtlasSecondaryButton, AtlasSegmentedControl, AtlasTextField,
    SectionLabel,
} from "atlas-controls.slint";
```

Composition rules:

- Use `AtlasModal` for top-level sheet chrome (scrim, 12 px radius, soft shadow, click-outside dismissal).
- Use `AtlasFieldGroup { SectionLabel { ... } AtlasTextField { ... } }` for compact form rows.
- Use `AtlasSegmentedControl` for backend/auth pickers; selected state is neutral, not accent.
- Use exactly one `AtlasPrimaryButton` per modal when there is a true default action. All other actions use `AtlasSecondaryButton`.
- Use `AtlasProgressBar` for operation progress in both modal and right-dock rows.
- Use `AtlasList` / `AtlasListRow` for inset saved-server and operations lists.

Do not duplicate these components with local `Rectangle` styles. If a component needs a new visual affordance, extend `atlas-controls.slint` and the semantic tokens in `theme.slint` first.

## 2.6 Shared right-side dock

Search and Operations share exactly one right-side slot: `RightDockPanel` in `assets/ui/components/right-dock-panel.slint`.

- `atlas.slint` owns one `right-dock-surface` property (`0` hidden, `1` Search, `2` Ops). Do not add parallel `search_visible` + `ops_visible` UI booleans.
- `AppShell` owns the matching `RightDockSurface` state and exposes `toggle_search_panel`, `open_search_panel`, `toggle_ops_panel`, and `close_right_dock`.
- `Cmd+F`, `Cmd+J`, and `Escape` route through `atlas-keymap`; do not hardcode right-dock shortcut handling in Slint.
- The content variants live in `search-panel.slint` and `ops-panel.slint`; they should be transparent dock contents, not separate dock shells.

## 3. Adding a new modal — canonical steps

A modal is a Slint component under `assets/ui/components/` that:

1. **Backdrop.** Use `AtlasModal` / `Theme.modal-scrim`; do not hand-roll a new scrim.
2. **Panel.** Use `AtlasModal`; modal sheets use `Theme.modal-radius` (12 px) and fit content in the 420–620 px range unless the existing surface requires more.
3. **Tokens only.** Every colour, radius, spacing, font, and duration is `Theme.*`. If a needed token doesn't exist, add it to `theme.slint` (both dark and light schemes) first.
4. **Escape closes.** Bind `Escape` in a local `FocusScope` to fire `close()`; the caller in `atlas.slint` toggles the modal's visibility off.
5. **Auto-focus the primary input on open.** Use `AtlasTextField { auto-focus: true; }` or the existing `input-focused` mechanism. Reference: `connect-server.slint`.
6. **Bubble `input-focused` up.** The modal must expose a `property <bool> input-focused` that reflects whichever internal `TextInput.has-focus` is currently active. The parent (`atlas.slint`) mirrors this into a root-level bool joined into `keymap-bypass-active`:

   ```slint
   // In atlas.slint, next to `connect-modal-input-focused`:
   property <bool> your-modal-input-focused: false;

   YourModal {
       changed input-focused => {
           root.your-modal-input-focused = self.input-focused;
       }
   }
   ```

   Then extend the disjunction:

   ```slint
   property <bool> keymap-bypass-active:
       any-modal-visible
       || text-focus-pane-id != -1
       || root.connect-modal-input-focused
       || root.your-modal-input-focused;
   ```

   And extend `any-modal-visible`:

   ```slint
   property <bool> any-modal-visible:
       root.palette-visible || root.bulk-rename-visible || root.op-modal-visible
       || root.connect-modal-visible || root.your-modal-visible;
   ```

   There is no `ActiveModal` enum — Atlas uses per-modal `*-visible` booleans OR'd into `any-modal-visible`. Do not invent a parallel modal enum. The shared right dock is the exception and uses `RightDockSurface` because Search/Ops must be mutually exclusive.
7. **Rust controller.** New feature directory under `crates/atlas-ui/src/<feature>/` with `mod.rs` + `controller.rs`. The controller holds per-session state (`parking_lot::RwLock<…>`), spawns any background work through the shared `atlas_remote::runtime::handle()` if it needs tokio, and exposes typed methods (`open()`, `close()`, `submit(…)`).
8. **Wire callbacks in `shell.rs::wire_callbacks`.** One `on_<something>` closure per Slint callback. Keep the closure body a one-liner that delegates to the controller.
9. **Live-verify via MCP.** Screenshot the new modal open, escape-closed, tab-navigated, and with a submit path exercised. See `docs/developer-setup.md` §MCP.

### macOS-native visual checks

For modal/menu PRs, verify the following before screenshots:

- No uppercase form labels; use sentence case via `SectionLabel`.
- Text fields/buttons/segmented controls are compact (~29 px high).
- Segmented controls are contiguous with dividers; no gapped button rows.
- Accent appears only on the true primary CTA and progress fill.
- Interior cards/lists have no drop shadow.
- Modal radius is larger than button/input radius.

## 4. Adding a new context menu

**Do not create a parallel menu system.** Extend the existing right-click / Menu-key flow.

- **Row TouchArea** in `assets/ui/views/details/row.slint` (or the equivalent per-view) emits `context-menu(x, y)` in **window-absolute coordinates**:

  ```slint
  root.context-menu(
      self.absolute-position.x + self.mouse-x,
      self.absolute-position.y + self.mouse-y,
  );
  ```

  This is required so the `ContextMenuArea.show()` at the window root paints the menu at the pointer, not at the row's top-left.

- The view's `context-menu` callback bubbles up to `atlas.slint`, which fires `details-row-context-menu(pane-id, index, x, y)` (or `grid-entry-context-menu`, …). Rust receives these signals and, before showing the menu, records the target:

  ```rust
  *self.context_menu_target.write() = Some((pane, path));
  ```

  See `crates/atlas-ui/src/shell.rs::set_context_menu_target` (~line 1333) and `context_menu_target()` accessor (~line 1352). Every `ctx-*` handler reads this before acting.

- **Adding a menu item** means: add a `MenuItem` in the shared context-menu block in `atlas.slint`; add a `callback ctx-<action>` and route it to the existing action dispatcher (do not create a bespoke path). The action must exist in `atlas-keymap::defaults::default_actions()` — new actions follow [`keybind-authoring.instructions.md`](keybind-authoring.instructions.md).

## 5. Modal ↔ pane chord routing — the ONE canonical pattern

This section codifies the single pattern that every text-input-bearing surface (address bars, modal inputs, search fields) must follow so `Cmd+A` / `Cmd+C` / `Cmd+V` behave natively while the input has focus.

### The invariants

1. There is **one** root `FocusScope` in `assets/ui/atlas.slint` that owns Pane-context keymap dispatch.
2. That `FocusScope` computes a single `keymap-bypass-active: bool` from:
   - `any-modal-visible` (any modal is up), OR
   - `text-focus-pane-id != -1` (a pane's address bar has focus), OR
   - each modal-specific `*-input-focused` bool bubbled up from a modal component.
3. When `keymap-bypass-active` is true, the Rust dispatcher (see `handle-key-chord` in `atlas.slint` and the dispatcher wiring in `crates/atlas-app/src/main.rs::build_dispatcher`) restricts to the `[Global]` context only. Pane-context bindings return `false`, so the key falls through to the focused `TextInput` and OS-native chords take effect.

### The plumbing

- **Per-pane text-input focus.** Each `Pane` exposes `property <bool> text-input-focused` that mirrors its address bar's `has-focus`. `atlas.slint` records the focused pane's id:

  ```slint
  changed text-input-focused => {
      if self.text-input-focused {
          text-focus-pane-id = pane.id;
      } else if text-focus-pane-id == pane.id {
          text-focus-pane-id = -1;
      }
  }
  ```

  The **anti-drift** part is the `else if` — blur only clears the id when the id matches the current focused pane, so a rapid focus-swap between two panes doesn't zero out a still-focused one.

- **Per-modal input focus.** Each modal exposes `input-focused: bool` and the parent mirrors it into a root-level bool that joins `keymap-bypass-active` (see §3 step 6).

### Reference implementations

- `crates/atlas-ui/src/palette/controller.rs` + `assets/ui/components/command-palette.slint`.
- `crates/atlas-ui/src/remote/…` + `assets/ui/components/connect-server.slint` (with `changed input-focused => { root.connect-modal-input-focused = self.input-focused; }` at the bottom).

If you find yourself adding a new state channel or a new keymap-context switch, **stop** — you're diverging from the pattern. Extend the disjunction instead.

## 6. Adding a new keybind

Split out into its own file: [`keybind-authoring.instructions.md`](keybind-authoring.instructions.md). TL;DR:

1. Register `ActionMeta` in `crates/atlas-keymap/src/defaults.rs::default_actions()`.
2. Register default per-OS binding via `default_bindings_for(platform)` in the same file.
3. Register the dispatcher handler in `crates/atlas-app/src/main.rs::build_dispatcher`.
4. Regen the checked-in TOMLs: `cargo test -p atlas-keymap regen_default_keymap -- --ignored`.
5. Add the action to `FOOTER_ACTIONS` in `main.rs` if it should show in the bottom footer.
6. Document it in `docs/keymap.md`.

Read [`keybind-authoring.instructions.md`](keybind-authoring.instructions.md) for the full walkthrough.

## 7. Cross-pane scroll preservation

**Never** replace a per-pane Slint model on every refresh. That silently detaches each `ListView` from its previous model and resets scroll offset to 0.

### The correct pattern

- Every `panes-*` array on `AtlasWindow` (e.g. `panes-details-rows`, `panes-grid-thumbnails`, `panes-miller-columns`, `panes-path-segments`) is backed by a **persistent** `Rc<VecModel<T>>` on the Rust side.
- The struct `OuterPaneModels` in `crates/atlas-ui/src/shell.rs` owns each of these `Rc<VecModel>`s. It calls `ensure_bound()` once at startup to bind them to the Slint properties.
- On every subsequent update, call `OuterPaneModels::sync_vec_model` (~`shell.rs:538`) which iterates through the current entries and only calls `set_row_data` on rows that actually changed. It does not swap the model.

### The bug this prevents

If you write `window.set_panes_details_rows(ModelRc::new(VecModel::from(new_rows)));` on every tick, the ListView drops its scroll offset and jumps to the top. Users lose their place every time a directory is refreshed or a tab is switched. The `sync_vec_model` helper is the single-source-of-truth workaround.

## 8. Location refactor gotchas

Pane locations, tab locations, and every callback signature that used to be `PathBuf` are now `atlas_core::Location`:

```rust
pub enum Location {
    Local(PathBuf),
    Remote(RemoteUri, BackendKind),
}
```

`BackendKind` variants: `Local`, `Sftp`, `Ftp`, `WebDav`, `S3`.

### Guardrails

- **Local-only fast paths** must call `Location::as_local() -> Option<&Path>` and short-circuit (return `None`, log at debug, skip the work) for `Remote(_)`. The current list of local-only fast paths includes: thumbnails (`atlas-thumbs`), native trash, `notify` watcher (`atlas-watch`), free-space queries, memory-mapped reads.
- **Backend-agnostic ops** go through `atlas-ops::execute_op`. Cross-backend copies (local ↔ remote, remote ↔ remote) stream through `atlas_remote::stream::stream_copy`. Do not attempt to `std::fs::copy` between a local and remote path.
- **Symlinks** and other OS-specific concepts do not exist for remote backends; guard with `as_local()` or handle the `Remote` variant explicitly.

## 9. Progress and cancellation for long-running ops

Adopted contract:

- **Under ~250 ms** foreground duration (`FOREGROUND_DEFER` in `crates/atlas-ui/src/ops/controller.rs`): no modal. A status toast is sufficient.
- **≥ 250 ms**: show the operation-progress modal (`assets/ui/components/operation-progress.slint`) centered on the workspace. It composes `AtlasModal`, `AtlasProgressBar`, and secondary buttons.
- The modal has two buttons:
  - **Cancel** — fires the `CancellationToken` associated with the op; `atlas-ops` propagates the cancel to workers; partially-transferred data is left in a documented "partial" state.
  - **Background** — dismisses the modal but keeps the op running under the ops panel (`assets/ui/components/ops-panel.slint`). Users can reopen the modal from the ops panel.

Any op that could exceed 250 ms **must** integrate the cancellation token from the start; retrofitting later is expensive.

## 10. Testing conventions

- **Unit tests** live inline under `#[cfg(test)] mod tests { … }` next to the code they cover. One test per behavior; behavior-focused names.
- **Integration tests** live under `crates/<crate>/tests/`. Use `tempfile::TempDir` for filesystem fixtures.
- **Mock servers** (SFTP, FTP, WebDAV, S3) spawn via `crates/atlas-remote/tests/common/mock.rs`. Skip with `MOCK_SERVERS_SKIP=1` when you have no Python + `uv` available. See `docs/developer-setup.md` §Mock servers.
- **Live UI verification** via the `computer-use-*` MCP tools (screenshots, keybind sequencing, click, type). Every UI PR ships with a screenshot. See `docs/developer-setup.md` §computer-use MCP.
- **Snapshot tests** where appropriate — Slint doesn't ship native snapshot testing yet; take a screenshot before + after and eyeball it, or use `insta` at the Rust boundary.

## Verification checklist before you open a PR

- [ ] Only tokens (`Theme.*`) — no hex, no ad-hoc fonts; new modal/menu chrome reuses `atlas-controls.slint`.
- [ ] macOS-native controls: compact heights, sentence-case labels, neutral segmented selection, restrained accent.
- [ ] Modal registers into `any-modal-visible` if it's a modal.
- [ ] Every internal `TextInput` bubbles its focus up to the root's `keymap-bypass-active` disjunction (or reuses `text-focus-pane-id` if it's a pane input).
- [ ] Rust controller lives under `crates/atlas-ui/src/<feature>/` with `mod.rs` + `controller.rs`.
- [ ] Callbacks are wired in `shell.rs::wire_callbacks`, one `on_*` per callback.
- [ ] Any operation that could exceed 250 ms uses the `FOREGROUND_DEFER` op-modal path with a `CancellationToken`.
- [ ] Cross-pane scroll preserved: every `panes-*` update goes through `OuterPaneModels::sync_vec_model`.
- [ ] Any new action ID lives in `atlas-keymap::defaults::default_actions()` and has a handler in `build_dispatcher`.
- [ ] `cargo fmt --all` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo nextest run --workspace --retries 3` pass.
- [ ] Live MCP screenshot attached to the PR.
