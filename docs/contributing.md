# Contributing

Thanks for your interest in Atlas. This document is the source of truth for
contribution workflow, commit format, PR standards, and the quality bar.

## Before you start

- Atlas is MIT-licensed. Contributions are accepted under the terms of
  [`LICENSE`](../LICENSE).
- Read [the architecture overview](../.github/instructions/architecture.instructions.md)
  before moving code across crates or adding a new subsystem.
- Read [the repository-wide Copilot instructions](../.github/copilot-instructions.md);
  those conventions apply to human contributors too.
- Use [developer setup](developer-setup.md) for toolchain installation and daily
  command lines.

## Workflow

1. Branch from `main` with a descriptive name, e.g. `feat/grid-view` or
   `fix/walker-symlink-loop`.
2. Make focused commits. One concern per commit.
3. Add tests for new behavior. The testing source of truth is
   [`.github/skills/testing/SKILL.md`](../.github/skills/testing/SKILL.md).
4. For hot-path performance changes, include benchmark evidence. The benchmark
   source of truth is
   [`.github/skills/write-benches/SKILL.md`](../.github/skills/write-benches/SKILL.md).
5. Run the local gates from [developer setup](developer-setup.md#daily-commands)
   before pushing.
6. Open a PR with motivation, what changed, test evidence, and user-visible
   impact.

## Commit message format

Atlas uses Conventional Commits:

```text
<type>(<scope>): <short summary in imperative mood>

<optional body explaining why; wrap at about 80 columns>
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

`<scope>` is usually the crate name without the `atlas-` prefix (`feat(fs):`,
`fix(keymap):`) or a repository area such as `ui`, `app`, `docs`, or `ci`.
Keep subjects at or below 72 characters, use imperative mood, and explain the
why in the body when context matters.

Performance commits follow the measured subject/body format in the
[write-benches skill](../.github/skills/write-benches/SKILL.md#commit-format-for-perf-changes).

When a change was drafted or assisted by Copilot, append:

```text
Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>
```

## Quality bar

Hard requirements:

- Formatting, clippy, nextest, doctests, and build gates are clean.
- New behavior includes tests at the right layer.
- Hot-path changes include before/after benchmark evidence.
- No `println!`, `eprintln!`, `dbg!`, or bare `unwrap()` outside tests.
- Public APIs have useful rustdoc.
- UI changes follow
  [UI composition](../.github/instructions/ui-composition.instructions.md) and
  include live visual verification when a Slint surface changes.

## Dependencies

- Prefer existing workspace dependencies from the root `Cargo.toml`.
- Adding a new workspace dependency requires PR justification and maintainer
  review. Cover binary size, compile time, license, and maintenance status.
- Crate-local dev-dependencies do not need special approval.

## PR standards

A PR description should include:

- Motivation / problem statement.
- Summary of changes.
- Test summary, including the exact commands run.
- Benchmark summary for hot-path changes, or `cold path — no measurement` when
  the [write-benches](../.github/skills/write-benches/SKILL.md) classification
  applies.
- UI screenshots for visible Slint changes.
- User-visible impact and migration notes, if any.

Keep PRs focused. Large refactors, behavior changes, and follow-up cleanups
belong in separate PRs unless they are inseparable.

## Documentation

Update the authoritative source for the concept you changed, then link to it
from callers. Do not copy how-to details into multiple docs. Current owners:

- Product overview: [`README.md`](../README.md).
- Contribution workflow and PR standards: this file.
- Machine setup and daily command lines: [`docs/developer-setup.md`](developer-setup.md).
- Testing lifecycle: [`.github/skills/testing/SKILL.md`](../.github/skills/testing/SKILL.md).
- Benchmark lifecycle: [`.github/skills/write-benches/SKILL.md`](../.github/skills/write-benches/SKILL.md).
- Keymap reference: [`docs/keymap.md`](keymap.md).
- Multi-pane user guide: [`docs/multi-pane.md`](multi-pane.md).
- Architecture, UI, keybind, and remote-backend authoring details:
  [`.github/instructions/`](../.github/instructions/).

## Reporting bugs

When opening an issue, include OS/version/architecture, Atlas version or commit
SHA, steps to reproduce, expected vs actual behavior, and relevant logs.
