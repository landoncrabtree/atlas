//! Directory creation primitive.

use std::path::Path;

pub(crate) fn mkdir_op(path: &Path, parents: bool) -> atlas_core::Result<()> {
    if parents {
        std::fs::create_dir_all(path)
            .map_err(|source| atlas_core::AtlasError::io(Some(path.to_path_buf()), source))
    } else {
        std::fs::create_dir(path)
            .map_err(|source| atlas_core::AtlasError::io(Some(path.to_path_buf()), source))
    }
}
