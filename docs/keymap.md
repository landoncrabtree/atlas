# Atlas Keymap

Atlas ships with a built-in default keymap and a user-editable override file. This document explains where the keymap file lives, the chord DSL, all default bindings, and how to customise them.

## Where the keymap file lives

| Platform | Path |
|---|---|
| macOS / Linux | `~/.config/atlas/keymaps/default.toml` |
| Linux (XDG) | `$XDG_CONFIG_HOME/atlas/keymaps/default.toml` |
| Windows | `%APPDATA%\Atlas\keymaps\default.toml` |

Atlas writes this file on first launch if it does not already exist.  You can also override the entire config directory by setting the `ATLAS_CONFIG_DIR` environment variable.

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

### Application

| Key | Action |
|---|---|
| `cmd-shift-p` | Toggle Command Palette |
| `cmd-p` | Go to Anything |
| `cmd-,` | Open Settings |
| `cmd-q` | Quit Atlas |

### Pane navigation

| Key | Action |
|---|---|
| `j` | Move cursor down |
| `k` | Move cursor up |
| `h` | Move cursor left |
| `l` | Move cursor right |
| `g g` | Move to top |
| `shift-g` | Move to bottom |
| `/` | Search in place |
| `enter` | Activate (open) |
| `backspace` | Go to parent directory |
| `alt-left` | Navigate back |
| `alt-right` | Navigate forward |

### Selection

| Key | Action |
|---|---|
| `space` | Toggle selection |
| `cmd-a` | Select all |
| `cmd-shift-a` | Deselect all |

### Tabs

| Key | Action |
|---|---|
| `cmd-t` | New tab |
| `cmd-w` | Close tab |
| `cmd-shift-t` | Reopen closed tab |
| `cmd-1` – `cmd-9` | Select tab 1–9 |
| `cmd-shift-[` | Previous tab |
| `cmd-shift-]` | Next tab |

### Pane splitting and focus

| Key | Action |
|---|---|
| `cmd-d` | Split pane right (horizontal) |
| `cmd-shift-d` | Split pane down (vertical) |
| `cmd-shift-w` | Close focused pane |
| `ctrl-h` | Focus left pane |
| `ctrl-j` | Focus pane below |
| `ctrl-k` | Focus pane above |
| `ctrl-l` | Focus right pane |

### View modes

| Key | Action |
|---|---|
| `cmd-alt-1` | Details view |
| `cmd-alt-2` | Grid view |
| `cmd-alt-3` | Gallery view |
| `cmd-alt-4` | Miller columns view |
| `cmd-alt-5` | Tree view |
| `cmd-shift-e` | Cycle view mode (Details → Grid → Gallery → Miller → Tree → …) |

### File operations

| Key | Action |
|---|---|
| `f2` | Rename |
| `f3` | View file |
| `f4` | Edit file |
| `f5` | Copy |
| `f6` | Move |
| `f7` | New directory |
| `f8` | Delete |
| `cmd-c` | Copy to clipboard |
| `cmd-x` | Cut to clipboard |
| `cmd-v` | Paste from clipboard |

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
