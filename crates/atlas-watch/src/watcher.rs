//! [`DirectoryWatcher`] and [`WatcherBuilder`] — the main entry points.

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use crossbeam_channel::{Receiver, Sender};
use dashmap::DashMap;
use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer_opt, DebounceEventResult, Debouncer, RecommendedCache};
use smallvec::SmallVec;

use atlas_core::Result;

use crate::{
    event::{FileEvent, FileEventKind},
    ids::RootId,
};

// ──────────────────────────────────────────────────────────────────────────────
// Internal command sent to the background thread
// ──────────────────────────────────────────────────────────────────────────────

enum WatchCmd {
    Watch { path: PathBuf, mode: RecursiveMode },
    Unwatch(PathBuf),
}

// ──────────────────────────────────────────────────────────────────────────────
// Builder
// ──────────────────────────────────────────────────────────────────────────────

/// Builder for [`DirectoryWatcher`].
///
/// # Example
///
/// ```no_run
/// use atlas_watch::WatcherBuilder;
/// use std::time::Duration;
///
/// let (watcher, rx) = WatcherBuilder::new()
///     .debounce(Duration::from_millis(300))
///     .recursive(true)
///     .follow_symlinks(false)
///     .build()
///     .expect("watcher init failed");
/// ```
pub struct WatcherBuilder {
    debounce: Duration,
    recursive: bool,
    follow_symlinks: bool,
}

impl Default for WatcherBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl WatcherBuilder {
    /// Create a builder with sensible defaults:
    /// 200 ms debounce, recursive, no symlink follow.
    pub fn new() -> Self {
        Self {
            debounce: Duration::from_millis(200),
            recursive: true,
            follow_symlinks: false,
        }
    }

    /// Override the debounce window.  Rapid-fire events within this window are
    /// coalesced into a single [`FileEvent`].
    pub fn debounce(mut self, d: Duration) -> Self {
        self.debounce = d;
        self
    }

    /// Whether to watch sub-directories recursively (default: `true`).
    pub fn recursive(mut self, b: bool) -> Self {
        self.recursive = b;
        self
    }

    /// Whether to follow symbolic links when descending into watched trees
    /// (default: `false`).
    ///
    /// Note: support for this option is platform-dependent.  On macOS FSEvents
    /// the watcher follows symlinks regardless of this setting.
    pub fn follow_symlinks(mut self, b: bool) -> Self {
        self.follow_symlinks = b;
        self
    }

    /// Consume the builder and return a `(DirectoryWatcher, Receiver<FileEvent>)` pair.
    ///
    /// The [`Receiver`] is an ordinary `crossbeam_channel` receiver; it may be
    /// used from any thread.  Events are dropped while the watcher is
    /// [`paused`](DirectoryWatcher::pause).
    pub fn build(self) -> Result<(DirectoryWatcher, Receiver<FileEvent>)> {
        let (event_tx, event_rx) = crossbeam_channel::unbounded::<FileEvent>();
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<WatchCmd>();

        let roots: Arc<DashMap<RootId, PathBuf>> = Arc::new(DashMap::new());
        let next_id = Arc::new(AtomicU64::new(0));
        let paused = Arc::new(AtomicBool::new(false));

        let roots_bg = roots.clone();
        let paused_bg = paused.clone();
        let debounce = self.debounce;
        let follow_symlinks = self.follow_symlinks;

        let handle = thread::Builder::new()
            .name("atlas-watch".to_owned())
            .spawn(move || {
                run_thread(
                    cmd_rx,
                    event_tx,
                    roots_bg,
                    paused_bg,
                    debounce,
                    follow_symlinks,
                );
            })
            .map_err(atlas_core::AtlasError::from)?;

        let watcher = DirectoryWatcher {
            cmd_tx: Some(cmd_tx),
            roots,
            next_id,
            paused,
            recursive: self.recursive,
            thread: Some(handle),
        };

        Ok((watcher, event_rx))
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// DirectoryWatcher
// ──────────────────────────────────────────────────────────────────────────────

/// A cross-platform, debounced watcher for one or more directory trees.
///
/// Instantiate with [`WatcherBuilder`].  Call [`add_root`](Self::add_root) to
/// begin watching directories.  Incoming events arrive on the
/// `Receiver<FileEvent>` returned by [`WatcherBuilder::build`].
///
/// # Pause / resume
///
/// Call [`pause`](Self::pause) before bulk operations (e.g., mass copy) to
/// suppress event storms.  Events that occur while paused are **dropped**, not
/// buffered.  Call [`resume`](Self::resume) when the operation completes.
///
/// # Shutdown
///
/// Call [`shutdown`](Self::shutdown) to stop the background thread cleanly.
/// Dropping the watcher without calling `shutdown` is also safe — the
/// background thread will exit when it detects the command channel is closed.
pub struct DirectoryWatcher {
    /// `Option` so we can `take()` it without a partial-move conflict when
    /// both `cmd_tx` and `thread` need to be finalized during shutdown.
    cmd_tx: Option<Sender<WatchCmd>>,
    roots: Arc<DashMap<RootId, PathBuf>>,
    next_id: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
    /// Whether new roots are watched recursively.
    recursive: bool,
    thread: Option<thread::JoinHandle<()>>,
}

impl DirectoryWatcher {
    /// Begin watching `path` (canonicalized) and return its [`RootId`].
    ///
    /// If `path` cannot be canonicalized or the underlying watcher rejects it,
    /// an error is returned and no root is registered.
    ///
    /// # macOS note
    ///
    /// `/var` is a symlink to `/private/var` on macOS; this method canonicalizes
    /// the path so that event path comparison works correctly across that
    /// symlink boundary.
    pub fn add_root(&self, path: PathBuf) -> Result<RootId> {
        let canonical = path
            .canonicalize()
            .map_err(|e| atlas_core::AtlasError::io(Some(path.clone()), e))?;

        let id = RootId(self.next_id.fetch_add(1, Ordering::SeqCst));
        self.roots.insert(id, canonical.clone());

        let mode = if self.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };

        let send_result = self
            .cmd_tx
            .as_ref()
            .ok_or_else(|| atlas_core::AtlasError::Other(anyhow::anyhow!("watcher is shut down")))
            .and_then(|tx| {
                tx.send(WatchCmd::Watch {
                    path: canonical,
                    mode,
                })
                .map_err(|_| {
                    atlas_core::AtlasError::Other(anyhow::anyhow!(
                        "watcher background thread has exited"
                    ))
                })
            });

        if send_result.is_err() {
            self.roots.remove(&id);
        }

        send_result.map(|()| id)
    }

    /// Stop watching the root identified by `root`.
    ///
    /// Future events from this root's directory tree will be silently discarded.
    /// Returns an error if `root` is not currently registered.
    pub fn remove_root(&self, root: RootId) -> Result<()> {
        let path = self
            .roots
            .remove(&root)
            .map(|(_, p)| p)
            .ok_or_else(|| atlas_core::AtlasError::Other(anyhow::anyhow!("unknown root id")))?;

        self.cmd_tx
            .as_ref()
            .ok_or_else(|| atlas_core::AtlasError::Other(anyhow::anyhow!("watcher is shut down")))
            .and_then(|tx| {
                tx.send(WatchCmd::Unwatch(path)).map_err(|_| {
                    atlas_core::AtlasError::Other(anyhow::anyhow!(
                        "watcher background thread has exited"
                    ))
                })
            })
    }

    /// Return a snapshot of all currently watched roots as `(id, canonical_path)` pairs.
    pub fn roots(&self) -> Vec<(RootId, PathBuf)> {
        self.roots
            .iter()
            .map(|e| (*e.key(), e.value().clone()))
            .collect()
    }

    /// Pause event delivery.
    ///
    /// While paused, incoming OS events are **dropped** rather than forwarded to
    /// the receiver.  This is useful during bulk operations to avoid flooding the
    /// consumer with transient intermediate states.
    pub fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    /// Resume event delivery after a [`pause`](Self::pause).
    pub fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
    }

    /// Returns `true` if the watcher is currently paused.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Shut down the background thread and release all OS watch handles.
    ///
    /// This consumes `self` and blocks until the background thread exits.
    pub fn shutdown(mut self) {
        // Drop the sender first so the background thread's recv loop unblocks.
        drop(self.cmd_tx.take());
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

impl Drop for DirectoryWatcher {
    fn drop(&mut self) {
        // Signal the background thread (in case shutdown() was not called).
        drop(self.cmd_tx.take());
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Background thread
// ──────────────────────────────────────────────────────────────────────────────

fn run_thread(
    cmd_rx: Receiver<WatchCmd>,
    event_tx: Sender<FileEvent>,
    roots: Arc<DashMap<RootId, PathBuf>>,
    paused: Arc<AtomicBool>,
    debounce: Duration,
    follow_symlinks: bool,
) {
    let cb = {
        let event_tx = event_tx.clone();
        let roots = roots.clone();
        let paused = paused.clone();
        move |result: DebounceEventResult| {
            if paused.load(Ordering::Relaxed) {
                return;
            }
            match result {
                Ok(events) => {
                    for de in events {
                        translate_and_send(&roots, &event_tx, de);
                    }
                }
                Err(errors) => {
                    for err in errors {
                        translate_error_and_send(&roots, &event_tx, err);
                    }
                }
            }
        }
    };

    let config = notify::Config::default().with_follow_symlinks(follow_symlinks);

    let mut debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache> =
        match new_debouncer_opt::<_, notify::RecommendedWatcher, RecommendedCache>(
            debounce,
            None,
            cb,
            RecommendedCache::new(),
            config,
        ) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("atlas-watch: failed to create debouncer: {e}");
                return;
            }
        };

    // Process commands until the sender end is dropped (shutdown) or errors out.
    for cmd in cmd_rx {
        match cmd {
            WatchCmd::Watch { path, mode } => {
                if let Err(e) = debouncer.watch(&path, mode) {
                    tracing::warn!("atlas-watch: failed to watch {path:?}: {e}");
                }
            }
            WatchCmd::Unwatch(path) => {
                if let Err(e) = debouncer.unwatch(&path) {
                    tracing::warn!("atlas-watch: failed to unwatch {path:?}: {e}");
                }
            }
        }
    }
    // `debouncer` drops here, stopping notify's internal thread.
}

// ──────────────────────────────────────────────────────────────────────────────
// Event translation helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Find the most-specific (longest-prefix) root that is a prefix of `path`.
///
/// When the user watches both `/a` and `/a/b`, an event in `/a/b/c` must be
/// attributed to `/a/b` — the more specific root — rather than to `/a`.
/// Using the longest prefix ensures each event lands in the intended sub-tree.
pub(crate) fn root_for_path(roots: &DashMap<RootId, PathBuf>, path: &Path) -> Option<RootId> {
    roots
        .iter()
        .filter(|entry| path.starts_with(entry.value()))
        .max_by_key(|entry| entry.value().components().count())
        .map(|entry| *entry.key())
}

fn translate_and_send(
    roots: &DashMap<RootId, PathBuf>,
    tx: &Sender<FileEvent>,
    de: notify_debouncer_full::DebouncedEvent,
) {
    use notify::event::{ModifyKind, RenameMode};
    use notify::EventKind;

    let instant = de.time;
    let event = &de.event;

    let (kind, paths): (FileEventKind, SmallVec<[PathBuf; 2]>) = match &event.kind {
        EventKind::Create(_) => (
            FileEventKind::Created,
            event.paths.iter().cloned().collect(),
        ),

        EventKind::Remove(_) => (
            FileEventKind::Removed,
            event.paths.iter().cloned().collect(),
        ),

        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
            // Both from and to are available — emit a single Renamed event with
            // paths = [old, new].
            (
                FileEventKind::Renamed,
                event.paths.iter().cloned().collect(),
            )
        }

        EventKind::Modify(ModifyKind::Name(_)) => {
            // Partial rename (From-only or To-only): the debouncer couldn't match
            // the pair.  Fall back to Modified so the consumer at least knows
            // something changed at this path.
            (
                FileEventKind::Modified,
                event.paths.iter().cloned().collect(),
            )
        }

        EventKind::Modify(_) => (
            FileEventKind::Modified,
            event.paths.iter().cloned().collect(),
        ),

        // `Any` is the catch-all / overflow sentinel — ask consumer to rescan.
        EventKind::Any | EventKind::Other => {
            (FileEventKind::Rescan, event.paths.iter().cloned().collect())
        }

        // Access events (open/close/read) are not relevant for a file explorer.
        EventKind::Access(_) => return,
    };

    // Determine the root from the first affected path.
    let root = paths
        .first()
        .and_then(|p| root_for_path(roots, p))
        .or_else(|| roots.iter().next().map(|e| *e.key()));

    let root = match root {
        Some(r) => r,
        None => {
            tracing::warn!("atlas-watch: received event with no matching root: {paths:?}");
            return;
        }
    };

    if tx
        .send(FileEvent {
            root,
            kind,
            paths,
            instant,
        })
        .is_err()
    {
        tracing::warn!("atlas-watch: event receiver dropped, event discarded");
    }
}

fn translate_error_and_send(
    roots: &DashMap<RootId, PathBuf>,
    tx: &Sender<FileEvent>,
    err: notify::Error,
) {
    tracing::warn!("atlas-watch: backend error: {err}");

    let paths: SmallVec<[PathBuf; 2]> = err.paths.iter().cloned().collect();

    let root = paths
        .first()
        .and_then(|p| root_for_path(roots, p))
        .or_else(|| roots.iter().next().map(|e| *e.key()));

    let root = match root {
        Some(r) => r,
        None => return,
    };

    if tx
        .send(FileEvent {
            root,
            kind: FileEventKind::Error,
            paths,
            instant: std::time::Instant::now(),
        })
        .is_err()
    {
        tracing::warn!("atlas-watch: event receiver dropped, error event discarded");
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Unit tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_for_path_longest_prefix() {
        let map: DashMap<RootId, PathBuf> = DashMap::new();
        let id_a = RootId(0);
        let id_b = RootId(1);
        map.insert(id_a, PathBuf::from("/a"));
        map.insert(id_b, PathBuf::from("/a/b"));

        // /a/b/c → longest prefix is /a/b
        assert_eq!(root_for_path(&map, Path::new("/a/b/c")), Some(id_b));

        // /a/x → only /a matches
        assert_eq!(root_for_path(&map, Path::new("/a/x")), Some(id_a));

        // /c → no root matches
        assert_eq!(root_for_path(&map, Path::new("/c")), None);
    }
}
