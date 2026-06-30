//! Platform-specific configuration directory resolution.
//!
//! The `ATLAS_CONFIG_DIR` environment variable, when set, overrides the
//! platform default and takes effect immediately.  This is useful for tests
//! and portable installations.

use std::path::PathBuf;

use atlas_core::Result;

/// Return the Atlas configuration directory for the current platform.
///
/// Precedence:
/// 1. `ATLAS_CONFIG_DIR` environment variable.
/// 2. Platform default via the [`directories`] crate:
///    - Linux/macOS: `~/.config/atlas`
///    - Windows: `%APPDATA%\Atlas`
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("ATLAS_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }

    let proj = directories::ProjectDirs::from("dev", "atlas", "atlas")
        .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?;

    Ok(proj.config_dir().to_path_buf())
}

/// Return the path to `config.toml` inside the Atlas configuration directory.
pub fn config_file_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// Return the path to `keymap.toml` inside the Atlas configuration directory.
pub fn keymap_file_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("keymap.toml"))
}

/// Ensure the Atlas configuration directory exists, creating it if necessary.
///
/// Returns the directory path on success.
pub fn ensure_config_dir() -> Result<PathBuf> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| {
        anyhow::anyhow!("failed to create config directory {}: {}", dir.display(), e)
    })?;
    Ok(dir)
}
