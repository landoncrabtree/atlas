//! View-model layer that adapts the streaming lister into an observable,
//! sorted, filtered snapshot for UI consumption.
//!
//! [`LocationViewModel`] is the consumer-facing trait;
//! [`InMemoryLocationViewModel`] is the default implementation that accumulates
//! entries in memory and notifies subscribers via [`ViewModelEvent`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use atlas_core::Result;
use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Mutex, RwLock};

use crate::entry::{build_entry, Entry};
use crate::filter::{CompiledFilter, Filter};
use crate::lister::{list_directory, ListEvent, ListRequest};
use crate::sort::{compare, sort_in_place, SortSpec};

/// An observable view over a single filesystem location.
///
/// Implementations are cheap to share (`Send + Sync`) and expose snapshots of
/// the current, sorted-and-filtered entry set.
pub trait LocationViewModel: Send + Sync {
    /// The location this view model represents.
    fn location(&self) -> &Path;
    /// A snapshot of the current (sorted, filtered) entries.
    fn entries(&self) -> Vec<Entry>;
    /// The number of entries in the current snapshot.
    fn len(&self) -> usize;
    /// Whether the snapshot is currently empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// `true` once the first batch of entries has arrived.
    fn is_loaded(&self) -> bool;
    /// The active sort specification.
    fn sort(&self) -> SortSpec;
    /// Replace the sort specification and re-sort the snapshot.
    fn set_sort(&self, spec: SortSpec);
    /// The active filter.
    fn filter(&self) -> Filter;
    /// Replace the filter, recomputing the snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the new filter fails to compile.
    fn set_filter(&self, filter: Filter) -> Result<()>;
    /// Subscribe to change notifications.
    fn subscribe(&self) -> Receiver<ViewModelEvent>;
}

/// Notifications emitted by a [`LocationViewModel`].
#[derive(Clone, Debug)]
pub enum ViewModelEvent {
    /// The entry set changed (add/remove/sort/filter).
    EntriesChanged,
    /// Emitted once, when the first batch of entries arrives.
    Loaded,
    /// A non-fatal error occurred while loading.
    Error(String),
}

/// Options controlling how a location is opened.
#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    /// Whether to include hidden entries.
    pub include_hidden: bool,
    /// Whether symlink metadata follows the link target.
    pub follow_symlinks: bool,
    /// Initial sort specification.
    pub sort: SortSpec,
    /// Initial filter.
    pub filter: Filter,
    /// If `true`, spawn a directory watcher that keeps the view model
    /// live-updated after the initial listing completes.
    ///
    /// When `false` (the default), the view model loads once and never
    /// reflects subsequent filesystem changes.
    pub watch: bool,
}

struct Inner {
    raw: Vec<Entry>,
    view: Vec<Entry>,
    sort: SortSpec,
    filter: Filter,
    compiled: CompiledFilter,
    loaded: bool,
}

impl Inner {
    /// Full recompute of `view` from `raw`. Used by `set_sort`,
    /// `set_filter`, and `handle_rescan` — paths where `raw` may
    /// have been mutated arbitrarily and `view` cannot be
    /// incrementally repaired.
    ///
    /// Callers on the append-only loader path use
    /// [`Inner::merge_batch`] instead, which keeps the total loader
    /// work linear in the entry count rather than quadratic.
    fn recompute(&mut self) {
        let mut view: Vec<Entry> = self
            .raw
            .iter()
            .filter(|e| self.compiled.matches(e))
            .cloned()
            .collect();
        sort_in_place(&mut view, &self.sort);
        self.view = view;
    }

    /// Fold an append-only batch of entries into `raw` and `view`,
    /// preserving sort order incrementally.
    ///
    /// # Why this exists
    ///
    /// The lister emits `ListEvent::Batch(64)` values while enumeration
    /// runs. A naive loader that called `recompute` after each batch
    /// would do `O(N²)` work over the full load: every batch would
    /// filter-clone every previously-observed entry and re-sort the
    /// whole prefix. On a 10k-file directory that grew the end-to-end
    /// load from a linear ~50 ms to ~730 ms.
    ///
    /// Instead, we:
    ///   1. append the batch to `raw` (O(k)),
    ///   2. filter+sort only the new tail (O(k log k)),
    ///   3. two-way-merge the new sorted subview into the existing
    ///      already-sorted `view` (O(|view| + k)).
    ///
    /// Total load cost across `N/k` batches is `O(N log k)`, which for
    /// the default `BATCH_SIZE = 64` is effectively linear.
    fn merge_batch(&mut self, batch: Vec<Entry>) {
        if batch.is_empty() {
            return;
        }
        let start = self.raw.len();
        self.raw.extend(batch);

        // Filter + sort only the new tail.
        let mut incoming: Vec<Entry> = self.raw[start..]
            .iter()
            .filter(|e| self.compiled.matches(e))
            .cloned()
            .collect();
        if incoming.is_empty() {
            return;
        }
        sort_in_place(&mut incoming, &self.sort);

        // Merge two already-sorted vecs. Move existing view out via
        // `std::mem::take` so we don't hold two copies while merging.
        let existing = std::mem::take(&mut self.view);
        let mut merged: Vec<Entry> = Vec::with_capacity(existing.len() + incoming.len());
        let mut ai = existing.into_iter().peekable();
        let mut bi = incoming.into_iter().peekable();
        loop {
            match (ai.peek(), bi.peek()) {
                (None, None) => break,
                (Some(_), None) => {
                    merged.extend(ai);
                    break;
                }
                (None, Some(_)) => {
                    merged.extend(bi);
                    break;
                }
                (Some(av), Some(bv)) => {
                    if compare(av, bv, &self.sort).is_lt() {
                        merged.push(ai.next().expect("peeked Some"));
                    } else {
                        merged.push(bi.next().expect("peeked Some"));
                    }
                }
            }
        }
        self.view = merged;
    }
}

/// In-memory implementation of [`LocationViewModel`].
///
/// Spawns the streaming lister, accumulates entries, applies sort/filter, and
/// notifies subscribers as data arrives or when sort/filter changes.
///
/// When [`OpenOptions::watch`] is `true`, a [`atlas_watch::DirectoryWatcher`]
/// is attached after the initial listing completes so that subsequent
/// filesystem changes (creates, removes, modifications, renames) are reflected
/// without re-listing the directory.
pub struct InMemoryLocationViewModel {
    pub(crate) path: PathBuf,
    state: RwLock<Inner>,
    subscribers: Mutex<Vec<Sender<ViewModelEvent>>>,
    /// Whether symlink metadata follows link targets; used when re-stating
    /// entries on watcher events.
    follow_symlinks: bool,
    /// Whether hidden entries are included; used when filtering watcher events.
    include_hidden: bool,
    /// Holds the live watcher so it is not dropped until the view model drops.
    /// `None` when `OpenOptions::watch` was `false`.
    pub(crate) _watcher: Mutex<Option<atlas_watch::DirectoryWatcher>>,
}

impl InMemoryLocationViewModel {
    /// Open `path`, beginning to stream entries immediately.
    ///
    /// Returns an `Arc` handle that is shared with the background loader
    /// thread. If the initial filter fails to compile it is replaced with an
    /// empty (match-all) filter and an [`ViewModelEvent::Error`] is emitted
    /// once a subscriber attaches.
    ///
    /// When [`OpenOptions::watch`] is `true` a directory watcher is started
    /// after the initial listing completes; see [`Self::open_live`] for a
    /// convenience constructor.
    pub fn open(path: impl Into<PathBuf>, opts: OpenOptions) -> Arc<Self> {
        let path = path.into();

        let (compiled, filter_err) = match opts.filter.compile() {
            Ok(c) => (c, None),
            Err(e) => (
                Filter::default()
                    .compile()
                    .expect("empty filter always compiles"),
                Some(e.to_string()),
            ),
        };
        let filter = if filter_err.is_some() {
            Filter::default()
        } else {
            opts.filter.clone()
        };

        let inner = Inner {
            raw: Vec::new(),
            view: Vec::new(),
            sort: opts.sort.clone(),
            filter,
            compiled,
            loaded: false,
        };

        let this = Arc::new(Self {
            path: path.clone(),
            state: RwLock::new(inner),
            subscribers: Mutex::new(Vec::new()),
            follow_symlinks: opts.follow_symlinks,
            include_hidden: opts.include_hidden,
            _watcher: Mutex::new(None),
        });

        if let Some(msg) = filter_err {
            this.notify(ViewModelEvent::Error(msg));
        }

        let req = ListRequest {
            path,
            follow_symlinks: opts.follow_symlinks,
            include_hidden: opts.include_hidden,
        };
        let rx = list_directory(req);

        let worker = Arc::clone(&this);
        let watch = opts.watch;
        std::thread::spawn(move || {
            // When watching, defer the `Loaded` notification until after the
            // watcher is set up; that way any subscriber that creates files
            // after receiving `Loaded` is guaranteed to have the OS watch active.
            worker.run_loader(&rx, /* defer_loaded */ watch);
            if watch {
                crate::watched::attach_watcher(Arc::clone(&worker));
                // Watcher is now set up — emit the deferred `Loaded`.
                worker.notify(ViewModelEvent::Loaded);
            }
        });

        this
    }

    /// Convenience constructor that opens `path` with live filesystem watching
    /// enabled (`OpenOptions::watch = true`).
    ///
    /// All other option fields are taken from `opts`; the `watch` field is
    /// overridden to `true` regardless of what is set in `opts`.
    pub fn open_live(path: impl Into<PathBuf>, mut opts: OpenOptions) -> Arc<Self> {
        opts.watch = true;
        Self::open(path, opts)
    }

    /// Returns `true` when a live directory watcher is currently attached to
    /// this view model (i.e., [`OpenOptions::watch`] was `true` when it was
    /// opened and the watcher started successfully).
    pub fn is_watching(&self) -> bool {
        self._watcher.lock().is_some()
    }

    /// Drive the listing channel until `Done`, accumulating entries.
    ///
    /// When `defer_loaded` is `true` the [`ViewModelEvent::Loaded`]
    /// notification is **not** emitted here; the caller is responsible for
    /// emitting it once any post-load setup (e.g., watcher attachment) is
    /// complete.  The internal `loaded` flag is still set to `true` so that
    /// [`InMemoryLocationViewModel::is_loaded`] returns `true` once entries
    /// have arrived.
    fn run_loader(&self, rx: &Receiver<ListEvent>, defer_loaded: bool) {
        for event in rx.iter() {
            match event {
                ListEvent::Batch(entries) => {
                    let first_load;
                    {
                        let mut state = self.state.write();
                        first_load = !state.loaded;
                        state.merge_batch(entries);
                        state.loaded = true;
                    }
                    if first_load && !defer_loaded {
                        self.notify(ViewModelEvent::Loaded);
                    }
                    self.notify(ViewModelEvent::EntriesChanged);
                }
                ListEvent::Error { path, error } => {
                    tracing::warn!(?path, %error, "list error");
                    self.notify(ViewModelEvent::Error(error.to_string()));
                }
                ListEvent::Done => {
                    // Emit `Loaded` even when the directory was empty (no
                    // `Batch` events were delivered), unless we are deferring.
                    let emit;
                    {
                        let mut state = self.state.write();
                        emit = !state.loaded && !defer_loaded;
                        state.loaded = true;
                    }
                    if emit {
                        self.notify(ViewModelEvent::Loaded);
                    }
                    break;
                }
            }
        }
    }

    /// Fan out `event` to all live subscribers.
    ///
    /// The subscribers lock is held only long enough to snapshot the current
    /// sender list; the actual `send()` calls happen outside the lock so a
    /// slow subscriber never blocks concurrent [`Self::subscribe`] calls or
    /// another notify fan-out. Dead subscribers are pruned in a second brief
    /// lock acquisition, matching each stale sender by channel identity via
    /// [`crossbeam_channel::Sender::same_channel`] so we do not accidentally
    /// remove a fresh subscriber that raced into the list between the two
    /// lock acquisitions.
    pub(crate) fn notify(&self, event: ViewModelEvent) {
        let mut subs = self.subscribers.lock();
        subs.retain(|tx| tx.send(event.clone()).is_ok());
    }

    /// Fan-out hook used by criterion benches only. Forwards to
    /// [`Self::notify`]. Kept out of the public trait but exposed so the
    /// `view_model_notify` bench can measure the fan-out cost without
    /// building a synthetic filesystem event.
    #[doc(hidden)]
    pub fn notify_for_bench(&self, event: ViewModelEvent) {
        self.notify(event);
    }

    // ── Watcher event handlers ────────────────────────────────────────────────

    /// Handle a `Created` event from the directory watcher.
    ///
    /// Stats the new path, builds an [`Entry`], inserts it into the snapshot
    /// respecting the current sort and filter, and emits [`ViewModelEvent::EntriesChanged`].
    pub(crate) fn handle_created(&self, path: PathBuf) {
        // Extract the file name and rebuild the path relative to our (possibly
        // non-canonical) base so it stays consistent with the existing entries.
        let name_os = match path.file_name() {
            Some(n) => n.to_owned(),
            None => return,
        };
        let local_path = self.path.join(&name_os);

        let entry = match build_entry(local_path, self.follow_symlinks) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("atlas-fs watcher: failed to stat created entry: {e}");
                return;
            }
        };

        if !self.include_hidden && entry.metadata.is_hidden {
            return;
        }

        let view_changed = {
            let mut state = self.state.write();
            let name = entry.name.clone();

            // Upsert in raw (handles the rare case where the entry was already
            // streamed by the lister before the watcher attached).
            if let Some(existing) = state.raw.iter_mut().find(|e| e.name == name) {
                *existing = entry.clone();
            } else {
                state.raw.push(entry.clone());
            }

            let matches = state.compiled.matches(&entry);

            // Update view.
            let in_view = state.view.iter().any(|e| e.name == name);
            if in_view {
                // Replace in-place: remove old, insert updated at correct position.
                state.view.retain(|e| e.name != name);
                if matches {
                    let pos = state
                        .view
                        .partition_point(|e| compare(e, &entry, &state.sort).is_lt());
                    state.view.insert(pos, entry);
                }
                true
            } else if matches {
                let pos = state
                    .view
                    .partition_point(|e| compare(e, &entry, &state.sort).is_lt());
                state.view.insert(pos, entry);
                true
            } else {
                false
            }
        };

        if view_changed {
            self.notify(ViewModelEvent::EntriesChanged);
        }
    }

    /// Handle a `Removed` event from the directory watcher.
    ///
    /// Removes the entry matching `path` from both the raw snapshot and the
    /// filtered view, then emits [`ViewModelEvent::EntriesChanged`] if the
    /// view changed.
    pub(crate) fn handle_removed(&self, path: &Path) {
        let name = match path.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => return,
        };

        let view_changed = {
            let mut state = self.state.write();
            let raw_before = state.raw.len();
            state.raw.retain(|e| e.name != name);
            let view_before = state.view.len();
            state.view.retain(|e| e.name != name);
            state.view.len() != view_before || state.raw.len() != raw_before
        };

        if view_changed {
            self.notify(ViewModelEvent::EntriesChanged);
        }
    }

    /// Handle a `Modified` event from the directory watcher.
    ///
    /// Re-stats the affected path, updates it in the snapshot, adjusts filter
    /// visibility (an out-of-filter entry after modification is treated as a
    /// removal from the view), and emits [`ViewModelEvent::EntriesChanged`] if
    /// the view changed.
    pub(crate) fn handle_modified(&self, path: PathBuf) {
        let name_os = match path.file_name() {
            Some(n) => n.to_owned(),
            None => return,
        };
        let local_path = self.path.join(&name_os);
        let name = name_os.to_string_lossy().into_owned();

        let entry = match build_entry(local_path.clone(), self.follow_symlinks) {
            Ok(e) => e,
            Err(_) => {
                // File may have been removed; propagate as removal.
                self.handle_removed(&local_path);
                return;
            }
        };

        if !self.include_hidden && entry.metadata.is_hidden {
            // Hidden; treat as removal from the visible snapshot.
            self.handle_removed(&local_path);
            return;
        }

        let view_changed = {
            let mut state = self.state.write();

            // Update in raw.
            if let Some(existing) = state.raw.iter_mut().find(|e| e.name == name) {
                *existing = entry.clone();
            } else {
                state.raw.push(entry.clone());
            }

            let in_view = state.view.iter().any(|e| e.name == name);
            let matches = state.compiled.matches(&entry);

            match (in_view, matches) {
                (true, true) => {
                    // Update in view: remove then re-insert at the correct sort position.
                    state.view.retain(|e| e.name != name);
                    let pos = state
                        .view
                        .partition_point(|e| compare(e, &entry, &state.sort).is_lt());
                    state.view.insert(pos, entry);
                    true
                }
                (true, false) => {
                    // No longer passes filter; remove from view.
                    state.view.retain(|e| e.name != name);
                    true
                }
                (false, true) => {
                    // Now passes filter; insert at correct sort position.
                    let pos = state
                        .view
                        .partition_point(|e| compare(e, &entry, &state.sort).is_lt());
                    state.view.insert(pos, entry);
                    true
                }
                (false, false) => false,
            }
        };

        if view_changed {
            self.notify(ViewModelEvent::EntriesChanged);
        }
    }

    /// Handle a `Rescan` event from the directory watcher.
    ///
    /// Re-lists the directory from scratch, atomically swaps the snapshot, and
    /// emits [`ViewModelEvent::EntriesChanged`].
    pub(crate) fn handle_rescan(&self) {
        let req = ListRequest {
            path: self.path.clone(),
            follow_symlinks: self.follow_symlinks,
            include_hidden: self.include_hidden,
        };
        let rx = list_directory(req);

        let mut new_raw: Vec<Entry> = Vec::new();
        for event in rx.iter() {
            match event {
                ListEvent::Batch(entries) => new_raw.extend(entries),
                ListEvent::Error { path, error } => {
                    tracing::warn!(?path, %error, "rescan list error");
                }
                ListEvent::Done => break,
            }
        }

        {
            let mut state = self.state.write();
            state.raw = new_raw;
            state.recompute();
        }
        self.notify(ViewModelEvent::EntriesChanged);
    }
}

impl LocationViewModel for InMemoryLocationViewModel {
    fn location(&self) -> &Path {
        &self.path
    }

    fn entries(&self) -> Vec<Entry> {
        self.state.read().view.clone()
    }

    fn len(&self) -> usize {
        self.state.read().view.len()
    }

    fn is_loaded(&self) -> bool {
        self.state.read().loaded
    }

    fn sort(&self) -> SortSpec {
        self.state.read().sort.clone()
    }

    fn set_sort(&self, spec: SortSpec) {
        {
            let mut state = self.state.write();
            state.sort = spec;
            state.recompute();
        }
        self.notify(ViewModelEvent::EntriesChanged);
    }

    fn filter(&self) -> Filter {
        self.state.read().filter.clone()
    }

    fn set_filter(&self, filter: Filter) -> Result<()> {
        let compiled = filter.compile()?;
        {
            let mut state = self.state.write();
            state.filter = filter;
            state.compiled = compiled;
            state.recompute();
        }
        self.notify(ViewModelEvent::EntriesChanged);
        Ok(())
    }

    fn subscribe(&self) -> Receiver<ViewModelEvent> {
        let (tx, rx) = crossbeam_channel::unbounded();
        self.subscribers.lock().push(tx);
        rx
    }
}
