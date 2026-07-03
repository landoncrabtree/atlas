<h1 align="center">Atlas</h1>

<p align="center">
  A fast, keyboard-driven cross-platform file explorer for developers and power users.
</p>

<p align="center">
  <a href="https://github.com/landoncrabtree/atlas/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/landoncrabtree/atlas/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="Status" src="https://img.shields.io/badge/status-early%20preview-orange">
  <img alt="Platforms" src="https://img.shields.io/badge/platforms-macOS%20%7C%20Linux%20%7C%20Windows-blue">
  <img alt="License" src="https://img.shields.io/badge/license-MIT-green">
</p>

Atlas combines the speed of terminal file managers with the polish of a native GUI: local and remote filesystems, multi-pane workflows, fast search, and a GPU-rendered Slint interface in one app.

## Screenshot

> Screenshot coming soon. Visual verification captures live outside the repository so large binary assets do not ship in the source tree.

## Features

- **Local + remote filesystems** — browse local paths alongside SFTP, FTP/FTPS, WebDAV, and S3-compatible storage in one interface.
- **Multiple view modes** — Details, Grid, Gallery, and Miller views share the same location model.
- **Tiling workspace** — N-pane splits, per-pane tabs, per-tab history, and tmux-style focus movement.
- **GPU-accelerated Slint UI** — built on Slint's Skia renderer with a 60–144 FPS responsiveness target.
- **Ripgrep-powered content search** — regex-capable search through the `grep` crate family.
- **Fuzzy navigation** — command palette (`⌘⇧P`) and goto-anything (`⌘P`) for actions, paths, and saved servers.
- **Keyboard-first controls** — Vim, WASD, arrow keys, function-key file operations, and fully editable TOML keymaps.
- **Cross-backend operations** — copy, move, delete, rename, mkdir, progress, cancellation, and native trash where available.
- **Remote workflow polish** — saved servers, pooled connections, retries, SFTP TOFU host-key prompts, and keychain-backed credentials.
- **macOS-native design language** — calm, dense, content-first UI that runs cross-platform.

## Performance goals

Atlas treats performance as a product feature: near-instant navigation, smooth 60–144 FPS rendering, low input latency, bounded memory, and efficient async filesystem work. See [`.github/instructions/performance.instructions.md`](.github/instructions/performance.instructions.md) for the full rulebook.

## Configuration

Atlas reads user configuration from `~/.config/atlas/config.toml` on macOS/Linux and `%APPDATA%\Atlas\config.toml` on Windows. A minimal config can start with:

```toml
[general]
dual_pane = true

[ui]
theme = "atlas-dark"
font_family = "Inter"
font_size = 14.0
density = "comfortable"
show_shortcuts = true

[view]
default_mode = "details"
show_hidden = false
natural_sort = true
dirs_first = true

[search]
min_query_length = 2
max_visible_results = 100
debounce_ms = 150
```

For the complete schema, see [`crates/atlas-config/src/schema.rs`](crates/atlas-config/src/schema.rs) and the generated skeleton at [`crates/atlas-config/src/skeleton.toml`](crates/atlas-config/src/skeleton.toml).

## Install

Build Atlas from source:

```bash
git clone https://github.com/landoncrabtree/atlas.git
cd atlas
cargo build --release
./target/release/atlas
```

Toolchain prerequisites and platform notes live in [`docs/developer-setup.md`](docs/developer-setup.md).

## Documentation

| Topic | Link |
|---|---|
| Contributing | [`docs/contributing.md`](docs/contributing.md) |
| Development setup | [`docs/developer-setup.md`](docs/developer-setup.md) |
| Keybindings | [`docs/keymap.md`](docs/keymap.md) |
| Multi-pane concepts | [`docs/multi-pane.md`](docs/multi-pane.md) |
| Design principles | [`.github/instructions/design.instructions.md`](.github/instructions/design.instructions.md) |
| Architecture | [`.github/instructions/architecture.instructions.md`](.github/instructions/architecture.instructions.md) |

## License

Atlas is MIT-licensed. See [`LICENSE`](LICENSE) for the full text.

## Acknowledgements

**Built with [Slint](https://slint.dev)** — the Rust-native GUI toolkit. Atlas also builds on Tantivy, ripgrep's `grep` crates, Nucleo, Tokio, and the Rust ecosystem.
