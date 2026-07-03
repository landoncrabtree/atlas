//! Single-directory streaming lister.
//!
//! [`list_directory`] reads one directory on a background thread and streams
//! batches of [`Entry`] values back over a [`crossbeam_channel`] so the UI
//! never blocks on enumeration.

use std::path::PathBuf;

use atlas_core::AtlasError;
use crossbeam_channel::Receiver;

use crate::entry::{build_entry, Entry};

/// Number of entries accumulated before a [`ListEvent::Batch`] is emitted.
pub(crate) const BATCH_SIZE: usize = 64;

/// Owning handle for the streaming listing returned by [`list_directory`].
///
/// Bundles the [`Receiver<ListEvent>`] with the [`JoinHandle`] of the
/// producer thread so the thread is guaranteed to be joined when the handle
/// is dropped — no detached / leaked producers. Mirrors the
/// [`crate::WalkHandle`] contract exactly so calling code can rely on a
/// single ownership story for both single-directory and recursive listings.
///
/// Dereferences to the underlying [`Receiver`] so existing call sites
/// (`for event in &handle`, `handle.iter()`, `handle.recv()`,
/// `handle.try_recv()`) work unchanged.
///
/// [`JoinHandle`]: std::thread::JoinHandle
pub struct ListHandle {
    rx: Receiver<ListEvent>,
    // `Option` so `Drop` / `into_receiver` can `.take()` and `.join()`.
    // Always `Some` for the entire lifetime except during Drop.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::ops::Deref for ListHandle {
    type Target = Receiver<ListEvent>;
    fn deref(&self) -> &Self::Target {
        &self.rx
    }
}

// Forward `for ev in &handle` to the receiver's iterator so existing
// call sites (`for event in &rx { … }`) work unchanged after switching
// from bare `Receiver` to `ListHandle`. Rust's auto-deref doesn't
// cover `IntoIterator` when written as `for … in &handle`.
impl<'a> IntoIterator for &'a ListHandle {
    type Item = ListEvent;
    type IntoIter = crossbeam_channel::Iter<'a, ListEvent>;
    fn into_iter(self) -> Self::IntoIter {
        self.rx.iter()
    }
}

impl ListHandle {
    /// Consume the handle and return just the [`Receiver`]. Joins the
    /// producer thread inline so ownership transfer doesn't leak.
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

impl Drop for ListHandle {
    fn drop(&mut self) {
        // Drop `rx` first so the sender in the producer thread sees the
        // channel disconnected and short-circuits on its next `tx.send()`.
        // Otherwise the producer would keep enumerating a potentially
        // large directory even though nobody is consuming.
        drop(std::mem::replace(
            &mut self.rx,
            crossbeam_channel::unbounded().1,
        ));
        // Then join the producer thread so no detached OS thread outlives
        // the handle. Nextest's LEAK detector catches any such thread.
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A request to list a single directory.
#[derive(Clone, Debug)]
pub struct ListRequest {
    /// The directory to list.
    pub path: PathBuf,
    /// Whether symlink metadata should be taken from the link target.
    pub follow_symlinks: bool,
    /// Whether hidden entries should be included.
    pub include_hidden: bool,
}

/// An event streamed by [`list_directory`] and [`crate::walk`].
#[derive(Debug)]
pub enum ListEvent {
    /// A batch of discovered entries, in arbitrary order.
    Batch(Vec<Entry>),
    /// A non-fatal error encountered while listing a particular path.
    Error {
        /// The path the error relates to.
        path: PathBuf,
        /// The underlying error.
        error: AtlasError,
    },
    /// Terminal event — always emitted exactly once at the end.
    Done,
}

/// List a single directory, streaming [`ListEvent`]s over a channel wrapped
/// by the returned [`ListHandle`].
///
/// Enumeration happens on a dedicated thread. Entries are emitted in
/// [`ListEvent::Batch`] chunks (of [`BATCH_SIZE`]) as they are discovered, and
/// a final [`ListEvent::Done`] is always sent — even when an error occurs.
///
/// The [`ListHandle`] joins the producer thread on drop, so the thread never
/// outlives the handle. If the caller stops consuming events early
/// (e.g. after collecting `limit` matches in an autocomplete search), the
/// producer detects the closed channel on its next send and short-circuits.
#[must_use]
pub fn list_directory(req: ListRequest) -> ListHandle {
    let (tx, rx) = crossbeam_channel::unbounded();

    let thread = std::thread::spawn(move || {
        let read_dir = match std::fs::read_dir(&req.path) {
            Ok(rd) => rd,
            Err(e) => {
                let _ = tx.send(ListEvent::Error {
                    path: req.path.clone(),
                    error: AtlasError::io(Some(req.path.clone()), e),
                });
                let _ = tx.send(ListEvent::Done);
                return;
            }
        };

        let mut batch: Vec<Entry> = Vec::with_capacity(BATCH_SIZE);

        for dirent in read_dir {
            let dirent = match dirent {
                Ok(d) => d,
                Err(e) => {
                    let _ = tx.send(ListEvent::Error {
                        path: req.path.clone(),
                        error: AtlasError::io(Some(req.path.clone()), e),
                    });
                    continue;
                }
            };

            let entry = match build_entry(dirent.path(), req.follow_symlinks) {
                Ok(entry) => entry,
                Err(error) => {
                    let path = dirent.path();
                    let _ = tx.send(ListEvent::Error { path, error });
                    continue;
                }
            };

            if !req.include_hidden && entry.metadata.is_hidden {
                continue;
            }

            batch.push(entry);
            if batch.len() >= BATCH_SIZE {
                let full = std::mem::replace(&mut batch, Vec::with_capacity(BATCH_SIZE));
                if tx.send(ListEvent::Batch(full)).is_err() {
                    return;
                }
            }
        }

        if !batch.is_empty() {
            let _ = tx.send(ListEvent::Batch(batch));
        }
        let _ = tx.send(ListEvent::Done);
    });

    ListHandle {
        rx,
        thread: Some(thread),
    }
}
