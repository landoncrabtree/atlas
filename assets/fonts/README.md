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
- **How it's registered**: bundled via `include_bytes!` and installed
  into Slint's font registry at startup with
  `slint::register_font_from_memory` — see
  `crates/atlas-app/src/main.rs::register_bundled_fonts`.

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
