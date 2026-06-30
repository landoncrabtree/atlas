//! Rename primitive.

use std::path::Path;

use crate::undo::UndoEntry;

pub(crate) fn rename_op(path: &Path, new_name: &str) -> atlas_core::Result<UndoEntry> {
    if new_name.chars().any(std::path::is_separator) {
        return Err(atlas_core::AtlasError::InvalidPath(new_name.to_owned()));
    }
    let old_path = path.to_path_buf();
    let parent = old_path.parent().ok_or_else(|| {
        atlas_core::AtlasError::InvalidPath(format!("path has no parent: {}", old_path.display()))
    })?;
    let new_path = parent.join(new_name);
    std::fs::rename(&old_path, &new_path)
        .map_err(|source| atlas_core::AtlasError::io(Some(old_path.clone()), source))?;
    Ok(UndoEntry::Rename {
        from: new_path,
        to: old_path,
    })
}
