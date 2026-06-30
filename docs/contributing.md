# Contributing

Thanks for your interest in Atlas. This document covers how to set up, build, and submit changes.

## Before you start

- Atlas is **proprietary**. External contributions are accepted at the maintainer's discretion under the terms of the LICENSE.
- Read [`.github/instructions/architecture.instructions.md`](../.github/instructions/architecture.instructions.md) to understand the crate layout and design principles.
- Read [`.github/copilot-instructions.md`](../.github/copilot-instructions.md) — those rules apply to human contributors too.

## Setup

See [`docs/developer-setup.md`](developer-setup.md) for toolchain prerequisites and platform notes.

```bash
git clone https://github.com/landoncrabtree/atlas.git
cd atlas
cargo build              # first build downloads + compiles Skia, ~5 min
cargo run -p atlas-app
```

## Workflow

1. **Branch** off `main` with a descriptive name: `feat/grid-view`, `fix/walker-symlink-loop`.
2. **Make focused commits** following the Conventional Commits format below.
3. **Run the local check suite** before pushing:
   ```bash
   cargo fmt --all
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```
4. **Open a PR** with a clear description of motivation, what changed, how you tested it, and any user-visible impact.

## Commit message format

We follow **Conventional Commits**:

```
<type>(<scope>): <short summary in imperative mood>

<optional longer body explaining why — wrap at ~80 cols>

- bulleted what-changed when there are multiple things
- keep each bullet to one line
```

| Type | Use for |
|---|---|
| `feat` | New user-visible functionality |
| `fix` | Bug fixes |
| `refactor` | Internal restructuring with no behavior change |
| `perf` | Performance improvements |
| `chore` | Tooling, dependencies, CI, repo housekeeping |
| `docs` | Documentation only |
| `test` | Tests only |

`<scope>` is the crate name without the `atlas-` prefix (`feat(fs): ...`, `fix(keymap): ...`) or `ui`, `app`, `docs`, `ci`.

## Code style and quality bar

Hard requirements:

- `cargo fmt --all` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- Tests added for new behavior.
- No `println!` / `eprintln!` / `dbg!` / bare `unwrap` outside tests.
- rustdoc on every `pub` item.

Performance is a feature — design for streaming and async; don't block the UI thread or any thread serving the UI. See the principles in [`.github/instructions/architecture.instructions.md`](../.github/instructions/architecture.instructions.md) and [`.github/instructions/performance.instructions.md`](../.github/instructions/performance.instructions.md).

## Adding a dependency

- Prefer existing workspace dependencies (declared in the root `Cargo.toml` under `[workspace.dependencies]`). Reach for crate-local `Cargo.toml` entries that reference them.
- Adding a **new** workspace dependency requires a justification in the PR description and maintainer review. Weigh: binary size impact, compile time, license, maintenance status.
- Crate-local dev-dependencies don't need approval.

## Tests

- Use `tempfile::TempDir` for filesystem tests; never read or write outside the workspace.
- Tests must not depend on each other or on global state. Use `serial_test` if you must mutate env vars.
- Integration tests live in `crates/<crate>/tests/`; unit tests live in `#[cfg(test)] mod tests` blocks.

## UI changes (`.slint`)

- New components go under `assets/ui/components/` or `assets/ui/views/`.
- Use the `Theme` global for colors, spacing, fonts. No hard-coded colors.
- Rust ↔ Slint state changes go through `AppShell` adapter methods in `atlas-ui`.
- Every callback dispatches a typed `UiAction`. Add new variants — don't bypass.

## Documentation

Source-of-truth docs:

- `README.md` — product-facing: what Atlas is, install, features, quick start.
- `docs/developer-setup.md` — toolchain, prerequisites, daily commands.
- `docs/contributing.md` — this file.
- `.github/copilot-instructions.md` — always-on conventions for Copilot and contributors.
- `.github/instructions/architecture.instructions.md` — crate layout, process model, threading, storage (deep dive).
- `.github/instructions/performance.instructions.md` — performance goals, principles, anti-patterns, benchmark methodology.

Update the relevant doc with your change. Keep `README.md` short and product-focused.

## Reporting bugs

When opening an issue, include:

- OS + version + architecture (e.g. `macOS 15.4 arm64`)
- Atlas version (`atlas --version`) and commit SHA if building from source
- Steps to reproduce
- Expected vs actual behavior
- Relevant log output (`RUST_LOG=atlas=debug cargo run -p atlas-app 2>&1 | tee atlas.log`)
