# Atlas

A cross-platform, performance-focused file explorer for developers and power users.

> Status: pre-alpha, in active development. macOS first; Linux and Windows to follow.

## Why

Existing file managers force a trade-off:
- **OS defaults** (Finder, Explorer, Nautilus) are slow, feature-thin, and not keyboard-driven.
- **Power-user GUIs** (fman, Marta, Total Commander, Krusader) have OFM ergonomics but are aging, often closed, frequently single-platform, and feel dated.
- **TUI tools** (Yazi, nnn) are fast and scriptable but ceiling-limited by the terminal.

Atlas combines Yazi's async-Rust speed, Marta-class polish, fman's plugin DX, and Total Commander's depth in a GPU-rendered modern UI.

## Stack

- **Rust** + **[Slint](https://slint.dev)** (GPU-rendered via Skia, declarative UI)
- **[tantivy](https://github.com/quickwit-oss/tantivy)** index in a background `atlas-indexd` daemon
- **[ignore](https://crates.io/crates/ignore)** parallel walker, **[notify](https://crates.io/crates/notify)** watcher
- **[nucleo](https://crates.io/crates/nucleo)** fuzzy matcher, **[grep](https://crates.io/crates/grep)** crates (ripgrep guts) for content search
- **SQLite** thumbnail cache, **TOML** config

## Repository layout

```
crates/
├── atlas-app        # Slint application binary
├── atlas-ui         # views, components, theme
├── atlas-core       # shared types, traits, events
├── atlas-fs         # filesystem abstraction + walker + ops
├── atlas-watch      # notify integration, debouncing
├── atlas-index      # tantivy schema + queries (library)
├── atlas-indexd     # background daemon binary
├── atlas-search    # unified search facade
├── atlas-ops        # file operations queue with progress
├── atlas-keymap     # action dispatch + keymap config
├── atlas-config     # TOML config load/save/watch
├── atlas-ipc        # daemon <-> app protocol + transport
└── atlas-thumbs     # thumbnail generator + sqlite cache
```

## Prerequisites

- **Rust stable** (auto-installed via `rust-toolchain.toml`).
- **macOS**: Apple Command Line Tools (`xcode-select --install`) provide the C/C++ toolchain that Skia's bindings need. The full Xcode IDE is **not** required.
- **Linux**: standard build essentials plus `libfontconfig1-dev`, `libxkbcommon-dev`, and either `libwayland-dev` or X11 dev headers depending on your session.
- **Windows**: MSVC build tools.

## Building

```bash
# build everything
cargo build

# run the app
cargo run -p atlas-app

# fmt + lint + test
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## License

Proprietary. All rights reserved. See `LICENSE`.
