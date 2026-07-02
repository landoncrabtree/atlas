---
name: add-keybind
description: Guide for adding or modifying a keybinding in Atlas. Use when the request is to register a new action, change a default chord, add a per-OS binding, or wire a dispatcher handler for a keyboard shortcut.
---

Atlas keybinds follow one hard rule:

> **One action ID per behaviour. Chords alias to actions, never the reverse.**

If you find yourself wanting a chord to do two things, split the two consumers into distinct actions or unify the behaviour into one handler — never introduce a duplicate action ID for the same chord.

## The four registration points

Every new keybind lives in exactly four places:

1. **Action metadata** — `crates/atlas-keymap/src/defaults.rs::default_actions()`. Add an `action!(id, title, description, contexts)` entry. `contexts` is `["Global"]`, `["Pane"]`, or both.
2. **Default per-OS binding** — `crates/atlas-keymap/src/defaults.rs::default_bindings_for(platform)`. Add the chord(s) for each target OS. On macOS use `cmd-*`; on Linux/Windows use `ctrl-*`. Reserve `ctrl-k` and other browser/editor-hot chords for their conventional uses (e.g., `remote::Connect` uses `cmd-k` on macOS but `ctrl-alt-k` on Linux/Windows).
3. **Dispatcher handler** — `crates/atlas-app/src/main.rs::build_dispatcher()`. Add a `.register(ActionId::new("namespace::YourAction"), move |_ctx| shell.do_thing())` (or the equivalent match arm depending on the current shape).
4. **Documentation** — `docs/keymap.md`. Add a row with both macOS and Linux/Windows chords, a one-line description, and the action ID.

## Namespace conventions

Reuse an existing namespace where it fits:

- `command_palette::`, `goto::`, `app::`, `workspace::`, `pane::`, `fs::`, `search::`, `rename::`, `ops::`, `remote::`, `nav::`.

## Regenerate the checked-in TOMLs

Atlas ships default keymap TOMLs under `assets/keymaps/`. They are derived from `defaults.rs` and must stay byte-identical. After changing `default_actions()` or `default_bindings_for(…)`:

```bash
cargo test -p atlas-keymap regen_default_keymap -- --ignored
```

Commit the resulting file changes alongside your Rust changes. `test_checked_in_default_toml_matches_emitter` fails in CI otherwise.

Users can force-refresh their local defaults with `rm -f ~/.config/atlas/keymaps/default.toml` before relaunching.

## Optional: footer chip

If the action deserves a discoverable chip in the bottom shortcut footer, add it to `FOOTER_ACTIONS` in `crates/atlas-app/src/main.rs`. The footer resolves the chord label from the live keymap and skips actions with no binding.

## Verify

- `cargo test -p atlas-keymap` — regen + roundtrip tests pass.
- `cargo build --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` — dispatcher wiring compiles clean.
- Launch the app, open the palette (`Cmd+Shift+P`), find your action by title. Selecting it should dispatch.
- Press the chord in a pane; verify behaviour + context (Pane vs Global).

For the full walkthrough with anti-patterns and cross-references, read [`.github/instructions/keybind-authoring.instructions.md`](../../instructions/keybind-authoring.instructions.md).
