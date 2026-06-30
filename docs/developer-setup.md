# Developer setup

## Toolchain

- **Rust stable** (pinned in `rust-toolchain.toml`)
- **macOS**: full **Xcode** required for `gpui` to compile Metal shaders.
  Command Line Tools alone do not ship `metal` / `metallib`.

## First-time macOS setup

```bash
# 1. Install Xcode from the App Store (one-time, ~12GB).
# 2. Switch the developer directory away from CLT:
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
sudo xcodebuild -license accept

# 3. Verify the Metal compiler:
xcrun --find metal
xcrun --find metallib
```

If you'd rather stay on Command Line Tools while iterating on non-GUI crates,
use the `nogui` profile:

```bash
cargo build --workspace \
  --exclude atlas-app \
  --exclude atlas-ui
```

## Daily commands

```bash
# Run the app
cargo run -p atlas-app

# Run the indexer daemon
cargo run -p atlas-indexd

# Lint + format
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings

# Tests
cargo test --workspace
```

## Crate dependency graph

```
atlas-app
 ├── atlas-ui ─────────┐
 ├── atlas-keymap      │
 ├── atlas-config ──┐  │
 ├── atlas-fs ──────┤  │
 └── atlas-core ←───┴──┘

atlas-indexd
 ├── atlas-index ── atlas-core
 ├── atlas-watch
 ├── atlas-ipc
 ├── atlas-fs
 └── atlas-config

atlas-search
 ├── atlas-index
 └── atlas-ipc
```
