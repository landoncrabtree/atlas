//! Typed configuration schema for Atlas.
//!
//! Every struct carries `#[serde(default)]` so partial TOML files are accepted
//! and missing fields fall back to their [`Default`] implementations.

use std::{collections::HashMap, path::PathBuf};

// ── Top-level ──────────────────────────────────────────────────────────────

/// Root configuration object.  All fields are optional in the TOML file;
/// missing sections and keys are filled in from the corresponding `Default`
/// implementation.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// General application behaviour.
    pub general: General,
    /// User-interface appearance.
    pub ui: Ui,
    /// File-list view options.
    pub view: View,
    /// Navigation history settings.
    pub navigation: Navigation,
    /// Background file-indexer settings.
    pub indexer: Indexer,
    /// Fuzzy / content search settings.
    pub search: Search,
    /// Thumbnail generation and cache settings.
    pub thumbnails: Thumbnails,
    /// Remote-backend resilience knobs (pool, retry, timeouts).
    pub remote: Remote,
    /// Sidebar bookmarks.
    pub bookmarks: Vec<Bookmark>,
}

// ── General ────────────────────────────────────────────────────────────────

/// General application behaviour settings.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    /// Directory to open on startup.  `None` means the user's home directory.
    pub start_path: Option<PathBuf>,
    /// Show a confirmation dialog before quitting.
    pub confirm_on_quit: bool,
    /// Follow symbolic links when listing directories.
    pub follow_symlinks: bool,
    /// Enable vim-inspired key bindings.
    pub vim_mode: bool,
    /// Open the app in dual-pane (split) layout on startup.
    pub dual_pane: bool,
}

// ── UI ─────────────────────────────────────────────────────────────────────

/// User-interface appearance settings.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    /// Colour theme name.  Built-in values: `"atlas-dark"`, `"atlas-light"`.
    pub theme: String,
    /// UI font family.
    pub font_family: String,
    /// UI font size in points.  Clamped to `[8.0, 72.0]` at load time.
    pub font_size: f32,
    /// Monospace font used in the preview pane and code views.
    pub monospace_font_family: String,
    /// Layout density.
    pub density: Density,
    /// Show the status bar at the bottom of the window.
    pub show_status_bar: bool,
    /// Show the breadcrumb path navigation above the file list.
    pub show_breadcrumbs: bool,
    /// Show the bottom shortcut-footer strip (e.g. `⌘C Copy · ⌘X Cut · …`).
    /// When `false`, the whole footer is hidden. Rebindings still take
    /// effect; the hint chips just aren't advertised.
    pub show_shortcuts: bool,
    /// Enable animations and transitions.
    pub animations: bool,
    /// Border thickness (in logical pixels) drawn inside the focused pane so
    /// the user can tell which pane will receive keystrokes. `0.0` disables
    /// the border entirely. Clamped to `[0.0, 6.0]` at load time.
    pub active_pane_border_px: f32,
}

/// Layout density of the file list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    /// Minimal row height.
    Compact,
    /// Balanced row height (default).
    #[default]
    Comfortable,
    /// Generous row height.
    Spacious,
}

impl<'de> serde::Deserialize<'de> for Density {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = <String as serde::Deserialize>::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "compact" => Ok(Self::Compact),
            "comfortable" => Ok(Self::Comfortable),
            "spacious" => Ok(Self::Spacious),
            _ => Err(serde::de::Error::unknown_variant(
                &s,
                &["compact", "comfortable", "spacious"],
            )),
        }
    }
}

// ── View ───────────────────────────────────────────────────────────────────

/// File-list view options.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct View {
    /// Default view mode.
    pub default_mode: ViewMode,
    /// Show hidden files and directories.
    pub show_hidden: bool,
    /// Use natural sort order (`file10` after `file9`).
    pub natural_sort: bool,
    /// List directories before files.
    pub dirs_first: bool,
    /// Default column to sort by.
    pub default_sort_key: SortKey,
    /// Default sort direction.
    pub default_sort_order: SortOrder,
    /// Details-view specific settings.
    pub details: DetailsView,
}

/// Details-view specific settings persisted to `[view.details]`.
///
/// Currently holds per-column-kind width overrides. Keys are `"name"`,
/// `"size"`, `"modified"`, `"kind"`, `"extension"` — matching the wire
/// string emitted by the UI's `ColumnKind::as_str()`. Missing keys fall
/// back to the built-in default widths.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DetailsView {
    /// Column width overrides in logical pixels, keyed by column kind.
    /// Written on quit; read on startup.
    pub column_widths: HashMap<String, f32>,
}

/// Available view modes for the file list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    /// Classic column list.
    #[default]
    Details,
    /// Icon grid.
    Grid,
    /// Large-preview gallery.
    Gallery,
    /// Miller columns.
    Miller,
    /// Recursive tree.
    Tree,
}

impl<'de> serde::Deserialize<'de> for ViewMode {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = <String as serde::Deserialize>::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "details" => Ok(Self::Details),
            "grid" => Ok(Self::Grid),
            "gallery" => Ok(Self::Gallery),
            "miller" => Ok(Self::Miller),
            "tree" => Ok(Self::Tree),
            _ => Err(serde::de::Error::unknown_variant(
                &s,
                &["details", "grid", "gallery", "miller", "tree"],
            )),
        }
    }
}

/// Column to sort the file list by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    /// Sort by file name.
    #[default]
    Name,
    /// Sort by file size.
    Size,
    /// Sort by last-modified time.
    Modified,
    /// Sort by file kind (directory / file).
    Kind,
    /// Sort by file extension.
    Extension,
}

impl<'de> serde::Deserialize<'de> for SortKey {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = <String as serde::Deserialize>::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "name" => Ok(Self::Name),
            "size" => Ok(Self::Size),
            "modified" => Ok(Self::Modified),
            "kind" => Ok(Self::Kind),
            "extension" => Ok(Self::Extension),
            _ => Err(serde::de::Error::unknown_variant(
                &s,
                &["name", "size", "modified", "kind", "extension"],
            )),
        }
    }
}

/// Sort direction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    /// Ascending (A → Z, smallest → largest).
    #[default]
    Asc,
    /// Descending (Z → A, largest → smallest).
    Desc,
}

impl<'de> serde::Deserialize<'de> for SortOrder {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = <String as serde::Deserialize>::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "asc" => Ok(Self::Asc),
            "desc" => Ok(Self::Desc),
            _ => Err(serde::de::Error::unknown_variant(&s, &["asc", "desc"])),
        }
    }
}

// ── Navigation ─────────────────────────────────────────────────────────────

/// Navigation history settings.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Navigation {
    /// Number of visited locations to keep in the back/forward history.
    pub history_size: usize,
    /// Re-open the last visited directory when Atlas starts.
    pub remember_last_location: bool,
    /// Last directory Atlas was viewing when it quit. Written on shutdown when
    /// `remember_last_location = true`; consumed on the next startup in place
    /// of `general.start_path`. Users generally should not edit this by hand.
    #[serde(default)]
    pub last_location: Option<PathBuf>,
}

// ── Indexer ────────────────────────────────────────────────────────────────

/// Background file-indexer settings.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Indexer {
    /// Enable the background file indexer for instant search.
    pub enabled: bool,
    /// Root directories to index.  Leave empty to configure via the UI.
    pub roots: Vec<PathBuf>,
    /// Honour `.gitignore` files during indexing.
    pub respect_gitignore: bool,
    /// Maximum memory the indexer may use (megabytes, minimum 16).
    pub max_memory_mb: u32,
}

// ── Search ─────────────────────────────────────────────────────────────────

/// Fuzzy and content search settings.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Search {
    /// Maximum number of fuzzy-search results to return per source (upstream cap).
    pub fuzzy_max_results: usize,
    /// Number of threads for content search.  `None` uses all available CPU cores.
    pub content_search_threads: Option<usize>,
    /// Glob patterns to exclude from search results.
    pub default_globs_exclude: Vec<String>,
    /// Minimum query length before the search dispatcher fires. Below this,
    /// the panel shows a hint and no work is scheduled. Clamped to `>= 1`.
    pub min_query_length: usize,
    /// Maximum number of ranked results actually pushed to the UI list.
    /// Bounded above by [`MAX_VISIBLE_RESULTS_CAP`] to protect the renderer
    /// from pathological result sets.
    pub max_visible_results: usize,
    /// Debounce delay in milliseconds between the last keystroke and the
    /// dispatched query. Coalesces bursts of typing into a single search.
    /// Clamped to the range `[0, 1000]` at wire-in time.
    pub debounce_ms: u32,
}

/// Hard cap on `search.max_visible_results`. Even if the user sets a larger
/// value, the UI truncates to this many rows to keep rendering responsive.
pub const MAX_VISIBLE_RESULTS_CAP: usize = 200;

// ── Thumbnails ─────────────────────────────────────────────────────────────

/// Thumbnail generation and cache settings.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Thumbnails {
    /// Generate image thumbnails in grid and gallery views.
    pub enabled: bool,
    /// Maximum disk space for the thumbnail cache (megabytes, minimum 1).
    pub cache_max_size_mb: u32,
    /// Number of threads for thumbnail generation.  `None` uses CPU core count.
    pub generation_threads: Option<usize>,
    /// Skip thumbnails for files larger than this limit (megabytes).
    pub generate_for_size_up_to_mb: u32,
}

// ── Remote ─────────────────────────────────────────────────────────────────

/// Remote-backend resilience knobs. Applied by
/// [`atlas_remote::pool::ConnectionPool`] and
/// [`atlas_remote::retry::with_retry`] on wire-in.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Remote {
    /// Connection-pool tunables.
    pub pool: RemotePool,
    /// Per-scheme timeout overrides (milliseconds). Any missing scheme
    /// falls back to `default_timeout_ms`. Keys are `"sftp"`, `"s3"`,
    /// `"webdav"`, `"ftp"`.
    pub timeout_ms: HashMap<String, u32>,
    /// Fallback timeout used when a scheme isn't listed in
    /// [`timeout_ms`](Self::timeout_ms).
    pub default_timeout_ms: u32,
    /// Per-scheme retry counts. Missing schemes use
    /// [`default_retries`](Self::default_retries).
    pub retries: HashMap<String, u32>,
    /// Fallback retry count.
    pub default_retries: u32,
    /// Initial backoff duration (milliseconds).
    pub backoff_initial_ms: u32,
    /// Cap on the backoff duration (milliseconds).
    pub backoff_max_ms: u32,
    /// Exponential growth factor applied between retries.
    pub backoff_multiplier: f32,
    /// Preview cache for remote file open (double-click / Enter on a
    /// remote pane).
    pub preview: RemotePreview,
}

/// Preview cache for remote file open (Phase 2.7). When the user
/// activates a file on a remote pane we download it into
/// `<cache_dir>/<sha256(uri:mtime:size)>/<name>` and hand the local
/// copy off to `open::that`. Subsequent opens of the same file are a
/// cache hit and skip the network entirely.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RemotePreview {
    /// Cache directory override. `None` → `ProjectDirs::cache_dir/preview`
    /// (per platform: `~/Library/Caches/dev.atlas.atlas/preview` on
    /// macOS, `$XDG_CACHE_HOME/atlas/preview` on Linux,
    /// `%LOCALAPPDATA%\atlas\preview` on Windows).
    pub cache_dir: Option<PathBuf>,
    /// Hard cap on the total on-disk cache size in bytes. When
    /// exceeded, oldest-accessed entries are evicted first (LRU).
    /// Default: 200 MB.
    pub max_bytes: u64,
    /// Maximum age of a cached preview before it is treated as stale
    /// and re-downloaded. Default: 24 h (86 400 s).
    pub max_age_secs: u64,
    /// Refuse to preview remote files larger than this cap. Above the
    /// limit the shell logs a hint and instructs the user to copy the
    /// file into a local pane first. Default: 100 MB.
    pub max_open_bytes: u64,
}

/// Connection-pool tunables.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RemotePool {
    /// Idle time-to-live before an unreferenced client is dropped
    /// (milliseconds).
    pub idle_ttl_ms: u32,
    /// Hard cap on the number of pooled clients. LRU eviction kicks
    /// in once this many are outstanding.
    pub max_connections: u32,
}

/// A named sidebar bookmark.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Bookmark {
    /// Human-readable label shown in the sidebar.
    pub name: String,
    /// Absolute path (tilde-expanded at load time).
    pub path: PathBuf,
}
