//! Storage path helpers for atlas-indexd.
//!
//! All persistent data lives under the platform-specific application-support
//! directory (`~/Library/Application Support/dev.atlas.atlas/` on macOS).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::{BaseDirs, ProjectDirs};
use sha2::{Digest, Sha256};

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("dev", "atlas", "atlas")
        .context("could not determine application support directory")
}

/// Base directory for atlas-indexd persistent data.
pub fn base_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.data_dir().to_path_buf())
}

/// Default daemon socket path under the application support directory.
pub fn socket_path() -> Result<PathBuf> {
    Ok(base_dir()?.join("indexd.sock"))
}

/// Per-root index directory: `<base_dir>/index/<sha256>/`.
///
/// The hash is derived from the canonical path string so the same directory
/// always maps to the same on-disk location.
pub fn index_root_dir(root_path: &Path) -> Result<PathBuf> {
    let mut hasher = Sha256::new();
    hasher.update(root_path.to_string_lossy().as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    Ok(base_dir()?.join("index").join(hash))
}

/// Log directory: `~/Library/Logs/Atlas/` on macOS.
pub fn logs_dir() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not determine home directory")?;
    Ok(base_dirs
        .home_dir()
        .join("Library")
        .join("Logs")
        .join("Atlas"))
}
