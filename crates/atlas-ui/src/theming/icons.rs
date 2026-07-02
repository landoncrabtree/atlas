//! Filetype icon glyph mapping (LSD-style, Nerd Font).
//!
//! Provides a single canonical function [`icon_for`] used by every view
//! (Details, Grid, Miller, Gallery) so a `.rs` file looks the same regardless
//! of which view is currently active. Every glyph is a single Unicode
//! scalar drawn from the Symbols Nerd Font Mono bundle
//! (`assets/fonts/SymbolsNerdFontMono-Regular.ttf`, registered in
//! `assets/ui/atlas.slint`). Slint views must bind
//! `font-family: Theme.icon-font-family` on the [`char`] label so the PUA
//! codepoints render as their intended Nerd Font glyphs instead of falling
//! back to the user's text font (which would render tofu).
//
// Icon map adapted from LSD (Apache-2.0) — see assets/fonts/LSD-LICENSE
// for the upstream Apache license text and
// <https://github.com/lsd-rs/lsd/blob/master/src/theme/icon.rs> for the
// full upstream mapping. Atlas ships a curated subset (~120 extensions
// + ~20 named files) covering the common cases.
//!
//! # Design
//!
//! - The mapping is a pure function of `(EntryKind, name, extension,
//!   permissions)`. No I/O, no allocations — safe to call on every
//!   visible row.
//! - Extension matching is case-insensitive because
//!   [`atlas_fs::Entry::extension()`] already lowercases.
//! - Symlink glyph handling matches the item-6 fix (`\u{f482}` healthy dir,
//!   `\u{f481}` healthy file, `\u{f00d7}` broken) — we do *not* recurse
//!   into the symlink target: a symlink stays a symlink at a glance.
//! - Executable detection uses the unix `x` permission bits when present;
//!   on Windows we rely on extension (`.exe`, `.bat`, `.cmd`) as a fallback.
//! - Named-file lookup (e.g. `Cargo.toml`, `Makefile`, `README`) runs
//!   *before* extension lookup so `Cargo.toml` gets the Rust icon
//!   even though the extension is `.toml`.

use atlas_fs::{Entry, EntryKind};

/// A single Nerd Font glyph paired with a short accessibility description.
///
/// `glyph` is a [`char`] (not `&str`) because every Nerd Font codepoint we
/// map to is a single Unicode scalar in the Private Use Area. Slint
/// consumes it via `SharedString::from(glyph.to_string())` at the view
/// controller boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconGlyph {
    /// The rendered glyph (a single Nerd Font PUA codepoint).
    pub glyph: char,
    /// Short human-readable description for a11y/tooltips
    /// (e.g. `"Rust source"`, `"folder"`).
    pub description: &'static str,
}

impl IconGlyph {
    const fn new(glyph: char, description: &'static str) -> Self {
        Self { glyph, description }
    }
}

// ── Kind glyphs (LSD default set) ──────────────────────────────────────────
//
// These are the kind-level fallbacks used when nothing more specific
// matches. Codepoints are copied from LSD's `IconTheme::unicode()` —
// see the LSD upstream link at the top of this file.

const KIND_DIR: IconGlyph = IconGlyph::new('\u{f115}', "folder"); //
const KIND_FILE: IconGlyph = IconGlyph::new('\u{f016}', "file"); //
const KIND_EXEC: IconGlyph = IconGlyph::new('\u{f489}', "executable"); //
const KIND_SYMLINK_FILE: IconGlyph = IconGlyph::new('\u{f481}', "symbolic link"); //
const KIND_BROKEN_SYMLINK: IconGlyph = IconGlyph::new('\u{f00d7}', "broken symlink"); // 󰃗
const KIND_OTHER: IconGlyph = IconGlyph::new('\u{f2dc}', "special file"); //

// ── Public API ─────────────────────────────────────────────────────────────

/// Return the Nerd Font icon glyph for the given filesystem entry.
///
/// Resolution order (first match wins):
///
/// 1. `EntryKind::Dir` → folder glyph.
/// 2. `EntryKind::Symlink` → healthy or broken-symlink glyph
///    (directory-symlink target-kind is *not* considered — the symlink
///    nature always wins at a glance).
/// 3. `EntryKind::Other` → special-file glyph.
/// 4. `EntryKind::File`, executable bit set → executable glyph. This
///    beats the extension mapping so a `chmod +x`'d `build.rs` looks
///    like a runnable script, not a Rust source file.
/// 5. `EntryKind::File`, name matches a well-known filename
///    (`Cargo.toml`, `Makefile`, `.gitignore`, …) → the named-file
///    glyph. This runs before extension lookup so `Cargo.toml` gets
///    the Rust crate glyph, not the generic TOML glyph.
/// 6. `EntryKind::File`, extension matches the LSD-style map → per-ext
///    glyph.
/// 7. Otherwise → generic file glyph.
#[must_use]
pub fn icon_for(entry: &Entry) -> IconGlyph {
    match &entry.kind {
        EntryKind::Dir => KIND_DIR,
        EntryKind::Symlink { broken: true, .. } => KIND_BROKEN_SYMLINK,
        EntryKind::Symlink { .. } => KIND_SYMLINK_FILE,
        EntryKind::Other => KIND_OTHER,
        EntryKind::File => {
            if is_executable(entry) {
                return KIND_EXEC;
            }
            if let Some(icon) = icon_for_name(&entry.name) {
                return icon;
            }
            if let Some(ext) = entry.extension() {
                if let Some(icon) = icon_for_extension(&ext) {
                    return icon;
                }
            }
            KIND_FILE
        }
    }
}

/// Look up a glyph by the entry's exact file name (case-insensitive).
///
/// Used for files whose meaning is defined by the *name* rather than the
/// extension (`Cargo.toml`, `Makefile`, `README`, `LICENSE`, dotfiles).
/// Returns [`None`] when the name isn't in the well-known set so the
/// caller can fall through to the extension map.
#[must_use]
pub fn icon_for_name(name: &str) -> Option<IconGlyph> {
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        // Build systems and package manifests
        "cargo.toml" | "cargo.lock" => IconGlyph::new('\u{e7a8}', "Rust manifest"), //
        "package.json" | "package-lock.json" => {
            IconGlyph::new('\u{e718}', "Node.js manifest") //
        }
        "yarn.lock" => IconGlyph::new('\u{e6a7}', "Yarn lockfile"), //
        "pnpm-lock.yaml" => IconGlyph::new('\u{f0089}', "pnpm lockfile"), // 󰂉
        "pyproject.toml" | "poetry.lock" | "pipfile" | "pipfile.lock" => {
            IconGlyph::new('\u{e73c}', "Python project") //
        }
        "requirements.txt" => IconGlyph::new('\u{e73c}', "Python requirements"), //
        "gemfile" | "gemfile.lock" => IconGlyph::new('\u{e21e}', "Ruby manifest"), //
        "go.mod" | "go.sum" => IconGlyph::new('\u{e627}', "Go module"),          //
        "makefile" | "gnumakefile" => IconGlyph::new('\u{f0229}', "Makefile"),   // 󰈩
        "justfile" | ".justfile" => IconGlyph::new('\u{f0229}', "Justfile"),     // 󰈩
        "cmakelists.txt" => IconGlyph::new('\u{e794}', "CMake project"),         //
        "dockerfile"
        | "containerfile"
        | ".dockerignore"
        | "docker-compose.yml"
        | "docker-compose.yaml"
        | "compose.yml"
        | "compose.yaml" => {
            IconGlyph::new('\u{f308}', "Docker file") //
        }
        // Version control
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitkeep" | ".mailmap" => {
            IconGlyph::new('\u{e65d}', "Git config") //
        }
        // Documentation / project meta
        "readme" | "readme.md" | "readme.txt" | "readme.rst" => {
            IconGlyph::new('\u{f00ba}', "readme") // 󰂺
        }
        "license" | "license.md" | "license.txt" | "licence" | "copying" => {
            IconGlyph::new('\u{f0219}', "license") // 󰈙
        }
        "changelog" | "changelog.md" | "changes" | "changes.md" => {
            IconGlyph::new('\u{f0219}', "changelog") // 󰈙
        }
        "authors" | "contributors" | "notice" => IconGlyph::new('\u{f0004}', "credits"), // 󰀄
        // Shell dotfiles
        ".bashrc" | ".bash_profile" | ".bash_history" | ".bash_logout" | ".zshrc" | ".zshenv"
        | ".zprofile" | ".zsh_history" | ".profile" | ".inputrc" => {
            IconGlyph::new('\u{f1183}', "shell rc") // 󱆃
        }
        ".vimrc" | ".gvimrc" | ".vim" => IconGlyph::new('\u{e62b}', "vim config"), //
        // Environment files: `.env` has no extension per Rust's Path
        // semantics (leading-dot files aren't parsed for one), so must
        // live in the named-file map. `.env.local`/`.env.production`
        // etc. still fall through to the extension map below.
        ".env" | ".envrc" => IconGlyph::new('\u{f462}', "environment file"), // 
        // Editor dotfiles
        ".editorconfig" => IconGlyph::new('\u{e615}', "editor config"), // 
        _ => return None,
    })
}

/// Look up a glyph by extension (must be already lowercased, as returned
/// by [`atlas_fs::Entry::extension()`]).
///
/// Curated subset of LSD's `get_default_icons_by_extension()` covering
/// the ~120 most common extensions. Returns [`None`] for unmapped
/// extensions so the caller can fall through to the generic file glyph.
#[must_use]
pub fn icon_for_extension(ext: &str) -> Option<IconGlyph> {
    Some(match ext {
        // ── Source: Rust / systems ──────────────────────────────────
        "rs" => IconGlyph::new('\u{e7a8}', "Rust source"), //
        "c" | "h" => IconGlyph::new('\u{e61e}', "C source"), //
        "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" => {
            IconGlyph::new('\u{e61d}', "C++ source") //
        }
        "cs" | "csx" | "csproj" => IconGlyph::new('\u{f031b}', "C# source"), // 󰌛
        "go" => IconGlyph::new('\u{e627}', "Go source"),                     //
        "zig" => IconGlyph::new('\u{e6a9}', "Zig source"),                   //
        "swift" => IconGlyph::new('\u{e755}', "Swift source"),               //
        "kt" | "kts" => IconGlyph::new('\u{e634}', "Kotlin source"),         //
        "java" | "jar" => IconGlyph::new('\u{e738}', "Java source"),         //
        "scala" | "sc" => IconGlyph::new('\u{e737}', "Scala source"),        //
        "clj" | "cljs" | "cljc" | "edn" => IconGlyph::new('\u{e768}', "Clojure source"), //
        "hs" | "lhs" => IconGlyph::new('\u{e777}', "Haskell source"),        //
        "erl" | "hrl" => IconGlyph::new('\u{e7b1}', "Erlang source"),        //
        "ex" | "exs" => IconGlyph::new('\u{e62d}', "Elixir source"),         //
        "ml" | "mli" => IconGlyph::new('\u{e67a}', "OCaml source"),          //
        "fs" | "fsi" | "fsx" | "fsscript" => IconGlyph::new('\u{e7a7}', "F# source"), //

        // ── Source: scripting ──────────────────────────────────────
        "py" | "pyc" | "pyo" | "pyd" | "pyw" | "pyi" | "pyx" | "pxd" => {
            IconGlyph::new('\u{e73c}', "Python source") //
        }
        "rb" | "erb" | "rake" | "gemspec" => {
            IconGlyph::new('\u{e21e}', "Ruby source") //
        }
        "php" | "phtml" => IconGlyph::new('\u{e73d}', "PHP source"), //
        "pl" | "pm" | "t" | "pod" => IconGlyph::new('\u{e769}', "Perl source"), //
        "lua" => IconGlyph::new('\u{e620}', "Lua source"),           //
        "r" | "rmd" => IconGlyph::new('\u{f07d4}', "R source"),      // 󰟔
        "jl" => IconGlyph::new('\u{e624}', "Julia source"),          //
        "dart" => IconGlyph::new('\u{e798}', "Dart source"),         //
        "nim" | "nims" => IconGlyph::new('\u{e677}', "Nim source"),  //
        "cr" => IconGlyph::new('\u{e62f}', "Crystal source"),        //

        // ── Web ────────────────────────────────────────────────────
        "js" | "cjs" | "mjs" => IconGlyph::new('\u{e74e}', "JavaScript source"), //
        "ts" | "cts" | "mts" => IconGlyph::new('\u{e628}', "TypeScript source"), //
        "jsx" => IconGlyph::new('\u{e7ba}', "React JSX"),                        //
        "tsx" => IconGlyph::new('\u{e7ba}', "React TSX"),                        //
        "vue" => IconGlyph::new('\u{f0844}', "Vue.js component"),                // 󰡄
        "svelte" => IconGlyph::new('\u{e697}', "Svelte component"),              //
        "html" | "htm" | "xhtml" => IconGlyph::new('\u{f13b}', "HTML document"), //
        "css" => IconGlyph::new('\u{e749}', "CSS stylesheet"),                   //
        "scss" | "sass" => IconGlyph::new('\u{e603}', "Sass stylesheet"),        //
        "less" => IconGlyph::new('\u{e60b}', "Less stylesheet"),                 //
        "styl" | "stylus" => IconGlyph::new('\u{e600}', "Stylus stylesheet"),    //
        "wasm" => IconGlyph::new('\u{e6a1}', "WebAssembly module"),              //

        // ── Shell / scripts ────────────────────────────────────────
        "sh" | "bash" | "zsh" | "fish" | "ksh" | "csh" | "tcsh" | "ash" | "awk" | "sed" => {
            IconGlyph::new('\u{f489}', "shell script") //
        }
        "bat" | "cmd" | "ps1" | "psm1" | "psd1" => {
            IconGlyph::new('\u{f17a}', "Windows script") //
        }
        "exe" | "dll" | "msi" => IconGlyph::new('\u{f17a}', "Windows executable"), //

        // ── Data / config ──────────────────────────────────────────
        "json" | "jsonc" | "json5" | "geojson" => {
            IconGlyph::new('\u{e60b}', "JSON data") //
        }
        "yaml" | "yml" => IconGlyph::new('\u{e6a8}', "YAML data"), //
        "toml" => IconGlyph::new('\u{e6b2}', "TOML config"),       //
        "xml" | "plist" | "xsd" | "xsl" | "xslt" => {
            IconGlyph::new('\u{f05c0}', "XML document") // 󰗀
        }
        "csv" | "tsv" => IconGlyph::new('\u{f1c3}', "spreadsheet data"), //
        "sql" | "psql" | "mysql" | "sqlite" | "sqlite3" | "db" | "sqlitedb" => {
            IconGlyph::new('\u{f1c0}', "database") //
        }
        "env" => IconGlyph::new('\u{f462}', "environment file"), //
        "ini" | "cfg" | "conf" | "config" | "properties" => {
            IconGlyph::new('\u{e615}', "config file") //
        }
        "log" => IconGlyph::new('\u{f18d}', "log file"), //

        // ── Documents ──────────────────────────────────────────────
        "md" | "markdown" | "mdown" | "mkd" | "mkdown" => {
            IconGlyph::new('\u{e73e}', "Markdown document") //
        }
        "rst" => IconGlyph::new('\u{e73e}', "reStructuredText document"), //
        "tex" | "sty" | "cls" | "bib" => {
            IconGlyph::new('\u{e69b}', "LaTeX document") //
        }
        "txt" | "text" => IconGlyph::new('\u{f0219}', "text document"), // 󰈙
        "pdf" => IconGlyph::new('\u{f1c1}', "PDF document"),            //
        "doc" | "docx" | "odt" | "rtf" => {
            IconGlyph::new('\u{f1c2}', "Word document") //
        }
        "xls" | "xlsx" | "ods" => IconGlyph::new('\u{f1c3}', "spreadsheet"), //
        "ppt" | "pptx" | "odp" => IconGlyph::new('\u{f1c4}', "presentation"), //
        "epub" | "mobi" | "azw3" | "fb2" => IconGlyph::new('\u{e28b}', "e-book"), //

        // ── Images ─────────────────────────────────────────────────
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "avif"
        | "heic" | "heif" | "jxl" | "apng" => IconGlyph::new('\u{f1c5}', "image"), //
        "svg" => IconGlyph::new('\u{f0721}', "SVG image"), // 󰜡
        "psd" => IconGlyph::new('\u{e7b8}', "Photoshop document"), //
        "ai" => IconGlyph::new('\u{e7b4}', "Illustrator document"), //
        "xcf" => IconGlyph::new('\u{f0338}', "GIMP image"), // 󰌸

        // ── Video ──────────────────────────────────────────────────
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "flv" | "wmv" | "m4v" | "mpg" | "mpeg" | "ogv"
        | "3gp" => IconGlyph::new('\u{f03d}', "video"), //
        "srt" | "sub" | "vtt" | "ass" => IconGlyph::new('\u{f020}', "subtitle"), //

        // ── Audio ──────────────────────────────────────────────────
        "mp3" | "wav" | "flac" | "ogg" | "oga" | "m4a" | "aac" | "wma" | "opus" | "aiff"
        | "ape" | "alac" => IconGlyph::new('\u{f001}', "audio"), //

        // ── Archives ───────────────────────────────────────────────
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "tbz" | "tbz2" | "xz" | "txz" | "lz" | "lz4"
        | "lzma" | "zst" | "zstd" | "7z" | "rar" | "iso" | "dmg" | "img" | "cab" | "ar" | "arj" => {
            IconGlyph::new('\u{f410}', "archive")
        } //
        "deb" => IconGlyph::new('\u{f187}', "Debian package"), //
        "rpm" => IconGlyph::new('\u{f17c}', "RPM package"),    //
        "apk" => IconGlyph::new('\u{e70e}', "Android package"), //

        // ── Notebook / IDE ─────────────────────────────────────────
        "ipynb" => IconGlyph::new('\u{e678}', "Jupyter notebook"), //
        "diff" | "patch" => IconGlyph::new('\u{e728}', "diff/patch"), //

        // ── Fonts ──────────────────────────────────────────────────
        "ttf" | "otf" | "woff" | "woff2" | "eot" | "pfa" | "pfb" => {
            IconGlyph::new('\u{f031}', "font file") //
        }

        // ── Certificates / keys ────────────────────────────────────
        "pem" | "crt" | "cer" | "der" | "key" | "pfx" | "p12" | "csr" => {
            IconGlyph::new('\u{f0306}', "certificate / key") // 󰌆
        }

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
        assert_eq!(icon_for(&dir("src")).glyph, '\u{f115}');
    }

    #[test]
    fn healthy_symlink_maps_to_symlink_glyph() {
        assert_eq!(icon_for(&symlink("link", false, None)).glyph, '\u{f481}');
    }

    #[test]
    fn broken_symlink_maps_to_broken_glyph() {
        assert_eq!(icon_for(&symlink("dead", true, None)).glyph, '\u{f00d7}');
    }

    #[test]
    fn symlink_to_dir_keeps_symlink_glyph_does_not_recurse() {
        let link = symlink("dir-link", false, Some(EntryKind::Dir));
        assert_eq!(icon_for(&link).glyph, '\u{f481}');
    }

    #[test]
    fn other_kind_maps_to_special_file_glyph() {
        assert_eq!(icon_for(&other("device")).glyph, '\u{f2dc}');
    }

    #[test]
    fn executable_bit_beats_extension() {
        // `.rs` normally → Rust crab but an executable `.rs` → exec glyph
        assert_eq!(icon_for(&executable("build.rs")).glyph, '\u{f489}');
    }

    #[test]
    fn unknown_extension_falls_back_to_file() {
        assert_eq!(icon_for(&file("weird.xyz")).glyph, '\u{f016}');
    }

    #[test]
    fn no_extension_and_no_named_match_falls_back_to_file() {
        assert_eq!(icon_for(&file("weird")).glyph, '\u{f016}');
    }

    #[test]
    fn uppercase_extension_still_matches() {
        // `Entry::extension()` already lowercases so `PHOTO.PNG` → "png"
        assert_eq!(icon_for(&file("PHOTO.PNG")).glyph, '\u{f1c5}');
    }

    #[test]
    fn descriptions_are_populated() {
        assert!(!icon_for(&dir("src")).description.is_empty());
        assert!(!icon_for(&file("main.rs")).description.is_empty());
        assert!(!icon_for(&file("weird.xyz")).description.is_empty());
        assert!(!icon_for(&symlink("l", false, None)).description.is_empty());
        assert!(!icon_for(&symlink("l", true, None)).description.is_empty());
        assert!(!icon_for(&executable("run")).description.is_empty());
        assert!(!icon_for(&other("dev")).description.is_empty());
    }

    // ── Named-file lookups ──────────────────────────────────────────

    #[test]
    fn cargo_toml_uses_rust_named_glyph_not_toml() {
        // Named-file must take precedence over extension.
        assert_eq!(icon_for(&file("Cargo.toml")).glyph, '\u{e7a8}');
        assert_eq!(icon_for(&file("Cargo.lock")).glyph, '\u{e7a8}');
    }

    #[test]
    fn package_json_uses_node_named_glyph() {
        assert_eq!(icon_for(&file("package.json")).glyph, '\u{e718}');
        assert_eq!(icon_for(&file("package-lock.json")).glyph, '\u{e718}');
    }

    #[test]
    fn makefile_uses_named_glyph() {
        assert_eq!(icon_for(&file("Makefile")).glyph, '\u{f0229}');
        assert_eq!(icon_for(&file("GNUmakefile")).glyph, '\u{f0229}');
    }

    #[test]
    fn justfile_uses_makefile_glyph() {
        assert_eq!(icon_for(&file("justfile")).glyph, '\u{f0229}');
        assert_eq!(icon_for(&file(".justfile")).glyph, '\u{f0229}');
    }

    #[test]
    fn readme_and_license_use_named_glyphs() {
        assert_eq!(icon_for(&file("README")).glyph, '\u{f00ba}');
        assert_eq!(icon_for(&file("README.md")).glyph, '\u{f00ba}');
        assert_eq!(icon_for(&file("LICENSE")).glyph, '\u{f0219}');
        assert_eq!(icon_for(&file("LICENSE.txt")).glyph, '\u{f0219}');
    }

    #[test]
    fn gitignore_uses_git_glyph_not_generic() {
        assert_eq!(icon_for(&file(".gitignore")).glyph, '\u{e65d}');
        assert_eq!(icon_for(&file(".gitattributes")).glyph, '\u{e65d}');
    }

    #[test]
    fn dockerfile_uses_docker_glyph() {
        assert_eq!(icon_for(&file("Dockerfile")).glyph, '\u{f308}');
        assert_eq!(icon_for(&file("docker-compose.yml")).glyph, '\u{f308}');
    }

    #[test]
    fn shell_dotfiles_share_glyph() {
        let bashrc = icon_for(&file(".bashrc")).glyph;
        let zshrc = icon_for(&file(".zshrc")).glyph;
        assert_eq!(bashrc, zshrc);
        assert_eq!(bashrc, '\u{f1183}');
    }

    // ── Extension coverage ─────────────────────────────────────────

    #[test]
    fn rust_extension_maps_to_rust_glyph() {
        assert_eq!(icon_for(&file("main.rs")).glyph, '\u{e7a8}');
    }

    #[test]
    fn c_and_cpp_have_distinct_glyphs() {
        assert_eq!(icon_for(&file("main.c")).glyph, '\u{e61e}');
        assert_eq!(icon_for(&file("main.h")).glyph, '\u{e61e}');
        assert_eq!(icon_for(&file("main.cpp")).glyph, '\u{e61d}');
        assert_eq!(icon_for(&file("main.hpp")).glyph, '\u{e61d}');
    }

    #[test]
    fn go_and_python_and_ruby_map_correctly() {
        assert_eq!(icon_for(&file("main.go")).glyph, '\u{e627}');
        assert_eq!(icon_for(&file("script.py")).glyph, '\u{e73c}');
        assert_eq!(icon_for(&file("app.rb")).glyph, '\u{e21e}');
    }

    #[test]
    fn java_kotlin_scala_swift() {
        assert_eq!(icon_for(&file("Main.java")).glyph, '\u{e738}');
        assert_eq!(icon_for(&file("App.kt")).glyph, '\u{e634}');
        assert_eq!(icon_for(&file("App.scala")).glyph, '\u{e737}');
        assert_eq!(icon_for(&file("App.swift")).glyph, '\u{e755}');
    }

    #[test]
    fn javascript_and_typescript() {
        assert_eq!(icon_for(&file("app.js")).glyph, '\u{e74e}');
        assert_eq!(icon_for(&file("app.mjs")).glyph, '\u{e74e}');
        assert_eq!(icon_for(&file("app.cjs")).glyph, '\u{e74e}');
        assert_eq!(icon_for(&file("app.ts")).glyph, '\u{e628}');
        assert_eq!(icon_for(&file("app.jsx")).glyph, '\u{e7ba}');
        assert_eq!(icon_for(&file("app.tsx")).glyph, '\u{e7ba}');
    }

    #[test]
    fn html_css_and_sass() {
        assert_eq!(icon_for(&file("page.html")).glyph, '\u{f13b}');
        assert_eq!(icon_for(&file("site.css")).glyph, '\u{e749}');
        assert_eq!(icon_for(&file("site.scss")).glyph, '\u{e603}');
    }

    #[test]
    fn config_data_formats() {
        assert_eq!(icon_for(&file("data.json")).glyph, '\u{e60b}');
        assert_eq!(icon_for(&file("data.yaml")).glyph, '\u{e6a8}');
        assert_eq!(icon_for(&file("data.yml")).glyph, '\u{e6a8}');
        assert_eq!(icon_for(&file("data.toml")).glyph, '\u{e6b2}');
        assert_eq!(icon_for(&file("data.xml")).glyph, '\u{f05c0}');
        assert_eq!(icon_for(&file("data.csv")).glyph, '\u{f1c3}');
    }

    #[test]
    fn env_and_ini_and_conf() {
        assert_eq!(icon_for(&file(".env")).glyph, '\u{f462}');
        assert_eq!(icon_for(&file("app.ini")).glyph, '\u{e615}');
        assert_eq!(icon_for(&file("app.conf")).glyph, '\u{e615}');
    }

    #[test]
    fn docs_markdown_and_text_and_pdf() {
        assert_eq!(icon_for(&file("notes.md")).glyph, '\u{e73e}');
        assert_eq!(icon_for(&file("notes.markdown")).glyph, '\u{e73e}');
        assert_eq!(icon_for(&file("notes.rst")).glyph, '\u{e73e}');
        assert_eq!(icon_for(&file("notes.txt")).glyph, '\u{f0219}');
        assert_eq!(icon_for(&file("paper.tex")).glyph, '\u{e69b}');
        assert_eq!(icon_for(&file("resume.pdf")).glyph, '\u{f1c1}');
    }

    #[test]
    fn image_extensions_share_image_glyph() {
        for ext in [
            "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff", "avif", "heic",
        ] {
            let f = file(&format!("photo.{ext}"));
            assert_eq!(icon_for(&f).glyph, '\u{f1c5}', "ext: .{ext}");
        }
    }

    #[test]
    fn svg_uses_dedicated_glyph_not_generic_image() {
        assert_ne!(icon_for(&file("icon.svg")).glyph, '\u{f1c5}');
        assert_eq!(icon_for(&file("icon.svg")).glyph, '\u{f0721}');
    }

    #[test]
    fn video_extensions_share_video_glyph() {
        for ext in ["mp4", "mov", "mkv", "avi", "webm", "flv", "wmv", "m4v"] {
            let f = file(&format!("movie.{ext}"));
            assert_eq!(icon_for(&f).glyph, '\u{f03d}', "ext: .{ext}");
        }
    }

    #[test]
    fn audio_extensions_share_audio_glyph() {
        for ext in ["mp3", "wav", "flac", "ogg", "m4a", "aac", "opus"] {
            let f = file(&format!("song.{ext}"));
            assert_eq!(icon_for(&f).glyph, '\u{f001}', "ext: .{ext}");
        }
    }

    #[test]
    fn archive_extensions_share_archive_glyph() {
        for ext in ["zip", "tar", "gz", "bz2", "xz", "7z", "rar", "zst"] {
            let f = file(&format!("bundle.{ext}"));
            assert_eq!(icon_for(&f).glyph, '\u{f410}', "ext: .{ext}");
        }
    }

    #[test]
    fn deb_rpm_apk_have_dedicated_glyphs() {
        assert_eq!(icon_for(&file("pkg.deb")).glyph, '\u{f187}');
        assert_eq!(icon_for(&file("pkg.rpm")).glyph, '\u{f17c}');
        assert_eq!(icon_for(&file("app.apk")).glyph, '\u{e70e}');
    }

    #[test]
    fn shell_scripts_share_glyph() {
        for ext in ["sh", "bash", "zsh", "fish", "ksh", "awk"] {
            let f = file(&format!("run.{ext}"));
            assert_eq!(icon_for(&f).glyph, '\u{f489}', "ext: .{ext}");
        }
    }

    #[test]
    fn windows_scripts_share_glyph() {
        // .exe/.bat/.cmd still trip the executable_bit_beats_extension
        // fallback only when the file has +x perms. Bare .ps1 uses the
        // Windows-script glyph.
        assert_eq!(icon_for(&file("script.ps1")).glyph, '\u{f17a}');
    }

    #[test]
    fn database_extensions_share_db_glyph() {
        for ext in ["sql", "db", "sqlite", "sqlite3"] {
            let f = file(&format!("data.{ext}"));
            assert_eq!(icon_for(&f).glyph, '\u{f1c0}', "ext: .{ext}");
        }
    }

    #[test]
    fn font_extensions_share_font_glyph() {
        for ext in ["ttf", "otf", "woff", "woff2", "eot"] {
            let f = file(&format!("font.{ext}"));
            assert_eq!(icon_for(&f).glyph, '\u{f031}', "ext: .{ext}");
        }
    }

    #[test]
    fn notebook_and_diff() {
        assert_eq!(icon_for(&file("analysis.ipynb")).glyph, '\u{e678}');
        assert_eq!(icon_for(&file("change.diff")).glyph, '\u{e728}');
        assert_eq!(icon_for(&file("bug.patch")).glyph, '\u{e728}');
    }

    #[test]
    fn certificates_and_keys_share_glyph() {
        for ext in ["pem", "crt", "cer", "key", "pfx"] {
            let f = file(&format!("id.{ext}"));
            assert_eq!(icon_for(&f).glyph, '\u{f0306}', "ext: .{ext}");
        }
    }

    #[test]
    fn every_glyph_is_a_pua_codepoint() {
        // Sanity: verify each returned glyph lives in the Nerd Font
        // Private Use Area ranges (Nerd Font uses U+E000..U+F8FF for the
        // BMP PUA and U+F0000..U+FFFFD for the SMP PUA). Text-font
        // fallback would render tofu for these, which is why we bind
        // `font-family: Theme.icon-font-family` in every view.
        fn is_pua(c: char) -> bool {
            let v = c as u32;
            (0xE000..=0xF8FF).contains(&v) || (0xF0000..=0xFFFFD).contains(&v)
        }
        for entry in [
            dir("d"),
            file("main.rs"),
            file("main.c"),
            file("main.cpp"),
            file("main.go"),
            file("script.py"),
            file("app.js"),
            file("app.ts"),
            file("data.json"),
            file("Cargo.toml"),
            file("README.md"),
            file("photo.png"),
            file("song.mp3"),
            file("bundle.zip"),
            symlink("l", false, None),
            symlink("l", true, None),
            other("dev"),
            executable("run"),
        ] {
            let g = icon_for(&entry).glyph;
            assert!(
                is_pua(g),
                "glyph {:?} (U+{:X}) not in Nerd Font PUA",
                g,
                g as u32
            );
        }
    }
}
