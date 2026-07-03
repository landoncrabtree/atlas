//! TOML serialization and deserialization of user keymaps.
//!
//! # Schema
//!
//! ```toml
//! [[bindings]]
//! context = "Pane"
//! key = "cmd-p"
//! action = "command_palette::Toggle"
//!
//! # Suppress a default binding:
//! [[bindings]]
//! context = "Pane"
//! key = "j"
//! action = ""
//! ```

use std::path::Path;

use atlas_core::Result;
use serde::{Deserialize, Serialize};

use crate::{ActionId, Binding, ChordSequence};

#[derive(Debug, Deserialize, Serialize)]
struct BindingEntry {
    context: String,
    key: String,
    action: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct KeymapFile {
    #[serde(default)]
    bindings: Vec<BindingEntry>,
}

/// Parse a TOML keymap file and return validated [`Binding`]s.
///
/// - An empty `action` string produces a suppression binding ([`ActionId::null()`]).
/// - Returns a precise error (with TOML span info when available) on invalid input.
pub fn load_keymap_toml(toml_text: &str) -> Result<Vec<Binding>> {
    let file: KeymapFile = toml::from_str(toml_text)
        .map_err(|error| anyhow::anyhow!("keymap TOML parse error: {error}"))?;

    let mut bindings = Vec::with_capacity(file.bindings.len());
    for entry in file.bindings {
        let sequence = ChordSequence::from_str(&entry.key)
            .map_err(|error| anyhow::anyhow!("invalid key {:?}: {error}", entry.key))?;
        let action = if entry.action.is_empty() {
            ActionId::null()
        } else {
            ActionId::new(entry.action)
        };
        bindings.push(Binding {
            sequence,
            context: entry.context,
            action,
        });
    }
    Ok(bindings)
}

/// Serialize a slice of bindings into a TOML keymap string.
///
/// The output is suitable for writing to `~/.config/atlas/keymap.toml` and
/// round-trips through [`load_keymap_toml`].
pub fn save_keymap_toml(bindings: &[Binding]) -> Result<String> {
    let entries: Vec<BindingEntry> = bindings
        .iter()
        .map(|binding| BindingEntry {
            context: binding.context.clone(),
            key: binding.sequence.display(),
            action: binding.action.0.clone(),
        })
        .collect();
    let file = KeymapFile { bindings: entries };
    toml::to_string_pretty(&file)
        .map_err(|error| anyhow::anyhow!("keymap TOML serialization error: {error}").into())
}

/// Render all default bindings as a heavily-commented TOML string tailored
/// to the current platform.
///
/// The output matches the schema accepted by [`load_keymap_toml`] and serves
/// as documentation for users who want to customise their keymap.  Every entry
/// is annotated with its human-readable action title.
///
/// The string is also the authoritative content written by
/// [`write_default_keymap_to`] and checked in at
/// `assets/keymaps/default.<platform>.toml`.
pub fn default_keymap_toml_string() -> String {
    default_keymap_toml_string_for(crate::PrettyPlatform::current())
}

/// Same as [`default_keymap_toml_string`] but for an explicit platform.
///
/// Called by [`write_default_keymap_to`] at first launch so users get an
/// idiomatic macOS / Windows / Linux keymap out of the box, and by tests
/// to compare against the checked-in reference files.
pub fn default_keymap_toml_string_for(platform: crate::PrettyPlatform) -> String {
    use crate::defaults::{default_actions, default_bindings_for};
    use std::collections::HashMap;

    let actions = default_actions();
    let title_map: HashMap<&str, &str> = actions
        .iter()
        .map(|a| (a.id.as_str(), a.title.as_str()))
        .collect();

    let platform_label = match platform {
        crate::PrettyPlatform::Mac => "macOS",
        crate::PrettyPlatform::Windows => "Windows",
        crate::PrettyPlatform::Linux => "Linux",
    };

    let mut out = String::with_capacity(4096);
    out.push_str(&format!("# Atlas default keymap ({platform_label}).\n"));
    out.push_str("# Copy this file to override defaults; add new [[bindings]] entries or\n");
    out.push_str("# suppress a default by setting its `action` to an empty string.\n");
    out.push_str("#\n");
    out.push_str("# Modifier aliases: cmd|meta|super|win, alt|option|opt, ctrl|control, shift.\n");
    out.push_str("# The keymap is literal — a binding for `cmd-c` requires the physical\n");
    out.push_str("# Cmd key on macOS or physical Super/Meta on Linux/Windows. If you want\n");
    out.push_str("# the same shortcut on every platform, use `ctrl-*`.\n");

    for binding in &default_bindings_for(platform) {
        out.push('\n');
        let title = title_map
            .get(binding.action.as_str())
            .copied()
            .unwrap_or(binding.action.as_str());
        out.push_str(&format!("# [{}] {}\n", binding.context, title));
        out.push_str("[[bindings]]\n");
        out.push_str(&format!("context = {:?}\n", binding.context));
        out.push_str(&format!("key = {:?}\n", binding.sequence.display()));
        out.push_str(&format!("action = {:?}\n", binding.action.as_str()));
    }

    out
}

/// Write the resolved default keymap to `path` as a heavily-commented TOML
/// file.
///
/// The file is suitable for users to copy and customise.  If the parent
/// directory does not exist it is created automatically.
///
/// # Errors
///
/// Returns an [`std::io::Error`] if the parent directory cannot be created or
/// the file cannot be written.
pub fn write_default_keymap_to(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = default_keymap_toml_string();
    std::fs::write(path, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[[bindings]]
context = "Pane"
key = "cmd-p"
action = "command_palette::Toggle"

[[bindings]]
context = "Global"
key = "f5"
action = "fs::Copy"

[[bindings]]
context = "Pane"
key = "j"
action = ""
"#;

    #[test]
    fn test_load_basic() {
        let bindings = load_keymap_toml(SAMPLE).unwrap();
        assert_eq!(bindings.len(), 3);
        assert_eq!(bindings[0].action, ActionId::new("command_palette::Toggle"));
        assert_eq!(bindings[1].action, ActionId::new("fs::Copy"));
        assert!(bindings[2].is_suppression());
    }

    #[test]
    fn test_load_invalid_key() {
        let bad = r#"
[[bindings]]
context = "Pane"
key = "cmd-blorp"
action = "foo::Bar"
"#;
        assert!(load_keymap_toml(bad).is_err());
    }

    #[test]
    fn test_save_round_trip() {
        let bindings = load_keymap_toml(SAMPLE).unwrap();
        let serialized = save_keymap_toml(&bindings).unwrap();
        let reloaded = load_keymap_toml(&serialized).unwrap();
        assert_eq!(bindings.len(), reloaded.len());
        for (left, right) in bindings.iter().zip(reloaded.iter()) {
            assert_eq!(left.sequence, right.sequence);
            assert_eq!(left.context, right.context);
            assert_eq!(left.action, right.action);
        }
    }

    #[test]
    fn test_empty_file() {
        let bindings = load_keymap_toml("").unwrap();
        assert!(bindings.is_empty());
    }

    #[test]
    fn test_suppression_binding() {
        let toml = r#"
[[bindings]]
context = "Pane"
key = "j"
action = ""
"#;
        let bindings = load_keymap_toml(toml).unwrap();
        assert!(bindings[0].is_suppression());
    }

    /// Verifies that [`default_keymap_toml_string`] round-trips through the
    /// loader without errors and that every default binding appears in the
    /// parsed output.
    #[test]
    fn test_default_keymap_toml_round_trips() {
        use crate::defaults::default_bindings;
        let toml = default_keymap_toml_string();
        let loaded = load_keymap_toml(&toml).expect("default keymap TOML must be valid");
        let expected = default_bindings();
        assert_eq!(
            loaded.len(),
            expected.len(),
            "round-tripped binding count mismatch"
        );
        for (got, want) in loaded.iter().zip(expected.iter()) {
            assert_eq!(got.sequence, want.sequence, "sequence mismatch");
            assert_eq!(got.context, want.context, "context mismatch");
            assert_eq!(got.action, want.action, "action mismatch");
        }
    }

    /// Ensures the checked-in per-platform default keymap files match the
    /// output of [`default_keymap_toml_string_for`] byte-for-byte.
    ///
    /// If this test fails, regenerate the files by running:
    /// ```text
    /// cargo test -p atlas-keymap -- --ignored regen_default_keymap
    /// ```
    #[test]
    fn test_checked_in_default_toml_matches_emitter() {
        use crate::PrettyPlatform;
        let cases = [
            (
                PrettyPlatform::Mac,
                "macos",
                include_str!("../../../assets/keymaps/default.macos.toml"),
            ),
            (
                PrettyPlatform::Linux,
                "linux",
                include_str!("../../../assets/keymaps/default.linux.toml"),
            ),
            (
                PrettyPlatform::Windows,
                "windows",
                include_str!("../../../assets/keymaps/default.windows.toml"),
            ),
        ];
        for (platform, name, checked_in) in cases {
            let expected = default_keymap_toml_string_for(platform);
            // Windows checkouts with git autocrlf=true rewrite the
            // checked-in TOML's LF line endings to CRLF, breaking the
            // byte-comparison even when the semantic content is
            // identical. `.gitattributes` pins these files to LF, but
            // add a defensive normalisation here so the test passes
            // even on a repo cloned before `.gitattributes` landed.
            let checked_in_normalised = checked_in.replace("\r\n", "\n");
            assert_eq!(
                expected.as_str(),
                checked_in_normalised.as_str(),
                "assets/keymaps/default.{name}.toml is stale — regenerate it by running \
                 `cargo test -p atlas-keymap -- --ignored regen_default_keymap`"
            );
        }
    }

    /// Helper to regenerate the checked-in per-platform default keymap files.
    ///
    /// Run with: `cargo test -p atlas-keymap -- --ignored regen_default_keymap`
    #[test]
    #[ignore]
    fn regen_default_keymap() {
        use crate::PrettyPlatform;
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/keymaps");
        for (platform, name) in [
            (PrettyPlatform::Mac, "macos"),
            (PrettyPlatform::Linux, "linux"),
            (PrettyPlatform::Windows, "windows"),
        ] {
            let content = default_keymap_toml_string_for(platform);
            let path = base.join(format!("default.{name}.toml"));
            std::fs::write(&path, &content)
                .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
        }
    }
}
