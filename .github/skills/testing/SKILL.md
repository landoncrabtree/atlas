---
name: testing
description: Guide for writing, running, and triaging Atlas tests. Use when adding test coverage, diagnosing failures, handling known flakes, or choosing the right test layer.
---

Atlas expects behavior coverage for new functionality and a disciplined retry
protocol for known filesystem-timing flakes. Use this skill before adding tests
or spending time on a failure that may be pre-existing.

## Writing tests

Test at the smallest layer that proves the behavior:

- **Unit tests** live next to the code in `#[cfg(test)] mod tests { ... }`.
- **Integration tests** live under `crates/<crate>/tests/`.
- **Doctests** live in doc comments on public APIs and run under `cargo test
  --doc`, not nextest.

Use [`cargo-nextest`](https://nexte.st) as the primary runner. CI uses the retry
envelope below so known flakes retry without being ignored or deleted.

## Test conventions

- Filesystem walker/lister tests use deterministic `tempfile::TempDir` fixtures
  and assert sorted, stable output. Never depend on the developer's real home
  directory or current config.
- View/controller tests use in-memory view models and drive controller methods;
  do not require Slint rendering for pure state transitions.
- UI state changes should be verified through property/model snapshots at the
  Rust boundary. Use live screenshots only for visual verification of Slint
  changes.
- Remote backend tests use mock servers from `tools/mock-servers/` and must obey
  the `MOCK_SERVERS_SKIP=1` gate so the rest of the workspace can run without a
  Python/uv environment.
- Environment-mutating tests use `serial_test` or an explicit scoped guard.
- New public APIs get behavior tests, not type-check stubs.

## Running tests

Mirror CI locally:

```bash
cargo nextest run --workspace --locked --retries 3 --no-fail-fast
cargo test --doc --workspace --locked
```

Useful narrower runs:

```bash
# One crate or one test filter
cargo nextest run -p atlas-fs --locked lister

# Skip mock-server-backed remote tests
MOCK_SERVERS_SKIP=1 cargo nextest run --workspace --locked --retries 3 --no-fail-fast

# Diagnostic: reduce timing noise and show logs
RUST_LOG=atlas=debug cargo test -p <crate> <test-name> -- --test-threads=1 --nocapture
```

`--retries 3` means one initial attempt plus three retries. A known flaky is not
a regression when it passes inside that envelope, except where noted below.

## Known flakies

These are retry-clean under normal conditions and should not be treated as
regressions unless they fail the parent-commit protocol too:

- `atlas-watch::test_*` — macOS FSEvents drops `Create` / `Modify` events under
  parallel test load.
- `atlas-config::watcher_reload_and_error` — FSEvents debouncer timing on macOS
  under load.
- `theming::watcher::hot_reload_on_file_change` — same FSEvents debouncer race,
  tested against the themes directory. **Can occasionally exhaust all 4 nextest
  attempts within a single run** (for example, run
  [`28665808289`](https://github.com/landoncrabtree/atlas/actions/runs/28665808289/job/85017127646));
  a full workflow re-run typically clears it. Do not treat as a regression
  unless it fails a second full re-run.
- `views::miller::controller::set_root_opens_one_column` — Miller controller
  waits on an async load that occasionally exceeds the fixture timeout on cold
  caches.

## Flaky test triage protocol

1. **Check whether your change touched the failing path.** If yes, root-cause it
   first. Do not blame the flakies list for code you changed.
2. **Reproduce on the parent commit** in a throwaway sibling worktree:

   ```bash
   git worktree add ../atlas-parent HEAD~1
   cd ../atlas-parent
   cargo test -p <failing-crate> <failing-test-name> -- --test-threads=1 --nocapture
   ```

   - Passes on parent → your PR introduced the regression. Fix it.
   - Fails on parent → pre-existing. Note it in the PR, re-run once, and
     continue if it clears.

3. **Clean up the worktree** when done:

   ```bash
   cd -
   git worktree remove ../atlas-parent
   ```

4. **Escalate if it fails twice in a row on the parent commit.** It may have
   flipped from flaky to always-failing.
5. **Use serial execution diagnostically.** `--test-threads=1` often stabilizes
   FSEvents failures and confirms the race is parallel-load sensitive.

## Anti-patterns

- Do not add `sleep(...)` to fix flakes. Use a polling `wait_until` helper that
  observes the expected state, or fix the underlying race.
- Do not delete, weaken, or `#[ignore]` a flaky test unless equivalent coverage
  replaces it.
- Do not blame sibling agents, local WIP, or other branches. The parent-commit
  worktree check is the evidence.
- Do not write tests that read or mutate `~/.config/atlas/` directly; use
  `ATLAS_CONFIG_DIR` or an in-memory model.
- Do not let doctest coverage silently depend on nextest; run doctests
  separately.
