<h1 align="center">Atlas</h1>

<p align="center">
  A cross-platform, performance-focused file explorer for developers and power users.
</p>

<p align="center">
  <a href="https://github.com/landoncrabtree/atlas/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/landoncrabtree/atlas/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="Status" src="https://img.shields.io/badge/status-pre--alpha-orange">
  <img alt="Platforms" src="https://img.shields.io/badge/platforms-macOS%20%7C%20Linux%20%7C%20Windows-blue">
  <img alt="License" src="https://img.shields.io/badge/license-proprietary-lightgrey">
</p>

> **Status**: pre-alpha. macOS first; Linux and Windows to follow.

---

## What is Atlas?

Atlas is a keyboard-first file manager built for people who live in their file tree: developers, sysadmins, data folks, and power users. It combines the **speed of terminal tools** (Yazi, nnn), the **ergonomics of orthodox file managers** (Total Commander, Marta, fman), and a **modern GPU-rendered UI**.

It's the file manager we wanted: fast, scriptable, keyboard-driven, with no compromises on polish.

## Why

Today's options force a trade-off:

- **OS defaults** (Finder, Explorer, Nautilus) are slow, feature-thin, and mouse-driven.
- **Power-user GUIs** (fman, Marta, Total Commander, Krusader) have great ergonomics but are aging, often closed, frequently single-platform, and feel dated.
- **TUI tools** (Yazi, nnn) are fast and scriptable but ceiling-limited by the terminal.

Atlas refuses the trade-off.

## Features

### Shipping in MVP

- **Multiple view modes** — Details, Grid, Gallery, Miller columns, Tree
- **Dual-pane layout** with cross-pane file operations
- **Tabs per pane**
- **Command palette** (`⌘⇧P`) and goto-anything (`⌘P`)
- **Keyboard-first** — every action is reachable without the mouse; vim navigation; configurable keymap
- **F-key file operations** — F3 view, F4 edit, F5 copy, F6 move, F7 mkdir, F8 delete
- **Fuzzy search** powered by `nucleo`
- **Content search** powered by the `ripgrep` engine, with regex
- **Background indexer daemon** — instant search across millions of files
- **GPU-rendered UI** via Slint + Skia, never blocks on I/O
- **Bulk rename** with regex and live preview
- **Themes** (dark + light, TOML-customizable)
- **TOML config** with hot reload

### Planned (post-MVP)

- Remote and cloud filesystems (SSH/SFTP, S3, Azure Blob, GCS, WebDAV, SMB)
- Plugin system with capability-based sandboxing
- N-pane splits and workspaces (save/restore layouts)
- Git-aware columns
- Embedded terminal
- Container/devcontainer awareness
- AI-assisted semantic search (local-model-first)

## Screenshots

> Coming soon. The shell is being assembled; expect screenshots once Grid and Miller views land.

## Installation

> Pre-built binaries will be published once Atlas reaches alpha. Until then, build from source.

### From source

```bash
git clone https://github.com/landoncrabtree/atlas.git
cd atlas
cargo build --release
./target/release/atlas
```

Toolchain prerequisites and platform-specific notes live in [`docs/developer-setup.md`](docs/developer-setup.md).

## Quick start

```bash
cargo run -p atlas-app           # run the app from source
cargo run -p atlas-indexd        # run the indexer daemon
```

Default keybindings:

| Key | Action |
|---|---|
| `⌘⇧P` | Open command palette |
| `⌘P` | Goto anything (paths) |
| `⌘T` / `⌘W` | New tab / close tab |
| `Tab` | Switch focus between panes |
| `hjkl` | Vim-style navigation |
| `Enter` | Activate (cd or open) |
| `Backspace` | Go up one directory |
| `Space` | Toggle selection |
| `F3` | View · `F4` Edit · `F5` Copy · `F6` Move · `F7` Mkdir · `F8` Delete |

User overrides live at `~/.config/atlas/keymap.toml`.

## Performance

Performance is a defining feature, not an afterthought. Goals (MVP):

- **Cold launch** to first interactive frame in under **200 ms** on M-series Macs
- **Smooth 60+ fps** scrolling through 100k-file directories
- **Fuzzy path search** across a 1M-doc index in under **50 ms** p99
- **Content search** within **1.2× of `ripgrep`** on the same fixture
- **Memory** under **250 MB** resident after an hour of typical use
- **Single-binary `.app` bundle** under **30 MB** compressed

Benchmark numbers will appear here as the harness lands.

## Configuration

User config lives at `~/.config/atlas/config.toml` (or `%APPDATA%\Atlas\config.toml` on Windows). The file is heavily commented and supports hot reload — save it, and Atlas picks up the change immediately.

```toml
[ui]
theme = "atlas-dark"
font_family = "Inter"
font_size = 14.0
density = "comfortable"

[view]
default_mode = "details"
show_hidden = false
natural_sort = true
dirs_first = true

[indexer]
enabled = true
roots = ["~/code", "~/Documents"]
respect_gitignore = true
```

## Documentation

- [`docs/developer-setup.md`](docs/developer-setup.md) — toolchain, prerequisites, daily commands
- [`docs/contributing.md`](docs/contributing.md) — workflow, commit conventions, code style

## Contributing

Atlas is proprietary, but contributions are welcome at the maintainer's discretion. Start with [`docs/contributing.md`](docs/contributing.md) for the workflow and quality bar.

## License

Atlas is **proprietary** software. All rights reserved. See [`LICENSE`](LICENSE).

Atlas builds on top of excellent open-source software — notably [Slint](https://slint.dev), [tantivy](https://github.com/quickwit-oss/tantivy), the [ripgrep](https://github.com/BurntSushi/ripgrep) family of crates, and [nucleo](https://github.com/helix-editor/nucleo). Atlas's use of Slint operates under the appropriate Slint license track (see [`docs/developer-setup.md`](docs/developer-setup.md)).
