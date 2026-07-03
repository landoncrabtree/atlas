//! Parallel recursive directory walker built on top of the [`ignore`] crate.
//!
//! [`walk`] traverses one or more roots in parallel and streams batches of
//! [`Entry`] values over a [`crossbeam_channel`], reusing the [`ListEvent`]
//! protocol from the single-directory lister.

use std::path::PathBuf;

use atlas_core::AtlasError;
use crossbeam_channel::{Receiver, Sender};
use ignore::{WalkBuilder, WalkState};

use crate::entry::build_entry;
use crate::lister::{ListEvent, BATCH_SIZE};

/// A request to recursively walk one or more roots.
#[derive(Clone, Debug)]
pub struct WalkRequest {
    /// Root directories to walk.
    pub roots: Vec<PathBuf>,
    /// Whether symlinks should be followed during traversal.
    pub follow_symlinks: bool,
    /// Whether hidden entries should be included.
    pub include_hidden: bool,
    /// When `true`, honor `.gitignore`/global/exclude/`.ignore` files; when
    /// `false`, disable all ignore filtering.
    pub respect_gitignore: bool,
    /// Optional maximum traversal depth (relative to each root).
    pub max_depth: Option<usize>,
}

/// Handle to an in-flight recursive walk.
///
/// Owns both the [`Receiver`] streaming [`ListEvent`]s AND the outer
/// producer thread's [`JoinHandle`]. On drop the producer thread is
/// joined so no worker threads outlive the handle — critical for
/// tests (nextest flags detached threads as "LEAK") and for the
/// workspace's "no unbounded thread lingering" performance clause
/// in `.github/instructions/performance.instructions.md`.
///
/// Dereferences to the underlying [`Receiver`] so existing call sites
/// (`for event in &handle`, `handle.iter()`, `handle.recv()`,
/// `handle.try_recv()`) work unchanged.
pub struct WalkHandle {
    rx: Receiver<ListEvent>,
    // `Option` so `Drop` can `.take()` and `.join()`. Always `Some`
    // for the entire lifetime except during Drop / `into_receiver`.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::ops::Deref for WalkHandle {
    type Target = Receiver<ListEvent>;
    fn deref(&self) -> &Self::Target {
        &self.rx
    }
}

// Forward `for ev in &handle` to the receiver's iterator so existing
// call sites (`for event in &rx { … }`) work unchanged after switching
// from bare `Receiver` to `WalkHandle`. Rust's auto-deref doesn't
// cover `IntoIterator` when written as `for … in &handle`.
impl<'a> IntoIterator for &'a WalkHandle {
    type Item = ListEvent;
    type IntoIter = crossbeam_channel::Iter<'a, ListEvent>;
    fn into_iter(self) -> Self::IntoIter {
        self.rx.iter()
    }
}

impl WalkHandle {
    /// Consume the handle and return just the [`Receiver`]. Joins the
    /// producer thread inline so ownership transfer doesn't leak.
    /// Useful when the caller needs `Receiver` by value (e.g. for
    /// `for ev in rx { … }` when `rx` is a local, not a borrow).
    #[must_use]
    pub fn into_receiver(mut self) -> Receiver<ListEvent> {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // Swap out the receiver so Drop doesn't try to double-drop
        // or close prematurely.
        std::mem::replace(&mut self.rx, crossbeam_channel::unbounded().1)
    }
}

impl Drop for WalkHandle {
    fn drop(&mut self) {
        // Drop `rx` first so the sender in worker threads sees the
        // channel disconnected and short-circuits on the next
        // `tx.send()`. Otherwise `ignore::WalkBuilder::build_parallel()`
        // keeps enqueuing into an unbounded buffer even though nobody
        // is consuming.
        drop(std::mem::replace(
            &mut self.rx,
            crossbeam_channel::unbounded().1,
        ));
        // Then join the producer thread so its ignore-crate worker
        // pool has fully wound down before Drop returns. If a caller
        // dropped the handle before consuming Done, this is where the
        // walker's pool actually stops.
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Recursively walk `req.roots`, streaming [`ListEvent`]s.
///
/// Traversal runs on a pool of worker threads sized to the available
/// parallelism. Entries are emitted in [`ListEvent::Batch`] chunks and a final
/// [`ListEvent::Done`] is always sent. The root entries themselves (depth 0)
/// are not emitted — only their descendants.
///
/// The returned [`WalkHandle`] joins the producer thread on drop so
/// worker threads never outlive the handle.
#[must_use]
pub fn walk(req: WalkRequest) -> WalkHandle {
    let (tx, rx) = crossbeam_channel::unbounded();

    if req.roots.is_empty() {
        let _ = tx.send(ListEvent::Done);
        // Spawn a trivial thread even here so the WalkHandle contract
        // ("Drop joins a thread") holds uniformly. Thread exits after
        // dropping tx.
        let thread = std::thread::spawn(move || drop(tx));
        return WalkHandle {
            rx,
            thread: Some(thread),
        };
    }

    let thread = std::thread::spawn(move || {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        let mut roots = req.roots.iter();
        let first = roots.next().expect("roots is non-empty");
        let mut builder = WalkBuilder::new(first);
        for root in roots {
            builder.add(root);
        }

        builder
            .threads(threads)
            .follow_links(req.follow_symlinks)
            .max_depth(req.max_depth)
            .hidden(!req.include_hidden);

        if !req.respect_gitignore {
            builder
                .git_ignore(false)
                .git_global(false)
                .git_exclude(false)
                .ignore(false)
                .parents(false);
        }

        let follow = req.follow_symlinks;
        builder.build_parallel().run(|| {
            let mut collector = Collector::new(tx.clone());
            Box::new(move |result| {
                match result {
                    Ok(dirent) => {
                        if dirent.depth() == 0 {
                            return WalkState::Continue;
                        }
                        match build_entry(dirent.into_path(), follow) {
                            Ok(entry) => collector.push(entry),
                            Err(error) => collector.error(error),
                        }
                    }
                    Err(err) => {
                        let io = err.io_error();
                        let error = match io {
                            Some(io) => AtlasError::io(
                                None,
                                std::io::Error::new(io.kind(), err.to_string()),
                            ),
                            None => AtlasError::InvalidPath(err.to_string()),
                        };
                        collector.send(ListEvent::Error {
                            path: PathBuf::new(),
                            error,
                        });
                    }
                }
                WalkState::Continue
            })
        });

        let _ = tx.send(ListEvent::Done);
    });

    WalkHandle {
        rx,
        thread: Some(thread),
    }
}

/// Per-worker accumulator that batches entries and flushes the remainder when
/// the worker's closure is dropped at the end of traversal.
struct Collector {
    batch: Vec<crate::entry::Entry>,
    tx: Sender<ListEvent>,
}

impl Collector {
    fn new(tx: Sender<ListEvent>) -> Self {
        Self {
            batch: Vec::with_capacity(BATCH_SIZE),
            tx,
        }
    }

    fn push(&mut self, entry: crate::entry::Entry) {
        self.batch.push(entry);
        if self.batch.len() >= BATCH_SIZE {
            self.flush();
        }
    }

    fn error(&mut self, error: AtlasError) {
        // Best-effort: associate with an empty path when none is known.
        self.send(ListEvent::Error {
            path: PathBuf::new(),
            error,
        });
    }

    fn send(&self, event: ListEvent) {
        let _ = self.tx.send(event);
    }

    fn flush(&mut self) {
        if !self.batch.is_empty() {
            let full = std::mem::replace(&mut self.batch, Vec::with_capacity(BATCH_SIZE));
            let _ = self.tx.send(ListEvent::Batch(full));
        }
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.flush();
    }
}
