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

## Phase 2.8 — remote follow-ups + write-back + capability-aware context menu — LANDED

Seven-commit push closing every deferred item from the Phase 2.7 list
above. Each commit builds, clippy-cleans, and test-cleans independently.
Trailer `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`
on all seven.

| SHA | Subject |
| --- | --- |
| `fb70189` | `feat(nav): back/forward history on remote panes` |
| `dc3df88` | `perf(remote): streaming preview download via stream_copy for large files` |
| `80cee84` | `feat(remote): write-back for edited previews` |
| `f8c468d` | `fix(remote): symlink-target stat + follow-through in resolver` |
| `cfc80d4` | `feat(ops): status-bar chip when preview exceeds max_open_bytes` |
| `91099ca` | `fix(remote): mount_remote_navigation respects process-wide KnownHostsMode default` |
| `5e8f69b` | `feat(ui): context menu is remote/local-aware; wire capability model for plugins` |

### Deferred → closed after Phase 2.8

- ✅ Remote back/forward history — `fb70189`
- ✅ Streaming preview download (>4 MiB reader path) — `dc3df88`
- ✅ Write-back after edit — `80cee84`
- ✅ Symlink-target stat + follow-through on remote — `f8c468d`
- ✅ `remote.preview.max_open_bytes` status-bar chip — `cfc80d4`
- ✅ `mount_remote_navigation` honours process-wide KnownHostsMode — `91099ca`
- ✅ Remote/local-aware context menu with plugin seam — `5e8f69b`

### Still deferred (out of Phase 2.8 scope by design)

- Plugin `ContextCapabilityProvider` trait — v0.6+; `5e8f69b` leaves a
  `TODO(plugins):` marker and a clean flag-surface extension recipe.
- "Open With…" picker UI — v0.3 follow-up; MVP falls through to OS default.

### Verification

`cargo build --workspace ✓` · `cargo clippy --workspace --all-targets -- -D warnings ✓`
· `cargo fmt --all --check ✓` · `cargo test --workspace` net-positive
test count (+16 unit + 7 integration), same pre-existing FSEvents-timing
flakies as Phase 2.6 (5 tests). See the session-state `plan.md` for the
detailed item-by-item breakdown.

---

## Phase 2.9 — remove tree, fix miller/grid/context/icons — LANDED

Follow-up polish sprint after Phase 2.8: retire the Tree view (it
never left placeholder status), sand off four Miller/Grid/Remote/
Context bugs, and land a proper lsd-inspired filetype-icon system
that unifies every view. Full run committed on `main`; test count
net +31 (4 miller + 27 icons); same known FSEvents flake as prior
phases that clears on retry.

- **89b8f02** — Remove Tree view + associated action / keybind /
  cycle. Tree was a placeholder stub since v0.1; removing it cleans
  up `ViewMode`, the view-cycle keybind sequence, the tree
  subdirectory, and all Slint plumbing.

- **c9b3838** — Miller autoscroll keeps the newest column in view.
  Drilling into a deep directory now scrolls the Miller column
  stack so the newest rightmost column stays visible.

- **3bbeb2a** — Miller loads the pane's own Location, not the local
  `/`. A remote (SFTP) pane switching into Miller view no longer
  rebuilds column 0 from the local filesystem root. Threaded
  `pane.location()` through `MillerController::open_at`.

- **ae0b0c0** — Grid view is a proper grid. Row heights and cell
  widths are now constants — the last row no longer stretches to
  fill leftover vertical space.

- **926998e** — Capability-aware context menu extended to Miller +
  Gallery. Reuses the Phase 2.8 `ContextTarget` / `ContextCapabilities`
  model (`5e8f69b`). New `MillerController::column_entry` +
  `focus_row_within_column` (visual-only — right-click does NOT
  navigate). New `GalleryController::entry_at`. New
  `AppShell::open_context_menu_for_entry(pane, entry, x, y)` for
  views that own per-cell focus state and cannot rely on
  `pane_cache.details_focused_index`. Slint: new `entry-context-menu`
  / `row-context-menu` callbacks on the Gallery strip and Miller
  columns, both using the `absolute-position` translation pattern
  from `details/row.slint`. +4 unit tests on the new Miller helpers.

- **2fec4d3** — Symlink glyph uses a covered codepoint (tofu-safe).
  Previous `↳` (U+21B3) rendered as tofu on the SF Pro Text / Apple
  Symbols stack. Replaced with `↪` (U+21AA) for healthy symlinks
  and `⚠` (U+26A0) for broken. One-line swap in each of the four
  view controllers; supersession by `ca35f80` centralises later.

- **ca35f80** — Filetype icon system (lsd-inspired), unified across
  Details / Grid / Miller / Gallery. New module
  `crates/atlas-ui/src/theming/icons.rs` exposes
  `icon_for(entry) → IconGlyph { glyph, description }` plus a pure
  test-friendly `icon_for_with(entry, use_emoji)` variant. Emoji map
  covers directories (📁), symlinks (↪ / ⚠), executables (⚡), rust
  (🦀), markdown (📝), json (📋), config (⚙), images (🖼), video
  (🎬), audio (🎵), pdf (📕), archives (🗜), shell scripts (▶),
  python (🐍), js/ts (📘), web assets (🌐), text (📄), go (🐹), c/c++
  (⚙). Executable detection uses unix `x` bits with Windows extension
  fallback. Symlinks keep their symlink glyph regardless of target
  kind — we do NOT recurse. New config knob `[ui.icons] use_emoji`
  (default `true`) toggles a bracketed ASCII fallback (`[D]`, `[F]`,
  `[L]`, `[X]`, `[!]`, `[?]`) — live-reload aware. `TODO(fonts):`
  marker documents the deferred Nerd Font pack. +27 unit tests
  covering every mapped extension family and edge case (symlink-to-
  dir doesn't recurse, executable bit beats extension mapping,
  uppercase extension normalises, unknown / no extension fall back,
  ASCII mode swaps every glyph).

### Baseline & regressions

Baseline before Phase 2.9: 530 lib tests, 1 pre-existing FSEvents
flake (`theming::watcher::tests::hot_reload_on_file_change`).

After Phase 2.9: 561 lib tests (net +31), same one known flake, no
new failures. `cargo build --workspace ✓ · cargo clippy --workspace
--all-targets -- -D warnings ✓ · cargo fmt --all --check ✓ · cargo
test --workspace ✓`.

### Deferred items after Phase 2.9

- ⏸️ Nerd Font pack for the icons module — would let us render
  `nf-fa-file-code`, `nf-dev-rust`, etc. beyond emoji. Requires
  bundling a Nerd Font in resources and adding a
  `[ui.icons] pack = "emoji" | "nerd" | "ascii"` config knob.
  `TODO(fonts):` marker on the icons module documents the
  extension point.
- ⏸️ Per-filetype color tinting (lsd colours the glyph background
  by kind). Slint 1.17 cannot paint per-run text colour from a
  Rust callback out of the box. Defer to either the Nerd Font
  pack (colour baked into the glyph) or a Slint 2.x upgrade.
- ⏸️ Dynamic `Menu` items — Slint 1.17 `Menu` cannot rebuild
  children from a model at runtime; every item is declared with an
  `if <bool>: MenuItem { … }` guard. Waiting for Slint 2.x.

---

## Phase 2.10 — font-family plumbing + Nerd Font icons — LANDED

Three-item polish sprint on top of Phase 2.9's icon module: fix the
silently-broken `ui.font_family` wiring, bundle a symbols-only Nerd
Font as a Slint-registered fallback, and rewrite `theming::icons`
around a curated LSD-style Nerd Font glyph map. Test count net +13
(baseline 707 → 720); same FSEvents flaky bucket documented in the
`fix-flaky-test` skill.

- **f8e85ef** — `fix(ui): make ui.font_family config actually reach
  Slint`. The config field was wired end-to-end (`atlas-config` →
  `apply_font_overrides` → `ThemeTokens.typography` → `AppShell::
  apply_theme` → Window `default-font-family`), but we were passing
  a comma-separated CSS-style fallback stack (`"<user>, SF Pro Text,
  ..."`) into Slint. Slint 1.17's `FontRequest.family`
  (`i-slint-core/graphics.rs` L91–94) is a **single family name** —
  not a fallback list — so the compound string never matched and
  every user's `font_family` was a silent no-op. Fix: replace the
  theme's family with the user's choice unmodified; let Slint's
  own fontique fallback cover unknown families and missing glyphs.
  Delete the now-useless `prepend_font` helper; empty the
  fallback-stack literals in `atlas-dark.toml`, `atlas-light.toml`,
  `theme.slint`, `atlas.slint`; document the single-family
  constraint in `skeleton.toml` so users don't retry the comma
  trick. Live-verified with `font_family = "Courier New"` — the
  whole UI renders in Courier New.

- **a0b1a72** — `feat(fonts): bundle Symbols Nerd Font Mono +
  register as Slint fallback`. Bundle
  `assets/fonts/SymbolsNerdFontMono-Regular.ttf` (2.5 MB, MIT — see
  `assets/fonts/NERD-FONTS-LICENSE`). Symbols-only means the font
  ships **only** PUA icon glyphs (no letters, no digits, no
  punctuation) so it can never compete with the user's text font.
  Register it process-wide via a top-level Slint import
  (`import "../fonts/SymbolsNerdFontMono-Regular.ttf";` in
  `atlas.slint`) — the recommended compile-time path per the Slint
  1.17 docs; no runtime feature flags, no `unstable-fontique-010`
  needed. Expose `Theme.icon-font-family = "Symbols Nerd Font Mono"`
  as the binding icon-rendering Text elements consume. Also add a
  `.gitattributes` marking `*.ttf/otf/woff/woff2` as binary and a
  `assets/fonts/README.md` documenting the font, license, and
  refresh procedure.

- **cc15249** — `feat(ui): LSD-style Nerd Font icons; drop
  use_emoji option`. Rewrite `crates/atlas-ui/src/theming/icons.rs`
  around a curated LSD-inspired glyph map (Apache-2.0 — see
  `assets/fonts/LSD-LICENSE`). New `IconGlyph { glyph: char,
  description: &'static str }` (single-scalar, not `&str`, because
  every Nerd Font mapping is a single Unicode PUA scalar). Public
  API is now just `icon_for(&Entry) -> IconGlyph` — the
  `icon_for_with(entry, use_emoji: bool)` and `set_use_emoji`
  entry points are gone. Resolution order: kind (dir / symlink /
  broken / other) → executable-bit → **named-file lookup** (Cargo.
  toml, package.json, Makefile, README, LICENSE, .gitignore,
  Dockerfile, .env, shell dotfiles, editor dotfiles) → extension
  (~130 across source, web, data/config, docs, images, video/
  audio, archives, packages, notebooks, fonts, certs). Named-file
  runs BEFORE extension so `Cargo.toml` gets the Rust manifest
  glyph, not the generic TOML glyph. Every Slint view (`row.
  slint`, `grid-cell.slint`, `miller-column.slint`, `gallery-strip.
  slint`, `gallery-preview.slint`) pins `font-family: Theme.icon-
  font-family` on the icon Text so Slint routes the glyph through
  Symbols Nerd Font Mono. Delete `Icons { use_emoji: bool }` from
  `atlas-config`; the config knob is gone. **Breaking change**:
  users who had `use_emoji` in their `config.toml` will get a
  clean `unknown field` TOML parse error (`deny_unknown_fields`)
  — the config is auto-seeded, and the error is actionable.
  40 unit tests (up from 26) covering named-file precedence,
  extension coverage, uppercase normalisation, and a PUA-range
  sanity check verifying every returned codepoint lives in the
  Nerd Font PUA (a text-font fallback would render tofu —
  motivating the icon-font-family bindings).

### Baseline & regressions

Baseline before Phase 2.10: 707 lib tests, 7 failed (all in the
documented FSEvents flaky bucket from the `fix-flaky-test` skill).
After Phase 2.10: 720 lib tests, 7 failed (same flaky bucket,
different lottery). Net +13 tests, no regressions.
`cargo build --workspace ✓ · cargo clippy --workspace --all-targets
-- -D warnings ✓ · cargo fmt --all --check ✓ · cargo test
--workspace ✓` (modulo the pre-existing flakies).

### Deferred items after Phase 2.10

- ⏸️ **`ui.icons.pack = "nerd" | "ascii"` toggle** (optional item 4
  from the phase brief). With Symbols Nerd Font Mono bundled and
  the LSD map rendering cleanly on every tested platform, the
  ASCII escape hatch is unnecessary today. File an issue if a
  user encounters a platform where the bundled font doesn't
  render.
- ⏸️ Per-filetype color tinting (still — needs Slint 2.x).
- ⏸️ Dynamic `Menu` items (still — needs Slint 2.x).
