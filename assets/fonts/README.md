# Bundled fonts

## `SymbolsNerdFontMono-Regular.ttf`

A patched *symbols-only* subset of Nerd Fonts — contains **only** icon
glyphs (Font Awesome, Devicons, Octicons, Powerline, Weather, Material,
Codicons, Pomicons, Seti-UI, IEC Power Symbols, Font Logos) mapped into
the Unicode Private Use Area (PUA). No letters, no digits, no
punctuation. This is deliberate: it acts as a **pure fallback font** —
the user's chosen text font renders letters, and Slint falls through to
Symbols Nerd Font Mono only for PUA codepoints (e.g. `\u{f07b}` folder,
`\u{e7a8}` rust, `\u{f0870}` markdown). No competition with the user's
typography choice.

- **Upstream**: <https://github.com/ryanoasis/nerd-fonts>, specifically
  `patched-fonts/NerdFontsSymbolsOnly/SymbolsNerdFontMono-Regular.ttf`.
- **License**: MIT — see [`NERD-FONTS-LICENSE`](NERD-FONTS-LICENSE).
  Copyright (c) 2014 Ryan L McIntyre.
- **Size**: ~2.5 MB.
- **How it's registered**: embedded at compile time via a top-level
  Slint `import "../fonts/SymbolsNerdFontMono-Regular.ttf"` in
  [`assets/ui/atlas.slint`](../ui/atlas.slint). The Slint compiler
  bakes the raw TTF bytes into a static `&[u8]` inside `atlas-ui`'s
  generated code and emits a `register_font_from_memory` call at
  window-construction time — no runtime file I/O, no `include_bytes!`
  needed in Rust. Cross-platform assurance that the bytes actually
  round-trip into every `atlas` binary (macOS Mach-O, Linux ELF,
  Windows PE) is asserted by
  [`crates/atlas-ui/tests/font_bundle.rs`](../../crates/atlas-ui/tests/font_bundle.rs)
  — a SHA-256 pin on the on-disk TTF plus a byte-scan of the release
  binary for the family name string + TrueType table tags.

## Windows escape hatch: `ui.icons.pack = "ascii"`

Hosts that can't register the bundled font at runtime (extremely rare
— every mainstream Windows/macOS/Linux Slint build ships FreeType,
which reads the embedded bytes fine) can set

```toml
[ui.icons]
pack = "ascii"
```

in `~/.config/atlas/config.toml` to swap every filetype icon for a
short bracketed ASCII fallback (`[D]` folder, `[c]` source code, `[i]`
image, `[v]` video, `[$]` shell script, etc.). See
`crates/atlas-ui/src/theming/icons.rs` for the full mapping.
Live-reloadable — edit the file and icons swap without restart.

## Icon-map attribution

The filetype-to-glyph mapping in
`crates/atlas-ui/src/theming/icons.rs` is adapted from
[LSD](https://github.com/lsd-rs/lsd) (Apache-2.0). See
[`LSD-LICENSE`](LSD-LICENSE) for the upstream Apache license text.

## Updating

If a newer release changes glyph mappings, re-download the font with:

```bash
curl -sL \
  "https://raw.githubusercontent.com/ryanoasis/nerd-fonts/master/patched-fonts/NerdFontsSymbolsOnly/SymbolsNerdFontMono-Regular.ttf" \
  -o assets/fonts/SymbolsNerdFontMono-Regular.ttf
```

Then re-verify the icon set renders in-app via the MCP live-verify path
(see `docs/developer-setup.md` §MCP).
