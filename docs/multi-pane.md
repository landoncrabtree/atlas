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

A tile in the workspace. Every pane has:

- **Tabs** — one or more location tabs.
- **View mode** — Details, Grid, Gallery, Miller, or Tree. Each pane cycles its own view mode independently.
- **Back/forward history** — per-tab, bounded by `navigation.history_size`.
- **Selection** — per-pane; drag from one pane to another triggers a copy/move.
- **Location** — the pane's active tab points at an `atlas_core::Location`. Locations are either `Local(PathBuf)` or `Remote(RemoteUri, BackendKind)` — see [Remote panes](#remote-panes) below.
- **Status bar** — each pane has its own bottom status bar showing entry counts, free-space (local) or connection chip (remote), view mode, and sort.

### Tab

A location within a pane. Each tab remembers:

- Its `Location` (Local path or Remote URI).
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
| Connect to server | `Cmd+K` (macOS) / `Ctrl+Alt+K` (Linux/Windows) | Open the Connect-to-Server modal to mount an SFTP/FTP/WebDAV/S3 backend in the focused pane |
| Toggle dual pane | `Cmd+\` | Add a second pane, or close it if one already exists |

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

Progress shows in the operations panel (bottom of the window). Operations that
take longer than ~250 ms also raise the operation-progress modal with Cancel /
Background buttons; hitting Background demotes the op back to the panel while
it keeps running.

Cross-pane copies work across backends too: dragging from a local pane into a
mounted SFTP/S3/WebDAV pane routes through `atlas_ops::execute_op`, which
picks the right path (native rename, spawn_blocking primitive, or
`atlas_remote::stream::stream_copy`) based on the source/destination
`BackendKind`.

---

## Remote panes

Any pane can hold a remote location instead of a local path. Open the
Connect-to-Server modal with **Cmd+K** (⌥⌃K on Linux/Windows) or the palette
entry "Connect to Server". Supported backends:

| Backend | Scheme | Underlying crate |
|---|---|---|
| SFTP | `sftp://user@host[:port]/path` | `russh` + `russh-sftp` |
| FTP  | `ftp://user@host[:port]/path`  | `suppaftp` |
| WebDAV | `webdav://user@host[:port]/path` | `reqwest` + `quick-xml` |
| S3   | `s3://bucket[/prefix]` | `object_store` (Apache Arrow) |

The modal parses either the individual form fields (host / port / path /
username / password / SSH key / IAM keys) or a free-form connection string.
Hitting **Save to keychain** persists the entry to `~/.config/atlas/servers.toml`
with a `credential_ref` pointing at the OS keychain — the actual secret never
touches the config file. Saved servers appear:

- In the connect modal's "Saved servers" list on next open.
- In the Cmd+P "Go to Anything" palette alongside local paths.

The pane's status bar renders a connection chip while remote:

- 🟢 **connected** — the backend client is healthy.
- 🟡 **reconnecting…** — the retry envelope in `atlas_remote::retry` is
  backing off between attempts on a transient network failure.
- 🔴 **disconnected** — auth failed, TLS/host key rejected, or all retries
  exhausted.

### TOFU (trust-on-first-use) for SSH host keys

The first time you SFTP to a host whose key is not in
`~/.config/atlas/known_hosts`, Atlas raises an accent-blue banner in the
Connect modal showing the offered fingerprint. **Trust always** stores the
key OpenSSH-compatibly and the pane mounts. If the fingerprint later changes,
Atlas raises a red-warning banner and refuses the connection until you
explicitly re-accept the new key.

### Connection pool + retry

Multiple panes / tabs pointing at the same host share a single backend client
via `atlas_remote::pool::ConnectionPool`. Idle clients evict after a TTL so a
quick navigate-back reuses the still-authenticated session, but a truly
dormant tab does not hold a socket open indefinitely.

Every network round-trip is wrapped by `atlas_remote::retry::RetryPolicy` —
transient `Network` errors retry with exponential backoff + jitter; auth /
not-found / already-exists errors propagate immediately.

---

## Cross-pane scroll preservation

Scrolling one pane never scrolls the other. Under the hood this is preserved
by binding each `panes-*` Slint property to a persistent `Rc<VecModel>` **once**
at startup and mutating rows in place on subsequent pushes (see
`OuterPaneModels::sync_vec_model` in `crates/atlas-ui/src/shell.rs`). Replacing
the outer model would invalidate the `ListView`'s scroll offset, so the shell
never calls `set_panes_*(new VecModel::from(…))` after the initial bind.

## Chord routing across modals and text inputs

Cmd+A / Cmd+C / Cmd+V and other pane chords are routed through a single
keymap-bypass gate in `atlas.slint`. When any modal is visible **or** any
text input owns focus (the address bar, palette input, bulk-rename inputs,
connect-modal fields), the dispatcher restricts to the `Global` context so
`Pane` bindings return `false` and the key falls through to the input's
native edit behaviour. See `docs/keymap.md` and
`.github/instructions/ui-composition.instructions.md` for details.

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

### Tune search-panel responsiveness

Atlas debounces the search-panel query, requires a minimum length before it hits the index, and caps the number of rendered rows to keep the ListView responsive on very common queries.

```toml
[search]
min_query_length    = 2     # default: 2 — chars before the query fires
max_visible_results = 100   # default: 100 — hard cap on rendered rows
debounce_ms         = 150   # default: 150 — quiet time before submit
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
