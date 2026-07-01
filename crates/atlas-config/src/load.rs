//! Configuration loading and validation.
//!
//! Every load function runs [`validate`] after parsing so the returned
//! [`Config`] is always within accepted bounds.  Validation issues emit
//! [`tracing::warn`] and fall back to safe defaults rather than aborting.

use std::collections::HashSet;
use std::path::Path;

use atlas_core::Result;

use super::paths::config_file_path;
use super::schema::Config;

// ── Public API ─────────────────────────────────────────────────────────────

/// Parse and validate a [`Config`] from a TOML string.
///
/// Returns an error if the TOML is syntactically invalid or contains unknown
/// fields (see `deny_unknown_fields` on the schema types).
pub fn load_from_str(toml_text: &str) -> Result<Config> {
    let config: Config = toml::from_str(toml_text)
        .map_err(|e| anyhow::anyhow!("failed to parse config TOML: {e}"))?;
    Ok(validate(config))
}

/// Load and validate a [`Config`] from a TOML file on disk.
pub fn load_from_file(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read config file {}: {}", path.display(), e))?;
    load_from_str(&text)
}

/// Load the configuration from the platform default path.
///
/// - If the file does not exist, [`Config::default`] is returned.
/// - If the file exists but cannot be parsed, a warning is logged and
///   [`Config::default`] is returned.
pub fn load() -> Result<Config> {
    let path = config_file_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    match load_from_file(&path) {
        Ok(cfg) => Ok(cfg),
        Err(e) => {
            tracing::warn!(
                "failed to load config from {}: {}; falling back to defaults",
                path.display(),
                e
            );
            Ok(Config::default())
        }
    }
}

// ── Validation ─────────────────────────────────────────────────────────────

/// Validate and normalise a parsed [`Config`].
///
/// All issues are logged as warnings; no field ever causes a hard failure.
fn validate(mut cfg: Config) -> Config {
    // ── ui.font_size ───────────────────────────────────────────────────────
    if !(8.0_f32..=72.0).contains(&cfg.ui.font_size) {
        tracing::warn!(
            "ui.font_size {} is outside [8.0, 72.0]; clamping",
            cfg.ui.font_size
        );
        cfg.ui.font_size = cfg.ui.font_size.clamp(8.0, 72.0);
    }

    // ── ui.active_pane_border_px ───────────────────────────────────────────
    if !(0.0_f32..=6.0).contains(&cfg.ui.active_pane_border_px) {
        tracing::warn!(
            "ui.active_pane_border_px {} is outside [0.0, 6.0]; clamping",
            cfg.ui.active_pane_border_px
        );
        cfg.ui.active_pane_border_px = cfg.ui.active_pane_border_px.clamp(0.0, 6.0);
    }

    // ── indexer.max_memory_mb ──────────────────────────────────────────────
    const MIN_MEMORY_MB: u32 = 16;
    if cfg.indexer.max_memory_mb < MIN_MEMORY_MB {
        tracing::warn!(
            "indexer.max_memory_mb {} is below minimum {}; using {}",
            cfg.indexer.max_memory_mb,
            MIN_MEMORY_MB,
            MIN_MEMORY_MB,
        );
        cfg.indexer.max_memory_mb = MIN_MEMORY_MB;
    }

    // ── thumbnails.cache_max_size_mb ───────────────────────────────────────
    if cfg.thumbnails.cache_max_size_mb < 1 {
        tracing::warn!(
            "thumbnails.cache_max_size_mb {} is below minimum 1; using 1",
            cfg.thumbnails.cache_max_size_mb
        );
        cfg.thumbnails.cache_max_size_mb = 1;
    }

    // ── Tilde expansion ────────────────────────────────────────────────────
    cfg.general.start_path = cfg.general.start_path.map(atlas_core::path::expand_tilde);
    cfg.navigation.last_location = cfg
        .navigation
        .last_location
        .map(atlas_core::path::expand_tilde);

    cfg.indexer.roots = cfg
        .indexer
        .roots
        .into_iter()
        .map(atlas_core::path::expand_tilde)
        .collect();

    cfg.bookmarks = cfg
        .bookmarks
        .into_iter()
        .map(|mut b| {
            b.path = atlas_core::path::expand_tilde(b.path);
            b
        })
        .collect();

    // ── Deduplicate indexer.roots ──────────────────────────────────────────
    let mut seen: HashSet<std::path::PathBuf> = HashSet::new();
    cfg.indexer.roots.retain(|p| seen.insert(p.clone()));

    cfg
}
