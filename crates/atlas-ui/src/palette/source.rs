//! [`PaletteSource`] trait and concrete implementations.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use atlas_fs::{walk, Entry, ListEvent, WalkRequest};
use atlas_keymap::{ActionRegistry, Keymap};
use parking_lot::Mutex;
use tracing::{debug, trace, warn};

/// Maximum paths kept in the in-memory cache.
const MAX_CACHED_PATHS: usize = 20_000;
/// Maximum directory depth to walk.
const MAX_WALK_DEPTH: usize = 6;

/// Directory names that are excluded from the palette walker.
///
/// These are either enormous (`Library`, `node_modules`), or backed by cloud
/// filesystems that can time out when scanned (`Google Drive`, `iCloud Drive`,
/// `Dropbox`, `OneDrive`). Skipping them keeps the walker fast, keeps the
/// cache useful, and prevents log noise.
const EXCLUDED_DIR_NAMES: &[&str] = &[
    "Library",
    "Applications",
    ".Trash",
    ".git",
    "node_modules",
    "target",
    "build",
    "dist",
    ".cache",
    ".cargo",
    ".rustup",
    ".npm",
    ".pnpm-store",
    ".yarn",
    ".gradle",
    ".m2",
    "venv",
    ".venv",
    "__pycache__",
    "Google Drive",
    "GoogleDrive",
    "iCloud Drive",
    "iCloudDrive",
    "Dropbox",
    "OneDrive",
    "Box",
    "pCloudDrive",
];

/// A single item shown in the command palette.
#[derive(Debug, Clone)]
pub struct PaletteItem {
    /// Stable identifier (action ID or absolute path string).
    pub id: String,
    /// Primary display line, matched against the query.
    pub title: String,
    /// Secondary display line (e.g. keybinding or parent directory).
    pub subtitle: String,
    /// Variant tag — drives what happens on confirm.
    pub kind: PaletteItemKind,
}

/// Whether a [`PaletteItem`] represents an action or a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteItemKind {
    /// A registered application action.
    Action,
    /// A filesystem path.
    Path,
    /// A saved remote server. `PaletteItem::id` holds the
    /// [`atlas_config::servers::SavedServer::id`] used to look the
    /// entry up when the user confirms it.
    Server,
}

/// Sink that receives [`PaletteItem`] values during population.
pub trait ItemSink {
    /// Push one item into the sink.
    fn push(&mut self, item: PaletteItem);
}

/// Provides candidate items for one palette mode (actions or goto-path).
pub trait PaletteSource: Send + Sync {
    /// Human-readable placeholder shown in the empty state.
    fn placeholder(&self) -> &'static str;
    /// Emit all candidate items into `sink`.
    fn populate(&self, sink: &mut dyn ItemSink);
}

/// Palette source that enumerates all registered actions.
pub struct ActionsSource {
    registry: Arc<ActionRegistry>,
    keymap: Arc<Keymap>,
}

impl ActionsSource {
    /// Create a new source backed by the given registry and keymap.
    #[must_use]
    pub fn new(registry: Arc<ActionRegistry>, keymap: Arc<Keymap>) -> Self {
        Self { registry, keymap }
    }

    fn keybinding_for(&self, action_id: &str) -> String {
        let contexts = [String::from("Global"), String::from("Pane")];
        self.keymap
            .bindings_for_contexts(&contexts)
            .into_iter()
            .find(|binding| binding.action.as_str() == action_id && !binding.is_suppression())
            .map(|binding| binding.sequence.display())
            .unwrap_or_default()
    }
}

impl PaletteSource for ActionsSource {
    fn placeholder(&self) -> &'static str {
        "Actions"
    }

    fn populate(&self, sink: &mut dyn ItemSink) {
        let mut actions: Vec<_> = self.registry.iter().collect();
        actions.sort_by(|a, b| a.title.cmp(&b.title));
        for meta in actions {
            if meta.id.is_null() {
                continue;
            }
            sink.push(PaletteItem {
                id: meta.id.as_str().to_owned(),
                title: meta.title.clone(),
                subtitle: self.keybinding_for(meta.id.as_str()),
                kind: PaletteItemKind::Action,
            });
        }
    }
}

/// An index of filesystem paths that can answer candidate queries.
pub trait PathIndex: Send + Sync {
    /// Return up to `limit` paths whose name or path matches `query`.
    fn candidates(&self, query: &str, limit: usize) -> Vec<PathBuf>;
}

/// A lightweight [`PathIndex`] that walks a root directory in the background
/// and caches up to [`MAX_CACHED_PATHS`] entries.
pub struct WalkerPathIndex {
    root: PathBuf,
    cache: Arc<Mutex<Vec<PathBuf>>>,
}

impl WalkerPathIndex {
    /// Create a new index rooted at `root` and start the background walker.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        let cache: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let cache_bg = Arc::clone(&cache);
        let root_bg = root.clone();

        if let Err(error) = std::thread::Builder::new()
            .name(String::from("atlas-palette-walker"))
            .spawn(move || {
                let request = WalkRequest {
                    roots: vec![root_bg],
                    follow_symlinks: false,
                    include_hidden: false,
                    respect_gitignore: true,
                    max_depth: Some(MAX_WALK_DEPTH),
                };
                let rx = walk(request);
                for event in &rx {
                    match event {
                        ListEvent::Batch(entries) => push_batch(&cache_bg, entries),
                        ListEvent::Error { path, error } => {
                            // Cloud-mount timeouts are expected on macOS
                            // (Google Drive / iCloud FUSE mounts can stall);
                            // demote them to trace so logs stay clean. Real
                            // errors still surface at debug.
                            let root_source = std::error::Error::source(&error);
                            let is_timeout = root_source
                                .and_then(|s| s.downcast_ref::<io::Error>())
                                .is_some_and(|e| e.kind() == io::ErrorKind::TimedOut);
                            if is_timeout {
                                trace!(?path, "palette walker skipped timed-out entry");
                            } else {
                                debug!(?path, ?error, "palette walker encountered an error");
                            }
                        }
                        ListEvent::Done => break,
                    }
                }
            })
        {
            warn!(?error, "failed to spawn atlas-palette-walker thread");
        }

        Self { root, cache }
    }

    /// Root directory used for the background walk.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn push_batch(cache: &Arc<Mutex<Vec<PathBuf>>>, entries: Vec<Entry>) {
    let mut cache = cache.lock();
    if cache.len() >= MAX_CACHED_PATHS {
        return;
    }

    for entry in entries {
        if cache.len() >= MAX_CACHED_PATHS {
            break;
        }
        if is_excluded(&entry.path) {
            continue;
        }
        cache.push(entry.path);
    }
}

/// Returns true if any component of `path` matches an excluded directory name.
///
/// Exclusion is component-wise (case-insensitive on the leaf) so both
/// `~/Google Drive/whatever` and `~/some/nested/node_modules/foo.js` get
/// filtered.
fn is_excluded(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        EXCLUDED_DIR_NAMES
            .iter()
            .any(|excluded| name.eq_ignore_ascii_case(excluded))
    })
}

impl PathIndex for WalkerPathIndex {
    fn candidates(&self, query: &str, limit: usize) -> Vec<PathBuf> {
        let query = query.to_lowercase();
        let mut results: Vec<PathBuf> = self
            .cache
            .lock()
            .iter()
            .filter(|path| {
                if query.is_empty() {
                    return true;
                }

                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.to_lowercase().contains(&query))
                    .unwrap_or(false)
                    || path.to_string_lossy().to_lowercase().contains(&query)
            })
            .take(limit)
            .cloned()
            .collect();
        results.sort_by_key(|path| path.components().count());
        results
    }
}

/// Palette source that emits filesystem paths from a [`PathIndex`].
pub struct GotoPathsSource {
    index: Arc<dyn PathIndex>,
}

impl GotoPathsSource {
    /// Create a new source backed by the given path index.
    #[must_use]
    pub fn new(index: Arc<dyn PathIndex>) -> Self {
        Self { index }
    }
}

impl PaletteSource for GotoPathsSource {
    fn placeholder(&self) -> &'static str {
        "Go to path"
    }

    fn populate(&self, sink: &mut dyn ItemSink) {
        for path in self.index.candidates("", 200) {
            let title = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned();
            let subtitle = path
                .parent()
                .and_then(|parent| parent.to_str())
                .unwrap_or_default()
                .to_owned();
            let id = path.to_string_lossy().into_owned();
            sink.push(PaletteItem {
                id,
                title,
                subtitle,
                kind: PaletteItemKind::Path,
            });
        }
    }
}

/// Palette source that emits the user's configured sidebar bookmarks.
///
/// Bookmarks live in `~/.config/atlas/config.toml` under `[[bookmarks]]` and
/// are surfaced in the command palette alongside actions and go-to paths so
/// the user can jump to a favourite location by name.
pub struct BookmarksSource {
    bookmarks: Vec<(String, PathBuf)>,
}

impl BookmarksSource {
    /// Build a source from a slice of `(name, path)` pairs.
    #[must_use]
    pub fn new(bookmarks: Vec<(String, PathBuf)>) -> Self {
        Self { bookmarks }
    }
}

impl PaletteSource for BookmarksSource {
    fn placeholder(&self) -> &'static str {
        "Bookmarks"
    }

    fn populate(&self, sink: &mut dyn ItemSink) {
        for (name, path) in &self.bookmarks {
            let subtitle = path.to_string_lossy().into_owned();
            let id = subtitle.clone();
            sink.push(PaletteItem {
                id,
                title: name.clone(),
                subtitle,
                kind: PaletteItemKind::Path,
            });
        }
    }
}

/// Palette source that emits saved-server entries (Cmd+K persisted
/// connections). Reused as a secondary source inside the goto/paths
/// palette so a user can jump straight from Cmd+P to a remote mount.
///
/// The list is loaded from `atlas_config::servers::list()` on every
/// [`Self::populate`] call — the config file is small and this keeps
/// entries fresh without wiring a change-notification channel.
pub struct SavedServersSource;

impl SavedServersSource {
    /// Construct a new source. Stateless — every populate re-reads
    /// `servers.toml`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SavedServersSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PaletteSource for SavedServersSource {
    fn placeholder(&self) -> &'static str {
        "Saved servers"
    }

    fn populate(&self, sink: &mut dyn ItemSink) {
        let servers = match atlas_config::servers::list() {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(error = %err, "palette: could not load servers.toml");
                return;
            }
        };
        for server in servers {
            let glyph = backend_glyph(server.backend);
            let title = if server.label.is_empty() {
                match (server.username.as_deref(), server.address.as_str()) {
                    (Some(u), h) if !u.is_empty() && !h.is_empty() => format!("{u}@{h}"),
                    _ => server.address.clone(),
                }
            } else {
                server.label.clone()
            };
            // Prefix the title with a small "server:" tag so the user
            // can tell path hits from server hits when the query is
            // empty. When the user types, the fuzzy matcher still
            // scores against the same string — the "server:" prefix
            // participates naturally and does not hurt short-query
            // ranking (nucleo strips the prefix on partial matches).
            let title = format!("{glyph}  {title}");
            let subtitle = saved_server_uri_display(&server);
            sink.push(PaletteItem {
                id: server.id,
                title,
                subtitle,
                kind: PaletteItemKind::Server,
            });
        }
    }
}

/// Build the same URI string the modal viewer renders for a saved
/// server. Duplicated intentionally so `atlas-ui::palette` doesn't
/// depend on `atlas-ui::remote` — that would introduce a cycle inside
/// the same crate module tree once `palette` is used from `shell`.
fn saved_server_uri_display(server: &atlas_config::servers::SavedServer) -> String {
    let mut s = String::new();
    s.push_str(server.backend.scheme());
    s.push_str("://");
    if let Some(user) = &server.username {
        s.push_str(user);
        s.push('@');
    }
    s.push_str(&server.address);
    if let Some(port) = server.port {
        s.push(':');
        s.push_str(&port.to_string());
    }
    if server.path.is_empty() {
        s.push('/');
    } else if !server.path.starts_with('/') {
        s.push('/');
        s.push_str(&server.path);
    } else {
        s.push_str(&server.path);
    }
    s
}

/// Return the small unicode glyph rendered next to a backend in the
/// palette + saved-servers modal. Thin wrapper around
/// [`atlas_core::BackendKind::glyph`] kept here so this module doesn't
/// grow a dependency on `remote::connect`.
fn backend_glyph(kind: atlas_core::BackendKind) -> &'static str {
    kind.glyph()
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_keymap::{ActionId, ActionMeta, ActionRegistry, Keymap};
    use std::time::Duration;

    struct VecSink(Vec<PaletteItem>);

    impl ItemSink for VecSink {
        fn push(&mut self, item: PaletteItem) {
            self.0.push(item);
        }
    }

    #[test]
    fn actions_source_populates_all_registered() {
        let mut registry = ActionRegistry::new();
        registry.register(ActionMeta {
            id: ActionId::new("test::FooBar"),
            title: String::from("Foo Bar"),
            description: None,
            contexts: vec![String::from("Global")],
        });
        registry.register(ActionMeta {
            id: ActionId::new("test::Baz"),
            title: String::from("Baz"),
            description: None,
            contexts: vec![String::from("Pane")],
        });

        let source = ActionsSource::new(Arc::new(registry), Arc::new(Keymap::with_defaults()));
        let mut sink = VecSink(Vec::new());
        source.populate(&mut sink);

        assert_eq!(sink.0.len(), 2);
        let ids: Vec<&str> = sink.0.iter().map(|item| item.id.as_str()).collect();
        assert!(ids.contains(&"test::FooBar"));
        assert!(ids.contains(&"test::Baz"));
    }

    #[test]
    fn actions_source_keybinding_subtitle() {
        // Build an explicit keymap so the expected chord doesn't depend
        // on the current OS's defaults table (macOS binds
        // `shift-cmd-p` → Toggle; Linux/Windows bind `ctrl-shift-p`).
        use atlas_keymap::{Binding, ChordSequence};
        let mut registry = ActionRegistry::new();
        registry.register(ActionMeta {
            id: ActionId::new("command_palette::Toggle"),
            title: String::from("Toggle Command Palette"),
            description: None,
            contexts: vec![String::from("Global")],
        });

        let chord = ChordSequence::from_str("shift-cmd-p").unwrap();
        let mut km = Keymap::empty();
        km.add_layer(
            "default",
            vec![Binding::new(
                chord.clone(),
                "Global",
                ActionId::new("command_palette::Toggle"),
            )],
        );

        let source = ActionsSource::new(Arc::new(registry), Arc::new(km));
        let mut sink = VecSink(Vec::new());
        source.populate(&mut sink);

        let item = sink.0.first().expect("should have one item");
        assert_eq!(item.subtitle, "shift-cmd-p");
    }

    #[test]
    fn bookmarks_source_populates_from_config() {
        let source = BookmarksSource::new(vec![
            (String::from("Home"), PathBuf::from("/home/user")),
            (String::from("Docs"), PathBuf::from("/home/user/Documents")),
        ]);
        let mut sink = VecSink(Vec::new());
        source.populate(&mut sink);

        assert_eq!(sink.0.len(), 2);
        assert_eq!(sink.0[0].title, "Home");
        assert_eq!(sink.0[0].subtitle, "/home/user");
        assert_eq!(sink.0[0].kind, PaletteItemKind::Path);
        assert_eq!(sink.0[1].title, "Docs");
    }

    #[test]
    fn walker_path_index_candidates_from_tempdir() {
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir should be created");
        let root = dir.path().to_path_buf();

        std::fs::write(root.join("alpha.txt"), b"").expect("alpha should be created");
        std::fs::write(root.join("beta.rs"), b"").expect("beta should be created");
        std::fs::create_dir(root.join("sub")).expect("subdir should be created");
        std::fs::write(root.join("sub/gamma.rs"), b"").expect("gamma should be created");

        let index = WalkerPathIndex::new(root);
        std::thread::sleep(Duration::from_millis(200));

        let results = index.candidates("alpha", 10);
        assert!(
            results
                .iter()
                .any(|path| path.file_name().and_then(|name| name.to_str()) == Some("alpha.txt")),
            "expected alpha.txt in results"
        );

        let all = index.candidates("", 100);
        assert!(!all.is_empty());
    }
}
