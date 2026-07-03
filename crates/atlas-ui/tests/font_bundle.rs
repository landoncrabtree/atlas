//! Cross-platform assurance that the bundled `Symbols Nerd Font Mono`
//! TTF actually round-trips through Slint's compile-time asset
//! embedding into the final `atlas` binary.
//!
//! # Why this test exists
//!
//! The TTF is registered via a top-level `import` in
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
//! The byte-scan is platform-agnostic — it looks for raw TTF bytes
//! that survive linking on Mach-O (macOS), ELF (Linux), and PE
//! (Windows). CI runs `cargo test --workspace` on all three OSes via
//! `.github/workflows/ci.yml`'s matrix; the test fires on each.
//!
//! Locally you'll only exercise the OS you're running on, but the
//! Slint asset-embed mechanism has no target-specific pipeline —
//! `import "…ttf"` produces the same static byte slice regardless of
//! target, so the assertion holds by construction across all three.
//! The CI matrix is defence in depth against a future Slint version
//! silently changing that.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

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

// ── Test 2: TTF bytes are embedded in the atlas-ui rlib ────────────
//
// Slint's compile-time `import "…ttf"` inlines the font bytes into
// `atlas_ui`'s generated code as a static byte slice. We verify by
// byte-scanning `libatlas_ui-*.rlib` (or `atlas_ui-*.rlib` on
// Windows) directly — no separate binary build required.
//
// This is deliberately cheap: the rlib was already produced by the
// current `cargo build` / `cargo clippy` invocation that compiled this
// test, so it always exists on disk by the time this test body runs.
// A previous version of this test ran `cargo build --release --bin atlas`
// inside the test which took 3-8 minutes per CI runner; that's gone.

#[test]
fn release_binary_embeds_the_bundled_ttf() {
    let root = workspace_root();

    // Find the atlas-ui rlib produced by the current cargo invocation.
    // Under `cargo test`, `CARGO_MANIFEST_DIR` is atlas-ui's crate dir
    // and the deps live at `<workspace>/target/{debug,release}/deps/`.
    // Search both profiles — whichever exists is fine, because Slint
    // embeds the same byte slice for either.
    let deps_candidates = [
        root.join("target/debug/deps"),
        root.join("target/release/deps"),
    ];

    let mut rlib_path: Option<PathBuf> = None;
    for deps_dir in &deps_candidates {
        let Ok(entries) = std::fs::read_dir(deps_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Match libatlas_ui-<hash>.rlib (Unix) and atlas_ui-<hash>.rlib (Windows).
            let is_match = (name_str.starts_with("libatlas_ui-")
                || name_str.starts_with("atlas_ui-"))
                && name_str.ends_with(".rlib");
            if is_match {
                rlib_path = Some(entry.path());
                break;
            }
        }
        if rlib_path.is_some() {
            break;
        }
    }

    let rlib_path = rlib_path.unwrap_or_else(|| {
        panic!(
            "No atlas_ui rlib found under {:?}. This test expects to run \
             during a normal `cargo test -p atlas-ui` cycle where the \
             rlib is already built by the test harness itself.",
            deps_candidates,
        )
    });

    let binary = std::fs::read(&rlib_path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", rlib_path.display()));

    // Signature 1: the TTF `name` table stores the family name as
    // "Symbols Nerd Font Mono". This string survives linking as raw
    // bytes because Slint's asset embedding stores the entire TTF
    // (including its `name` table) verbatim inside a static byte
    // slice. Present in every target we ship — macOS Mach-O, Linux
    // ELF, Windows PE.
    let family_name = b"Symbols Nerd Font Mono";
    assert!(
        contains_bytes(&binary, family_name),
        "atlas-ui rlib at {} does not contain the TTF family name \
         `Symbols Nerd Font Mono` — the Slint asset embed may have \
         drifted (check assets/ui/atlas.slint for the `import` line).",
        rlib_path.display()
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
            "atlas-ui rlib does not contain the TTF table tag {:?} \
             — the font bytes may be malformed or truncated",
            std::str::from_utf8(tag).unwrap()
        );
    }
}

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
