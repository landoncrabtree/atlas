---
name: add-ui-surface
description: Guide for adding a new UI surface in Atlas (a modal, panel, view mode, or context menu). Use when the request involves creating or extending anything visible in the Slint window (assets/ui/*.slint) or its Rust controller under crates/atlas-ui/src/.
---

Atlas has a single canonical flow for every new UI surface. Do not improvise; follow the flow in [`.github/instructions/ui-composition.instructions.md`](../../instructions/ui-composition.instructions.md).

## Before you write any code

1. Read `assets/ui/theme.slint` — every visible property must come from `Theme.*`, never hex or pixel literals.
2. Read the components in `assets/ui/components/` (bulk-rename, command-palette, connect-server, operation-progress, ops-panel, pane, search-panel). One of them is almost certainly the closest analog to what you are building — copy its shape.
3. Read `.github/instructions/design.instructions.md` and `.github/instructions/ui-composition.instructions.md` in full.

## The canonical flow

For a **new modal**:

1. Component file under `assets/ui/components/<name>.slint` (modals live in `components/`, there is no `panels/` directory).
2. Scrim + centered rounded rectangle, all tokens from `Theme.*`.
3. `Escape` closes the modal.
4. Auto-focus the primary input on open (`changed open => { text-input.focus(); }`).
5. Expose an `input-focused` bool. `atlas.slint` mirrors it into a root property joined into `keymap-bypass-active` and adds the modal's visibility flag to `any-modal-visible`.
6. Add a Rust controller under `crates/atlas-ui/src/<feature>/` with `mod.rs` + `controller.rs`.
7. Wire callbacks via `on_<something>` closures in `crates/atlas-ui/src/shell.rs::wire_callbacks`.
8. **Screenshot the change via the `computer-use-*` MCP tools before opening the PR.**

For a **new context menu item**: extend the existing right-click flow, do NOT create a parallel menu. Row `TouchArea` emits `context-menu(x, y)` in window-absolute coordinates (`self.absolute-position.x + self.mouse-x`), the shell records `context_menu_target` before showing, and the `ctx-*` callback dispatches through the existing action system.

For a **new keybind that shows the surface**: follow [`.github/instructions/keybind-authoring.instructions.md`](../../instructions/keybind-authoring.instructions.md) — action metadata in `crates/atlas-keymap/src/defaults.rs`, per-OS defaults, dispatcher handler in `crates/atlas-app/src/main.rs::build_dispatcher`, regen TOMLs, optional `FOOTER_ACTIONS` chip, update `docs/keymap.md`.

## The ONE canonical chord routing pattern

Every text-input-bearing surface uses the same pattern so `Cmd+A` / `Cmd+C` / `Cmd+V` behave natively while an input owns focus:

- Root `FocusScope` in `atlas.slint` computes `keymap-bypass-active = any-modal-visible || text-focus-pane-id != -1 || <every modal's input-focused bool>`.
- When true, the Rust dispatcher restricts to `[Global]` context; Pane bindings return false; keys fall through to the focused TextInput.
- Modals bubble `input-focused` up via a `changed input-focused` handler on the parent. The address bar mirrors this via `text-focus-pane-id`.

Do not invent a new state channel. Extend the disjunction.

## Verify

- `cargo build --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo nextest run --workspace --retries 3` all green.
- Live-verify with the `computer-use-*` MCP tools: `take_screenshot` before + after, `send_keybind` to exercise the new chord, `type_text` in the input.
- Attach the MCP screenshot to the PR — mandatory for any UI change.

Read the full instruction file for the exhaustive checklist.
