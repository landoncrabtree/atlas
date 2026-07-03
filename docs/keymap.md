# Atlas Keymap

Atlas ships with a built-in default keymap and a user-editable override file.
This document explains where the keymap file lives, the chord DSL, all
default bindings per platform, and how to customise them.

## Where the keymap file lives

| Platform | Path |
|---|---|
| macOS / Linux | `~/.config/atlas/keymaps/default.toml` |
| Linux (XDG)   | `$XDG_CONFIG_HOME/atlas/keymaps/default.toml` |
| Windows       | `%APPDATA%\Atlas\keymaps\default.toml` |

Atlas writes this file on first launch if it does not already exist. You can
also override the entire config directory by setting the `ATLAS_CONFIG_DIR`
environment variable — this override takes effect immediately and is the
recommended path for tests and portable installs.

The keymap ships as **three per-platform TOMLs** under `assets/keymaps/`:
`default.macos.toml`, `default.linux.toml`, `default.windows.toml`. First
launch on any platform copies the appropriate file to
`~/.config/atlas/keymaps/default.toml`. To pick up new defaults after
upgrading Atlas or editing the shipped keymap:

```bash
rm -f ~/.config/atlas/keymaps/default.toml   # will be re-seeded on next launch
```

The checked-in files are generated from Rust —
`crates/atlas-keymap/src/defaults.rs::default_bindings_for(platform)` is
the source of truth. If you edit that function, regenerate the TOMLs with:

```bash
cargo test -p atlas-keymap regen_default_keymap -- --ignored
```

A companion test (`test_checked_in_default_toml_matches_emitter`) fails on
a normal test run if the checked-in files drift from the Rust source.

## The default keymap is per-OS, not "smart"

Atlas ships **three independent keymaps** — one per OS. They are related
by convention (macOS chords use ⌘, Linux/Windows chords use Ctrl for the
same actions) but the dispatcher does **not** silently remap `cmd-*` to
`ctrl-*` at runtime. What the TOML file says is what the dispatcher
matches:

- On macOS, `cmd-d` fires when you press ⌘+D on the physical keyboard.
- On Linux/Windows, `cmd-d` fires only when the physical `Super` / `Meta`
  / `Win` key is pressed with D — **not** when Ctrl+D is pressed.
- The Linux/Windows shipped default is `ctrl-d`. If you want ⌘+D-style
  behaviour on those platforms, you press Ctrl+D, and the keymap TOML
  literally binds `ctrl-d`.

The tables below list each OS's shipped defaults side by side so the
underlying pattern is easy to see, but each row on each side is an
independent binding in that platform's TOML file. If you edit
`~/.config/atlas/keymaps/default.toml` on macOS to add `ctrl-d`, that
chord fires only on the physical Ctrl key — not ⌘.

Rationale: Vim-style `Ctrl+H/J/K/L` pane focus stays on the physical
Ctrl key on every platform because muscle memory from tmux / vim expects
it. If Atlas silently remapped `cmd` to `ctrl` at dispatch time, we
could not express "Ctrl+L on every platform" without special-casing.
Per-OS TOMLs keep the model simple.

## Chord DSL

A chord is a key press, optionally combined with modifier keys. Chords
are written as hyphen-separated tokens:

```
<modifier>-<modifier>-…<key>
```

### Modifier aliases

All aliases resolve to the same internal representation; the canonical
serialised form is shown in the **Writes as** column. The aliases exist
so users can copy a TOML fragment between platforms and have it parse —
whether that fragment actually **fires** on the target platform depends
on which physical modifier the parser resolves the alias to (see the
per-OS tables below).

| Aliases | Writes as | Physical key |
|---|---|---|
| `cmd`, `meta`, `super`, `win` | `cmd` | ⌘ Command (macOS) / Super / Meta / Win (Linux, Windows) |
| `alt`, `option`, `opt` | `alt` | Option (macOS) / Alt (Linux, Windows) |
| `ctrl`, `control` | `ctrl` | Control on every platform |
| `shift` | `shift` | Shift on every platform |

The alias set lets you write `super-d`, `meta-d`, `win-d`, or `cmd-d`
and get the same TOML — but on Linux `cmd-d` requires the physical
Super/Meta/Win key, not Ctrl. On macOS `cmd-d` requires ⌘. If you want
the same chord on every OS, use `ctrl-*` and add it explicitly to each
platform's user override file.

### Key names

- **Printable characters** — `a`–`z`, `0`–`9`, `,`, `/`, `[`, `]`, etc.
- **Function keys** — `f1`–`f24`.
- **Named keys** — `escape` (or `esc`), `tab`, `enter` (or `return`),
  `backspace` (or `back`), `delete` (or `del`), `space`, `up`, `down`,
  `left`, `right`, `home`, `end`, `pageup` (or `pgup`), `pagedown` (or
  `pgdn`), `insert` (or `ins`).

### Examples

```toml
key = "cmd-p"           # ⌘+P on macOS; Super+P on Linux/Windows
key = "ctrl-p"          # Ctrl+P everywhere
key = "cmd-shift-p"     # ⌘+Shift+P on macOS; Super+Shift+P on Linux/Windows
key = "ctrl-alt-t"      # Ctrl+Alt+T everywhere
key = "f5"              # F5
key = "escape"          # Escape
key = "g g"             # Two-chord sequence: press G then G
key = "cmd-k cmd-s"     # Two-chord sequence: ⌘+K then ⌘+S (macOS)
```

Chord sequences (space-separated) are supported in both default and user
bindings.

## Default bindings

The tables below list the shipped defaults for each platform. A **Global**
binding fires regardless of which widget has focus; a **Pane** binding
fires only when a file pane has keyboard focus and no modal or focused
text input is bypassing (see the *Chord routing* section further down).

The single-modifier convention in these tables:

- **macOS**: the primary modifier is `cmd` (⌘).
- **Linux / Windows**: the primary modifier is `ctrl`.
- **Vim-style pane focus (`ctrl-h/j/k/l`)** stays on physical Ctrl on
  every platform — muscle memory from tmux/vim expects this.

### Application

| Action ID | macOS | Linux | Windows | Context |
|---|---|---|---|---|
| `command_palette::Toggle` | `cmd-shift-p` | `ctrl-shift-p` | `ctrl-shift-p` | Global |
| `goto::Anything` | `cmd-p` | `ctrl-p` | `ctrl-p` | Global |
| `app::OpenSettings` | `cmd-,` | `ctrl-,` | `ctrl-,` | Global |
| `app::Quit` | `cmd-q` | `ctrl-q` | `ctrl-q` | Global |

### Pane navigation

Atlas has **converged navigation semantics**: `hjkl`, `wasd`, and the
arrow keys ALL bind to the same four directional actions
(`pane::MoveLeft`, `pane::MoveRight`, `pane::MoveUp`, `pane::MoveDown`)
regardless of user config. There is **no** vim-mode switch to enable —
keyboard-first navigation is Atlas's north star. When a modal or text
input (address bar, search box, command palette) has focus, the
dispatcher restricts to `[Global]` context, so vim / wasd letters fall
through to the input natively — typing in the address bar never
collides with navigation.

Each directional action resolves to a per-view meaning via a single
lookup table (`crates/atlas-ui/src/views/navigation.rs`). See the
**Navigation semantics** table below.

`fs::View` is the single action for **"open the focused entry"** — it
`cd`s into folders and hands files off to the OS default handler.
Multiple chords bind to it (`.`, `enter`); on Details / Miller `Right`
also resolves to it via `pane::MoveRight` + the ViewNavigation table.
There is no separate `activate` action. This is the canonical pattern:
**one action per behaviour, N chords aliased onto that action**, never
the other way round.

| Action ID | macOS | Linux | Windows | Context |
|---|---|---|---|---|
| `pane::MoveUp` | `k`, `w`, `up` | `k`, `w`, `up` | `k`, `w`, `up` | Pane |
| `pane::MoveDown` | `j`, `s`, `down` | `j`, `s`, `down` | `j`, `s`, `down` | Pane |
| `pane::MoveLeft` | `h`, `a`, `left` | `h`, `a`, `left` | `h`, `a`, `left` | Pane |
| `pane::MoveRight` | `l`, `d`, `right` | `l`, `d`, `right` | `l`, `d`, `right` | Pane |
| `pane::GoUp` (parent) | `,`, `backspace` | `,`, `backspace` | `,`, `backspace` | Pane |
| `fs::View` (cd / open) | `.`, `enter` | `.`, `enter` | `.`, `enter` | Pane |
| `pane::MoveToTop` | `g g` | `g g` | `g g` | Pane |
| `pane::MoveToBottom` | `shift-g` | `shift-g` | `shift-g` | Pane |
| `pane::SearchInPlace` | `/` | `/` | `/` | Pane |
| `pane::Back` | `alt-left` | `alt-left` | `alt-left` | Pane |
| `pane::Forward` | `alt-right` | `alt-right` | `alt-right` | Pane |

#### Navigation semantics per view

`pane::MoveLeft` / `pane::MoveRight` / `pane::MoveUp` / `pane::MoveDown`
resolve per view via a shared `ViewNavAction` dispatch table. Adding a
new view means adding one arm to `ViewNavAction::for_mode` —
nothing else changes.

| View    | Left            | Right          | Up          | Down        | Enter / dbl-click |
|---------|-----------------|----------------|-------------|-------------|-------------------|
| Details | `pane::GoUp`    | `fs::View`     | focus−1     | focus+1     | `fs::View`        |
| Miller  | `pane::GoUp`    | `fs::View`     | focus−1     | focus+1     | `fs::View`        |
| Grid    | col−1 (wraps)   | col+1 (wraps)  | row−1       | row+1       | `fs::View`        |
| Gallery | prev item       | next item      | no-op       | no-op       | `fs::View`        |

Grid Left/Right **wrap across rows** like text-editor caret motion —
pressing Right at the end of a row lands on col 0 of the next row.
Grid Up/Down clamp at grid edges (no wrap).

Miller single-click on a folder opens the child column immediately —
this mirrors macOS Finder's Column View and is intentional. Files still
require Enter / double-click to open.

### Selection

Space toggles the focused row's selected state (yazi / nnn / ranger mark
idiom); arrow / `j` / `k` / `w` / `s` navigation is **focus-only** and
never disturbs the selection, so multi-select is built up by focus-and-
toggle. Shift+arrow / Shift+`j` / Shift+`k` / Shift+`w` / Shift+`s`
extends a range from the anchor.

Selection is currently supported in Details and Grid. Miller has a
hierarchical column-stack model where cross-column multi-select is not
a well-defined operation; Gallery is single-focus. Extending multi-
select to those views is left for a future release.

| Action ID | macOS | Linux | Windows | Context |
|---|---|---|---|---|
| `pane::ToggleSelection` | `space` | `space` | `space` | Pane |
| `pane::ExtendDown` | `shift-down`, `shift-j`, `shift-s` | `shift-down`, `shift-j`, `shift-s` | `shift-down`, `shift-j`, `shift-s` | Pane |
| `pane::ExtendUp` | `shift-up`, `shift-k`, `shift-w` | `shift-up`, `shift-k`, `shift-w` | `shift-up`, `shift-k`, `shift-w` | Pane |
| `pane::SelectAll` | `cmd-a` | `ctrl-a` | `ctrl-a` | Pane |
| `pane::DeselectAll` | `cmd-shift-a` | `ctrl-shift-a` | `ctrl-shift-a` | Pane |

### Tabs

Each pane owns its own tab stack (see [`docs/multi-pane.md`](multi-pane.md)).
These bindings act on the focused pane.

| Action ID | macOS | Linux | Windows | Context |
|---|---|---|---|---|
| `tab::New` | `cmd-t` | `ctrl-t` | `ctrl-t` | Global |
| `tab::Close` | `cmd-w` | `ctrl-w` | `ctrl-w` | Global |
| `tab::Reopen` | `cmd-shift-t` | `ctrl-shift-t` | `ctrl-shift-t` | Global |
| `tab::Select1`…`tab::Select9` | `cmd-1`…`cmd-9` | `ctrl-1`…`ctrl-9` | `ctrl-1`…`ctrl-9` | Global |
| `tab::CyclePrev` | `cmd-shift-[` | `ctrl-shift-[` | `ctrl-shift-[` | Pane |
| `tab::CycleNext` | `cmd-shift-]` | `ctrl-shift-]` | `ctrl-shift-]` | Pane |

### Pane splitting and focus

Vim-style pane focus (`ctrl-h/j/k/l`) stays on physical Ctrl on macOS —
muscle memory from tmux/vim expects this. On Linux/Windows, `Ctrl+H` is
the OS-native "toggle hidden files" chord (Nautilus, Nemo, Thunar,
Dolphin all use it), so `pane::FocusLeft` shifts to `Ctrl+Shift+H` there
and `Ctrl+H` binds to `pane::ToggleHidden` instead.

`pane::ToggleHidden` is a per-pane runtime toggle: split screens can
show different states, and the toggle does **not** persist to
`config.toml` (edit `[view].show_hidden` for a persistent default).

| Action ID | macOS | Linux | Windows | Context |
|---|---|---|---|---|
| `pane::SplitRight` | `cmd-d` | `ctrl-d` | `ctrl-d` | Global |
| `pane::SplitDown` | `cmd-shift-d` | `ctrl-shift-d` | `ctrl-shift-d` | Global |
| `pane::Close` | `cmd-shift-w` | `ctrl-shift-w` | `ctrl-shift-w` | Global |
| `pane::FocusLeft` | `ctrl-h` | `ctrl-shift-h` | `ctrl-shift-h` | Pane |
| `pane::FocusDown` | `ctrl-j` | `ctrl-j` | `ctrl-j` | Pane |
| `pane::FocusUp` | `ctrl-k` | `ctrl-k` | `ctrl-k` | Pane |
| `pane::FocusRight` | `ctrl-l` | `ctrl-l` | `ctrl-l` | Pane |
| `pane::ToggleHidden` | `cmd-.` | `ctrl-h` | `ctrl-h` | Pane |
| `workspace::ToggleDualPane` | `cmd-\` | `ctrl-\` | `ctrl-\` | Global |

### View modes

`cmd-1`…`cmd-9` are taken by tab selection, so view-mode switching uses
`cmd-alt-*`. **`view::Tree` no longer ships**; the tree view no
longer appears in the rotation. `view::Cycle` rotates through the remaining modes:
Details → Grid → Gallery → Miller → Details.

| Action ID | macOS | Linux | Windows | Context |
|---|---|---|---|---|
| `view::Details` | `cmd-alt-1` | `ctrl-alt-1` | `ctrl-alt-1` | Global |
| `view::Grid` | `cmd-alt-2` | `ctrl-alt-2` | `ctrl-alt-2` | Global |
| `view::Gallery` | `cmd-alt-3` | `ctrl-alt-3` | `ctrl-alt-3` | Global |
| `view::Miller` | `cmd-alt-4` | `ctrl-alt-4` | `ctrl-alt-4` | Global |
| `view::Cycle` | `cmd-shift-e` | `ctrl-shift-e` | `ctrl-shift-e` | Pane |

### File operations

`fs::Copy` / `fs::Cut` / `fs::Paste` route through the OS clipboard, so
paths copied in Atlas can be pasted into Finder, Explorer, VS Code,
TextEdit, or any other app.

| Action ID | macOS | Linux | Windows | Context |
|---|---|---|---|---|
| `fs::Copy` | `cmd-c` | `ctrl-c` | `ctrl-c` | Pane |
| `fs::Cut` | `cmd-x` | `ctrl-x` | `ctrl-x` | Pane |
| `fs::Paste` | `cmd-v` | `ctrl-v` | `ctrl-v` | Pane |
| `fs::Rename` | `f2` | `f2` | `f2` | Pane |
| `rename::OpenBulk` | `shift-f2` | `shift-f2` | `shift-f2` | Pane |
| `fs::Mkdir` (New Folder) | `cmd-shift-n` | `ctrl-shift-n` | `ctrl-shift-n` | Pane |
| `fs::Delete` (→ Trash) | `cmd-backspace` | `delete` | `delete` | Pane |

The `fs::Duplicate` action (copy the focused entry into the same
directory with a `(copy)` suffix) is registered but ships **unbound** by
default. Rebind by adding a `[[bindings]]` entry to your user keymap.

### Remote filesystems

`remote::Connect` opens the Connect-to-Server modal, matching Finder's
⌘K "Connect to Server". Supported backends: SFTP, FTP, WebDAV, S3. The
modal also lists previously-saved servers (persisted to
`~/.config/atlas/servers.toml` with only opaque `credential_ref` handles
— secrets live in the OS keychain). Cmd+P (Go to Anything) surfaces those
saved servers as palette entries alongside local paths.

On Linux/Windows the chord shifts to `ctrl-alt-k` to avoid stomping
`Ctrl+K`, which browsers and editors overwhelmingly reserve for "focus
search".

| Action ID | macOS | Linux | Windows | Context |
|---|---|---|---|---|
| `remote::Connect` | `cmd-k` | `ctrl-alt-k` | `ctrl-alt-k` | Global |

### Search / ops right dock

Search and Operations share the same right-side dock slot. `search::Toggle`
and `ops::TogglePanel` toggle their own surface, or swap the shared slot when
the other surface is showing. `ui::Cancel` closes the active right-dock surface.

| Action ID | macOS | Linux | Windows | Context |
|---|---|---|---|---|
| `ui::Cancel` | `escape` | `escape` | `escape` | Global |
| `search::Toggle` | `cmd-f` | `ctrl-f` | `ctrl-f` | Global |
| `search::Open` (focus query) | `cmd-shift-f` | `ctrl-shift-f` | `ctrl-shift-f` | Global |
| `ops::TogglePanel` | `cmd-j` | `ctrl-j` | `ctrl-j` | Global |

## Adding a new keybind

See [`.github/instructions/keybind-authoring.instructions.md`](../.github/instructions/keybind-authoring.instructions.md)
for the full end-to-end workflow (register the action metadata, add
per-OS defaults, wire a dispatcher handler, regen the keymap TOMLs,
decide whether to add a footer chip, update this doc).

## Customising the keymap

The user keymap file at `~/.config/atlas/keymaps/default.toml` uses the
same `[[bindings]]` schema as the built-in defaults. Anything you add
here layers on top of the shipped defaults for the current OS.

```toml
[[bindings]]
context = "Pane"
key = "ctrl-n"
action = "pane::MoveDown"
```

### Rebinding an action

Add a `[[bindings]]` entry with your preferred chord. User bindings
overlay the defaults, so the old chord still works until you suppress
it (see below).

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
# Disable the default Cmd+Q quit binding on macOS.
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

Bindings in a more specific context (e.g. `"Pane"`) take precedence over
`"Global"` for the same chord.

### Cross-platform user keymaps

Each OS has its own default keymap file (`default.macos.toml`,
`default.linux.toml`, `default.windows.toml`), and each is seeded into
your user override on first launch on that OS. If you want the same
custom chord on every OS:

- Prefer `ctrl-*` — Control is Control everywhere, and both macOS and
  Linux/Windows dispatch it identically.
- If you must use a platform-primary modifier, add a `[[bindings]]`
  entry to your user file on each machine. Atlas does not silently
  rewrite `cmd-*` into `ctrl-*` at dispatch time — the chord you write
  is the chord the dispatcher matches.

## Chord routing while modals or text inputs are focused

Atlas routes every physical key through the `handle-key-chord` callback
in `assets/ui/atlas.slint`, which forwards to the Rust `Dispatcher` in
`crates/atlas-keymap`. To keep chords like `Cmd+A`, `Cmd+C`, `Cmd+V`
doing the right thing when a text input has focus (select-all / copy /
paste inside the text field, not on the pane's row list), the dispatcher
runs a **keymap-bypass gate**:

- When any modal is visible (command palette, search panel, bulk-rename,
  operation-progress, connect-server) — `Pane` bindings are suppressed.
  Only `Global` chords fire.
- When any pane's address-bar `TextInput` has focus — same behaviour:
  `Pane` chords fall through to the native edit shortcuts.
- When a modal's own `TextInput` has focus (e.g. the connect-modal host /
  password fields) — the modal's `input-focused` property bubbles up to
  the root `FocusScope` and enables the same bypass.

The gate is anti-drift: focus-in sets the id, focus-out only clears it
if the id matches, so cross-pane focus jumps (blur A → focus B in
either order) never leave the gate stuck open.

If you are adding a new modal with a text input, wire its `input-focused`
up through the parent chain to the root `FocusScope`'s
`keymap-bypass-active` property. See
[`.github/instructions/ui-composition.instructions.md`](../.github/instructions/ui-composition.instructions.md)
for the canonical pattern.

## Footer chips

A curated subset of actions appears as chord chips in the bottom footer
strip. The list lives in `crates/atlas-app/src/main.rs::FOOTER_ACTIONS`
and today shows: Copy, Cut, Paste, Rename, New Folder, Trash, Goto,
Palette, Search. Chords are looked up live from the keymap, so user
rebindings appear immediately and a chord that was unbound is silently
dropped from the footer. Toggle the whole strip via the
`ui.show_shortcuts` config key.
