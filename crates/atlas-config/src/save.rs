//! Comment-preserving configuration serialisation.
//!
//! [`save_to_string`] reads the existing config file (or the built-in
//! [`skeleton_toml`] if no file exists yet) into a [`toml_edit::DocumentMut`],
//! then surgically updates only the values while keeping every user comment
//! and key ordering intact.

use atlas_core::Result;

use super::paths::{config_file_path, ensure_config_dir};
use super::schema::Config;

// ── Public API ─────────────────────────────────────────────────────────────

/// Serialise `config` to a TOML string, preserving any comments that exist in
/// the on-disk file.
///
/// Algorithm:
/// 1. Read the existing file (or use [`skeleton_toml`] if absent) as the
///    comment-bearing base document.
/// 2. Serialise `config` to a plain TOML string and parse it as a second
///    document containing only values.
/// 3. Recursively merge the value document into the base document, touching
///    only leaf values so comment decorations are retained.
/// 4. Return `base_doc.to_string()`.
pub fn save_to_string(config: &Config) -> Result<String> {
    // Read existing file, or fall back to the skeleton.
    let existing = config_file_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok());
    let base_src: String = existing.unwrap_or_else(|| skeleton_toml().to_string());

    let mut base_doc: toml_edit::DocumentMut = base_src
        .parse()
        .map_err(|e| anyhow::anyhow!("failed to parse base TOML document: {e}"))?;

    // Serialise the config and parse it as a clean document (no comments).
    let config_str =
        toml::to_string(config).map_err(|e| anyhow::anyhow!("failed to serialise config: {e}"))?;
    let config_doc: toml_edit::DocumentMut = config_str
        .parse()
        .map_err(|e| anyhow::anyhow!("failed to parse serialised config: {e}"))?;

    merge_docs(&mut base_doc, &config_doc);

    Ok(base_doc.to_string())
}

/// Write `config` to the platform default config file, preserving comments.
///
/// The config directory is created if it does not already exist.
pub fn save(config: &Config) -> Result<()> {
    ensure_config_dir()?;
    let path = config_file_path()?;
    let contents = save_to_string(config)?;
    std::fs::write(&path, contents)
        .map_err(|e| anyhow::anyhow!("failed to write config to {}: {}", path.display(), e))?;
    Ok(())
}

/// Return the heavily-commented TOML skeleton written on first save.
///
/// The skeleton documents every option with its default value and purpose.
pub fn skeleton_toml() -> &'static str {
    include_str!("skeleton.toml")
}

// ── Merge helpers ──────────────────────────────────────────────────────────

/// Merge `src` document into `dst`, updating values while keeping `dst`'s
/// comment decorations.
fn merge_docs(dst: &mut toml_edit::DocumentMut, src: &toml_edit::DocumentMut) {
    for (key, src_item) in src.iter() {
        match src_item {
            toml_edit::Item::Table(src_tbl) => {
                let dst_item = dst
                    .entry(key)
                    .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
                if let Some(dst_tbl) = dst_item.as_table_mut() {
                    merge_tables(dst_tbl, src_tbl);
                } else {
                    *dst_item = toml_edit::Item::Table(src_tbl.clone());
                }
            }
            toml_edit::Item::ArrayOfTables(aot) => {
                // Arrays of tables (e.g. [[bookmarks]]) are replaced wholesale.
                if let Some(existing) = dst.get_mut(key) {
                    *existing = toml_edit::Item::ArrayOfTables(aot.clone());
                } else {
                    dst.insert(key, toml_edit::Item::ArrayOfTables(aot.clone()));
                }
            }
            toml_edit::Item::Value(src_val) => {
                let dst_item = dst
                    .entry(key)
                    .or_insert(toml_edit::Item::Value(src_val.clone()));
                if let toml_edit::Item::Value(dst_val) = dst_item {
                    *dst_val = src_val.clone();
                }
            }
            toml_edit::Item::None => {}
        }
    }
}

/// Recursively merge `src` table into `dst` table.
fn merge_tables(dst: &mut toml_edit::Table, src: &toml_edit::Table) {
    for (key, src_item) in src.iter() {
        match src_item {
            toml_edit::Item::Table(src_tbl) => {
                let dst_item = dst
                    .entry(key)
                    .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
                if let Some(dst_tbl) = dst_item.as_table_mut() {
                    merge_tables(dst_tbl, src_tbl);
                } else {
                    *dst_item = toml_edit::Item::Table(src_tbl.clone());
                }
            }
            toml_edit::Item::Value(src_val) => {
                let dst_item = dst
                    .entry(key)
                    .or_insert(toml_edit::Item::Value(src_val.clone()));
                if let toml_edit::Item::Value(dst_val) = dst_item {
                    *dst_val = src_val.clone();
                }
            }
            other => {
                dst.insert(key, other.clone());
            }
        }
    }
}
