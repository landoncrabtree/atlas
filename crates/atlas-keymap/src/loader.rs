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
}
