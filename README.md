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

- **Rust** + **[gpui](https://gpui.rs)** (GPU-accelerated UI, the framework behind Zed)
- **[tantivy](https://github.com/quickwit-oss/tantivy)** index in a background `atlas-indexd` daemon
- **[ignore](https://crates.io/crates/ignore)** parallel walker, **[notify](https://crates.io/crates/notify)** watcher
- **[nucleo](https://crates.io/crates/nucleo)** fuzzy matcher, **[grep](https://crates.io/crates/grep)** crates (ripgrep guts) for content search
- **SQLite** thumbnail cache, **TOML** config

## Repository layout

```
crates/
├── atlas-app        # gpui application binary
├── atlas-ui         # views, components, theme
├── atlas-core       # shared types, traits, events
├── atlas-fs         # filesystem abstraction + walker + ops
├── atlas-watch      # notify integration, debouncing
├── atlas-index      # tantivy schema + queries (library)
├── atlas-indexd     # background daemon binary
├── atlas-search     # unified search facade
├── atlas-ops        # file operations queue with progress
├── atlas-keymap     # action dispatch + keymap config
├── atlas-config     # TOML config load/save/watch
├── atlas-ipc        # daemon <-> app protocol + transport
└── atlas-thumbs     # thumbnail generator + sqlite cache
```

## Prerequisites (macOS)

Atlas uses [gpui](https://gpui.rs) for GPU rendering. gpui compiles Metal shaders at build time and **requires the full Xcode toolchain** (Apple's Command Line Tools alone are not sufficient — they do not ship the `metal` / `metallib` utilities).

```bash
# Install Xcode from the App Store, then point the developer directory at it:
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
sudo xcodebuild -license accept

# Verify the Metal toolchain is available:
xcrun --find metal
xcrun --find metallib
```

The non-UI crates (`atlas-fs`, `atlas-index`, `atlas-indexd`, etc.) build fine with just Command Line Tools, so library development can proceed without Xcode.

## Building

```bash
# build everything (requires full Xcode on macOS)
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
