//! Full-walk and subtree ingestion for atlas-indexd.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use atlas_config::Config;
use atlas_fs::{ListEvent, WalkRequest};
use atlas_index::IndexDoc;
use atlas_ipc::protocol::Notification;

use crate::state::IndexRoot;

const PROGRESS_BATCH_SIZE: u64 = 512;

/// Rebuild the entire index for `root`.
pub fn rebuild_root(
    root: Arc<IndexRoot>,
    config: Arc<Config>,
    notifications: broadcast::Sender<Notification>,
) {
    if root.indexing.swap(true, Ordering::SeqCst) {
        debug!(root = %root.path.display(), "full ingest already running");
        return;
    }

    let started = Instant::now();
    let root_path = root.path.clone();
    let result = ingest_scope(&root, &root_path, &config, &notifications, true);
    root.indexing.store(false, Ordering::SeqCst);

    match result {
        Ok(()) => {
            let _ = notifications.send(Notification::IndexComplete {
                root: root_path,
                took_ms: started.elapsed().as_millis() as u64,
            });
        }
        Err(error) => {
            let message = error.to_string();
            warn!(root = %root.path.display(), %message, "full ingest failed");
            let _ = notifications.send(Notification::IndexError {
                root: root.path.clone(),
                message,
            });
        }
    }
}

/// Rebuild a subtree within an existing indexed root.
pub fn rebuild_subtree(
    root: Arc<IndexRoot>,
    subtree: PathBuf,
    config: Arc<Config>,
    notifications: broadcast::Sender<Notification>,
) {
    if !subtree.starts_with(&root.path) {
        warn!(
            root = %root.path.display(),
            subtree = %subtree.display(),
            "ignoring subtree ingest outside root"
        );
        return;
    }

    if let Err(error) = ingest_scope(&root, &subtree, &config, &notifications, true) {
        let message = error.to_string();
        warn!(root = %root.path.display(), subtree = %subtree.display(), %message, "subtree ingest failed");
        let _ = notifications.send(Notification::IndexError {
            root: root.path.clone(),
            message,
        });
    }
}

fn ingest_scope(
    root: &Arc<IndexRoot>,
    scope: &Path,
    config: &Arc<Config>,
    notifications: &broadcast::Sender<Notification>,
    clear_first: bool,
) -> Result<()> {
    if clear_first {
        let writer = root.writer.lock();
        writer.remove_path(scope)?;
        writer.remove_subtree(scope)?;
    }

    let mut files = 0_u64;
    let mut bytes = 0_u64;

    if let Ok(meta) = std::fs::symlink_metadata(scope) {
        let doc = IndexDoc::from_path_and_metadata(scope.to_path_buf(), &meta);
        root.writer.lock().upsert(&doc)?;
        let _ = root.mark_pending();
        files += 1;
        bytes += doc.size;
    }

    let rx = atlas_fs::walk(WalkRequest {
        roots: vec![scope.to_path_buf()],
        follow_symlinks: config.general.follow_symlinks,
        include_hidden: true,
        respect_gitignore: config.indexer.respect_gitignore,
        max_depth: None,
    });

    for event in &rx {
        match event {
            ListEvent::Batch(entries) => {
                for entry in entries {
                    let meta = std::fs::symlink_metadata(&entry.path)
                        .map_err(|error| {
                            atlas_core::AtlasError::io(Some(entry.path.clone()), error)
                        })
                        .with_context(|| format!("stat {} during ingest", entry.path.display()))?;
                    let doc = IndexDoc::from_path_and_metadata(entry.path.clone(), &meta);
                    root.writer.lock().upsert(&doc)?;
                    let _ = root.mark_pending();
                    files += 1;
                    bytes += doc.size;
                }

                if files.is_multiple_of(PROGRESS_BATCH_SIZE) {
                    let _ = notifications.send(Notification::IndexProgress {
                        root: root.path.clone(),
                        files,
                        bytes,
                    });
                }
            }
            ListEvent::Error { path, error } => {
                warn!(path = %path.display(), %error, "walk error during ingest");
            }
            ListEvent::Done => break,
        }
    }

    let _ = notifications.send(Notification::IndexProgress {
        root: root.path.clone(),
        files,
        bytes,
    });

    commit_root(root)
}

fn commit_root(root: &IndexRoot) -> Result<()> {
    {
        let mut writer = root.writer.lock();
        writer.commit()?;
    }
    root.reader.read().reload()?;
    let _ = root.take_pending();
    Ok(())
}
