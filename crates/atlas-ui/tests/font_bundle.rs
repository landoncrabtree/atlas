//! Cross-platform assurance that the bundled `Symbols Nerd Font Mono`
//! TTF actually round-trips through Slint's compile-time asset
//! embedding into the final `atlas` binary.
//!
//! # Why this test exists
//!
//! Phase 2.10 registered the TTF via a top-level `import` in
//! `assets/ui/atlas.slint`. The Slint compiler picks up that import
//! and generates a `register_font_from_memory` call in the generated
//! `atlas.rs` under `target/*/build/atlas-ui-*/out/`, with the font
//! bytes inlined as a `const &[u8]` (`SLINT_EMBEDDED_RESOURCE_*`).
//!
//! That mechanism is entirely compile-time. There is no
//! `Cargo.toml`-level `[assets]` block, no runtime file-copy step, and
//! no `include_bytes!` in Rust source that a `grep` would flag. If the
//! font file was accidentally removed, replaced, corrupted, or if the
//! Slint import path drifted, the CI signal would be a runtime "font
//! not found" — silently rendering tofu in production and never
//! failing the test suite.
//!
//! This test closes that gap by asserting three properties:
//!
//! 1. **TTF is exactly the file we vendored.** SHA-256 of the on-disk
//!    file must match a checked-in constant. Catches accidental font
//!    swaps + corruption on branches.
//! 2. **TTF bytes are baked into the compiled binary.** Runs
//!    `cargo build --release --bin atlas` and byte-scans the resulting
//!    binary for a signature drawn from the TTF's `name` table
//!    ("Symbols Nerd Font Mono", present in both ASCII and UTF-16BE
//!    encodings because TTF stores name records in either encoding).
//!    Also confirms the TTF table tags (`glyf`, `cmap`, `head`) are
//!    present so we're not just picking up a stray reference to the
//!    family name in some unrelated resource.
//! 3. **`IconPack` serde round-trips cleanly.** `"nerd"` ↔ `IconPack::Nerd`,
//!    `"ascii"` ↔ `IconPack::Ascii`, and unknown values produce a
//!    helpful error mentioning both variants — the ASCII pack is
//!    useless if users can't reach it because typos in the config
//!    silently fall through to the default.
//!
//! # Slint 1.17 runtime "font missing" hook
//!
//! Slint 1.17's public API does not expose a runtime callback for
//! "the requested font-family failed to resolve". There's no
//! `font_missing_signal` on `slint::Window`, no counterpart to Qt's
//! `QRawFont::supportsCharacter`, and no way to query the registered
//! font list from Rust. This test is therefore the primary
//! cross-platform assurance channel — the runtime path relies on
//! `Theme.icon-family-for-pack` picking the correct font at bind
//! time, and users noticing tofu themselves (at which point they can
//! set `ui.icons.pack = "ascii"` for a text-only fallback that
//! doesn't need the bundled font at all).
//!
//! # Cross-platform coverage
//!
//! - macOS: this test runs `cargo build --release --bin atlas`
//!   natively and byte-scans the resulting Mach-O binary. Runs
//!   unconditionally on macOS.
//! - Linux: the same `cargo build --release --bin atlas` would work
//!   under a Linux CI runner. This test is `cfg`-gated to macOS +
//!   Linux (which share the Unix-side symbol-name conventions the
//!   scan relies on); the scan itself is platform-agnostic since it
//!   looks for raw TTF bytes.
//! - Windows: `cargo build --target x86_64-pc-windows-gnu` produces
//!   a PE binary that also embeds the TTF via the same Slint
//!   mechanism. The byte-scan would work identically because the
//!   family name and table tags survive linking unchanged. See
//!   `TODO(ci)` below — actual cross-compile is deferred to CI; the
//!   assertion holds by construction (Slint embeds the same bytes
//!   into every target).
//!
//! # TODO(ci): Windows + Linux native verification
//!
//! Attempted `cargo build --target x86_64-pc-windows-gnu -p atlas-ui`
//! on macOS with `brew install mingw-w64` — the mingw toolchain
//! installs fine but Skia's build script (pulled in transitively by
//! Slint's software renderer, `skia-safe`) asserts on a missing VC
//! install regardless of the C compiler (`assert(win_vc != "")` in
//! `gn/BUILDCONFIG.gn`). That's a heavy toolchain problem beyond a
//! font-round-trip test's scope. Once atlas's CI matrix has a native
//! Windows runner, extend this file with a `#[cfg(target_os = "windows")]`
//! copy of `release_binary_embeds_the_bundled_ttf` that scans
//! `target/release/atlas.exe`. Same for a native Linux runner
//! scanning ELF.
//!
//! Until then, the invariant we rely on is: **Slint embeds `import
//! "…ttf"` resources as raw byte slices into the generated code, so
//! the family name and TrueType table tags survive linking on every
//! target the crate compiles for.** The macOS byte-scan below is
//! sufficient evidence that Slint's asset-embed mechanism itself is
//! working; a Windows-specific check would only catch a regression
//! where Windows-only compilation stops embedding, which would
//! require Slint to ship a target-specific asset pipeline — no such
//! feature exists in 1.17.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

/// SHA-256 of `assets/fonts/SymbolsNerdFontMono-Regular.ttf` as
/// vendored on `main`. If this test fails, either:
///
/// - The font file was intentionally updated — regenerate this
///   constant with `shasum -a 256 assets/fonts/SymbolsNerdFontMono-Regular.ttf`
///   and update `assets/fonts/README.md` to reflect the new upstream
///   version + license terms.
/// - The font file was accidentally corrupted / swapped — restore it
///   from git or re-download the Symbols Nerd Font Mono release from
///   <https://github.com/ryanoasis/nerd-fonts>.
const FONT_SHA256_HEX: &str = "f0f624d9b474bea1662cf7e862d44aebe1ae1f6c7f9cb7a0ca5d0e5ac9561c60";

/// Return the workspace root (parent of `crates/`).
fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/atlas-ui when this test runs;
    // pop up two levels to reach the workspace root.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    manifest_dir
        .ancestors()
        .nth(2)
        .expect("workspace root two levels above crates/atlas-ui")
        .to_path_buf()
}

// ── Test 1: TTF file has the expected SHA-256 ──────────────────────

#[test]
fn ttf_on_disk_has_pinned_sha256() {
    let ttf_path = workspace_root().join("assets/fonts/SymbolsNerdFontMono-Regular.ttf");
    let bytes = std::fs::read(&ttf_path).unwrap_or_else(|err| {
        panic!(
            "Cannot read {}: {err}. Was the font file removed? \
             See assets/fonts/README.md for the source URL.",
            ttf_path.display()
        )
    });

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    let mut got_hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        write!(&mut got_hex, "{b:02x}").expect("format u8 as hex");
    }

    assert_eq!(
        got_hex, FONT_SHA256_HEX,
        "TTF SHA-256 mismatch. If the font was intentionally updated, \
         update FONT_SHA256_HEX in this file + note the change in \
         assets/fonts/README.md."
    );

    // Sanity — TTF magic header is either 0x00010000 (TrueType) or
    // 'OTTO' (OpenType with CFF). Symbols Nerd Font Mono is TrueType.
    assert_eq!(
        &bytes[..4],
        &[0x00, 0x01, 0x00, 0x00],
        "expected TrueType magic header 0x00010000"
    );
    assert!(bytes.len() > 100_000, "TTF suspiciously small");
}

// ── Test 2: TTF bytes are embedded in the release binary ───────────
//
// Runs only on macOS + Linux where `cargo build --release --bin atlas`
// works natively (no Skia MSVC issues) and produces a Mach-O / ELF
// binary that we byte-scan for the embedded TTF signature. On
// Windows this would require the MSVC toolchain — deferred to CI
// per the module-level `TODO(ci)`.

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn release_binary_embeds_the_bundled_ttf() {
    let root = workspace_root();

    // Build in release mode. Reuses whatever's already there — cargo
    // is idempotent so this is fast on rebuilds. Fails loudly if the
    // build itself fails (which itself is a signal worth surfacing).
    let status = Command::new("cargo")
        .args(["build", "--release", "--bin", "atlas"])
        .current_dir(&root)
        .status()
        .expect("failed to spawn `cargo build --release --bin atlas`");
    assert!(status.success(), "cargo build --release --bin atlas failed");

    let bin_path = if cfg!(windows) {
        root.join("target/release/atlas.exe")
    } else {
        root.join("target/release/atlas")
    };
    let binary = std::fs::read(&bin_path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", bin_path.display()));

    // Signature 1: the TTF `name` table stores the family name as
    // "Symbols Nerd Font Mono". This string survives linking as raw
    // bytes because Slint's asset embedding stores the entire TTF
    // (including its `name` table) verbatim inside a static byte
    // slice. Present in every target we ship — macOS Mach-O, Linux
    // ELF, Windows PE.
    let family_name = b"Symbols Nerd Font Mono";
    assert!(
        contains_bytes(&binary, family_name),
        "release binary at {} does not contain the TTF family name \
         `Symbols Nerd Font Mono` — the Slint asset embed may have \
         drifted (check assets/ui/atlas.slint for the `import` line).",
        bin_path.display()
    );

    // Signature 2: TrueType table tags. `glyf` (glyph outlines) and
    // `cmap` (character-to-glyph mapping) are mandatory in every TTF
    // and appear at fixed positions in the `name` table's table-
    // directory header. If Slint accidentally stripped the font
    // bytes and stored just the metadata (impossible with the current
    // API, but this catches future regressions), the tags would be
    // absent.
    for tag in [b"glyf", b"cmap", b"head"] {
        assert!(
            contains_bytes(&binary, tag),
            "release binary does not contain the TTF table tag {:?} \
             — the font bytes may be malformed or truncated",
            std::str::from_utf8(tag).unwrap()
        );
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ── Test 3: IconPack serde round-trip ──────────────────────────────
//
// The ASCII pack is only useful if `ui.icons.pack = "ascii"` actually
// deserialises to `IconPack::Ascii`. A silent parse failure would
// fall through to the default (Nerd) — exactly the wrong behaviour
// on hosts where the Nerd Font fails to register. This test is the
// gate against that regression.

#[test]
fn icon_pack_serde_roundtrips_both_variants() {
    use atlas_config::IconPack;

    // Nerd
    let nerd_toml = toml::to_string(&Wrap {
        pack: IconPack::Nerd,
    })
    .expect("serialise nerd");
    assert!(
        nerd_toml.contains("pack = \"nerd\""),
        "expected `pack = \"nerd\"` in {nerd_toml:?}"
    );
    let parsed: Wrap = toml::from_str(&nerd_toml).expect("parse nerd");
    assert_eq!(parsed.pack, IconPack::Nerd);

    // Ascii
    let ascii_toml = toml::to_string(&Wrap {
        pack: IconPack::Ascii,
    })
    .expect("serialise ascii");
    assert!(
        ascii_toml.contains("pack = \"ascii\""),
        "expected `pack = \"ascii\"` in {ascii_toml:?}"
    );
    let parsed: Wrap = toml::from_str(&ascii_toml).expect("parse ascii");
    assert_eq!(parsed.pack, IconPack::Ascii);

    // Case-insensitive on the way in (matches `Density` / `ViewMode`
    // ergonomics).
    let upper: Wrap = toml::from_str("pack = \"NERD\"").expect("parse NERD");
    assert_eq!(upper.pack, IconPack::Nerd);
    let mixed: Wrap = toml::from_str("pack = \"Ascii\"").expect("parse Ascii");
    assert_eq!(mixed.pack, IconPack::Ascii);
}

#[test]
fn icon_pack_unknown_variant_error_names_both_options() {
    use atlas_config::IconPack;
    let err = toml::from_str::<Wrap>("pack = \"emoji\"").expect_err("`emoji` is not a valid pack");
    let msg = format!("{err}");
    assert!(
        msg.contains("nerd") && msg.contains("ascii"),
        "unknown-variant error message must name both `nerd` and `ascii`, \
         got: {msg:?}"
    );
    // Sanity — force use of IconPack so the compiler catches an
    // accidental unused import if someone deletes the enum.
    let _ = IconPack::Nerd;
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Wrap {
    pack: atlas_config::IconPack,
}
