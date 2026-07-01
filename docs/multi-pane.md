# Multi-pane workspace

Atlas uses a **tmux-style tiling workspace** where any number of panes are arranged
in a binary split tree.  Each pane is fully independent — it owns its own tabs,
back/forward history, view mode, selection, and sort settings.

---

## Concepts

### Workspace

The root container.  Internally represented as a binary `SplitLayout` tree.

```
┌─────────────────────────────────────────┐
│              AtlasWindow                │
│  ┌──────────┬──────────┬─────────────┐  │
│  │  Pane 0  │  Pane 1  │   Pane 2    │  │
│  │  (tabs)  │  (tabs)  │   (tabs)    │  │
│  └──────────┴──────────┴─────────────┘  │
└─────────────────────────────────────────┘
```

### Pane

A tile in the workspace.  Every pane has:

- **Tabs** — one or more location tabs.
- **View mode** — Details, Grid, Gallery, Miller, or Tree.  Each pane cycles
  its own view mode independently.
- **Back/forward history** — per-tab, bounded by `navigation.history_size`.
- **Selection** — per-pane; drag from one pane to another triggers a copy/move.

### Tab

A location within a pane.  Each tab remembers:

- Its directory location.
- Its own back/forward history.
- Its sort settings (column + direction, may differ from the global default).
- Its filter (hidden-file visibility override).

---

## Keyboard bindings

For the full, up-to-date binding list see [`docs/keymap.md`](keymap.md).
The core multi-pane bindings are:

| Action | Default binding | Description |
|---|---|---|
| Split right | `Cmd+D` | Split the focused pane horizontally (side-by-side) |
| Split down | `Cmd+Shift+D` | Split the focused pane vertically (stacked) |
| Close pane | `Cmd+Shift+W` | Close the focused pane (refused if it is the last pane) |
| Focus left | `Ctrl+H` | Move focus to the nearest pane on the left |
| Focus down | `Ctrl+J` | Move focus to the nearest pane below |
| Focus up | `Ctrl+K` | Move focus to the nearest pane above |
| Focus right | `Ctrl+L` | Move focus to the nearest pane on the right |
| Cycle view | `Cmd+Shift+E` | Rotate the focused pane's view mode (Details→Grid→Gallery→Miller→Tree→…) |
| New tab | `Cmd+T` | Open a new tab in the focused pane |
| Close tab | `Cmd+W` | Close the active tab in the focused pane |
| Cycle tab ← | `Cmd+Shift+[` | Switch to the previous tab |
| Cycle tab → | `Cmd+Shift+]` | Switch to the next tab |
| Tab 1–9 | `Cmd+1`…`Cmd+9` | Jump directly to a tab by position |

---

## Splitting panes

### Two-pane side-by-side

```
Before: [  Pane 0  ]

Press Cmd+D

After:  [ Pane 0 | Pane 1 ]
```

### Three-pane layout

```
Start:  [ Pane 0 | Pane 1 ]

Focus Pane 1, press Cmd+Shift+D

After:  [ Pane 0 | Pane 1 ]
                   ───────
                   Pane 2
```

ASCII diagram of the resulting binary split tree:

```
Split(Horizontal, ratio=0.5)
├── Leaf(Pane 0)
└── Split(Vertical, ratio=0.5)
    ├── Leaf(Pane 1)
    └── Leaf(Pane 2)
```

### Four-pane 2×2 grid

```
[  Pane 0  |  Pane 1  ]
[  Pane 2  |  Pane 3  ]
```

Achieved by:
1. `Cmd+D` → splits pane 0 → `[0 | 1]`
2. Focus pane 0, `Cmd+Shift+D` → `[0/2 | 1]`
3. Focus pane 1, `Cmd+Shift+D` → `[0/2 | 1/3]`

---

## Drag-and-drop between panes

- **Drag** a file or folder from one pane and **drop** into another pane's
  directory listing → **copy** operation (F5).
- Hold **Alt** while dragging → **move** operation (F6).

Progress is shown in the ops panel (bottom of the window).

---

## Configuration recipes

All settings below go in `~/.config/atlas/config.toml`.

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

### Restore last visited directory on startup

```toml
[navigation]
remember_last_location = true
```

> **Note:** persist-on-quit is tracked in `gap-remember-last-location`; startup
> restoration already works when `general.start_path` is set.

### Set default view mode

```toml
[view]
default_mode = "miller"   # details | grid | gallery | miller | tree
```

### Show hidden files by default

```toml
[view]
show_hidden = true
```

### Increase fuzzy search results

```toml
[search]
fuzzy_max_results = 100   # default: 50
```

### Tune thumbnail generation

```toml
[thumbnails]
cache_max_size_mb     = 1000   # default: 500
generation_threads    = 4      # default: num_cpus clamped to 4
```

---

## Saving workspace layouts

Save/restore of named workspace layouts is planned for v0.3+.
See `design/multi-pane-refactor.md` for the roadmap.
