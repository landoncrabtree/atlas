---
applyTo: "**/*.slint,**/theme.rs,**/theming/**,assets/themes/**"
description: "Atlas UI/UX design philosophy: Apple HIG-inspired principles, tokens, component patterns. Apply to any Slint component, theme token, or visual layout work."
---

# Design philosophy

Atlas's visual identity is **calm, dense but breathing, and content-first**. It should feel like a native macOS utility (Finder, Sequoia System Settings, TablePlus, Linear) — while remaining cross-platform.

We do not ship SwiftUI. We use Slint. But every component we build should look and behave as if a macOS engineer designed it, and then adapt gracefully to Linux and Windows without emulation of any single platform's chrome.

## The six principles

Ordered by how often you'll invoke them.

### 1. Deference

The UI recedes; the user's content and choices dominate. No chrome fights for attention. Titles are quiet. Toolbars are unobtrusive. Chrome should look like it's been etched out of the workspace, not painted on top.

- **Do**: quiet-gray borders (~0.5α on accent-neutral), muted secondary text, translucent panels.
- **Don't**: brand marks in the content area, bright accent everywhere, heavy fills.

### 2. Clarity

Every glyph, label, and control has a single obvious meaning. Legible typography. Meaningful whitespace. Precise iconography.

- **Do**: SF Pro / Inter with a strict type scale, sentence-case section labels, compact controls, purposeful whitespace.
- **Don't**: mystery-meat icons, decorative separators, loud all-caps form labels.

### 3. Depth

Layered planes establish hierarchy. Modals float above panels. Panels sit on the workspace. The workspace sits on the window. Depth is expressed through **subtle background differences and blurred materials**, rarely through drop shadows.

- **Do**: 3–6 percent brightness deltas between layers; `Theme.panel_bg` above `Theme.bg`; soft modal shadows only for top-level sheets.
- **Don't**: nested cards inside modals, hard borders that duplicate depth, shadows on interior groupings.

### 4. Consistency

Same interaction, same treatment. A row hover looks the same in Details, Grid, Miller, and Gallery. Buttons follow one visual grammar. Text inputs follow one grammar.

- **Do**: single source of truth in `Theme` global; component compositions rather than one-off styles.
- **Don't**: local color overrides in components; ad-hoc padding numbers.

### 5. Progressive disclosure

Show what the user needs; hide the rest until asked. Hover to reveal secondary actions. Expand to see detail. Popover for extended data.

- **Do**: hover-revealed action buttons on rows; collapsed metadata by default; inline expandable groups.
- **Don't**: 12 tiny buttons on every row all the time.

### 6. User in control

Animations are **responsive, not showy**. Under 300ms always. Motion serves comprehension (where did this panel come from, where did this row go).

- **Do**: 150ms ease-out for hover; 200ms cubic-bezier for enter; 250ms fade for overlay.
- **Don't**: bounce, elastic, or > 300ms curves; animation on every state.

---

## macOS-native modal/menu rules

Atlas surfaces should read like Sonoma/Sequoia sheets: hierarchy over boxes, compact controls, and one clear default action.

- **Hierarchy over boxes.** Prefer labels, whitespace, list materials, and subtle contrast to stacked rounded rectangles.
- **Restrained accent.** Use `Theme.accent` for the single primary CTA and active progress fill. Segmented-control selection, saved-server lists, and secondary actions are neutral. **Confirmation modals — replace-item prompts, cancel dialogs — keep every button neutral;** accent color is reserved for actions that are unambiguously "the right one" (Connect, Rename N files, Save). See §Confirmation modals below.
- **Varied radii.** Modal sheets are `radius-xl` (14 px), lists/cards `radius-md` (8 px), buttons `radius-sm` (6 px), text fields `field-radius` (5 px). Avoid using one radius everywhere.
- **Compact controls.** Text fields, segmented controls, and buttons are 29 px tall unless a platform-specific surface already defines a different height.
- **Soft borders.** Field/list borders are semantic aliases around ~15–20% contrast; focus uses a stronger neutral border, not an accent outline.
- **Sentence-case labels.** Form labels are `Backend`, `Host`, `Authentication` — 12 px semibold, muted. Do not use bold caps for form labels.
- **Rhythm.** Label → 6 px → control, then 12–20 px before the next section depending on grouping. Do not make every gap identical.
- **Modal depth.** The outer sheet uses `Theme.modal-radius`, `Theme.modal-scrim`, and a soft wide shadow. Interior cards/lists do not cast shadows.

---

## Design tokens

Base palette/chrome tokens are owned by the `Theme` global (Slint) and mirrored by `ThemeTokens` (Rust). Semantic macOS aliases are derived in `assets/ui/theme.slint` from those base tokens. Every visible surface reads from `Theme.*`; **no hard-coded hex values in components**.

### Palette

Dark theme (default):

| Token | Value | Purpose |
|---|---|---|
| `bg` | `#0f1115` | Workspace surface |
| `panel_bg` | `#171a20` | Panels one layer above bg |
| `panel_bg_elevated` | `#1e232b` | Modals, floats |
| `fg` | `#e6ebf4` | Primary text |
| `fg_muted` | `#8b95a7` | Secondary text, chevrons, hints |
| `fg_faint` | `#5a6478` | Tertiary text, disabled |
| `border` | `#20262f` | 1-px separators, panel edges |
| `border_strong` | `#2b3341` | Emphasized borders |
| `accent` | `#4a9eff` | Primary interactive (buttons, focus, active tab) |
| `accent_fg` | `#ffffff` | Foreground on accent fills |
| `accent_soft` | `#4a9eff26` | Accent at ~15% alpha for backgrounds |
| `selection_bg` | `#3d7ac9` | Selected row/cell |
| `selection_fg` | `#ffffff` | Text on selection |
| `hover_bg` | `#ffffff0d` | ~5% white for row hover |
| `error` | `#ff6b6b` | Destructive / errors |
| `success` | `#5bd18a` | Success confirmation |
| `warning` | `#f5c274` | Warnings |

Light theme mirrors semantically (`bg` = `#ffffff`, `panel_bg` = `#f6f8fb`, etc.). Never invert accent hue — keep same blue with tuned brightness.

### macOS semantic aliases

Defined in `assets/ui/theme.slint` as derived `out property` values.

| Token | Value / source | Use |
|---|---|---|
| `modal-scrim` | `#00000066` | Modal backdrop, ~40% dim |
| `modal-bg` | `panel-bg-elevated` | Sheet surface |
| `modal-border` | `border.with-alpha(0.72)` | Soft sheet edge |
| `modal-radius` | `radius-xl` (14 px) | Modal sheet corners |
| `modal-shadow-blur` / `modal-shadow-y` | `40px` / `10px` | Soft sheet lift |
| `control-height` | `29px` | Buttons, fields, segmented controls |
| `field-radius` | `5px` | Text fields |
| `button-radius` | `6px` | Buttons |
| `segmented-radius` | `6px` | Segmented control container |
| `list-radius` | `8px` | Saved-server lists, grouped tray lists |
| `section-label-size` | `12px` | Sentence-case form section labels |
| `placeholder-color` | `fg-faint.with-alpha(0.74)` | Low-contrast examples |
| `field-bg` / `field-border` | theme-derived neutral fill/border | Compact text fields |
| `secondary-button-*` | theme-derived neutral fill/border | Cancel, Save + Connect, Background |
| `segmented-*` | theme-derived neutral fills/dividers | Contiguous segmented controls |
| `progress-height` / `progress-track` | `4px`, neutral track | NSProgressIndicator-style bars |

### Typography

- **Family** (body): `SF Pro Text` → `SF Pro Display` (large) → `Inter` → `system-ui` fallback.
- **Family** (monospace): `SF Mono` → `JetBrains Mono` → `Menlo` → `monospace`.

Type scale (px):

| Style | Size | Weight | Line-height | Use |
|---|---|---|---|---|
| `micro` | 10 | 600 | 1.2 | Column headers and tiny metadata |
| `caption` | 11 | 400 | 1.35 | Status bar, hints, timestamps |
| `small` | 12 | 400 | 1.4 | Secondary body |
| `section_label` | 12 | 600 | 1.3 | Sentence-case form labels |
| `body` | 13 | 400 | 1.45 | Primary body — most rows, most labels |
| `body_emphasis` | 13 | 500 | 1.45 | Emphasized body — selected row name |
| `title` | 15 | 600 | 1.3 | Modal titles, panel titles |
| `headline` | 17 | 600 | 1.25 | Larger empty-state headings |
| `display` | 22 | 700 | 1.2 | Empty-state hero text |

Weights above 700 are prohibited (they read as heavy on macOS).

### Spacing (8pt grid)

`space_1` = 4, `space_2` = 8, `space_3` = 12, `space_4` = 16, `space_5` = 20, `space_6` = 24, `space_8` = 32, `space_10` = 40.

Use `space_2` for tight groups, `space_4` for panel padding, `space_6` between sections.

### Radii

| Token | Value | Use |
|---|---|---|
| `radius_xs` | 4 | Chips, small badges |
| `radius_sm` | 6 | Buttons, inputs, rows |
| `radius_md` | 8 | Cards, palette items |
| `radius_lg` | 10 | Modals, tooltips |
| `radius_xl` | 14 | Modal sheets, palette panel, floating menus |

Never use square corners for interactive elements.

macOS modal/menu aliases intentionally vary radius by role: `modal-radius` = `radius_xl` (14), `list-radius` 8, `button-radius` 6, `field-radius` 5. Every top-level sheet in Atlas — conflict prompt, operation-progress, palette, connect, bulk-rename — reads with the same soft macOS Sequoia curvature; interior controls keep the smaller varied radii per HIG.

### Elevation

Prefer background contrast over shadows. When a shadow is truly needed (top-level modals, tooltips), use a **single subtle drop**:

- Modal overlay backdrop: `Theme.modal-scrim` covering the workspace.
- Modal panel: `Theme.modal-shadow-blur` (40), `Theme.modal-shadow-y` (10), `Theme.modal-shadow-color` (~25% black).
- Interior cards/lists: no shadow — use `Theme.list-bg`, separators, and whitespace.
- Palette/list row hover: no shadow — use `hover_bg` instead.

### Chrome heights

| Element | Height |
|---|---|
| Titlebar / traffic lights row | 36 |
| Tab bar | 32 |
| Row (default, "comfortable") | 30 |
| Row (compact) | 24 |
| Row (spacious) | 38 |
| Status bar | 24 |
| Address bar | 30 |
| Palette input | 44 |
| macOS control | 29 |
| Progress bar | 4 |

### Motion

| Interaction | Duration | Easing |
|---|---|---|
| Hover fill | 120 ms | `ease-out` |
| Focus ring | 100 ms | `ease-out` |
| Enter (panel opens) | 200 ms | `cubic-bezier(0.16, 1, 0.3, 1)` (spring-out) |
| Exit (panel closes) | 160 ms | `ease-in` |
| Selection change | 100 ms | `linear` |

Do NOT animate scroll position, cursor, or focused-row indicator movement — those are user-driven and should be instant.

---

## Component patterns

### Shared macOS controls

`assets/ui/components/atlas-controls.slint` is the shared library for modal/menu controls. Use it before drawing local rectangles:

| Component | Compose with |
|---|---|
| `AtlasModal` | Top-level sheet chrome: scrim, 12 px radius, soft shadow, title slot, click-outside dismissal |
| `SectionLabel` | Sentence-case section labels (`Backend`, `Host`, `Authentication`) |
| `AtlasFieldGroup` | Label/control grouping with the canonical 6 px label gap |
| `AtlasTextField` | 29 px compact text input, soft neutral border, low-contrast placeholder |
| `AtlasSegmentedControl` | Contiguous picker with hairline dividers and neutral selected pill |
| `AtlasPrimaryButton` | Single accent CTA in a modal (`Connect`, destructive confirmations only when semantic) |
| `AtlasSecondaryButton` | Cancel, Save + Connect, Background, Browse, Clear Completed |
| `AtlasProgressBar` | 4 px rounded NSProgressIndicator-style track/fill |
| `AtlasList` / `AtlasListRow` | Inset saved-server lists and operations rows |

Do not create another modal chrome, progress bar, segmented control, or button style unless this component library cannot express the interaction.

### Titlebar

- No app name in the content area — the OS window title (or a custom titlebar with only traffic lights + a tiny centered path breadcrumb) is enough.
- **Never** duplicate the word "Atlas" inside the workspace. The window is Atlas.
- Centered secondary content (current pane path, truncated with ellipsis mid-string).
- Left: traffic-light spacer only on macOS.
- Right: view mode toggle, sort menu, workspace controls — small icon buttons at `Theme.fg_muted`, hover to `Theme.fg`.

### Address bar

- Inset within the toolbar, not full-width.
- Radius `sm` (6). Fill `panel_bg_elevated`. 1-px border in `border`.
- On focus: border transitions to `accent`, no shadow.
- Placeholder in `fg_faint`; input text in `fg`.
- Height 30. Horizontal padding 12.

### Breadcrumbs

- Segments in `fg_muted`, current in `fg`.
- Separator: `›` in `fg_faint`, not `/`. 6-px horizontal padding around each segment.
- Segment hover: `hover_bg` background, `radius_xs` (4), no underline.
- Truncate the middle segments with `…` when the sum exceeds the container.

### Tabs

- Pill shape, `radius_sm`.
- Inactive: `fg_muted`, no background.
- Hover: `hover_bg`.
- Active: `panel_bg_elevated` fill; a 2-px `accent` bottom indicator OR (preferred) subtle top border.
- Close (`×`) glyph shown on hover only (progressive disclosure).
- New-tab `+` at the end of the strip, muted until hovered.

### Details / list view

- Header row: `micro` label style (10px, uppercase, `fg_muted`, letter-spacing 0.06em). Height 28.
- Row height 30 (comfortable).
- **No vertical column dividers.** Whitespace does the job.
- **No alternating row shading** by default. Optional in config for high-density users.
- Row hover: `hover_bg` fill, `radius_sm`.
- Row selected: `selection_bg` fill, `selection_fg` text, `radius_sm`.
- Row focused (keyboard, not selected): 1-px `accent` inset border, no fill.
- Icons: 16-px monochrome glyphs at `fg_muted`; folder color-shift optional via config.
- Size column right-aligned, tabular-lining numerals if the font supports them.
- Timestamps human-friendly ("2 hours ago"); toggle to absolute in config.

### Grid view

- Cell size defaults to 128 with 12-px gap. `radius_md` on cell bg on hover.
- Label under the thumbnail; two-line max; middle-truncated with ellipsis.
- Focused cell: 2-px `accent` outline (inset — no growth of layout).
- Selected cell: `selection_bg` at 25% alpha behind the thumbnail; label uses `selection_fg`.

### Miller columns

- Column width 240 default; user-resizable.
- Column header: name of the parent directory in `micro` style. Height 28.
- Divider between columns: 1-px `border` line.
- Focused column: subtle `accent_soft` background wash + 1-px `accent` right edge.
- Selected row in each column persists visually even when column isn't focused (dimmed to 60% opacity of `selection_bg`).

### Gallery view

- Preview: `panel_bg` fill for the frame, `radius_md`, generous padding, image `image-fit contain`.
- Metadata sidebar: 280 wide; labels in `micro` style, values in `body`.
- Strip: 96-px thumbs, `radius_sm`, 8-px gap. Focused thumbnail: 2-px `accent` outline.

### Status bar

- 24 tall. `panel_bg` fill. 1-px top border in `border`.
- Text in `caption` style (11px, 400). Segments separated by 16-px whitespace, no dividers.
- **Per-pane, not window-level.** Each pane owns its own status bar; there is no shared status row along the bottom of the workspace.
- Left: entry count and selection.
- Middle: **connection chip** for remote panes — backend glyph (🔐 SFTP, 📡 FTP, 🌐 WebDAV, ☁️ S3) + host + colour-coded state token (`success` connected, `warning` reconnecting, `error` failed).
- Right: indexer state (local panes) or backend-specific hint, view mode, sort.
- The status bar hosts progress previews for background ops that started from this pane; clicking opens the ops panel.

### Address bar chord routing

The address bar accepts native TextInput semantics **only when it has focus**. When a pane's address bar owns focus:

- `Cmd+A` / `Cmd+C` / `Cmd+V` / `Cmd+X` operate on the input's text (native TextInput behaviour), not on the pane selection.
- `Escape` clears the input and returns focus to the pane.
- Arrow keys navigate within the input.

This is the same convention used by every modal text input (Connect modal, palette prompt, search field, bulk-rename fields). See the "Modal chord routing" section below.

### Modal chord routing

There is exactly **one** canonical pattern for keyboard routing between modals and the underlying pane. It is documented in [`ui-composition.instructions.md`](ui-composition.instructions.md) §5; the essentials:

- The root `FocusScope` in `assets/ui/atlas.slint` sets `keymap-bypass-active = any-modal-visible || text-focus-pane-id != -1 || connect-modal-input-focused` (extend the disjunction whenever a new modal text input is added).
- When `keymap-bypass-active` is true, the Rust dispatcher restricts to the `[Global]` context; Pane bindings return `false` and the key falls through to the focused TextInput, where OS-native shortcuts (`Cmd+A`, `Cmd+C`, arrows) work.
- Modal components must **bubble their `input-focused` state up** to the root as a named property — don't invent parallel state buses.

### Operation-progress modal

For operations whose foreground duration exceeds ~250 ms (`FOREGROUND_DEFER` in `atlas-ui::ops::controller`), we show a small centered progress modal instead of a status-bar toast:

- Panel: `AtlasModal`, ~440 wide × 148 tall, no nested card.
- Icon column: 22 px kind glyph in `Theme.icon-font-family`, muted color — chrome, not accent.
- Rows: op title in `body_emphasis` (500 weight, 13 px), per-file subtitle in `caption` `fg_muted`.
- Progress: `AtlasProgressBar`; track is neutral, the bar fill is the only accent region.
- Buttons: **Cancel** and **Background** both use `AtlasSecondaryButton`; neither is an accent CTA (cancel dialogs have no single truly-primary action).
- Under 250 ms: no modal at all — a status toast is enough.

### Confirmation modals (replace-item, destructive prompts)

Confirmation modals follow Apple's compact replace-item sheet pattern (see Finder's "An item already exists" prompt as the canonical reference):

- Panel: `AtlasModal`, ~460 wide × ~172 tall (Finder-compact — packs a single-paragraph body next to an icon without ever growing past three lines for typical names).
- **Horizontal icon + body row.** A 48 px kind glyph (Nerd Font, muted) sits on the left; a warning-triangle badge (`\u{f071}` in `Theme.warning`) is overlaid at the bottom-right to signal "this needs a decision". The body flows to the right as one wrapped sentence in `body` (13 px, 400) — no forced line break between statement and question.
- **All buttons neutral.** Replace-item and cancel-op dialogs have no single truly-primary action: Stop is the safe default, but Replace and Keep Both are both legitimate. Per §macOS-native modal/menu rules, use `AtlasSecondaryButton` for every action and let the user's read of the sentence pick the right one. Accent color is reserved for actions where "the right choice" is unambiguous (Connect, Rename N files, Save).
- **Progressive disclosure.** "Apply to all remaining conflicts" (and equivalent batch hints) live at the far left of the button row as a `caption`-weight (11 pt, `fg_muted`) checkbox, visually recessive so they don't compete with the primary decision.
- **Focus lands on the safe default** (Stop / Cancel). Escape maps to the safe default too.

### Command palette + goto anywhere

Reference: macOS Spotlight (⌘ Space) and Raycast. The palette sits **on** the workspace with a subtle scrim; it is not a dimmed modal cell.

- Overlay: `Theme.modal-scrim` (40 % black) covering the workspace.
- Panel: `Theme.modal-bg`, `radius-xl` (14 px) — matching every other polish-pass modal — soft `modal-shadow-*` drop, `modal-border` edge, 560 px wide, centered horizontally, 20 % from the top.
- Input row: 48 px tall, no border. A small `nf-fa-search` (`\u{f002}`) glyph in `fg_faint` anchors the left; query text at 16 px regular. Placeholder in `fg_faint`.
- Divider: single 1 px hairline in `Theme.border` between input and results — barely visible.
- Result row: 44 px, 12 px horizontal padding, category glyph column on the left (18 px). Hover fill is `hover_bg`; selected row is `accent_soft` background + 2 px `accent` left rail.
- Result title in `body` (bumps to `body_emphasis` weight 500 when selected), subtitle in `caption` `fg_muted`.
- Kbd chips (e.g. `⌘⇧P`): mono font, 11 px, `panel_bg` fill, `radius_xs`, 4 px padding.

### Connect-server modal (Cmd+K)

- Use `AtlasModal`, `AtlasFieldGroup`, `SectionLabel`, `AtlasTextField`, `AtlasSegmentedControl`, `AtlasList`, and button components.
- Backend and authentication selectors are contiguous segmented controls; selected state is neutral, not accent.
- Body inputs are 29 px compact fields with friendly placeholders (`files.example.com`, `user@example.com`, `/home/user`).
- Saved Servers is an inset `AtlasList`, not a detached giant card.
- Footer: **Connect** is the only `AtlasPrimaryButton`. **Save + Connect** and **Cancel** are secondary.
- On first SFTP connection to an unknown host, the modal transitions to a **host-key prompt** panel showing the fingerprint + comparison hint; Accept persists into `~/.config/atlas/known_hosts` (OpenSSH format), Reject aborts.
- Every input bubbles focus state to the root chord dispatcher via the `input-focused` bool — see [`ui-composition.instructions.md`](ui-composition.instructions.md) §5.

### Bulk rename modal

- Same overlay + panel treatment as palette but wider (620) and taller (variable up to 500). Chrome uses `AtlasModal` (`modal-radius` = `radius-xl`).
- Header row: 15 px semibold "Rename" title + a muted `body` (12 px, `fg-muted`) items counter on the same line — sentence case, no bold caps.
- Inputs: `SectionLabel` ("Find", "Replace with") + `AtlasTextField` — 29 px compact fields, soft neutral border, low-contrast placeholder. Escape on either field cancels.
- Toggle pills (Regex, Case insensitive): `Theme.secondary-button-bg`/`border`; selected state uses the neutral `segmented-selected-bg` fill — no accent flood on toggles.
- Preview table: wrapped in a neutral list container (`list-bg` / `list-border` / `list-radius`). Same Details-view row grammar; conflict rows tinted with `Theme.error` at low alpha; `is-unchanged` rows dimmed to 45 %.
- Confirm button: `AtlasPrimaryButton` — this modal has exactly one truly-primary action (commit the rename), which is unambiguous, so accent is correct here. Cancel is `AtlasSecondaryButton`.

### Inline rename cell (F2 / context-menu Rename on a single entry)

Single-entry rename is a **view-level cell swap**, not a modal — matches Finder's in-place inline-rename affordance. When a `RenameSession` targets `(pane_id, entry_index)`, the four views swap the filename Text label at that row for an `InlineRenameCell`:

- **Fill.** `Theme.selection-bg` (the current selection color; on Atlas dark that's the same blue as `.selected` rows).
- **Radius.** `Theme.radius-sm` (6 px). Inline cells are compact — **the `radius_xl` modal radius does not apply here**. Inline rename is grammar-with-the-row, not chrome on top.
- **Foreground.** `Theme.selection-fg`. Caret color mirrors this so the insertion point stays visible against the fill.
- **No border.** Selection fill is the affordance.
- **No shadow.** Interior; a shadow here would fight the row's own selection background.
- **No motion.** Finder's inline rename does not animate — the cell just switches. Animation on cell switch would draw attention away from the label the user is about to type over.
- **Font.** Adopted per-view so the cell replaces the label without jumping the layout:
  - Details: `body` (13 px), 26 px cell height, HorizontalLayout stretch on the name column.
  - Grid: `body` (13 px), matches `label-height`, single-line editable (elided label swaps for editable single-line).
  - Miller: `small` (12 px), 22 px cell height, focused column only.
  - Gallery: `title` (15 px), 28 px cell height, sits above the metadata rows.
- **Pre-selection.** On open the stem is pre-selected via
  `set-selection-offsets(start, end)` from
  `atlas_ui::rename_inline::stem_range`: whole name for directories
  and dotfiles, everything before the final dot for ordinary files.
  Compound extensions (`archive.tar.gz`) strip only the final `.gz`
  — matches Finder precisely.
- **Commit / cancel.** Enter and blur (click outside, focus loss)
  commit; Escape cancels. Blur-commit is a deliberate Finder-parity
  detail — click outside a Finder inline rename applies the change,
  it does not revert.
- **Validation.** Inline error text under the cell in `Theme.error`
  at `field-label-size` caption weight; visible only when the
  buffer fails `validate_name`. No animation.
- **Sibling collision.** Not detected at the cell layer — commit
  routes through `OpsController::submit_rename` and any collision
  surfaces as an `OpEvent::Conflict` which opens the shared
  `AtlasConflictModal` (§Confirmation modals). One conflict prompt
  for all mutation ops, no bespoke rename dialog.
- **Chord routing.** The cell's `input-focused` bubbles through the
  view → pane → root's `keymap-bypass-active` disjunction so
  `Cmd+A`, `Cmd+C`, `Cmd+V`, and arrow keys route natively to the
  TextInput per `ui-composition.instructions.md` §5.

### Ops panel

- Right-docked in the same slot and width as the Search side panel via `RightDockPanel`; Search and Ops are mutually-exclusive content variants.
- `Cmd+J` toggles Ops in the shared slot. If Search is showing, `Cmd+J` swaps to Ops; if Ops is showing, `Cmd+F` swaps back to Search.
- Interior rows live in `AtlasList`; row height is 44 px.
- Progress uses `AtlasProgressBar` (4 px, `radius-xs` rounded ends); only the progress fill uses `accent`.
- Cancel/Dismiss icon-only controls are revealed on hover. Terminal done/failed/cancelled rows fade to 60% opacity after 5s.

### Search side panel

- Right-docked in `RightDockPanel`, the shared slot also used by Ops. Only one right-dock surface is visible at a time.
- Input at top (`title` size). Results below in Details-view grammar.
- Content matches: monospace snippet, matched spans wrapped in `accent`.

---

## What NOT to do

- **No duplicate branding** in the content area. The window title bar shows "Atlas"; nothing else should.
- **No bright colors** outside the semantic set. If you want a "cool green", it goes through `success`.
- **No accent floods.** One primary CTA per modal; segmented selections and secondary actions stay neutral.
- **No shadows on interior panels** in dark mode. Contrast alone inside a modal/menu.
- **No inline styles.** Every visible property comes from `Theme.*`.
- **No animations exceeding 300 ms**.
- **No bold weights above 700**.
- **No square corners on interactive elements.**
- **No all-caps form labels** (`HOST`, `AUTHENTICATION`) in modal/menu forms.
- **No rounded-rects inside rounded-rects** unless the inner element is a true list/input/button.
- **No ad-hoc shared dimensions inside components** — promote reusable geometry to tokens.
- **No unique fonts per component.** One family for body, one for mono.
- **No copy-paste theming.** If two components look alike, share a sub-component.

---

## When creating a new component

1. Start from the token table. Reach for the right token by intent, not by hex.
2. Pick a chrome height from the standard list.
3. Pick a radius from the standard list.
4. Pick a type style from the scale.
5. Use `space_*` for padding; no custom pixel values.
6. Wire a hover and a focus state. Never ship a component without both.
7. Verify the same component looks correct in light and dark by swapping `Theme.dark`.
8. Show it to yourself in 3 densities (compact, comfortable, spacious) if it's a row.

## When touching an existing component

- Keep the token contract. If you need a new token, add it to the theme first and let both dark and light schemes define it.
- Do not raise the visual weight of anything. If it already looks like it fights for attention, tone it down.
- Prefer deleting decoration over adding it.
