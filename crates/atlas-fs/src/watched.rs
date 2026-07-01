//! Watcher integration for [`InMemoryLocationViewModel`].
//!
//! When [`crate::OpenOptions::watch`] is `true`, [`attach_watcher`] is called
//! from the loader thread after the initial listing completes. It builds a
//! non-recursive [`atlas_watch::DirectoryWatcher`] with a 200 ms debounce,
//! stores it inside the view model (keeping it alive), and spawns a background
//! thread that translates incoming [`atlas_watch::FileEvent`]s into snapshot
//! mutations and [`crate::ViewModelEvent`] notifications.
//!
//! # Shutdown
//!
//! The event thread holds only a [`Weak`] reference to the view model. When the
//! last strong [`Arc`] is dropped, `Weak::upgrade` returns `None` and the
//! thread exits. Dropping the view model also drops the stored
//! `DirectoryWatcher`, which closes the event channel and unblocks the
//! thread's receive call.

use std::sync::{Arc, Weak};
use std::time::Duration;

use atlas_watch::{FileEventKind, WatcherBuilder};

use crate::view_model::{InMemoryLocationViewModel, ViewModelEvent};

/// Attach a live directory watcher to `vm` and start the event-processing
/// thread.
///
/// Called from the loader thread (which holds an `Arc`) immediately after
/// [`InMemoryLocationViewModel::run_loader`] completes. The watcher is stored
/// inside the view model so its lifetime is tied to the view model's lifetime.
pub(crate) fn attach_watcher(vm: Arc<InMemoryLocationViewModel>) {
    let path = vm.path.clone();

    let (watcher, event_rx) = match WatcherBuilder::new()
        .debounce(Duration::from_millis(200))
        .recursive(false)
        .build()
    {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!("atlas-fs: failed to build directory watcher: {e}");
            return;
        }
    };

    if let Err(e) = watcher.add_root(path) {
        tracing::warn!("atlas-fs: failed to add watch root: {e}");
        return;
    }

    // Store the watcher in the view model so it is not dropped until the VM
    // itself is dropped, which would stop event delivery.
    *vm._watcher.lock() = Some(watcher);

    // The event thread holds only a Weak so it does not prevent the VM from
    // being dropped when the caller releases its Arc.
    let vm_weak: Weak<InMemoryLocationViewModel> = Arc::downgrade(&vm);

    if let Err(e) = std::thread::Builder::new()
        .name("atlas-fs-watch-events".to_owned())
        .spawn(move || run_event_loop(vm_weak, event_rx))
    {
        tracing::warn!("atlas-fs: failed to spawn watcher event thread: {e}");
    }
    // `vm` (the strong Arc from the loader thread) drops here; the VM stays
    // alive via the caller's Arc and the stored watcher.
}

fn run_event_loop(
    vm_weak: Weak<InMemoryLocationViewModel>,
    event_rx: crossbeam_channel::Receiver<atlas_watch::FileEvent>,
) {
    for event in event_rx.iter() {
        // If the view model has been dropped, stop processing.
        let Some(vm) = vm_weak.upgrade() else { break };

        match event.kind {
            FileEventKind::Created => {
                if let Some(path) = event.paths.first().cloned() {
                    vm.handle_created(path);
                }
            }
            FileEventKind::Removed => {
                if let Some(path) = event.paths.first() {
                    vm.handle_removed(path);
                }
            }
            FileEventKind::Modified => {
                if let Some(path) = event.paths.first().cloned() {
                    vm.handle_modified(path);
                }
            }
            FileEventKind::Renamed => {
                // paths[0] = old path, paths[1] = new path.
                if let Some(old) = event.paths.first() {
                    vm.handle_removed(old);
                }
                if let Some(new) = event.paths.get(1).cloned() {
                    vm.handle_created(new);
                }
            }
            FileEventKind::Rescan => {
                vm.handle_rescan();
            }
            FileEventKind::Error => {
                let path_hint = event
                    .paths
                    .first()
                    .map(|p| format!("{}", p.display()))
                    .unwrap_or_else(|| "<unknown>".to_owned());
                tracing::warn!("atlas-fs watcher: backend error for {path_hint}");
                vm.notify(ViewModelEvent::Error(format!(
                    "watcher error for {path_hint}"
                )));
            }
        }
    }
}
