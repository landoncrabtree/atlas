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

/// Recursively walk `req.roots`, streaming [`ListEvent`]s over the returned
/// channel.
///
/// Traversal runs on a pool of worker threads sized to the available
/// parallelism. Entries are emitted in [`ListEvent::Batch`] chunks and a final
/// [`ListEvent::Done`] is always sent. The root entries themselves (depth 0)
/// are not emitted — only their descendants.
#[must_use]
pub fn walk(req: WalkRequest) -> Receiver<ListEvent> {
    let (tx, rx) = crossbeam_channel::unbounded();

    if req.roots.is_empty() {
        let _ = tx.send(ListEvent::Done);
        return rx;
    }

    std::thread::spawn(move || {
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

    rx
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
