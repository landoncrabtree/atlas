# Atlas Keymap

Atlas ships with a built-in default keymap and a user-editable override file. This document explains where the keymap file lives, the chord DSL, all default bindings, and how to customise them.

## Where the keymap file lives

| Platform | Path |
|---|---|
| macOS / Linux | `~/.config/atlas/keymaps/default.toml` |
| Linux (XDG) | `$XDG_CONFIG_HOME/atlas/keymaps/default.toml` |
| Windows | `%APPDATA%\Atlas\keymaps\default.toml` |

Atlas writes this file on first launch if it does not already exist. You can also override the entire config directory by setting the `ATLAS_CONFIG_DIR` environment variable — this override takes effect immediately and is the recommended path for tests and portable installs.

The keymap ships as three per-platform TOMLs under `assets/keymaps/`:
`default.macos.toml`, `default.linux.toml`, `default.windows.toml`. First launch on any platform copies the appropriate file to `~/.config/atlas/keymaps/default.toml`. To pick up new defaults after upgrading Atlas or editing the shipped keymap:

```bash
rm -f ~/.config/atlas/keymaps/default.toml   # will be re-seeded on next launch
```

The checked-in files are generated from Rust — `crates/atlas-keymap/src/defaults.rs::default_bindings_for` is the source of truth. If you edit that function, regenerate the TOMLs with:

```bash
cargo test -p atlas-keymap regen_default_keymap -- --ignored
```

A companion test (`test_checked_in_default_toml_matches_emitter`) fails on `cargo test` if the checked-in files drift from the Rust source.

## Chord DSL

A chord is a key press, optionally combined with modifier keys. Chords are written as hyphen-separated tokens:

```
<modifier>-<modifier>-...<key>
```

### Modifier aliases

All aliases resolve to the same internal representation; the canonical serialised form is shown in the "Writes as" column.

| Aliases | Writes as | Notes |
|---|---|---|
| `cmd`, `meta`, `super`, `win` | `cmd` | **Primary modifier.** Maps to ⌘ Command on macOS; Ctrl-equivalent on Linux/Windows at dispatch time. |
| `alt`, `option`, `opt` | `alt` | Option on macOS, Alt on Linux/Windows. |
| `ctrl`, `control` | `ctrl` | Control key on all platforms. |
| `shift` | `shift` | Shift key. |

> **Cross-platform note**: A binding written as `cmd-d` fires when you press ⌘D on macOS and Ctrl+D on Linux/Windows. You do not need separate bindings per platform.

### Key names

- **Printable characters** — `a`–`z`, `0`–`9`, `,`, `/`, `[`, `]`, etc.
- **Function keys** — `f1`–`f24`
- **Named keys** — `escape` (or `esc`), `tab`, `enter` (or `return`), `backspace` (or `back`), `delete` (or `del`), `space`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup` (or `pgup`), `pagedown` (or `pgdn`), `insert` (or `ins`)

### Examples

```toml
key = "cmd-p"           # Cmd/Ctrl + P
key = "cmd-shift-p"     # Cmd/Ctrl + Shift + P
key = "ctrl-alt-t"      # Ctrl + Alt + T
key = "f5"              # F5
key = "escape"          # Escape
key = "g g"             # Two-chord sequence: press G then G
key = "cmd-k cmd-s"     # Two-chord sequence: Cmd+K then Cmd+S
```

Chord sequences (space-separated) are supported in both default and user bindings.

## Default bindings

The table below lists all bindings that Atlas ships with. A **Global** binding fires regardless of which widget has focus; a **Pane** binding fires when a pane has focus.

Modifier convention: the tables use `cmd` as shorthand for the platform's primary modifier — Command (⌘) on macOS, Control on Linux/Windows. Physical `ctrl` bindings (vim-style pane focus, remote::Connect on Linux/Windows) stay literal on every platform.

### Application

| Key | Action |
|---|---|
| `cmd-shift-p` | `command_palette::Toggle` — Toggle Command Palette |
| `cmd-p` | `goto::Anything` — Go to Anything (includes saved remote servers) |
| `cmd-,` | `app::OpenSettings` |
| `cmd-q` | `app::Quit` |

### Pane navigation

`fs::View` is the single action for "open the focused entry" — it cd's into folders and hands files off to the OS default handler. Multiple chords bind to it (`l`, `right`, `.`, `enter`); there is no separate `activate` action. This is the canonical pattern: **one action per behaviour, N chords aliased onto that action**, never the other way round.

| Key | Action |
|---|---|
| `j` / `down` | `pane::MoveDown` |
| `k` / `up` | `pane::MoveUp` |
| `h` / `left` / `,` | `pane::GoUp` (parent directory) |
| `l` / `right` / `.` / `enter` | `fs::View` (cd into folder, or open file) |
| `g g` | `pane::MoveToTop` |
| `shift-g` | `pane::MoveToBottom` |
| `/` | `pane::SearchInPlace` |
| `backspace` | `pane::GoUp` |
| `alt-left` | `pane::Back` |
| `alt-right` | `pane::Forward` |

### Selection

Space toggles the focused row's selected state (yazi / nnn / ranger mark idiom); arrow / j / k navigation is **focus-only** and never disturbs the selection, so multi-select is built up by focus-and-toggle. Shift+arrow / Shift+j/k extends a range from the anchor.

| Key | Action |
|---|---|
| `space` | `pane::ToggleSelection` |
| `shift-down` / `shift-j` | `pane::ExtendDown` |
| `shift-up` / `shift-k` | `pane::ExtendUp` |
| `cmd-a` | `pane::SelectAll` |
| `cmd-shift-a` | `pane::DeselectAll` |

### Tabs

| Key | Action |
|---|---|
| `cmd-t` | `tab::New` |
| `cmd-w` | `tab::Close` |
| `cmd-shift-t` | `tab::Reopen` |
| `cmd-1` … `cmd-9` | `tab::Select1` … `tab::Select9` |
| `cmd-shift-[` | `tab::CyclePrev` |
| `cmd-shift-]` | `tab::CycleNext` |

### Pane splitting and focus

Vim-style pane focus (`ctrl-h/j/k/l`) stays on physical Ctrl on every platform — muscle memory from tmux/vim expects this.

| Key | Action |
|---|---|
| `cmd-d` | `pane::SplitRight` |
| `cmd-shift-d` | `pane::SplitDown` |
| `cmd-shift-w` | `pane::Close` |
| `cmd-\` | `workspace::ToggleDualPane` |
| `ctrl-h` | `pane::FocusLeft` |
| `ctrl-j` | `pane::FocusDown` |
| `ctrl-k` | `pane::FocusUp` |
| `ctrl-l` | `pane::FocusRight` |

### View modes

| Key | Action |
|---|---|
| `cmd-alt-1` | `view::Details` |
| `cmd-alt-2` | `view::Grid` |
| `cmd-alt-3` | `view::Gallery` |
| `cmd-alt-4` | `view::Miller` |
| `cmd-shift-e` | `view::Cycle` — Details → Grid → Gallery → Miller → … |

### File operations

`fs::Copy` / `fs::Cut` / `fs::Paste` route through the OS clipboard, so paths copied in Atlas can be pasted into Finder, Explorer, VS Code, TextEdit, or any other app.

| Key | Action |
|---|---|
| `f2` | `fs::Rename` |
| `shift-f2` | `rename::OpenBulk` (bulk-rename modal) |
| `cmd-shift-n` | `fs::Mkdir` (Finder / Explorer / Nautilus convention) |
| `cmd-c` | `fs::Copy` |
| `cmd-x` | `fs::Cut` |
| `cmd-v` | `fs::Paste` |
| `cmd-backspace` (macOS) / `delete` (Linux/Windows) | `fs::Delete` (moves to Trash) |

The `fs::Duplicate` action (copy the focused entry into the same directory with a `(copy)` suffix) is registered but ships **unbound** by default. Rebind by adding a `[[bindings]]` entry — see below.

### Remote filesystems

`remote::Connect` opens the Connect-to-Server modal, matching Finder's ⌘K "Connect to Server". Supported backends: SFTP, FTP, WebDAV, S3. The modal also lists previously-saved servers (persisted to `~/.config/atlas/servers.toml`). Cmd+P (Go to Anything) surfaces those saved servers as palette entries alongside local paths.

| Key | Action | Platform |
|---|---|---|
| `cmd-k` | `remote::Connect` | macOS |
| `ctrl-alt-k` | `remote::Connect` | Linux / Windows (avoids stomping Ctrl+K which browsers/editors reserve for "focus search") |

### Search / ops panel

| Key | Action |
|---|---|
| `cmd-f` | `search::Toggle` — toggle right-hand search panel |
| `cmd-shift-f` | `search::Open` — open panel and focus query input |
| `cmd-j` | `ops::TogglePanel` — toggle bottom operations tray |

## Adding a new keybind

See `.github/instructions/keybind-authoring.instructions.md` for the full end-to-end workflow (register the action metadata, add per-OS defaults, wire a dispatcher handler, regen keymap TOMLs, decide whether to add a footer chip, update this doc).

## Customising the keymap

The keymap file at `~/.config/atlas/keymaps/default.toml` uses the same `[[bindings]]` schema as the built-in defaults:

```toml
[[bindings]]
context = "Pane"
key = "ctrl-n"
action = "pane::MoveDown"
```

### Rebinding an action

Add a `[[bindings]]` entry with your preferred chord. User bindings overlay the defaults, so the old chord still works until you suppress it (see below).

```toml
# Map Ctrl+N to move cursor down (vim-style j is still active too).
[[bindings]]
context = "Pane"
key = "ctrl-n"
action = "pane::MoveDown"
```

### Suppressing a default binding

Set `action = ""` to disable a chord from the default layer:

```toml
# Disable the default Cmd+Q quit binding.
[[bindings]]
context = "Global"
key = "cmd-q"
action = ""
```

### Context selection

| Context | When it applies |
|---|---|
| `"Global"` | Always — regardless of focused widget |
| `"Pane"` | When a file pane has keyboard focus |

Bindings in a more specific context (e.g. `"Pane"`) take precedence over `"Global"` for the same chord.

### Cross-platform bindings

You only need one binding. Write `cmd-d` and Atlas dispatches ⌘D on macOS and Ctrl+D on Linux/Windows automatically.

If you need a platform-specific binding that diverges (rare), use the relevant modifier directly: `ctrl-d` for Linux/Windows-only, `cmd-d` for macOS-primary.

## Chord routing while modals or text inputs are focused

Atlas routes every physical key through the `handle-key-chord` callback in `assets/ui/atlas.slint`, which forwards to the Rust `Dispatcher` in `crates/atlas-keymap`. To keep chords like `Cmd+A`, `Cmd+C`, `Cmd+V` doing the right thing when a text input has focus (select-all / copy / paste inside the text field, not on the pane's row list), the dispatcher runs a **keymap-bypass gate**:

- When any modal is visible (command palette, search panel, bulk-rename, operation-progress, connect-server) — `Pane` bindings are suppressed. Only `Global` chords fire.
- When any pane's address-bar `TextInput` has focus — same behaviour: `Pane` chords fall through to the native edit shortcuts.
- When a modal's own `TextInput` has focus (e.g. the connect-modal host / password fields) — the modal's `input-focused` property bubbles up to the root `FocusScope` and enables the same bypass.

The gate is anti-drift: focus-in sets the id, focus-out only clears it if the id matches, so cross-pane focus jumps (blur A → focus B in either order) never leave the gate stuck open.

If you are adding a new modal with a text input, wire its `input-focused` up through the parent chain to the root `FocusScope`'s `keymap-bypass-active` property. See `.github/instructions/ui-composition.instructions.md` for the canonical pattern.

## Footer chips

A curated subset of actions appears as chord chips in the bottom footer strip. The list lives in `crates/atlas-app/src/main.rs::FOOTER_ACTIONS` and today shows: Copy, Cut, Paste, Rename, New Folder, Trash, Goto, Palette, Search. Chords are looked up live from the keymap, so user rebindings appear immediately and a chord that was unbound is silently dropped from the footer. Toggle the whole strip via the `ui.show_shortcuts` config key.
