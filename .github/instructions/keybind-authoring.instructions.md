---
applyTo: "crates/atlas-keymap/**,crates/atlas-app/src/main.rs,docs/keymap.md,assets/keymaps/**"
description: "End-to-end workflow for adding a new keybind in Atlas. Register the action metadata, add per-OS defaults, wire the dispatcher handler, regenerate the checked-in TOMLs, update the footer if needed, and document the binding."
---

# Keybind authoring

Atlas keybinds follow one hard rule:

> **One action ID per behaviour. Chords alias to actions, never the reverse.**

This means `l`, `right`, `.`, and `enter` all bind to the single action `fs::View`. It never means `l` triggers three separate action IDs. Uphold this rule when adding new keybinds — if you find yourself wanting a chord to do two things, split the chord's two consumers into distinct actions and alias them together in the dispatcher, or unify the behaviour into one handler.

## The four registration points

Every new action lives in **exactly four** places:

1. **Metadata**: `crates/atlas-keymap/src/defaults.rs::default_actions()`.
2. **Default per-OS bindings**: `crates/atlas-keymap/src/defaults.rs::default_bindings_for(platform)`.
3. **Dispatcher handler**: `crates/atlas-app/src/main.rs::build_dispatcher()`.
4. **Documentation**: `docs/keymap.md`.

Missing any of these breaks the feature. The regen test enforces (1) + (2) round-trip; the compiler enforces (3); (4) is enforced by review.

## 1. Register the action metadata

In `crates/atlas-keymap/src/defaults.rs`, add an `action!(…)` entry inside `default_actions()`:

```rust
action!(
    "namespace::PascalCaseVerb",     // ActionId — dot/colon separated, PascalCase verb
    "Human-friendly Title",           // shown in palette, settings, footer
    Some("Long-form description.".into()),
    &["Global"]                       // contexts: "Global", "Pane", or both
),
```

**Namespaces already in use** (grep them):

- `command_palette::` — palette open/close.
- `goto::` — quick-open surfaces.
- `app::` — application-scope (settings, quit).
- `workspace::` — pane splits, focus movement, dual-pane toggle.
- `pane::` — per-pane state (view mode cycling).
- `fs::` — filesystem actions (View, Copy, Cut, Paste, Rename, Mkdir, Delete, …).
- `search::` — search panel + inline search.
- `rename::` — bulk-rename modal.
- `ops::` — operations panel + running-op controls.
- `remote::` — Connect modal, saved-servers, TOFU flow.
- `nav::` — back/forward/up.

Pick an existing namespace if it fits. Introduce a new one only when you're building a new subsystem (justify it in the PR description).

**Contexts.** `Global` bindings dispatch regardless of focus. `Pane` bindings dispatch only when the focused pane owns keyboard focus AND no modal / focused text input is bypassing (see [`ui-composition.instructions.md`](ui-composition.instructions.md) §5). Most actions are `Global`; put an action in `Pane` only when the semantics is inherently pane-local (row-level nav, view-mode cycling).

## 2. Register the default per-OS binding

Still in `crates/atlas-keymap/src/defaults.rs`, add the default chord(s) inside `default_bindings_for(platform)`. The function branches on the target OS; add the appropriate chord for each.

Rules of thumb:

- **macOS**: `Cmd+*` for global actions, `Ctrl+*` reserved for shell / terminal habits, `Shift+*` for expand/select variants.
- **Linux + Windows**: **`Ctrl+*` for global actions** (the equivalent of macOS `Cmd+*`). Reserve `Ctrl+K`, `Ctrl+F`, and other browser/editor-hot chords for their conventional uses — for instance, `remote::Connect` uses `Cmd+K` on macOS but `Ctrl+Alt+K` on Linux/Windows because browsers and editors reserve `Ctrl+K` for "focus search".
- **Chord aliases**: if the action should be reachable from multiple chords, add multiple bindings for the same action ID. Do **not** create a duplicate action ID for the second chord.
- **Modifier order**: use the canonical Slint order (`shift-`, `ctrl-`, `alt-`, `cmd-`). The regen test enforces this.

Existing landmark bindings (as of Phase 2.5):

| Action | macOS | Linux / Windows |
|---|---|---|
| `fs::Copy` / `fs::Cut` / `fs::Paste` | `cmd-c` / `cmd-x` / `cmd-v` | `ctrl-c` / `ctrl-x` / `ctrl-v` |
| `fs::Mkdir` | `cmd-shift-n` | `ctrl-shift-n` |
| `fs::Delete` | `cmd-backspace` | `delete` |
| `fs::View` | `l`, `right`, `.`, `enter` | same |
| `rename::OpenBulk` | `shift-f2` | `shift-f2` |
| `workspace::ToggleDualPane` | `cmd-\` | `ctrl-\` |
| `search::Toggle` / `search::Open` | `cmd-f` / `cmd-shift-f` | `ctrl-f` / `ctrl-shift-f` |
| `ops::TogglePanel` | `cmd-j` | `ctrl-j` |
| `remote::Connect` | `cmd-k` | `ctrl-alt-k` |

## 3. Register the dispatcher handler

In `crates/atlas-app/src/main.rs::build_dispatcher(…)`, add a match arm (or a `.register(…)` call, depending on the current shape) that dispatches the action to the shell / controller method:

```rust
dispatcher.register(
    ActionId::new("namespace::YourAction"),
    move |_ctx| shell.do_thing(),
);
```

The handler runs on the Slint event loop. If the work is I/O-bound, delegate to the feature controller which posts to a worker via `crossbeam-channel` / `atlas_remote::runtime::handle()`.

## 4. Regenerate the checked-in default keymap TOMLs

Atlas ships default keymap TOMLs under `assets/keymaps/` (one per platform). They are **derived** from `defaults.rs` and must stay byte-identical:

```bash
cargo test -p atlas-keymap regen_default_keymap -- --ignored
```

This rewrites the TOML files from the current `default_bindings_for(…)` output. Commit the resulting file changes alongside your Rust changes.

A companion test, `test_checked_in_default_toml_matches_emitter`, runs on every `cargo test` and fails if the checked-in TOML drifts from what the emitter would produce. If CI complains about that test, you forgot the regen step — run it, commit the diff.

**Local reset for users.** If a user (or you) has an old `~/.config/atlas/keymaps/default.toml` from a previous run, Atlas will not overwrite it. Remove it to pick up the new defaults:

```bash
rm -f ~/.config/atlas/keymaps/default.toml
```

## 5. Footer chip (optional)

If the new action deserves a discoverable chip in the bottom shortcut footer, add it to `FOOTER_ACTIONS` in `crates/atlas-app/src/main.rs`:

```rust
const FOOTER_ACTIONS: &[(&str, &str)] = &[
    // …existing…
    ("namespace::YourAction", "Short Label"),
];
```

The footer resolves the chip's rendered chord from the live keymap at startup and on every keymap reload, so the label follows even if the user rebinds the action. Actions with no bound chord are silently skipped (the chip disappears).

Toggle the footer via `ui.show_shortcuts` in `config.toml`.

## 6. Documentation

Add a row to `docs/keymap.md` under the correct category (Navigation / File operations / View modes / Search / Ops / Remote / Workspace / Command palette). Include:

- Both macOS and Linux/Windows chords in one row.
- A one-line description.
- The action ID, in a `code` span.

If the action is context-sensitive (Pane-only), note that in the description.

## 7. Verify

- `cargo test -p atlas-keymap` — the regen and roundtrip tests must pass.
- `cargo build --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` — the dispatcher wiring must compile clean.
- Launch the app, open the palette (`Cmd+Shift+P`), find your action by title. It should be listed and dispatch when selected.
- Press the chord in a real pane; verify the correct behaviour and the correct context (Pane vs Global).
- If you added a footer chip, verify it renders and updates the chord label on the fly if you flip a config setting or rebind.

## Anti-patterns

| Don't | Why |
|---|---|
| Reuse an existing action ID for a "similar" behaviour | One action ID = one behaviour. Add a new action; alias chords if you want the same chord to reach multiple. |
| Hard-code the chord in the dispatcher or a Slint file | Keymaps must be user-rebindable; the source of truth is `atlas-keymap`. |
| Skip the regen test | The checked-in TOMLs will drift; `test_checked_in_default_toml_matches_emitter` will fail in CI. |
| Use the `Pane` context for a global action | Pane bindings do not fire when a modal or focused text input is up. Users will report the action "sometimes doesn't work". |
| Handle the action inline in `atlas.slint` callbacks | Business logic in Slint is not user-rebindable and evades tests. Route it through the dispatcher. |

## Cross-references

- [`ui-composition.instructions.md`](ui-composition.instructions.md) — how modals + focused text inputs interact with keymap contexts.
- `crates/atlas-keymap/src/defaults.rs` — the source of truth.
- `crates/atlas-app/src/main.rs::build_dispatcher` — where handlers wire in.
- `docs/keymap.md` — user-facing keymap reference.
