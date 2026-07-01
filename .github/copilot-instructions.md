# Copilot instructions for Atlas

These instructions apply to all Copilot interactions in this repository (chat, code review, coding agent). They supplement, not replace, per-task instructions.

## Project at a glance

Atlas is a cross-platform, performance-focused file explorer for developers and power users, written in **Rust** with a **Slint** (Skia renderer) UI. Architecture is a Cargo workspace of small, focused crates. macOS is the primary target; Linux and Windows follow. License is **proprietary**.

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

**Do not** introduce a new GUI framework, a new async runtime in library crates (use channels), or a new error library (use `thiserror` + `anyhow`).

We already evaluated and rejected **gpui** (requires full Xcode for Metal shaders) for Slint. Do not re-suggest gpui.

## Workspace layout

13 crates under `crates/atlas-*` — see [`.github/instructions/architecture.instructions.md`](instructions/architecture.instructions.md) for the full inventory, purpose, and key dependencies per crate.

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

The non-negotiable headline: **no blocking I/O on the UI thread or any thread serving the UI.** See [`.github/instructions/performance.instructions.md`](instructions/performance.instructions.md) for the full set of goals, principles, anti-patterns, and the benchmark methodology.

### Concurrency

- The UI process uses Slint's event loop. Cross-thread updates go through `slint::invoke_from_event_loop`.
- The indexer daemon uses `tokio` (multi-threaded runtime).
- Library crates **do not** depend on tokio or a specific runtime; expose channel-based APIs that work with either.

### Filesystem

- Use `atlas_fs` for any directory listing or walking. Do not call `std::fs::read_dir` directly outside of `atlas-fs` and `atlas-indexd`.
- Always tilde-expand user-facing paths via `atlas_core::path::expand_tilde`.
- Be symlink-aware: capture targets, mark broken symlinks, never silently follow.

### UI (Slint + Rust glue)

- `.slint` components live under `assets/ui/`. Split by responsibility; one entry component imports the rest.
- Use the `Theme` global for colors/spacing/fonts. No hard-coded colors in components.
- Rust ↔ Slint state goes through `AppShell` adapter methods in `atlas-ui`; never let arbitrary code touch the `slint::Weak<Window>` directly.
- Every Slint callback dispatches a typed `UiAction` to the `ActionSink`. Add new variants to `UiAction` rather than calling business logic from the callback.

### Configuration

- All user-facing config lives in `~/.config/atlas/config.toml` (or `%APPDATA%\Atlas\config.toml` on Windows). Tests use `ATLAS_CONFIG_DIR` override.
- New config fields require `#[serde(default)]` + `#[serde(deny_unknown_fields)]` and a sane `Default`.
- Saving must preserve user comments (use `toml_edit`, not `toml::to_string`).

### Testing

- Every public API has tests. Aim for behavior tests, not just type-check stubs.
- Use `tempfile::TempDir` for filesystem tests; never read/write outside `target/` or tempdirs.
- Tests must not depend on each other or on global state. Use `serial_test` if you must mutate env vars.

## Commit conventions

- **Conventional Commits**: `feat(crate):`, `fix(crate):`, `refactor(crate):`, `chore:`, `docs:`, `test:`, `perf:`.
- Subject ≤ 72 chars, imperative mood (`add`, not `added`).
- Body explains the *why* — wraps at 80 cols — bullet the *what* when there are several changes.
- One concern per commit. Split unrelated changes.
- When the change was drafted or assisted by Copilot, append the trailer:
  ```
  Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>
  ```

## Pull requests

- Title mirrors the commit subject.
- Description states: motivation, what changed, how it was tested, and any user-visible impact.
- Link to the issue or plan todo where applicable.
- Keep PRs focused; large refactors get their own PR.
- CI (`fmt`, `clippy`, `test`, `build`) must be green before merge.

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

- All documentation is in the `docs/` directory.
- The source-of-truth docs are:
  - `.github/instructions/architecture.instructions.md` — crate layout, process model, threading, storage.
  - `.github/instructions/performance.instructions.md` — performance goals and benchmark methodology.
  - `.github/instructions/multi-pane-refactor.instructions.md` — N-pane workspace design contract; read before touching `workspace.rs`, `shell.rs`, or any Slint pane/workspace component.
  - `docs/developer-setup.md` — toolchain, prerequisites, daily commands.
  - `docs/contributing.md` — contributing guidelines.
  - `docs/multi-pane.md` — user-facing guide to the tiling workspace (concepts, keybinds, config recipes).
  - `docs/keymap.md` — full default keymap reference.
- For any significant changes (producer, consumer, API, performance, etc.), update the relevant doc(s) to ensure consistency and clarity.
- All documentation must be up-to-date and accurately reflect the current state of the repository.
