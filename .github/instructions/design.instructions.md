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

- **Do**: SF Pro / Inter with a strict type scale, uppercase micro-labels for section headers, 8pt grid spacing.
- **Don't**: mystery-meat icons, decorative separators, ALL-CAPS body text.

### 3. Depth

Layered planes establish hierarchy. Modals float above panels. Panels sit on the workspace. The workspace sits on the window. Depth is expressed through **subtle background differences and blurred materials**, rarely through drop shadows.

- **Do**: 3–6 percent brightness deltas between layers; `Theme.panel_bg` above `Theme.bg`; use a blurred/translucent overlay for palettes and menus.
- **Don't**: box shadows on flat surfaces; hard borders that duplicate depth.

### 4. Consistency

Same interaction, same treatment. A row hover looks the same in Details, Grid, Miller, Tree, and Gallery. Buttons follow one visual grammar. Text inputs follow one grammar.

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

## Design tokens

Owned by the `Theme` global (Slint) and mirrored by `ThemeTokens` (Rust). Every visible surface reads from Theme; **no hard-coded hex values in components**.

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

### Typography

- **Family** (body): `SF Pro Text` → `SF Pro Display` (large) → `Inter` → `system-ui` fallback.
- **Family** (monospace): `SF Mono` → `JetBrains Mono` → `Menlo` → `monospace`.

Type scale (px):

| Style | Size | Weight | Line-height | Use |
|---|---|---|---|---|
| `micro` | 10 | 600 (uppercase, letter-spacing 0.06em) | 1.2 | Section labels, column headers |
| `caption` | 11 | 400 | 1.35 | Status bar, hints, timestamps |
| `small` | 12 | 400 | 1.4 | Secondary body |
| `body` | 13 | 400 | 1.45 | Primary body — most rows, most labels |
| `body_emphasis` | 13 | 500 | 1.45 | Emphasized body — selected row name |
| `title` | 15 | 600 | 1.3 | Panel titles, palette prompt |
| `headline` | 17 | 600 | 1.25 | Modal titles |
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
| `radius_xl` | 14 | The window itself if we ever draw it |

Never use square corners for interactive elements.

### Elevation

Prefer background contrast over shadows. When a shadow is truly needed (modals, tooltips), use a **single subtle drop**:

- Modal overlay backdrop: `#000000 40%` fill covering the workspace.
- Modal panel: no shadow (rely on backdrop) OR `0 8 24 rgba(0,0,0,0.35)` if the modal must float on a bright pane.
- Palette results item hover: no shadow — use `hover_bg` instead.

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

### Tree view

- Row height 26 (denser than Details).
- Indent 16-px per level.
- Chevron: `›` (collapsed) / `⌄` (expanded) at `fg_muted`, `fg` on hover. Rotates smoothly (100 ms).
- Kind glyph after the chevron.
- Selected: `selection_bg` fill on the full row, `radius_sm`.

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

- Panel: `panel_bg_elevated`, `radius_lg`, ~420 wide.
- Rows: op summary in `body`, per-file progress in `caption` `fg_muted`, primary bar `accent`, secondary spinner while the queue drains.
- Buttons: **Cancel** (fires the shared `CancellationToken`), **Background** (dismisses the modal but keeps the op running under the ops panel).
- Under 250 ms: no modal at all — a status toast is enough.

### Command palette + goto anywhere

- Overlay: `#000000` at 40% alpha covering the workspace.
- Panel: `panel_bg_elevated`, `radius_lg`, 560-px wide, centered horizontally, 20% from the top.
- Prompt input: 44 tall, no border, `title` style. Placeholder in `fg_faint`.
- Result row: 40 tall, 16-px padding, hover `hover_bg`, selected `accent_soft` background + `accent` left rail (2 px).
- Result title in `body`, subtitle in `caption` `fg_muted`.
- Kbd chips (e.g. `⌘⇧P`): mono font, 11 px, `panel_bg` fill, `radius_xs`, 4-px padding.

### Connect-server modal (Cmd+K)

- Same overlay + panel treatment as palette (`panel_bg_elevated`, `radius_lg`, ~560 wide).
- Header row with backend selector (SFTP / FTP / WebDAV / S3) as a segmented control; the active pill uses `accent_soft` fill.
- Body: form inputs share the address-bar treatment; secret fields render as `••••` and never persist to `servers.toml` — they go into the OS keychain under the `com.atlas.credentials` namespace.
- Footer: **Save & Connect** (primary), **Connect once** (secondary), **Cancel**.
- On first SFTP connection to an unknown host, the modal transitions to a **host-key prompt** panel showing the fingerprint + comparison hint; Accept persists into `~/.config/atlas/known_hosts` (OpenSSH format), Reject aborts.
- Every input bubbles focus state to the root chord dispatcher via the `input-focused` bool — see [`ui-composition.instructions.md`](ui-composition.instructions.md) §5.

### Bulk rename modal

- Same overlay + panel treatment as palette but wider (620) and taller (variable up to 500).
- Header row: `headline` style title + `caption` count of items.
- Inputs: standard address-bar treatment.
- Preview table: same visual grammar as Details. Conflicts row: `error` fg on the "proposed" cell.
- Confirm button: primary style. Disabled: 40% alpha.

### Ops panel

- Docked bottom, 200 tall when open, translucent `panel_bg_elevated`, 1-px top `border`.
- Row height 44 (needs the progress bar). Progress bar height 4, `radius_xs`, filled `accent`, unfilled `panel_bg`.
- Cancel/Dismiss icon-only, revealed on hover.
- Terminal (done/failed) rows fade to 60% opacity after 5 s.

### Search side panel

- Right-docked, 320 wide, `panel_bg` fill, 1-px left `border`.
- Input at top (`title` size). Results below in Details-view grammar.
- Content matches: monospace snippet, matched spans wrapped in `accent`.

---

## What NOT to do

- **No duplicate branding** in the content area. The window title bar shows "Atlas"; nothing else should.
- **No bright colors** outside the semantic set. If you want a "cool green", it goes through `success`.
- **No shadows on flat panels** in dark mode. Contrast alone.
- **No inline styles.** Every visible property comes from `Theme.*`.
- **No animations exceeding 300 ms**.
- **No bold weights above 700**.
- **No square corners on interactive elements.**
- **No pixel values inside components** — always tokens.
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
