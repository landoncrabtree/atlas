//! Incremental event handling for atlas-indexd.

use std::sync::Arc;

use tracing::{error, warn};

use atlas_index::IndexDoc;
use atlas_ipc::protocol::Notification;
use atlas_watch::{FileEvent, FileEventKind};

use crate::daemon::Daemon;

/// Apply a single filesystem event to the daemon's indexes.
pub async fn apply(daemon: &Arc<Daemon>, event: FileEvent) {
    let Some(root) = daemon.root_by_id(event.root) else {
        warn!(?event.root, "received event for unknown root");
        return;
    };

    match event.kind {
        FileEventKind::Created => {
            for path in &event.paths {
                upsert_path(daemon, &root, path, true);
            }
        }
        FileEventKind::Modified => {
            for path in &event.paths {
                upsert_path(daemon, &root, path, false);
            }
        }
        FileEventKind::Removed => {
            for path in &event.paths {
                remove_path(&root, path);
            }
        }
        FileEventKind::Renamed => {
            if event.paths.len() < 2 {
                warn!(root = %root.path.display(), "rename event missing paths");
                return;
            }
            let old_path = &event.paths[0];
            let new_path = &event.paths[1];
            remove_path(&root, old_path);
            upsert_path(daemon, &root, new_path, true);
        }
        FileEventKind::Rescan => {
            warn!(root = %root.path.display(), "watcher requested rescan");
            daemon.spawn_full_reindex(root);
        }
        FileEventKind::Error => {
            let message = event
                .paths
                .first()
                .map(|path| format!("watch error for {}", path.display()))
                .unwrap_or_else(|| "watch error".to_string());
            error!(root = %root.path.display(), %message, "watcher reported an error");
            let _ = daemon.notifications_tx().send(Notification::IndexError {
                root: root.path.clone(),
                message,
            });
        }
    }
}

fn upsert_path(
    daemon: &Arc<Daemon>,
    root: &Arc<crate::state::IndexRoot>,
    path: &std::path::Path,
    deep_scan: bool,
) {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            let is_dir = meta.is_dir();
            let doc = IndexDoc::from_path_and_metadata(path.to_path_buf(), &meta);
            if let Err(error) = root.writer.lock().upsert(&doc) {
                error!(path = %path.display(), %error, "failed to upsert path");
                return;
            }
            let _ = root.mark_pending();
            if deep_scan && is_dir {
                daemon.spawn_subtree_reindex(Arc::clone(root), path.to_path_buf());
            }
        }
        Err(error) => {
            error!(path = %path.display(), %error, "failed to stat path");
        }
    }
}

fn remove_path(root: &Arc<crate::state::IndexRoot>, path: &std::path::Path) {
    let writer = root.writer.lock();
    if let Err(error) = writer.remove_path(path) {
        error!(path = %path.display(), %error, "failed to remove path");
    }
    if let Err(error) = writer.remove_subtree(path) {
        error!(path = %path.display(), %error, "failed to remove subtree");
    }
    let _ = root.mark_pending();
}
