//! View-model layer that adapts the streaming lister into an observable,
//! sorted, filtered snapshot for UI consumption.
//!
//! [`LocationViewModel`] is the consumer-facing trait;
//! [`InMemoryLocationViewModel`] is the default implementation that accumulates
//! entries in memory and notifies subscribers via [`ViewModelEvent`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use atlas_core::Result;
use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Mutex, RwLock};
use smallvec::SmallVec;

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
    fn entries(&self) -> Arc<[Entry]>;
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
    /// Name → index into `raw`. Maintained alongside `raw` on every
    /// mutation so watcher event handlers can locate an existing entry
    /// in O(1) instead of an O(N) linear scan.
    ///
    /// A directory can only contain one entry per name at the OS level,
    /// so a plain `HashMap` (last-write-wins) is safe: transient
    /// concurrent create + rename events can look like duplicates, and
    /// the map tolerates that by simply overwriting the previous
    /// mapping rather than panicking.
    raw_index: HashMap<String, usize>,
    view: Vec<Entry>,
    sort: SortSpec,
    filter: Filter,
    compiled: CompiledFilter,
    loaded: bool,
}

impl Inner {
    /// Rebuild `raw_index` from scratch. Used by paths that replace
    /// `raw` wholesale (rescan) and by the initial bulk-populate path.
    fn rebuild_raw_index(&mut self) {
        self.raw_index.clear();
        self.raw_index.reserve(self.raw.len());
        for (i, entry) in self.raw.iter().enumerate() {
            self.raw_index.insert(entry.name.clone(), i);
        }
    }

    /// Upsert `entry` into `raw` and keep `raw_index` consistent.
    /// Returns `true` when the entry was newly inserted, `false` when
    /// it replaced an existing entry with the same name.
    fn upsert_raw(&mut self, entry: Entry) -> bool {
        if let Some(&idx) = self.raw_index.get(&entry.name) {
            self.raw[idx] = entry;
            false
        } else {
            let name = entry.name.clone();
            let idx = self.raw.len();
            self.raw.push(entry);
            self.raw_index.insert(name, idx);
            true
        }
    }

    /// Remove and return the raw entry with `name`, keeping
    /// `raw_index` consistent. Uses `swap_remove` so the operation is
    /// O(1); the moved-to-slot entry's index is patched in the map.
    fn remove_raw(&mut self, name: &str) -> Option<Entry> {
        let idx = self.raw_index.remove(name)?;
        let last_idx = self.raw.len() - 1;
        let removed = self.raw.swap_remove(idx);
        if idx != last_idx {
            // The last element moved into slot `idx`. Fix its index in
            // the map so future lookups still find it.
            let moved_name = self.raw[idx].name.clone();
            self.raw_index.insert(moved_name, idx);
        }
        Some(removed)
    }

    /// Locate the position of `target` in the sorted `view` via a
    /// binary search that leans on `compare`'s stable name-tie-break:
    /// two entries only compare equal when their names match, so the
    /// entry (if present) lives at `partition_point(...)`.
    fn view_position_of(&self, target: &Entry) -> Option<usize> {
        let pos = self
            .view
            .partition_point(|e| compare(e, target, &self.sort).is_lt());
        if pos < self.view.len() && self.view[pos].name == target.name {
            Some(pos)
        } else {
            None
        }
    }

    /// Insert `entry` into `view` at the correct sort position and
    /// return the inserted index.
    fn view_insert_sorted(&mut self, entry: Entry) -> usize {
        let pos = self
            .view
            .partition_point(|e| compare(e, &entry, &self.sort).is_lt());
        self.view.insert(pos, entry);
        pos
    }
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
        self.raw_index.reserve(batch.len());
        for (i, entry) in batch.iter().enumerate() {
            // Watcher upserts may have raced ahead of the lister and
            // planted an entry with the same name already; overwrite
            // the mapping so future lookups target the latest slot.
            self.raw_index.insert(entry.name.clone(), start + i);
        }
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
    subscribers: Mutex<Arc<[Sender<ViewModelEvent>]>>,
    /// Cached snapshot of the current sorted+filtered view. Populated
    /// lazily by [`Self::entries`] on the first read after a mutation
    /// and reused until the next mutation.
    ///
    /// Publishing is O(N) in the view size — one `Arc::from` allocation
    /// plus N `Entry` clones. Making it lazy so it runs at most once
    /// per batch of mutations (rather than once per mutation) is the
    /// key scaling win of the watcher-burst path: a 1000-event burst
    /// on a 10k-entry pane goes from 1000 × O(10k) republish work to
    /// a single O(10k) publish on the next read.
    ///
    /// The wrapping `Mutex` is only held long enough to hand out or
    /// replace an `Arc` clone — a single atomic op. Reads never see a
    /// mid-mutation state because mutations invalidate the flag first
    /// and this lock is only touched from `entries()`.
    published: Mutex<Arc<[Entry]>>,
    /// Set to `true` by every mutation and cleared by
    /// [`Self::entries`] once it has refreshed [`Self::published`]
    /// from the current [`Inner::view`]. Ordering is `Release` on
    /// invalidation and `Acquire` on the fast-path read so the
    /// snapshot store is visible to whichever thread rebuilds next.
    published_dirty: AtomicBool,
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
            raw_index: HashMap::new(),
            view: Vec::new(),
            sort: opts.sort.clone(),
            filter,
            compiled,
            loaded: false,
        };

        let this = Arc::new(Self {
            path: path.clone(),
            state: RwLock::new(inner),
            subscribers: Mutex::new(Arc::from(
                Vec::<Sender<ViewModelEvent>>::new().into_boxed_slice(),
            )),
            published: Mutex::new(Arc::from(Vec::<Entry>::new().into_boxed_slice()) as Arc<[Entry]>),
            published_dirty: AtomicBool::new(false),
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
                    self.invalidate_published();
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
    /// The subscribers list is stored as an immutable `Arc<[Sender]>`; we
    /// snapshot the current list under a brief `Mutex` acquisition (one
    /// atomic Arc-increment), drop the lock, then iterate and send outside
    /// the lock. A slow subscriber therefore never blocks concurrent
    /// [`Self::subscribe`] calls or another notify fan-out.
    ///
    /// Dead subscribers are pruned in a second brief lock acquisition,
    /// identified by channel identity via
    /// [`crossbeam_channel::Sender::same_channel`] so we do not accidentally
    /// remove a fresh subscriber that raced into the list between the two
    /// lock acquisitions.
    pub(crate) fn notify(&self, event: ViewModelEvent) {
        let subs: Arc<[Sender<ViewModelEvent>]> = {
            let guard = self.subscribers.lock();
            if guard.is_empty() {
                return;
            }
            Arc::clone(&*guard)
        };

        let mut dead: SmallVec<[Sender<ViewModelEvent>; 4]> = SmallVec::new();
        for tx in subs.iter() {
            if tx.send(event.clone()).is_err() {
                dead.push(tx.clone());
            }
        }

        if !dead.is_empty() {
            let mut owned = self.subscribers.lock();
            let filtered: Vec<Sender<ViewModelEvent>> = owned
                .iter()
                .filter(|tx| !dead.iter().any(|d| d.same_channel(tx)))
                .cloned()
                .collect();
            *owned = Arc::from(filtered.into_boxed_slice());
        }
    }

    /// Mark [`Self::published`] as stale. Every mutation path that
    /// touches `view` must call this before releasing the state lock;
    /// the next [`Self::entries`] read will rebuild `published`.
    fn invalidate_published(&self) {
        self.published_dirty.store(true, Ordering::Release);
    }

    /// Fan-out hook used by criterion benches only. Forwards to
    /// [`Self::notify`]. Kept out of the public trait but exposed so the
    /// `view_model_notify` bench can measure the fan-out cost without
    /// building a synthetic filesystem event.
    #[doc(hidden)]
    pub fn notify_for_bench(&self, event: ViewModelEvent) {
        self.notify(event);
    }

    /// Bench hook forwarding to [`Self::handle_created`]. `#[doc(hidden)]`;
    /// exists only so the `view_model_watcher` bench can drive the
    /// handler without going through the real notify/debouncer plumbing.
    #[doc(hidden)]
    pub fn handle_created_for_bench(&self, path: PathBuf) {
        self.handle_created(path);
    }

    /// Bench hook forwarding to [`Self::handle_removed`].
    #[doc(hidden)]
    pub fn handle_removed_for_bench(&self, path: PathBuf) {
        self.handle_removed(&path);
    }

    /// Bench hook forwarding to [`Self::handle_modified`].
    #[doc(hidden)]
    pub fn handle_modified_for_bench(&self, path: PathBuf) {
        self.handle_modified(path);
    }

    // ── Watcher event handlers ────────────────────────────────────────────────

    /// Handle a `Created` event from the directory watcher.
    ///
    /// Stats the new path, builds an [`Entry`], inserts it into the snapshot
    /// respecting the current sort and filter, and emits
    /// [`ViewModelEvent::EntriesChanged`].
    ///
    /// Uses `raw_index` to locate any pre-existing entry with the same name
    /// in O(1), and `view_position_of` for the sorted view lookup in
    /// O(log N). Only the vec-shift on view insert/remove remains O(N);
    /// the previous linear scans of `raw` and `view` are gone.
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

            // Upsert in raw (also handles the rare case where the entry was
            // already streamed by the lister before the watcher attached).
            state.upsert_raw(entry.clone());

            let matches = state.compiled.matches(&entry);

            let existing_pos = state.view_position_of(&entry);
            let changed = match (existing_pos, matches) {
                (Some(pos), true) => {
                    state.view.remove(pos);
                    state.view_insert_sorted(entry);
                    true
                }
                (Some(pos), false) => {
                    state.view.remove(pos);
                    true
                }
                (None, true) => {
                    state.view_insert_sorted(entry);
                    true
                }
                (None, false) => false,
            };
            if changed {
                // view mutated; the outer viewmodel invalidates publish after the guard drops.
            }
            changed
        };

        if view_changed {
            self.invalidate_published();
            self.notify(ViewModelEvent::EntriesChanged);
        }
    }

    /// Handle a `Removed` event from the directory watcher.
    ///
    /// Removes the entry matching `path` from both the raw snapshot and the
    /// filtered view, then emits [`ViewModelEvent::EntriesChanged`] if the
    /// view changed.
    ///
    /// Uses `raw_index` for O(1) raw location (and `swap_remove` for O(1)
    /// raw removal); the view lookup is O(log N) via `view_position_of`.
    pub(crate) fn handle_removed(&self, path: &Path) {
        let name = match path.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => return,
        };

        let view_changed = {
            let mut state = self.state.write();
            let removed = state.remove_raw(&name);
            let view_removed = match &removed {
                Some(entry) => state
                    .view_position_of(entry)
                    .map(|pos| {
                        state.view.remove(pos);
                        true
                    })
                    .unwrap_or(false),
                None => false,
            };
            let changed = removed.is_some() || view_removed;
            if changed {
                // view mutated; the outer viewmodel invalidates publish after the guard drops.
            }
            changed
        };

        if view_changed {
            self.invalidate_published();
            self.notify(ViewModelEvent::EntriesChanged);
        }
    }

    /// Handle a `Modified` event from the directory watcher.
    ///
    /// Re-stats the affected path, updates it in the snapshot, adjusts filter
    /// visibility (an out-of-filter entry after modification is treated as a
    /// removal from the view), and emits [`ViewModelEvent::EntriesChanged`] if
    /// the view changed.
    ///
    /// Same O(1) raw lookup and O(log N) view lookup story as
    /// [`Self::handle_created`]: the previous entry's position is found
    /// via `raw_index` (for the sort key) then relocated in view via
    /// `view_position_of`, avoiding two linear scans.
    pub(crate) fn handle_modified(&self, path: PathBuf) {
        let name_os = match path.file_name() {
            Some(n) => n.to_owned(),
            None => return,
        };
        let local_path = self.path.join(&name_os);

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

            // The old entry (if any) is needed to locate the current
            // view position under the current sort key, because the
            // modified entry's sort key may have moved.
            let old_entry = state
                .raw_index
                .get(&entry.name)
                .map(|&i| state.raw[i].clone());
            state.upsert_raw(entry.clone());

            let existing_pos = old_entry
                .as_ref()
                .and_then(|old| state.view_position_of(old));
            let matches = state.compiled.matches(&entry);

            let changed = match (existing_pos, matches) {
                (Some(pos), true) => {
                    state.view.remove(pos);
                    state.view_insert_sorted(entry);
                    true
                }
                (Some(pos), false) => {
                    state.view.remove(pos);
                    true
                }
                (None, true) => {
                    state.view_insert_sorted(entry);
                    true
                }
                (None, false) => false,
            };
            if changed {
                // view mutated; the outer viewmodel invalidates publish after the guard drops.
            }
            changed
        };

        if view_changed {
            self.invalidate_published();
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
            state.rebuild_raw_index();
            state.recompute();
        }
        self.invalidate_published();
        self.notify(ViewModelEvent::EntriesChanged);
    }
}

impl LocationViewModel for InMemoryLocationViewModel {
    fn location(&self) -> &Path {
        &self.path
    }

    /// Load the current sorted+filtered snapshot.
    ///
    /// Publishes lazily on the first read after a mutation: if the
    /// dirty flag is set, this call takes a read lock on `state`,
    /// clones the view into a fresh `Arc<[Entry]>`, and stores it in
    /// [`InMemoryLocationViewModel::published`] so subsequent reads
    /// return an atomic Arc load. Concurrent readers may race and
    /// perform the rebuild independently — that is idempotent because
    /// both see the same `view` under their read locks — but only one
    /// rebuild's worth of allocation ever survives after `published`
    /// settles.
    fn entries(&self) -> Arc<[Entry]> {
        if self.published_dirty.load(Ordering::Acquire) {
            let snap: Arc<[Entry]> = {
                let g = self.state.read();
                Arc::from(g.view.as_slice())
            };
            // Store *before* clearing the dirty flag so a concurrent
            // mutation that fires between the store and the clear
            // will re-set dirty and the next read will rebuild.
            *self.published.lock() = Arc::clone(&snap);
            self.published_dirty.store(false, Ordering::Release);
            snap
        } else {
            Arc::clone(&*self.published.lock())
        }
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
        self.invalidate_published();
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
        self.invalidate_published();
        self.notify(ViewModelEvent::EntriesChanged);
        Ok(())
    }

    fn subscribe(&self) -> Receiver<ViewModelEvent> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut guard = self.subscribers.lock();
        let mut v: Vec<Sender<ViewModelEvent>> = guard.iter().cloned().collect();
        v.push(tx);
        *guard = Arc::from(v.into_boxed_slice());
        rx
    }
}
