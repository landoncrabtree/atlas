# Copilot instructions for Atlas

These instructions apply to all Copilot interactions in this repository (chat, code review, coding agent). They supplement, not replace, per-task instructions.

## Project at a glance

Atlas is a cross-platform, performance-focused file explorer for developers and power users, written in **Rust** with a **Slint** (Skia renderer) UI. Architecture is a Cargo workspace of small, focused crates. macOS is the primary target; Linux and Windows follow. License is **MIT**.

## North-star principles

1. **Performance is a feature.** Never block the UI thread on I/O. Stream results. Virtualize lists. Cache aggressively. Measure before optimizing — but design for speed.
2. **Async-first, channel-based APIs.** Library crates expose work via `crossbeam-channel` receivers or futures, not synchronous blocking calls. The UI consumes them; consumers pick their executor.
3. **Crate boundaries are real boundaries.** Each crate owns one concern. Cross-crate coupling goes through `atlas-core` types or explicit traits — never reach into another crate's internals.
4. **Local-first, no telemetry.** No phone-home, no analytics, no remote logging. Period.
5. **Keyboard-first UX.** Every action must be reachable by keyboard. Mouse is convenience, not primary.

## Tech stack (do not change without discussion)

| Concern | Tool |
|---|---|
| Language | Rust stable (pinned in `rust-toolchain.toml`) |
| UI | Slint 1.17 + Skia renderer |
| FS walking | `ignore` (ripgrep's) |
| FS watching | `notify` + `notify-debouncer-full` |
| Search index | `tantivy` (in a separate `atlas-indexd` daemon) |
| Fuzzy matching | `nucleo` |
| Content search | `grep` + `grep-regex` (ripgrep guts) |
| Config | TOML via `serde` + `toml_edit` (preserves comments) |
| IPC | Unix domain sockets via `interprocess` + `bincode` framing |
| Logging | `tracing` + `tracing-subscriber` |
| Thumbnail cache | `rusqlite` (WAL mode) |
| Concurrency primitives | `parking_lot`, `crossbeam-channel`, `arc-swap`, `dashmap`, `rayon` |
| Remote SFTP | `russh` + `russh-sftp` |
| Remote FTP/FTPS | `suppaftp` |
| Remote WebDAV | `reqwest` + `quick-xml` |
| Remote S3 (and compatibles) | `object_store` |
| Remote runtime | `tokio` (multi-threaded, shared inside `atlas-remote` + `atlas-ops` only) |

**Do not** introduce a new GUI framework, a new async runtime in library crates outside the remote/ops shared tokio (use channels), or a new error library (use `thiserror` + `anyhow`). **OpenDAL was removed** — do not re-introduce it or any other unified remote-fs abstraction; every backend has its own dedicated crate under `atlas-remote::vm::<scheme>`.

We already evaluated and rejected **gpui** (requires full Xcode for Metal shaders) for Slint. Do not re-suggest gpui.

## Workspace layout

14 crates under `crates/atlas-*` — see [`.github/instructions/architecture.instructions.md`](instructions/architecture.instructions.md) for the full inventory, purpose, and key dependencies per crate.

Each crate's `Cargo.toml` consumes dependencies via `workspace.dependencies`. Add to the **crate's** `Cargo.toml` from existing workspace deps. **Do not** add new dependencies to the root workspace `Cargo.toml` without explicit approval.

## Coding standards

### General Rust

- **`cargo fmt --all`** must pass.
- **`cargo clippy --workspace --all-targets -- -D warnings`** must pass.
- No `println!`, `eprintln!`, `dbg!`, or bare `unwrap()` / `expect()` in non-test code. Use `tracing::{info,warn,error,debug}!` and `?` with proper error types.
- Prefer `&str` / `&Path` in arguments; return owned types only when ownership is transferred.
- Module-level rustdoc on every module; doc comments on every `pub` item.
- Workspace lints (`unsafe_op_in_unsafe_fn`, `unreachable_pub`, `unused_must_use = deny`, `dbg_macro`, `todo`, `print_stdout`, `print_stderr`) are part of the contract.

### Error handling

- Library crates: define a `thiserror::Error` enum (or use `atlas_core::AtlasError`); never return `anyhow::Error` from library APIs.
- Binaries and integration glue: `anyhow::Result` is fine.
- Always include enough context to debug: paths, operation names, source errors via `#[source]`.

### Performance hygiene

The non-negotiable headline: **no blocking I/O on the UI thread or any thread serving the UI.**
Measure before optimizing hot paths; use [`.github/skills/write-benches/SKILL.md`](skills/write-benches/SKILL.md) for benchmark setup, interpretation, and perf commit format.
Performance rules live in [`.github/instructions/performance.instructions.md`](instructions/performance.instructions.md).

### Concurrency

- The UI process uses Slint's event loop. Cross-thread updates go through `slint::invoke_from_event_loop`.
- The indexer daemon uses `tokio` (multi-threaded runtime).
- Library crates **do not** depend on tokio or a specific runtime; expose channel-based APIs that work with either.

### Filesystem

- Use `atlas_fs` for any local directory listing or walking. Do not call `std::fs::read_dir` directly outside of `atlas-fs` and `atlas-indexd`.
- Always tilde-expand user-facing paths via `atlas_core::path::expand_tilde`.
- Be symlink-aware: capture targets, mark broken symlinks, never silently follow.
- Pane locations are `atlas_core::Location`, an enum of `Local(PathBuf)` and `Remote(RemoteUri, BackendKind)`. **Never assume a location is local.** Use `Location::as_local()` to short-circuit local-only fast paths (thumbnails, native trash, notify watcher, disk-free space); everything else must route through the backend-agnostic `atlas-fs` / `atlas-ops` interfaces.

### Remote filesystems

- Remote work lives in `atlas-remote` with one submodule per scheme: `vm/sftp.rs`, `vm/ftp.rs`, `vm/webdav.rs`, `vm/s3.rs`. Extending remote support (new backend, new capability) follows [`.github/instructions/remote-backend-authoring.instructions.md`](instructions/remote-backend-authoring.instructions.md).
- All backends share one tokio runtime (`atlas_remote::runtime::handle()`), a connection pool with idle eviction, an exponential-backoff retry envelope, and a TOFU flow for host-key acceptance.
- Credentials never touch `servers.toml`. Only opaque `credential_ref` handles are persisted; secrets live in the OS keychain (macOS Keychain, libsecret, Windows Credential Manager) under the `com.atlas.credentials` namespace.

### UI (Slint + Rust glue)

- `.slint` components live under `assets/ui/`. Split by responsibility; one entry component imports the rest.
- Use the `Theme` global for colors/spacing/fonts. No hard-coded colors in components.
- Rust ↔ Slint state goes through `AppShell` adapter methods in `atlas-ui`; never let arbitrary code touch the `slint::Weak<Window>` directly.
- Every Slint callback dispatches a typed `UiAction` to the `ActionSink`. Add new variants to `UiAction` rather than calling business logic from the callback.
- New modals, panels, view modes, or context menus must follow the canonical flow in [`.github/instructions/ui-composition.instructions.md`](instructions/ui-composition.instructions.md). Every UI PR includes a screenshot from the `computer-use-*` MCP tools proving the change looks right.

### Keybinds

- **One action ID per behaviour.** Multiple chords may alias onto the same action, never the other way round. Example: `l`, `right`, `.`, `enter` all bind to `fs::View`.
- Adding a new keybind follows [`.github/instructions/keybind-authoring.instructions.md`](instructions/keybind-authoring.instructions.md) — register the action metadata in `crates/atlas-keymap/src/defaults.rs`, add per-OS defaults, wire the dispatcher handler in `crates/atlas-app/src/main.rs::build_dispatcher`, regen the TOMLs, and update `docs/keymap.md`.

### Configuration

- All user-facing config lives in `~/.config/atlas/config.toml` (or `%APPDATA%\Atlas\config.toml` on Windows). Tests use `ATLAS_CONFIG_DIR` override.
- New config fields require `#[serde(default)]` + `#[serde(deny_unknown_fields)]` and a sane `Default`.
- Saving must preserve user comments (use `toml_edit`, not `toml::to_string`).

### Testing

New behavior needs behavior coverage; writing, running, and flaky-triage rules
live in [`.github/skills/testing/SKILL.md`](skills/testing/SKILL.md).

## Commit conventions and pull requests

Commit format, PR standards, and the Copilot trailer live in
[`docs/contributing.md`](../docs/contributing.md). Keep commits focused and run
the gates from [`docs/developer-setup.md`](../docs/developer-setup.md#daily-commands)
before pushing.

## What NOT to do

- Don't add `unsafe` without a `// SAFETY:` block explaining the invariants.
- Don't introduce new GUI frameworks or async runtimes.
- Don't add telemetry, crash reporters, or analytics.
- Don't put business logic in `.slint` callbacks — dispatch a `UiAction`.
- Don't modify the root workspace `Cargo.toml` to add new dependencies without discussion.
- Don't write to `~/.config/atlas/` from tests (use `ATLAS_CONFIG_DIR`).
- Don't suggest gpui — we evaluated and rejected it.
- Don't propose porting to Electron, Tauri, Flutter, or web technologies.
- Don't add markdown files for planning or notes inside the repo; planning lives outside source.

## Documentation

- All documentation is in the `docs/` directory (user-facing) and `.github/instructions/` (contributor / AI conventions).
- The source-of-truth docs are:
  - `.github/instructions/architecture.instructions.md` — crate layout, process model, threading, storage.
  - `.github/instructions/performance.instructions.md` — performance philosophy and Rust best practices.
  - `.github/instructions/design.instructions.md` — Apple-HIG-inspired UI/UX tokens and component patterns.
  - `.github/instructions/ui-composition.instructions.md` — canonical flow for adding a new modal, panel, view mode, or context menu.
  - `.github/instructions/keybind-authoring.instructions.md` — end-to-end keybind workflow.
  - `.github/instructions/remote-backend-authoring.instructions.md` — end-to-end remote-backend workflow.
  - `.github/skills/testing/SKILL.md` — testing lifecycle, commands, and flaky-triage protocol.
  - `.github/skills/write-benches/SKILL.md` — benchmark lifecycle, result assessment, and perf commit format.
  - `docs/developer-setup.md` — toolchain, prerequisites, daily commands, mock servers, MCP tooling.
  - `docs/contributing.md` — contributing guidelines and PR standards.
  - `docs/multi-pane.md` — user-facing guide to the tiling workspace.
  - `docs/keymap.md` — full default keymap reference.
- Cloud-agent skills live under `.github/skills/<skill-name>/SKILL.md` (per the
  [GitHub add-skills spec](https://docs.github.com/en/copilot/how-tos/copilot-on-github/customize-copilot/customize-cloud-agent/add-skills)).
- For any significant changes (producer, consumer, API, performance, etc.), update the relevant doc(s) to ensure consistency and clarity.
- All documentation must be up-to-date and accurately reflect the current state of the repository.
