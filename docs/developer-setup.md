# Developer setup

## Toolchain

- **Rust stable** (pinned in `rust-toolchain.toml` — `1.90` at time of writing).
  If you don't already have `rustup` installed:
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
  Running any `cargo` command from the workspace root picks up the pinned
  toolchain automatically.
- A C/C++ toolchain for Skia bindings (Apple Command Line Tools /
  `build-essential` / MSVC Build Tools).

The full Xcode IDE is **not** required on macOS — Slint with the Skia renderer
uses prebuilt shaders and only needs a working C++ compiler, which the
Command Line Tools package provides.

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

## Daily commands

These are the workspace-wide gates every PR must satisfy:

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

To fix formatting locally: `cargo fmt --all`.

Run the app or the indexer daemon during development:

```bash
cargo run -p atlas-app                     # run the app
cargo run -p atlas-indexd                  # run the indexer daemon
```

Debug logging is controlled by `RUST_LOG` (see [tracing docs](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html) for the filter grammar):

```bash
RUST_LOG=atlas=debug ./target/debug/atlas > /tmp/atlas.log 2>&1 &
tail -F /tmp/atlas.log
```

Common filters:

| Filter | Purpose |
|---|---|
| `atlas=info` | Everything Atlas-owned at info+ |
| `atlas=debug` | Every controller decision and dispatcher fire |
| `atlas_remote=trace,atlas=info` | Network round-trip tracing without spamming UI logs |
| `atlas_keymap=debug` | Watch chord resolution + `keymap-bypass-active` transitions |

## UI authoring

Slint `.slint` files live in `assets/ui/` and are compiled at build time by
`atlas-app/build.rs` via `slint-build::compile`. The `slint::include_modules!()`
macro in `atlas-app/src/main.rs` imports every component declared with `export`.

Layout on disk:

| Directory | Purpose |
|---|---|
| `assets/ui/atlas.slint` | The root `AtlasWindow`. Minimise edits here to reduce merge friction. |
| `assets/ui/theme.slint` | The `Theme` global. Every visible property comes from here. |
| `assets/ui/pane-data.slint` | `PaneSlintData` struct + parallel per-pane data types. |
| `assets/ui/components/` | Reusable widgets (address bar, breadcrumbs, pane, ops panel, connect-server modal, command palette, bulk-rename, operation-progress, search panel, tab bar, titlebar, shortcut footer). |
| `assets/ui/views/{details,grid,gallery,miller,tree}/` | Per-view-mode rendering + row templates. |

Rust-side controllers live under `crates/atlas-ui/src/<feature>/` with a
`mod.rs` + `controller.rs` split — see `remote/`, `palette/`, `search/`,
`rename/`, `ops/`.

For live Slint previews while editing UI:

```bash
cargo install slint-viewer
slint-viewer assets/ui/atlas.slint
```

For end-to-end UI verification (screenshots, driving keybinds), use the
`computer-use-*` MCP tools described below — every UI PR should include a
screenshot proving the change looks right in the running app.

### Reset the local keymap after editing defaults

Atlas seeds `~/.config/atlas/keymaps/default.toml` from
`assets/keymaps/default.<platform>.toml` on first launch. To pick up new
defaults after a keymap change, delete the local file:

```bash
rm -f ~/.config/atlas/keymaps/default.toml
```

If you edit `crates/atlas-keymap/src/defaults.rs`, regenerate the
per-platform TOMLs so `cargo test` stays green:

```bash
cargo test -p atlas-keymap regen_default_keymap -- --ignored
```

A companion test (`test_checked_in_default_toml_matches_emitter`) fails on
normal `cargo test` if the checked-in files drift.

## User config paths

The default location for user config, keymaps, themes, saved-server records,
and the `known_hosts` store is resolved by `atlas_config::paths`. Override
with `ATLAS_CONFIG_DIR=/some/tmpdir` for tests and portable installs.

| Platform | Path |
|---|---|
| macOS / Linux | `~/.config/atlas/` |
| Linux (XDG) | `$XDG_CONFIG_HOME/atlas/` |
| Windows | `%APPDATA%\Atlas\` |

Inside the config directory:

| File | Purpose |
|---|---|
| `config.toml` | Typed user config (see `atlas_config::schema`) |
| `keymaps/default.toml` | User keymap override (layered on top of defaults) |
| `themes/*.toml` | User themes |
| `servers.toml` | Saved remote-server catalogue (see `atlas_config::servers`) |
| `known_hosts` | OpenSSH-compatible SSH host-key store for TOFU |

Secrets never live in `servers.toml` — only opaque `credential_ref` handles
into the OS keychain. Inspect / clear during development:

```bash
cat ~/.config/atlas/servers.toml       # view saved entries
rm ~/.config/atlas/servers.toml        # start fresh (secrets in keychain remain until explicitly purged)
```

## Mock servers for `atlas-remote` integration tests

`tools/mock-servers/` contains four Python-based servers used by the
`atlas-remote` integration suite so we can exercise SFTP / FTP / WebDAV / S3
backends without hitting real infrastructure.

Layout:

```
tools/mock-servers/
├── sftp_server.py       # paramiko-backed SFTP
├── ftp_server.py        # pyftpdlib-backed FTP
├── webdav_server.py     # wsgidav + cheroot
├── s3_server.py         # moto.server-backed S3
├── mock_common.py       # shared CLI + READY-line contract
├── pyproject.toml       # uv-managed
├── uv.lock
├── requirements.txt     # bare-pip fallback
└── README.md
```

Each server prints exactly one line to stdout once bound —
`READY port=<N>` — then serves until it receives `SIGTERM`, at which point
it prints `SHUTDOWN` and exits. Everything else goes to stderr.

### Running a server standalone

Recommended entry point is [`uv`](https://docs.astral.sh/uv/) — it manages a
pinned virtualenv next to the servers:

```bash
cd tools/mock-servers
uv sync   # one-shot: installs pinned deps into .venv/

# SFTP:
uv run sftp_server.py --port 2222 --data-dir /tmp/atlas-sftp \
    --user atlas --password atlas
# FTP:
uv run ftp_server.py --port 2121 --data-dir /tmp/atlas-ftp \
    --user atlas --password atlas
# WebDAV:
uv run webdav_server.py --port 8080 --data-dir /tmp/atlas-webdav \
    --user atlas --password atlas
# S3 (bucket auto-created; fixed test creds documented in the tool README):
uv run s3_server.py --port 5000 --bucket atlas-test
```

If `uv` isn't available, fall back to plain `pip`:

```bash
cd tools/mock-servers
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
.venv/bin/python sftp_server.py --port 2222 --data-dir /tmp/x --user atlas --password atlas
```

### How the Rust harness spawns them

`crates/atlas-remote/tests/common/mock.rs` runs each server as a subprocess,
parses the `READY port=<N>` line, and sends `SIGTERM` when the `MockXxxServer`
value drops. The `spawn_*` helpers pre-run `uv sync` on first use so the
harness is self-contained after a clean checkout.

To skip every remote integration test (offline, CI without Python, or a
hostile sandbox):

```bash
MOCK_SERVERS_SKIP=1 cargo test --workspace
```

Each of `crates/atlas-remote/tests/{sftp,ftp,webdav,s3,cross_backend_stream}.rs`
short-circuits when this env var is set.

## `computer-use-*` MCP tools

`tools/computer-use-mcp/` is a small MCP server that lets Copilot CLI (or any
MCP client) drive Atlas via desktop automation — screenshots, mouse, keyboard.
It sits alongside the stock `computer-use` nut-js server, and works around
Slint / Skia apps that occasionally ignore keystrokes delivered through the
usual accessibility APIs.

The server uses [`pyautogui`](https://pyautogui.readthedocs.io) which, on
macOS, routes keyboard events through `Quartz.CGEventCreateKeyboardEvent` —
the same path physical keyboards use — so Slint/Skia apps see the events
reliably. On Linux and Windows pyautogui uses the native automation APIs
directly.

### Installing

Prerequisites: `uv` (recommended) or Python ≥ 3.10. On macOS, System Settings →
Privacy & Security must grant **Accessibility**, **Screen Recording**, and
**Input Monitoring** to the Python interpreter that runs the server (the
first time a tool call is issued, macOS will prompt).

Register with Copilot CLI by adding an entry to your MCP config, or:

```bash
copilot mcp add computer-use-py -- uv run --script \
    $(pwd)/tools/computer-use-mcp/server.py
```

Restart the Copilot CLI session so the new server is discovered.

### Tools exposed

| Tool | Purpose |
|---|---|
| `take_screenshot` | PNG of the whole virtual screen |
| `get_cursor_position` | `{x, y}` in logical pixels |
| `screen_size` | `{width, height}` in logical pixels |
| `move_mouse_to(x, y, duration_ms?)` | Move the mouse cursor |
| `left_click(x?, y?)` / `right_click(x?, y?)` / `double_click(x?, y?)` | Mouse buttons |
| `type_text(text, interval_ms?)` | Type ASCII characters |
| `send_keybind(keys)` | Press a shortcut, e.g. `cmd+d`, `cmd+shift+p`, `f5`, `escape` |
| `scroll(dy, dx?, x?, y?)` | Wheel scroll (+dy = down, +dx = right) |
| `drag_to(x, y, duration_ms?, button?)` | Press-and-drag from current position |

### Coordinate system

On macOS Retina displays there are **two** coordinate systems and MCP tools
use only one of them:

| System | Typical for a MacBook Pro 15 | Used by |
|---|---|---|
| Logical (points) | 1512 × 982 | `computer-use-*` tool inputs |
| Physical (pixels) | 2000 × 1305 | Screenshots returned by `take_screenshot` |

Scale factor ≈ 1.323×. If you locate a UI element in a screenshot at physical
pixel `(x, y)`, divide by the scale factor before feeding it back into
`left_click` or `move_mouse_to`. `screen_size` reports the logical size, so
if your resolution differs you can compute the ratio directly.

### Example: launch atlas, split a pane, screenshot

```
# 1. Launch under RUST_LOG so failures leave a trace:
RUST_LOG=atlas=debug ./target/debug/atlas > /tmp/atlas.log 2>&1 &

# 2. From the Copilot CLI (or any MCP client), drive the app:
computer-use-take_screenshot()
computer-use-send_keybind("cmd+d")         # split pane right
computer-use-take_screenshot()             # verify the split rendered
computer-use-send_keybind("cmd+k")         # open Connect-to-Server modal
computer-use-type_text("sftp://user@example.com/tmp")
computer-use-send_keybind("enter")
computer-use-take_screenshot()             # verify TOFU banner or auth prompt
```

Live UI verification like this is expected on any PR that changes a Slint
component. Attach the screenshot(s) to the PR description.

## Crate dependency graph

```
atlas-app
 ├── atlas-ui ─────────┐
 ├── atlas-keymap      │
 ├── atlas-config ──┐  │
 ├── atlas-fs ──────┤  │
 ├── atlas-ops      │  │
 ├── atlas-remote ──┤  │
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

atlas-remote
 ├── atlas-core
 ├── atlas-config
 └── atlas-fs
```

Per-backend remote crates (`russh`, `russh-sftp`, `suppaftp`, `reqwest`,
`quick-xml`, `object_store`, `keyring`) all live behind
`atlas-remote::vm::BackendClient`; consumers only see `atlas_fs::LocationViewModel`.

## Known flakies

These two tests occasionally fail under high parallel load and are known to
be filesystem-timing flakies rather than real bugs. Both retry-clean:

- `atlas_config::watcher_reload_and_error` — FSEvents timing race between
  `notify` debounce and the test's assertions.
- `atlas_watch::test_created_event` — macOS FSEvents drops the `Create`
  event under parallel test load.

If you hit one, re-run the specific test:

```bash
cargo test -p atlas-config watcher_reload_and_error
cargo test -p atlas-watch  test_created_event
```

Don't chase these unless you can reproduce them deterministically — see the
`fix-flaky-test` skill in `.github/skills/fix-flaky-test/SKILL.md` for the
protocol we follow.

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

If either variable is unset, the corresponding step is silently skipped — you
still get a working (unsigned) bundle and DMG that testers can right-click → Open.

### Notes

- Icons: place artwork PNGs in `dist/icons/atlas.iconset/` before release;
  `build-app.sh` generates a solid-color placeholder automatically so the
  bundle is always valid.
- Themes and keymaps in `assets/` are copied into `Contents/Resources/` so
  Atlas can seed user directories on first launch.
- `atlas-indexd` is bundled at `Contents/MacOS/atlas-indexd`; a LaunchAgent
  plist is placed at
  `Contents/Library/LaunchAgents/dev.atlas.atlas-indexd.plist` for
  post-install daemon registration.

### Planned (v0.2)

- Sparkle auto-updater integration
- Linux `.deb` / `.rpm` / AppImage
- Windows MSI / MSIX

## Licensing note (Slint)

Slint ships under three license tracks: **GPLv3**, the free **Royalty-Free
Desktop License** (with attribution conditions, available to qualifying
individuals/small companies), and a paid **commercial license**. Atlas is
published under a proprietary license, so the project must hold either the
RFD or the commercial license before distribution. See
<https://slint.dev/pricing> for current terms.
