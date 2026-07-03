---
name: fix-flaky-test
description: Protocol for handling a test that appears flaky. Use before investing debugging time in what may already be a known flaky, to distinguish "your PR broke it" from "this has always been racy on this box."
---

Atlas has a small but non-zero set of tests that flake under specific conditions (parallel test load, FSEvents timing, macOS I/O bursts). Before spending an hour root-causing a failure, run this protocol.

## Known flakies (retry-clean, do not treat as regressions)

- `atlas-watch::test_*` — macOS FSEvents drops `Create` / `Modify` events under parallel test load.
- `atlas-config::watcher_reload_and_error` — FSEvents debouncer timing on macOS under load.
- `theming::watcher::hot_reload_on_file_change` — same FSEvents debouncer race, tested against the themes directory. **Can occasionally exhaust all 4 nextest retries within a single run** (e.g. [run 28665808289](https://github.com/landoncrabtree/atlas/actions/runs/28665808289/job/85017127646)); a full workflow re-run typically clears it. Do not treat as a regression unless it fails a second full re-run.
- `views::miller::controller::set_root_opens_one_column` — Miller controller waits on an async load that occasionally exceeds the fixture timeout on cold caches.

CI runs `cargo nextest run --workspace --locked --retries 3 --no-fail-fast`, so these retry automatically and do not fail the pipeline unless they fail 4 times in a row. Locally, mirror that with `cargo nextest run --workspace --retries 3`. If a test fails on both attempts, or fails without the retry envelope, treat it as a real bug — with the noted exception that `hot_reload_on_file_change` has been observed to burn all 4 retries in a single macOS-runner job; re-run the full workflow before escalating.

## Protocol

1. **Confirm the failure is not in the docs / not in your PR.** If your change did not touch the failing crate's code paths, jump to step 2. If it did, root-cause it — do not blame the flakies list.

2. **Reproduce on the parent commit** to prove it's pre-existing:

   ```bash
   # From the repo root, create a temporary worktree at HEAD~1
   git worktree add ../atlas-parent HEAD~1
   cd ../atlas-parent
   cargo test -p <failing-crate> -- --test-threads=1 <failing-test-name>
   ```

   - Test **passes** on parent → your PR introduced the regression. Root-cause and fix.
   - Test **fails** on parent → it's pre-existing. Note the failure in the PR description ("pre-existing flaky, tracked separately"), retry once to confirm it re-runs green, and continue.

3. **Cleanup the worktree** when done:

   ```bash
   cd - && git worktree remove ../atlas-parent
   ```

4. **If it re-runs green once**, treat it as a known flaky. If it fails a second time in a row on the parent commit, escalate: it may have flipped from "flaky" to "always failing", which is a new bug worth investigating.

5. **Reduce parallelism as a diagnostic**. `cargo test -p <crate> -- --test-threads=1` often stabilizes FSEvents-timing failures. If the test passes serial but fails parallel, that confirms the flaky classification.

## Anti-patterns

- **Do not "fix" a flaky by adding `sleep(…)`.** That masks the timing issue and slows every CI run. Fix the underlying race, or add an explicit `wait_until` helper that polls for the expected state.
- **Do not delete or `#[ignore]` a flaky test** unless you have a replacement covering the same behaviour.
- **Do not blame the sibling agent's WIP or other branches** for the flaky — the git-worktree parent-commit check is definitive.

## When it isn't in the known-flakies list

Treat it as a real bug. Root-cause via `RUST_LOG=<crate>=debug cargo test -p <crate> -- --nocapture <test-name>` and follow the trace. Do not add to the flakies list without a maintainer's OK.
