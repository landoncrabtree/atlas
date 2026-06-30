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

/// List a single directory, streaming [`ListEvent`]s over the returned channel.
///
/// Enumeration happens on a dedicated thread. Entries are emitted in
/// [`ListEvent::Batch`] chunks (of [`BATCH_SIZE`]) as they are discovered, and
/// a final [`ListEvent::Done`] is always sent — even when an error occurs.
#[must_use]
pub fn list_directory(req: ListRequest) -> Receiver<ListEvent> {
    let (tx, rx) = crossbeam_channel::unbounded();

    std::thread::spawn(move || {
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

    rx
}
