//! Integration tests for `atlas-config`.
//!
//! Tests that mutate `ATLAS_CONFIG_DIR` are annotated with `#[serial]` to
//! prevent races with other parallel tests.

use std::path::PathBuf;
use std::time::Duration;

use atlas_config::*;
use serial_test::serial;

// ── Helpers ─────────────────────────────────────────────────────────────────

struct EnvGuard(&'static str);
impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::remove_var(self.0);
    }
}

fn set_config_dir(dir: &std::path::Path) -> EnvGuard {
    std::env::set_var("ATLAS_CONFIG_DIR", dir);
    EnvGuard("ATLAS_CONFIG_DIR")
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// `Config::default()` must survive a TOML round-trip without changing values.
#[test]
fn default_roundtrip() {
    let cfg = Config::default();
    let toml_str = toml::to_string(&cfg).expect("serialize");
    let restored: Config = toml::from_str(&toml_str).expect("deserialize");

    assert_eq!(cfg.ui.theme, restored.ui.theme);
    assert_eq!(cfg.ui.font_size, restored.ui.font_size);
    assert_eq!(cfg.ui.font_family, restored.ui.font_family);
    assert_eq!(cfg.view.default_mode, restored.view.default_mode);
    assert_eq!(
        cfg.navigation.history_size,
        restored.navigation.history_size
    );
    assert_eq!(cfg.indexer.max_memory_mb, restored.indexer.max_memory_mb);
    assert_eq!(
        cfg.search.fuzzy_max_results,
        restored.search.fuzzy_max_results
    );
    assert_eq!(
        cfg.thumbnails.cache_max_size_mb,
        restored.thumbnails.cache_max_size_mb
    );
    assert!(restored.bookmarks.is_empty());
}

/// A config file containing only a `[ui]` section must load without error;
/// all other sections fall back to their defaults.
#[test]
fn partial_config_ui_only() {
    let toml = r#"
[ui]
theme = "atlas-light"
font_size = 16.0
"#;
    let cfg = load_from_str(toml).expect("partial config should load");
    assert_eq!(cfg.ui.theme, "atlas-light");
    assert_eq!(cfg.ui.font_size, 16.0);
    // Other sections are defaults.
    assert_eq!(cfg.view.default_mode, ViewMode::Details);
    assert_eq!(cfg.navigation.history_size, 100);
    assert!(cfg.bookmarks.is_empty());
}

/// `load_from_str` must reject unknown top-level fields (deny_unknown_fields).
#[test]
fn unknown_fields_rejected() {
    let toml = r#"
[general]
confirm_on_quit = false
totally_bogus_key = "should fail"
"#;
    assert!(
        load_from_str(toml).is_err(),
        "expected an error for unknown fields"
    );
}

/// `load()` must always return `Ok`, even when the config file has unknown
/// fields that would cause `load_from_str` to fail.
#[test]
#[serial]
fn load_recovers_from_bad_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = set_config_dir(dir.path());

    let bad_toml = "[general]\nbogus = true\n";
    std::fs::write(dir.path().join("config.toml"), bad_toml).expect("write");

    let cfg = load().expect("load() should never fail");
    // Falls back to defaults.
    assert_eq!(cfg.ui.theme, Config::default().ui.theme);
}

/// Tilde in `bookmarks[].path` and `indexer.roots` must be expanded.
#[test]
fn tilde_expansion() {
    let toml = r#"
[indexer]
roots = ["~/projects", "/absolute/path"]

[[bookmarks]]
name = "Home"
path = "~"

[[bookmarks]]
name = "Docs"
path = "~/Documents"
"#;
    let cfg = load_from_str(toml).expect("load");

    for root in &cfg.indexer.roots {
        assert!(
            !root.starts_with("~"),
            "indexer root should have tilde expanded: {root:?}"
        );
    }
    for bm in &cfg.bookmarks {
        assert!(
            !bm.path.starts_with("~"),
            "bookmark path should have tilde expanded: {:?}",
            bm.path
        );
    }
}

/// `save_to_string` must preserve comments that exist in the on-disk file.
#[test]
#[serial]
fn save_preserves_comments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = set_config_dir(dir.path());

    // Write a config file with comments.
    let commented = r#"# My Atlas config

[ui]
# Preferred font
font_family = "Fira Code"
theme = "atlas-dark"
"#;
    std::fs::write(dir.path().join("config.toml"), commented).expect("write");

    let cfg = load_from_str(commented).expect("load");
    let saved = save_to_string(&cfg).expect("save_to_string");

    assert!(saved.contains("# My Atlas config"), "top comment preserved");
    assert!(
        saved.contains("# Preferred font"),
        "inline comment preserved"
    );
    assert!(saved.contains("Fira Code"), "value preserved");
}

/// `ATLAS_CONFIG_DIR` overrides the platform default path.
#[test]
#[serial]
fn atlas_config_dir_override() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = set_config_dir(dir.path());

    let resolved = config_dir().expect("config_dir");
    assert_eq!(
        resolved.canonicalize().unwrap_or(resolved.clone()),
        dir.path()
            .canonicalize()
            .unwrap_or(dir.path().to_path_buf())
    );

    // Writing and reading back through the override.
    let toml = "[ui]\ntheme = \"atlas-light\"\n";
    std::fs::write(dir.path().join("config.toml"), toml).expect("write");
    let cfg = load().expect("load");
    assert_eq!(cfg.ui.theme, "atlas-light");
}

/// `Density` must parse regardless of case.
#[test]
fn density_case_insensitive() {
    let cases = [
        (r#"density = "Compact""#, Density::Compact),
        (r#"density = "COMFORTABLE""#, Density::Comfortable),
        (r#"density = "spacious""#, Density::Spacious),
    ];
    for (fragment, expected) in cases {
        let toml = format!("[ui]\n{fragment}\n");
        let cfg = load_from_str(&toml).expect("load");
        assert_eq!(cfg.ui.density, expected, "fragment: {fragment}");
    }
}

/// `ViewMode` must parse regardless of case.
#[test]
fn view_mode_case_insensitive() {
    let cases = [
        (r#"default_mode = "Details""#, ViewMode::Details),
        (r#"default_mode = "GRID""#, ViewMode::Grid),
        (r#"default_mode = "gallery""#, ViewMode::Gallery),
        (r#"default_mode = "Miller""#, ViewMode::Miller),
        (r#"default_mode = "TREE""#, ViewMode::Tree),
    ];
    for (fragment, expected) in cases {
        let toml = format!("[view]\n{fragment}\n");
        let cfg = load_from_str(&toml).expect("load");
        assert_eq!(cfg.view.default_mode, expected, "fragment: {fragment}");
    }
}

/// The built-in `skeleton_toml()` string must parse successfully.
#[test]
fn skeleton_parses() {
    let cfg = load_from_str(skeleton_toml()).expect("skeleton_toml must parse");
    // Skeleton has the defaults.
    assert_eq!(cfg.ui.theme, "atlas-dark");
    assert_eq!(cfg.ui.font_size, 14.0);
    assert!(!cfg.view.show_hidden);
}

/// `indexer.roots` must be deduplicated.
#[test]
fn indexer_roots_deduplication() {
    let toml = r#"
[indexer]
roots = ["/foo", "/bar", "/foo", "/bar"]
"#;
    let cfg = load_from_str(toml).expect("load");
    assert_eq!(cfg.indexer.roots.len(), 2);
}

/// `font_size` is clamped when out of range.
#[test]
fn font_size_clamped() {
    let too_small = "[ui]\nfont_size = 1.0\n";
    let cfg = load_from_str(too_small).expect("load");
    assert!(cfg.ui.font_size >= 8.0);

    let too_large = "[ui]\nfont_size = 999.0\n";
    let cfg = load_from_str(too_large).expect("load");
    assert!(cfg.ui.font_size <= 72.0);
}

/// Optional path helpers return sensible values.
#[test]
#[serial]
fn path_helpers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = set_config_dir(dir.path());

    let cf = config_file_path().expect("config_file_path");
    assert_eq!(cf.file_name().and_then(|n| n.to_str()), Some("config.toml"));

    let kf = keymap_file_path().expect("keymap_file_path");
    // Phase 1 moved the keymap to keymaps/default.toml; assert the filename only.
    assert_eq!(
        kf.file_name().and_then(|n| n.to_str()),
        Some("default.toml")
    );

    let ensured = ensure_config_dir().expect("ensure_config_dir");
    assert!(ensured.is_dir());
}

/// Bookmarks serialise and de-serialise correctly.
#[test]
fn bookmarks_roundtrip() {
    let toml = r#"
[[bookmarks]]
name = "Src"
path = "/usr/src"

[[bookmarks]]
name = "Bin"
path = "/usr/bin"
"#;
    let cfg = load_from_str(toml).expect("load");
    assert_eq!(cfg.bookmarks.len(), 2);
    assert_eq!(cfg.bookmarks[0].name, "Src");
    assert_eq!(cfg.bookmarks[0].path, PathBuf::from("/usr/src"));
    assert_eq!(cfg.bookmarks[1].name, "Bin");
}

// ── Watcher ──────────────────────────────────────────────────────────────────

/// Write a file, observe `Reloaded`; inject a syntax error, observe
/// `LoadError`; verify the ArcSwap still holds the last good config.
#[test]
#[serial]
fn watcher_reload_and_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = set_config_dir(dir.path());

    let config_path = dir.path().join("config.toml");

    // Write an initial valid config.
    std::fs::write(
        &config_path,
        "[ui]\ntheme = \"atlas-light\"\nfont_size = 12.0\n",
    )
    .expect("write initial");

    let (watcher, arc, rx) = ConfigWatcher::start().expect("start watcher");

    // Give FSEvents / kqueue time to fully initialise before writing.
    std::thread::sleep(Duration::from_millis(500));

    // ── Change 1: valid update ────────────────────────────────────────────
    std::fs::write(
        &config_path,
        "[ui]\ntheme = \"atlas-dark\"\nfont_size = 18.0\n",
    )
    .expect("write update");

    let event = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("should receive event within 5 s");
    assert!(
        matches!(event, ConfigEvent::Reloaded),
        "expected Reloaded, got {event:?}"
    );
    assert_eq!(arc.load().ui.theme, "atlas-dark");
    assert_eq!(arc.load().ui.font_size, 18.0);

    // ── Change 2: syntax error ────────────────────────────────────────────
    std::fs::write(&config_path, "[[[ not valid toml\n").expect("write bad");

    let event = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("should receive error event within 5 s");
    assert!(
        matches!(event, ConfigEvent::LoadError(_)),
        "expected LoadError, got {event:?}"
    );
    // ArcSwap retains the last good value.
    assert_eq!(arc.load().ui.theme, "atlas-dark");

    watcher.stop();
}
