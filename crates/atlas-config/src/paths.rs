//! Platform-specific configuration directory resolution.
//!
//! The `ATLAS_CONFIG_DIR` environment variable, when set, overrides the
//! platform default and takes effect immediately.  This is useful for tests
//! and portable installations.
//!
//! Defaults:
//! * **Unix (macOS & Linux):** `$XDG_CONFIG_HOME/atlas`, or `~/.config/atlas`
//!   if `XDG_CONFIG_HOME` is unset. We deliberately do NOT use macOS's
//!   `~/Library/Application Support/` — power-user configs belong in
//!   `~/.config` for easy dotfile management and discoverability.
//! * **Windows:** `%APPDATA%\Atlas`.

use std::path::PathBuf;

use atlas_core::Result;

/// Return the Atlas configuration directory for the current platform.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("ATLAS_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }

    #[cfg(unix)]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            let base = PathBuf::from(xdg);
            if !base.as_os_str().is_empty() {
                return Ok(base.join("atlas"));
            }
        }
        let home = std::env::var_os("HOME")
            .ok_or_else(|| anyhow::anyhow!("HOME environment variable not set"))?;
        Ok(PathBuf::from(home).join(".config").join("atlas"))
    }

    #[cfg(windows)]
    {
        let appdata = std::env::var_os("APPDATA")
            .ok_or_else(|| anyhow::anyhow!("APPDATA environment variable not set"))?;
        Ok(PathBuf::from(appdata).join("Atlas"))
    }
}

/// Return the path to `config.toml` inside the Atlas configuration directory.
pub fn config_file_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// Return the path to `servers.toml`, the persisted catalogue of remote
/// servers the user has connected to. See [`crate::servers`] for the file
/// schema. Missing on first launch; created by
/// [`crate::servers::save`].
pub fn servers_file_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("servers.toml"))
}

/// Return the path to the primary keymap file (`keymaps/default.toml`).
///
/// User overrides may add additional keymap files under `keymaps/`; the loader
/// layers them in name-sorted order.
pub fn keymap_file_path() -> Result<PathBuf> {
    Ok(keymaps_dir()?.join("default.toml"))
}

/// Return the directory that holds user keymap files (`keymaps/`).
pub fn keymaps_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("keymaps"))
}

/// Return the directory that holds user theme files (`themes/`).
pub fn themes_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("themes"))
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
