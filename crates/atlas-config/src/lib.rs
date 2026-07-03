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
pub mod servers;
pub mod watcher;

// ── Re-exports ──────────────────────────────────────────────────────────────

pub use schema::{
    Bookmark, Config, Density, DetailsView, General, IconPack, Icons, Indexer, Navigation, Remote,
    RemotePool, RemotePreview, Search, SortKey, SortOrder, Thumbnails, Ui, View, ViewMode,
    MAX_VISIBLE_RESULTS_CAP,
};

pub use load::{load, load_from_file, load_from_str};
pub use paths::{
    config_dir, config_file_path, ensure_config_dir, keymap_file_path, keymaps_dir,
    known_hosts_file_path, servers_file_path, themes_dir,
};
pub use save::{ensure_config_file, save, save_to_string, skeleton_toml};
pub use servers::{
    add_or_replace as add_or_replace_server, delete as delete_server, list as list_servers,
    load as load_servers, save as save_servers, SavedServer, SavedServersFile,
};
pub use watcher::{ConfigEvent, ConfigWatcher};
