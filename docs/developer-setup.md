# Developer setup

This document is the source of truth for machine setup and daily command lines.
For how to write or triage tests, use
[`.github/skills/testing/SKILL.md`](../.github/skills/testing/SKILL.md). For how
to write or assess benchmarks, use
[`.github/skills/write-benches/SKILL.md`](../.github/skills/write-benches/SKILL.md).

## Toolchain

- **Rust stable** is pinned in `rust-toolchain.toml`. Any `cargo` command from
  the workspace root uses it automatically.
- Install Rust with rustup if needed:

  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  rustup show
  ```

- Install a C/C++ toolchain for Slint/Skia bindings:
  - macOS: Apple Command Line Tools (`xcode-select --install`). The full Xcode
    IDE is not required.
  - Debian/Ubuntu:

    ```bash
    sudo apt install -y build-essential pkg-config libdbus-1-dev \
        libfontconfig1-dev libxkbcommon-dev libwayland-dev \
        libxcb1-dev libxrandr-dev libxi-dev libgl1-mesa-dev
    ```

- Install nextest once per machine:

  ```bash
  cargo install cargo-nextest --locked
  ```

## Repository initialization

```bash
git clone https://github.com/landoncrabtree/atlas.git
cd atlas
cargo build --workspace --locked
```

## Daily commands

These are the workspace-wide gates every PR should satisfy:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --retries 3 --no-fail-fast
cargo test --doc --workspace --locked
cargo build --workspace --locked
```

To fix formatting locally:

```bash
cargo fmt --all
```

Run narrower test diagnostics with the commands in the
[testing skill](../.github/skills/testing/SKILL.md#running-tests).

## Bench commands

Use benches for hot-path performance work:

```bash
# One crate
cargo bench -p atlas-fs

# Whole workspace
cargo bench --workspace

# Save and compare a named baseline
cargo bench -p atlas-fs --bench sort -- --save-baseline main
cargo bench -p atlas-fs --bench sort -- --baseline main
```

Interpretation rules, hot/cold path classification, and commit format live in
[write-benches](../.github/skills/write-benches/SKILL.md).

## Running Atlas locally

```bash
cargo run -p atlas-app
cargo run -p atlas-indexd
```

Debug logging uses `RUST_LOG`:

```bash
RUST_LOG=atlas=debug cargo run -p atlas-app
RUST_LOG=atlas_remote=trace,atlas=info cargo run -p atlas-app
RUST_LOG=atlas_keymap=debug cargo run -p atlas-app
```

## Keymap regeneration

If you edit `crates/atlas-keymap/src/defaults.rs`, regenerate the checked-in
per-platform TOMLs:

```bash
cargo test -p atlas-keymap regen_default_keymap -- --ignored
```

To force a local Atlas install to re-seed defaults on next launch:

```bash
rm -f ~/.config/atlas/keymaps/default.toml
```

## User config paths

The default config directory is resolved by `atlas_config::paths`. Use
`ATLAS_CONFIG_DIR=/path/to/fixture` for tests and portable installs.

| Platform | Path |
|---|---|
| macOS / Linux | `~/.config/atlas/` |
| Linux (XDG) | `$XDG_CONFIG_HOME/atlas/` |
| Windows | `%APPDATA%\Atlas\` |

Inside the config directory:

| File | Purpose |
|---|---|
| `config.toml` | Typed user config |
| `keymaps/default.toml` | User keymap override layered on defaults |
| `themes/*.toml` | User themes |
| `servers.toml` | Saved remote-server catalogue; no secrets |
| `known_hosts` | OpenSSH-compatible SSH host-key store for TOFU |

Secrets never live in `servers.toml`; only opaque `credential_ref` handles are
persisted there. Actual credentials live in the OS keychain.

## Mock servers for remote tests

`tools/mock-servers/` contains Python mock servers for SFTP, FTP, WebDAV, and
S3. They are used by `atlas-remote` integration tests and print
`READY port=<N>` when bound.

Recommended setup:

```bash
cd tools/mock-servers
uv sync
uv run sftp_server.py --port 2222 --data-dir ./data-sftp --user atlas --password atlas
```

Plain-pip fallback:

```bash
cd tools/mock-servers
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
.venv/bin/python sftp_server.py --port 2222 --data-dir ./data-sftp --user atlas --password atlas
```

Skip mock-backed remote tests when Python/uv is unavailable:

```bash
MOCK_SERVERS_SKIP=1 cargo nextest run --workspace --locked --retries 3 --no-fail-fast
```

## computer-use MCP for UI verification

`tools/computer-use-mcp/` lets Copilot CLI drive Atlas through screenshots,
mouse, keyboard, and text input. Use it for visible Slint changes.

Prerequisites: `uv` or Python ≥ 3.10. On macOS, grant Accessibility, Screen
Recording, and Input Monitoring to the Python interpreter that runs the server.

Register it with Copilot CLI:

```bash
copilot mcp add computer-use-py -- uv run --script \
    $(pwd)/tools/computer-use-mcp/server.py
```

Restart Copilot CLI after adding the server.

| Tool | Purpose |
|---|---|
| `take_screenshot` | PNG of the virtual screen |
| `send_keybind(keys)` | Press shortcuts such as `cmd+d`, `cmd+shift+p`, `escape` |
| `type_text(text)` | Type text into focused controls |
| `left_click` / `right_click` / `double_click` | Mouse buttons |
| `move_mouse_to` / `drag_to` / `scroll` | Pointer and wheel input |

On Retina macOS displays, tool inputs use logical points while screenshots are
physical pixels. Divide screenshot coordinates by the scale factor before
clicking.

Example smoke:

```bash
RUST_LOG=atlas=debug cargo run -p atlas-app
# Then use MCP: take_screenshot, send_keybind("cmd+d"), take_screenshot.
```

