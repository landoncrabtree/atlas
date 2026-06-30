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

use crate::entry::Entry;
use crate::filter::{CompiledFilter, Filter};
use crate::lister::{list_directory, ListEvent, ListRequest};
use crate::sort::{sort_in_place, SortSpec};

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
}

/// In-memory implementation of [`LocationViewModel`].
///
/// Spawns the streaming lister, accumulates entries, applies sort/filter, and
/// notifies subscribers as data arrives or when sort/filter changes.
pub struct InMemoryLocationViewModel {
    path: PathBuf,
    state: RwLock<Inner>,
    subscribers: Mutex<Vec<Sender<ViewModelEvent>>>,
}

impl InMemoryLocationViewModel {
    /// Open `path`, beginning to stream entries immediately.
    ///
    /// Returns an `Arc` handle that is shared with the background loader
    /// thread. If the initial filter fails to compile it is replaced with an
    /// empty (match-all) filter and an [`ViewModelEvent::Error`] is emitted
    /// once a subscriber attaches.
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
        std::thread::spawn(move || {
            worker.run_loader(&rx);
        });

        this
    }

    fn run_loader(&self, rx: &Receiver<ListEvent>) {
        for event in rx.iter() {
            match event {
                ListEvent::Batch(entries) => {
                    let first_load;
                    {
                        let mut state = self.state.write();
                        first_load = !state.loaded;
                        state.raw.extend(entries);
                        state.loaded = true;
                        state.recompute();
                    }
                    if first_load {
                        self.notify(ViewModelEvent::Loaded);
                    }
                    self.notify(ViewModelEvent::EntriesChanged);
                }
                ListEvent::Error { path, error } => {
                    tracing::warn!(?path, %error, "list error");
                    self.notify(ViewModelEvent::Error(error.to_string()));
                }
                ListEvent::Done => break,
            }
        }
    }

    fn notify(&self, event: ViewModelEvent) {
        let mut subs = self.subscribers.lock();
        subs.retain(|tx| tx.send(event.clone()).is_ok());
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
