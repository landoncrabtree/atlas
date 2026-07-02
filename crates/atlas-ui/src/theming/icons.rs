//! Filetype icon glyph mapping (lsd-inspired).
//!
//! Provides a single canonical function [`icon_for`] used by every view
//! (Details, Grid, Miller, Gallery) so a `.rs` file looks the same regardless
//! of which view is currently active. The mapping deliberately favours single
//! Unicode-emoji codepoints (BMP or SMP) that render on the app's default
//! font stack (`SF Pro Text` → Apple emoji fallback on macOS, Segoe UI
//! Emoji on Windows, Noto Emoji on Linux). ASCII fallback is available for
//! terminals and low-graphics setups via the `[ui.icons] use_emoji = false`
//! config knob.
//!
//! # Design
//!
//! - The mapping is a pure function of `(EntryKind, extension, permissions)`.
//!   No I/O, no allocations — safe to call on every visible row.
//! - The default is emoji-on. Users opt into ASCII via
//!   [`set_use_emoji(false)`]. The value is a process-wide
//!   [`AtomicBool`], set once at startup from
//!   [`atlas_config::Icons::use_emoji`] and updated on live-reload.
//! - Extension matching is case-insensitive because
//!   [`atlas_fs::Entry::extension()`] already lowercases.
//! - Symlink glyph handling matches the item-6 fix
//!   (`↪` healthy / `⚠` broken) — we do *not* recurse into the symlink
//!   target: a symlink to a directory keeps the symlink glyph so the user
//!   can see the link nature at a glance.
//! - Executable detection uses the unix `x` permission bits when present;
//!   on Windows we rely on extension (`.exe`, `.bat`, `.cmd`) as a fallback.
//!
//! # TODO(fonts)
//!
//! Support Nerd Fonts for finer glyphs (\u{f07b} folder, \u{f15b} file,
//! \u{e7a8} rust, \u{e73c} python, etc.) — this would require adding a
//! `[ui.icons] pack = "emoji" | "nerd" | "ascii"` field and a bundled or
//! user-supplied Nerd Font in the resources bundle.

use std::sync::atomic::{AtomicBool, Ordering};

use atlas_fs::{Entry, EntryKind};

/// A glyph paired with a short accessibility description.
///
/// `glyph` is a `&'static str` (not `char`) so that emoji sequences using
/// the variation-selector-16 (`U+FE0F`, e.g. `⚙️` = `U+2699 U+FE0F`) and
/// multi-char ASCII fallbacks (`[D]`, `[F]`) can share the same type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconGlyph {
    /// The rendered glyph (Unicode emoji, symbol, or ASCII fallback).
    pub glyph: &'static str,
    /// Short human-readable description for a11y/tooltips
    /// (e.g. `"Rust source"`, `"folder"`).
    pub description: &'static str,
}

impl IconGlyph {
    const fn new(glyph: &'static str, description: &'static str) -> Self {
        Self { glyph, description }
    }
}

/// Process-wide emoji-vs-ASCII toggle. Set once at startup from
/// [`atlas_config::Icons::use_emoji`]; may be updated on config live-reload.
static USE_EMOJI: AtomicBool = AtomicBool::new(true);

/// Update the global emoji-vs-ASCII toggle. Called by the shell on config
/// wire-in and on live-reload.
pub fn set_use_emoji(v: bool) {
    USE_EMOJI.store(v, Ordering::Relaxed);
}

/// Read the current emoji-vs-ASCII toggle.
#[must_use]
pub fn use_emoji() -> bool {
    USE_EMOJI.load(Ordering::Relaxed)
}

// ── ASCII fallbacks ────────────────────────────────────────────────────────

const ASCII_DIR: IconGlyph = IconGlyph::new("[D]", "folder");
const ASCII_FILE: IconGlyph = IconGlyph::new("[F]", "file");
const ASCII_SYMLINK: IconGlyph = IconGlyph::new("[L]", "symbolic link");
const ASCII_BROKEN_SYMLINK: IconGlyph = IconGlyph::new("[!]", "broken symlink");
const ASCII_EXEC: IconGlyph = IconGlyph::new("[X]", "executable");
const ASCII_OTHER: IconGlyph = IconGlyph::new("[?]", "special file");

// ── Emoji glyphs — grouped for readability ─────────────────────────────────

const EMOJI_DIR: IconGlyph = IconGlyph::new("📁", "folder");
const EMOJI_SYMLINK: IconGlyph = IconGlyph::new("↪", "symbolic link");
const EMOJI_BROKEN_SYMLINK: IconGlyph = IconGlyph::new("⚠", "broken symlink");
const EMOJI_EXEC: IconGlyph = IconGlyph::new("⚡", "executable");
const EMOJI_OTHER: IconGlyph = IconGlyph::new("⚙\u{fe0f}", "special file");
const EMOJI_FALLBACK: IconGlyph = IconGlyph::new("·", "file");

const EMOJI_RUST: IconGlyph = IconGlyph::new("🦀", "Rust source");
const EMOJI_MARKDOWN: IconGlyph = IconGlyph::new("📝", "Markdown document");
const EMOJI_JSON: IconGlyph = IconGlyph::new("📋", "JSON data");
const EMOJI_CONFIG: IconGlyph = IconGlyph::new("⚙\u{fe0f}", "configuration file");
const EMOJI_IMAGE: IconGlyph = IconGlyph::new("🖼", "image");
const EMOJI_VIDEO: IconGlyph = IconGlyph::new("🎬", "video");
const EMOJI_AUDIO: IconGlyph = IconGlyph::new("🎵", "audio");
const EMOJI_PDF: IconGlyph = IconGlyph::new("📕", "PDF document");
const EMOJI_ARCHIVE: IconGlyph = IconGlyph::new("🗜", "archive");
const EMOJI_SHELL: IconGlyph = IconGlyph::new("▶", "shell script");
const EMOJI_PYTHON: IconGlyph = IconGlyph::new("🐍", "Python source");
const EMOJI_JS_TS: IconGlyph = IconGlyph::new("📘", "JavaScript/TypeScript source");
const EMOJI_WEB: IconGlyph = IconGlyph::new("🌐", "web asset");
const EMOJI_TEXT: IconGlyph = IconGlyph::new("📄", "text document");
const EMOJI_GO: IconGlyph = IconGlyph::new("🐹", "Go source");

// ── Public API ─────────────────────────────────────────────────────────────

/// Return the icon glyph for the given entry, reading the process-wide
/// emoji toggle set by [`set_use_emoji`].
///
/// This is the convenience entry point for production code. Tests and
/// pure use-sites should prefer [`icon_for_with`] to avoid depending on
/// shared global state.
#[must_use]
pub fn icon_for(entry: &Entry) -> IconGlyph {
    icon_for_with(entry, use_emoji())
}

/// Pure variant of [`icon_for`] that takes the emoji toggle as an
/// explicit parameter, decoupling it from process-wide state.
///
/// The mapping is:
///
/// | Entry class        | Emoji                                | ASCII |
/// |--------------------|--------------------------------------|-------|
/// | Directory          | 📁                                   | `[D]` |
/// | Symlink (healthy)  | ↪                                    | `[L]` |
/// | Symlink (broken)   | ⚠                                    | `[!]` |
/// | Executable file    | ⚡                                   | `[X]` |
/// | Special (Other)    | ⚙\u{fe0f}                            | `[?]` |
/// | File (by ext)      | see [`icon_for_extension`]           | `[F]` |
/// | File (unmapped)    | ·                                    | `[F]` |
#[must_use]
pub fn icon_for_with(entry: &Entry, use_emoji: bool) -> IconGlyph {
    match &entry.kind {
        EntryKind::Dir => {
            if use_emoji {
                EMOJI_DIR
            } else {
                ASCII_DIR
            }
        }
        EntryKind::Symlink { broken: true, .. } => {
            if use_emoji {
                EMOJI_BROKEN_SYMLINK
            } else {
                ASCII_BROKEN_SYMLINK
            }
        }
        EntryKind::Symlink { .. } => {
            if use_emoji {
                EMOJI_SYMLINK
            } else {
                ASCII_SYMLINK
            }
        }
        EntryKind::Other => {
            if use_emoji {
                EMOJI_OTHER
            } else {
                ASCII_OTHER
            }
        }
        EntryKind::File => {
            if is_executable(entry) {
                if use_emoji {
                    return EMOJI_EXEC;
                }
                return ASCII_EXEC;
            }
            if !use_emoji {
                return ASCII_FILE;
            }
            match entry.extension() {
                Some(ext) => icon_for_extension(&ext).unwrap_or(EMOJI_FALLBACK),
                None => EMOJI_FALLBACK,
            }
        }
    }
}

/// Return the emoji glyph for a lowercase extension, if mapped.
///
/// Extension must be already lowercased (as returned by
/// [`atlas_fs::Entry::extension()`]). Returns [`None`] for unmapped
/// extensions so the caller can decide on a fallback.
#[must_use]
pub fn icon_for_extension(ext: &str) -> Option<IconGlyph> {
    Some(match ext {
        "rs" => EMOJI_RUST,
        "md" | "markdown" => EMOJI_MARKDOWN,
        "json" => EMOJI_JSON,
        "toml" | "yaml" | "yml" | "ini" | "conf" | "cfg" => EMOJI_CONFIG,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" => EMOJI_IMAGE,
        "mp4" | "mov" | "mkv" | "avi" | "webm" => EMOJI_VIDEO,
        "mp3" | "wav" | "flac" | "ogg" | "m4a" => EMOJI_AUDIO,
        "pdf" => EMOJI_PDF,
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" => EMOJI_ARCHIVE,
        "sh" | "bash" | "zsh" | "fish" => EMOJI_SHELL,
        "py" => EMOJI_PYTHON,
        "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" => EMOJI_JS_TS,
        "html" | "htm" | "css" | "scss" | "sass" => EMOJI_WEB,
        "txt" | "log" => EMOJI_TEXT,
        "c" | "h" | "cpp" | "hpp" | "cc" | "hh" => EMOJI_CONFIG,
        "go" => EMOJI_GO,
        _ => return None,
    })
}

/// Detect whether a file has any unix `x` permission bit set.
///
/// Falls back to extension sniffing on non-unix platforms (or when the
/// permission bits aren't available). Only regular files are considered
/// — directories are not treated as executables even though they have
/// `x` bits set for traversal.
fn is_executable(entry: &Entry) -> bool {
    if !matches!(entry.kind, EntryKind::File) {
        return false;
    }
    if let Some(mode) = entry.metadata.permissions_mode {
        if mode & 0o111 != 0 {
            return true;
        }
    }
    matches!(entry.extension().as_deref(), Some("exe" | "bat" | "cmd"))
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file(name: &str) -> Entry {
        Entry {
            path: PathBuf::from(format!("/tmp/{name}")),
            name: name.to_string(),
            kind: EntryKind::File,
            metadata: atlas_fs::Metadata::default(),
        }
    }

    fn dir(name: &str) -> Entry {
        Entry {
            path: PathBuf::from(format!("/tmp/{name}")),
            name: name.to_string(),
            kind: EntryKind::Dir,
            metadata: atlas_fs::Metadata::default(),
        }
    }

    fn symlink(name: &str, broken: bool, target_kind: Option<EntryKind>) -> Entry {
        // `target_kind` documents the test intent (we assert the glyph does
        // NOT depend on what the target is). Not consumed by icon_for.
        let _ = target_kind;
        Entry {
            path: PathBuf::from(format!("/tmp/{name}")),
            name: name.to_string(),
            kind: EntryKind::Symlink {
                target: Some(PathBuf::from("/some/target")),
                broken,
            },
            metadata: atlas_fs::Metadata::default(),
        }
    }

    fn executable(name: &str) -> Entry {
        Entry {
            path: PathBuf::from(format!("/tmp/{name}")),
            name: name.to_string(),
            kind: EntryKind::File,
            metadata: atlas_fs::Metadata {
                permissions_mode: Some(0o755),
                ..Default::default()
            },
        }
    }

    fn other(name: &str) -> Entry {
        Entry {
            path: PathBuf::from(format!("/tmp/{name}")),
            name: name.to_string(),
            kind: EntryKind::Other,
            metadata: atlas_fs::Metadata::default(),
        }
    }

    #[test]
    fn directory_maps_to_folder_glyph() {
        assert_eq!(icon_for_with(&dir("src"), true).glyph, "📁");
    }

    #[test]
    fn healthy_symlink_maps_to_hook_arrow() {
        assert_eq!(
            icon_for_with(&symlink("link", false, None), true).glyph,
            "↪"
        );
    }

    #[test]
    fn broken_symlink_maps_to_warning() {
        assert_eq!(icon_for_with(&symlink("dead", true, None), true).glyph, "⚠");
    }

    #[test]
    fn symlink_to_dir_keeps_symlink_glyph_does_not_recurse() {
        // A symlink to a dir must render as a symlink, NOT as a folder.
        let link = symlink("dir-link", false, Some(EntryKind::Dir));
        assert_eq!(icon_for_with(&link, true).glyph, "↪");
    }

    #[test]
    fn executable_bit_beats_extension() {
        // `.rs` normally → 🦀 but an executable `.rs` (weird but legal) → ⚡
        assert_eq!(icon_for_with(&executable("build.rs"), true).glyph, "⚡");
    }

    #[test]
    fn rust_source_maps_to_crab() {
        assert_eq!(icon_for_with(&file("main.rs"), true).glyph, "🦀");
    }

    #[test]
    fn markdown_maps_to_memo() {
        assert_eq!(icon_for_with(&file("README.md"), true).glyph, "📝");
        assert_eq!(icon_for_with(&file("notes.markdown"), true).glyph, "📝");
    }

    #[test]
    fn json_maps_to_clipboard() {
        assert_eq!(icon_for_with(&file("package.json"), true).glyph, "📋");
    }

    #[test]
    fn toml_and_yaml_map_to_gear() {
        assert_eq!(icon_for_with(&file("Cargo.toml"), true).glyph, "⚙\u{fe0f}");
        assert_eq!(icon_for_with(&file("config.yaml"), true).glyph, "⚙\u{fe0f}");
        assert_eq!(icon_for_with(&file("config.yml"), true).glyph, "⚙\u{fe0f}");
    }

    #[test]
    fn image_extensions_all_map_to_frame() {
        for ext in ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico"] {
            let f = file(&format!("photo.{ext}"));
            assert_eq!(icon_for_with(&f, true).glyph, "🖼", "extension: .{ext}");
        }
    }

    #[test]
    fn video_extensions_map_to_clapperboard() {
        for ext in ["mp4", "mov", "mkv", "avi", "webm"] {
            let f = file(&format!("movie.{ext}"));
            assert_eq!(icon_for_with(&f, true).glyph, "🎬", "extension: .{ext}");
        }
    }

    #[test]
    fn audio_extensions_map_to_musical_note() {
        for ext in ["mp3", "wav", "flac", "ogg", "m4a"] {
            let f = file(&format!("song.{ext}"));
            assert_eq!(icon_for_with(&f, true).glyph, "🎵", "extension: .{ext}");
        }
    }

    #[test]
    fn pdf_maps_to_book() {
        assert_eq!(icon_for_with(&file("resume.pdf"), true).glyph, "📕");
    }

    #[test]
    fn archive_extensions_map_to_compression() {
        for ext in ["zip", "tar", "gz", "bz2", "xz", "7z", "rar"] {
            let f = file(&format!("bundle.{ext}"));
            assert_eq!(icon_for_with(&f, true).glyph, "🗜", "extension: .{ext}");
        }
    }

    #[test]
    fn shell_scripts_map_to_play_triangle() {
        for ext in ["sh", "bash", "zsh", "fish"] {
            let f = file(&format!("run.{ext}"));
            assert_eq!(icon_for_with(&f, true).glyph, "▶", "extension: .{ext}");
        }
    }

    #[test]
    fn python_maps_to_snake() {
        assert_eq!(icon_for_with(&file("script.py"), true).glyph, "🐍");
    }

    #[test]
    fn javascript_typescript_map_to_blue_book() {
        for ext in ["js", "mjs", "cjs", "ts", "tsx", "jsx"] {
            let f = file(&format!("app.{ext}"));
            assert_eq!(icon_for_with(&f, true).glyph, "📘", "extension: .{ext}");
        }
    }

    #[test]
    fn web_assets_map_to_globe() {
        for ext in ["html", "htm", "css", "scss", "sass"] {
            let f = file(&format!("page.{ext}"));
            assert_eq!(icon_for_with(&f, true).glyph, "🌐", "extension: .{ext}");
        }
    }

    #[test]
    fn text_and_log_map_to_document() {
        assert_eq!(icon_for_with(&file("notes.txt"), true).glyph, "📄");
        assert_eq!(icon_for_with(&file("errors.log"), true).glyph, "📄");
    }

    #[test]
    fn go_maps_to_hamster() {
        assert_eq!(icon_for_with(&file("main.go"), true).glyph, "🐹");
    }

    #[test]
    fn unknown_extension_falls_back_to_dot() {
        assert_eq!(icon_for_with(&file("weird.xyz"), true).glyph, "·");
    }

    #[test]
    fn no_extension_falls_back_to_dot() {
        assert_eq!(icon_for_with(&file("Makefile"), true).glyph, "·");
    }

    #[test]
    fn uppercase_extension_still_matches_image() {
        // `Entry::extension()` already lowercases so `PHOTO.PNG` → "png"
        assert_eq!(icon_for_with(&file("PHOTO.PNG"), true).glyph, "🖼");
    }

    #[test]
    fn other_kind_maps_to_gear() {
        assert_eq!(icon_for_with(&other("device"), true).glyph, "⚙\u{fe0f}");
    }

    #[test]
    fn ascii_mode_swaps_all_glyphs() {
        assert_eq!(icon_for_with(&dir("src"), false).glyph, "[D]");
        assert_eq!(icon_for_with(&file("main.rs"), false).glyph, "[F]");
        assert_eq!(
            icon_for_with(&symlink("link", false, None), false).glyph,
            "[L]"
        );
        assert_eq!(
            icon_for_with(&symlink("dead", true, None), false).glyph,
            "[!]"
        );
        assert_eq!(icon_for_with(&executable("run.sh"), false).glyph, "[X]");
        assert_eq!(icon_for_with(&other("device"), false).glyph, "[?]");
    }

    #[test]
    fn descriptions_are_populated() {
        assert!(!icon_for_with(&dir("src"), true).description.is_empty());
        assert!(!icon_for_with(&file("main.rs"), true).description.is_empty());
        assert!(!icon_for_with(&symlink("l", false, None), true)
            .description
            .is_empty());
        assert!(!icon_for_with(&symlink("l", true, None), true)
            .description
            .is_empty());
        assert!(!icon_for_with(&executable("run"), true)
            .description
            .is_empty());
        assert!(!icon_for_with(&other("dev"), true).description.is_empty());
        assert!(!icon_for_with(&file("weird.xyz"), true)
            .description
            .is_empty());
    }

    #[test]
    fn global_toggle_default_is_emoji_on() {
        // Sanity: production entry point respects the global default (true).
        // We deliberately do NOT mutate the global here to keep this test
        // safe under parallel execution.
        assert!(use_emoji());
        assert_eq!(icon_for(&dir("src")).glyph, "📁");
    }
}
