//! Typed TOML configuration system for Atlas with hot reload.
//!
//! # Usage
//!
//! ```no_run
//! // Load from the platform default path (never panics).
//! let config = atlas_config::load().unwrap();
//!
//! // Start a hot-reload watcher.
//! let (watcher, arc, _events) = atlas_config::ConfigWatcher::start().unwrap();
//! let _current = arc.load();
//! drop(watcher); // or watcher.stop()
//! ```

pub mod defaults;
pub mod load;
pub mod paths;
pub mod save;
pub mod schema;
pub mod watcher;

// ── Re-exports ──────────────────────────────────────────────────────────────

pub use schema::{
    Bookmark, Config, Density, General, Indexer, Navigation, Search, SortKey, SortOrder,
    Thumbnails, Ui, View, ViewMode,
};

pub use load::{load, load_from_file, load_from_str};
pub use paths::{config_dir, config_file_path, ensure_config_dir, keymap_file_path};
pub use save::{save, save_to_string, skeleton_toml};
pub use watcher::{ConfigEvent, ConfigWatcher};
