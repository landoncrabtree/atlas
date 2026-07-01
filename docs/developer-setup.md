# Developer setup

## Toolchain

- **Rust stable** (pinned in `rust-toolchain.toml`)
- A C/C++ toolchain for Skia bindings (Apple Command Line Tools / build-essential / MSVC)

The full Xcode IDE is **not** required on macOS — Slint with the Skia renderer uses
prebuilt shaders and only needs a working C++ compiler, which the Command Line Tools
package provides.

## First-time macOS setup

```bash
xcode-select --install        # if you don't already have CLT
rustup show                   # confirms the toolchain installs from rust-toolchain.toml
```

## First-time Linux setup (Debian/Ubuntu)

```bash
sudo apt install -y build-essential pkg-config libfontconfig1-dev libxkbcommon-dev \
    libwayland-dev libxcb1-dev libxrandr-dev libxi-dev libgl1-mesa-dev
```

## UI authoring

Slint `.slint` files live in `assets/ui/` and are compiled at build time by
`atlas-app/build.rs` via `slint-build::compile`. The `slint::include_modules!()`
macro in `atlas-app/src/main.rs` imports every component declared with `export`.

For live previews while editing UI:

```bash
cargo install slint-viewer
slint-viewer assets/ui/atlas.slint
```

## Daily commands

```bash
cargo run -p atlas-app                     # run the app
cargo run -p atlas-indexd                  # run the indexer daemon

cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
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

## Packaging (macOS)

```bash
dist/release.sh
```

Produces `target/dist/Atlas.app` and `target/dist/Atlas-<version>.dmg`.

Individual steps:

```bash
dist/build-app.sh   # compile release binaries, assemble Atlas.app
dist/sign.sh        # code-sign with hardened runtime (requires credentials)
dist/build-dmg.sh   # create compressed DMG
dist/notarize.sh    # submit to Apple Notary Service and staple ticket
```

Signing and notarization require environment variables:

| Variable | Description |
|----------|-------------|
| `ATLAS_SIGNING_IDENTITY` | `Developer ID Application: Name (TEAMID)` — from your Apple developer keychain |
| `ATLAS_NOTARY_PROFILE` | Keychain profile name for `notarytool`; set via `xcrun notarytool store-credentials <profile-name>` |

If either variable is unset, the corresponding step is silently skipped — you still get a working (unsigned) bundle and DMG that testers can right-click → Open.

### Notes

- Icons: place artwork PNGs in `dist/icons/atlas.iconset/` before release; `build-app.sh` generates a solid-color placeholder automatically so the bundle is always valid.
- Themes and keymaps in `assets/` are copied into `Contents/Resources/` so Atlas can seed user directories on first launch.
- `atlas-indexd` is bundled at `Contents/MacOS/atlas-indexd`; a LaunchAgent plist is placed at `Contents/Library/LaunchAgents/dev.atlas.atlas-indexd.plist` for post-install daemon registration.

### Planned (v0.2)

- Sparkle auto-updater integration
- Linux `.deb` / `.rpm` / AppImage
- Windows MSI / MSIX

## Licensing note (Slint)

Slint ships under three license tracks: **GPLv3**, the free **Royalty-Free Desktop
License** (with attribution conditions, available to qualifying individuals/small
companies), and a paid **commercial license**. Atlas is published under a proprietary
license, so the project must hold either the RFD or the commercial license before
distribution. See <https://slint.dev/pricing> for current terms.
