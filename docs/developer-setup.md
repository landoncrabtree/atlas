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

The full Xcode IDE is **not** required on macOS — Slint 1.17 with the Skia
renderer uses prebuilt shaders and only needs a working C++ compiler, which
the Command Line Tools package provides.

## First-time macOS setup

```bash
xcode-select --install        # if you don't already have CLT
rustup show                   # confirms the toolchain installs from rust-toolchain.toml
```

## First-time Linux setup (Debian/Ubuntu)

```bash
sudo apt install -y build-essential pkg-config libdbus-1-dev \
    libfontconfig1-dev libxkbcommon-dev libwayland-dev \
    libxcb1-dev libxrandr-dev libxi-dev libgl1-mesa-dev
```

`libdbus-1-dev` is needed for `keyring` and `arboard`; the CI matrix
installs it explicitly on Linux runners.

## Daily commands

These are the workspace-wide gates every PR must satisfy:

```bash
cargo build --workspace
cargo nextest run --workspace --retries 3      # tests — see "Running tests" below
cargo test --doc --workspace                   # doctests (nextest does not run doctests yet)
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
RUST_LOG=atlas=debug ./target/debug/atlas > atlas.log 2>&1 &
tail -F atlas.log
```

Common filters:

| Filter | Purpose |
|---|---|
| `atlas=info` | Everything Atlas-owned at info+ |
| `atlas=debug` | Every controller decision and dispatcher fire |
| `atlas_remote=trace,atlas=info` | Network round-trip tracing without spamming UI logs |
| `atlas_keymap=debug` | Watch chord resolution + `keymap-bypass-active` transitions |

## Running tests

Atlas uses [`cargo-nextest`](https://nexte.st) as the primary test runner —
both for local development and in CI (`.github/workflows/ci.yml`). Nextest
runs each test binary in its own process (better isolation), executes
binaries in parallel, and supports **per-test retries**, which lets us
tolerate the small set of documented filesystem-timing flakies without
silencing them via `#[ignore]`.

Install nextest once per machine:

```bash
cargo install cargo-nextest --locked
```

Run the full suite the way CI does:

```bash
cargo nextest run --workspace --retries 3
```

Doctests do not run under nextest (upstream limitation) — invoke them
separately:

```bash
cargo test --doc --workspace
```

Run a single test with logging:

```bash
RUST_LOG=atlas_remote=debug \
    cargo nextest run -p atlas-remote sftp::list_dir_streams -- --nocapture
```

The `--retries 3` flag is the same value the CI matrix uses
(`cargo nextest run --workspace --locked --retries 3 --no-fail-fast`).
Keeping the local invocation aligned with CI is the whole point: a test
that CI passes should pass locally, and a test that fails locally after
three attempts is a real bug, not a flake.

### Known filesystem-timing flakies

nextest's `--retries` handles the small documented set of tests that
occasionally flake under high parallel load. These are timing races, not
correctness bugs — they retry clean and should not be treated as
regressions:

- `atlas-watch::test_*` — macOS FSEvents drops `Create` / `Modify` events
  under parallel test load.
- `atlas-config::watcher_reload_and_error` — `notify` debouncer race
  against the assertion window.
- `theming::watcher::hot_reload_on_file_change` — same FSEvents
  debouncer race as above, tested against the themes directory.
  Can occasionally exhaust all 4 nextest retries within a single run
  (see e.g. run [`28665808289`][flake-hot-reload]); a full workflow
  re-run typically clears it.
- `views::miller::controller::set_root_opens_one_column` — Miller
  controller waits on an async load that occasionally exceeds the
  fixture timeout on cold caches.

[flake-hot-reload]: https://github.com/landoncrabtree/atlas/actions/runs/28665808289/job/85017127646

Follow the protocol in
[`.github/skills/fix-flaky-test/SKILL.md`](../.github/skills/fix-flaky-test/SKILL.md)
before spending debug time on any of these — the fastest path is to run
the failing test on the parent commit and confirm the failure is
pre-existing.

Do **not** add `sleep(…)` to "fix" a flaky. Fix the underlying race, or
add an explicit `wait_until` helper that polls for the expected state.

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
| `assets/ui/components/` | Reusable widgets and modals (address bar, breadcrumbs, pane, ops panel, connect-server modal, command palette, bulk-rename, operation-progress, search panel, tab bar, titlebar, shortcut footer). |
| `assets/ui/views/{details,grid,gallery,miller}/` | Per-view-mode rendering + row templates. |

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
per-platform TOMLs so the checked-in files stay byte-identical:

```bash
cargo test -p atlas-keymap regen_default_keymap -- --ignored
```

A companion test (`test_checked_in_default_toml_matches_emitter`) fails on
a normal test run if the checked-in files drift. This is one of the few
places where `cargo test` (not `cargo nextest`) is called out explicitly:
the regen test is `#[ignore]`d and needs the plain-test `--ignored` flag.

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
uv run sftp_server.py --port 2222 --data-dir ./data-sftp \
    --user atlas --password atlas
# FTP:
uv run ftp_server.py --port 2121 --data-dir ./data-ftp \
    --user atlas --password atlas
# WebDAV:
uv run webdav_server.py --port 8080 --data-dir ./data-webdav \
    --user atlas --password atlas
# S3 (bucket auto-created; fixed test creds documented in the tool README):
uv run s3_server.py --port 5000 --bucket atlas-test
```

If `uv` isn't available, fall back to plain `pip`:

```bash
cd tools/mock-servers
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
.venv/bin/python sftp_server.py --port 2222 --data-dir ./data-sftp \
    --user atlas --password atlas
```

### How the Rust harness spawns them

`crates/atlas-remote/tests/common/mock.rs` runs each server as a subprocess,
parses the `READY port=<N>` line, and sends `SIGTERM` when the `MockXxxServer`
value drops. The `spawn_*` helpers pre-run `uv sync` on first use so the
harness is self-contained after a clean checkout.

To skip every remote integration test (offline, CI without Python, or a
hostile sandbox):

```bash
MOCK_SERVERS_SKIP=1 cargo nextest run --workspace
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
RUST_LOG=atlas=debug ./target/debug/atlas > atlas.log 2>&1 &

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

## Licensing note (Slint)

Slint ships under three license tracks: **GPLv3**, the free **Royalty-Free
Desktop License** (with attribution conditions, available to qualifying
individuals/small companies), and a paid **commercial license**. Atlas is
published under a proprietary license, so the project must hold either the
RFD or the commercial license before distribution. See
<https://slint.dev/pricing> for current terms.
