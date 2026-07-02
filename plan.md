# Atlas — rolling plan

This file records phases-in-flight and their exit criteria. Landed
phases keep a short "LANDED" section pinned here as the durable
audit trail; superseded design notes are pruned once the follow-up
phase lands.

---

## Phase 2.7 — remote `fs::View` + preview cache — LANDED

**Reported bug** (SFTP demo pane on `sftp://demo@test.rebex.net:22`):
double-clicking `pub/` or `readme.txt` shipped a bare basename to
`open::that`, which handed it to `/usr/bin/open` on macOS and
returned `ExitStatus(256)`. Root cause: `AppShell::view_focused_entry`
/ `view_entry_at_index` / `view_path` read `entry.path` (which the
SFTP backend surfaces as the basename when listing an
empty-relative-to-root path) and unconditionally hit the local-OS
fast path.

**Fix shape.** Introduce a single canonical resolver
[`resolve_entry_location(pane_loc, entry) -> Location`] and a single
navigation funnel [`AppShell::navigate_pane_to_location`] that
dispatches [`Location::Local`] through `NavigationController` and
[`Location::Remote`] through a fresh
`RemoteLocationViewModel::open_live_sftp_with_options` mount using
`atlas_ops::credentials_for` (session cache → keychain →
`Credentials::Anonymous`). Every `fs::View` call site, breadcrumb
click, address-bar submit, and Go-Up funnel through the same helpers.

**Preview cache** (`atlas_ui::remote::preview`). Remote-file
activation materialises to `~/Library/Caches/dev.atlas.atlas/preview/
<sha256(uri, mtime, size)>/<name>` (via `directories::ProjectDirs`)
and then calls `open::that(cached_path)`. LRU-caps total on-disk
footprint at `remote.preview.max_bytes` (default 200MB) and refuses
files above `remote.preview.max_open_bytes` (default 100MB) with a
tracing hint suggesting `Cmd+C` copy to a local pane. All knobs live
in `[remote.preview]` in `config.toml` and hot-reload with the rest of
the config.

**Testing seams.** `PreviewCache::with_opener(cfg, Arc<dyn OpenHandler>)`
substitutes a recording double for `open::that`. Cache tests inject
bytes via `stage_bytes_for_test`; the second activate then hits the
sync cache-hit fast path, asserting `download_count()` stays at 1.

**Coverage.** Every view mode (Details, Grid, Miller, Tree, palette
open, context-menu Open, keyboard Enter) funnels through the fixed
dispatcher — verified via callback grep + MCP-driven live check.
Gallery does not double-click-to-open files (uses inline preview) so
its callback path is unchanged.

**Deliverables landed:**

1. `feat(remote): preview cache module + config knobs`
   — `atlas-config` `RemotePreview`, `atlas-ui::remote::preview`
     module, `atlas-ops::credentials_for` public, shared
     `atlas_remote::runtime::handle()`.
2. `fix(remote): route fs::View through Location resolver` — pure
   resolver / breadcrumb / address-bar helpers in
   `atlas-ui::remote::resolve`, `AppShell::view_entry` and the
   navigation funnel `navigate_pane_to_location`.
3. `test(remote): integration tests for preview cache download + cache-hit`
   — three `#[test]`s in `crates/atlas-ui/tests/remote_preview.rs`
   using the shared `MockSftpServer` harness: real-SFTP download →
   `RecordingOpener` invocation, second-open uses cache
   (`download_count` stays at 1), and directory resolver produces a
   `Location::Remote(uri.join(name), Sftp)`.

**Test count delta.** atlas-ui lib tests 247 → 262 (+15 new: 7
resolve, 8 preview counting the pre-existing 5 + 3 net-new). Plus
3 integration tests in `remote_preview.rs`. Total delta: +18.

**Deferred / follow-ups.**

* Back/forward on remote panes still runs through
  `NavigationController::navigate_pane_no_push`, which early-returns
  on `Location::Remote`. Symmetrical fix should introduce a
  no-push counterpart to `navigate_pane_to_location` and rewire
  `back_focused` / `forward_focused` — filed as a P2.
* Streaming preview download (`>4 MiB` reader path in the spec) —
  current implementation buffers via `vm.read`. Fine for typical
  read-me / config files; would matter for big-media preview.
* Write-back after edit — the preview cache is read-only from the
  UI's POV; if the user edits the cached file in an external editor,
  we don't push those bytes back to the remote. Documented as a
  known limitation.
* Symlink-target `stat` on remote — Phase 2.7 treats a symlink on a
  remote pane as a file (goes through preview cache); walking the
  symlink target via `RemoteLocationViewModel::stat` is a nice-to-have.
* `remote.preview.max_open_bytes` toast on the status bar — for MVP
  we log a `tracing::warn`; a proper status-bar chip lives in the UX
  polish phase.
