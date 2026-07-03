//! Top-level async dispatcher for filesystem operations.
//!
//! [`execute_op`] is the single entrypoint that
//! [`crate::queue::OperationQueue`] drives on the shared tokio
//! runtime. It matches on [`OpKind`], picks the right routing based
//! on the [`Location`] backends of source and destination, and
//! forwards to either the sync local primitives in
//! [`crate::primitives`] or the async remote helpers in
//! [`crate::remote`].
//!
//! # Routing matrix
//!
//! | Src | Dst | Route |
//! |-----|-----|-------|
//! | Local  | Local  | Existing sync primitives via `spawn_blocking`. |
//! | Local  | Remote | `remote::copy_local_*_to_remote` |
//! | Remote | Local  | `remote::copy_remote_*_to_local` |
//! | Remote (same host) | Remote (same host) | Backend-native rename for Move; stream_copy for Copy |
//! | Remote (host A) | Remote (host B) | Two `RemoteLocationViewModel`s + `stream_copy` |
//!
//! # Cancellation
//!
//! Every routing branch consults the [`AtomicU8`] flag word between
//! logical sub-steps. The local primitives already do this; the
//! remote helpers in [`crate::remote`] mirror the same pattern.

use std::sync::atomic::AtomicU8;
use std::sync::Arc;
use std::time::Duration;

use atlas_core::{AtlasError, Location, Result as AtlasResult};

use crate::conflict::ConflictPolicy;
use crate::op::{OpEvent, OpId, OpKind, Operation};
use crate::primitives::copy::{copy_items, count_path};
use crate::primitives::delete::delete_paths;
use crate::primitives::mkdir::mkdir_op;
use crate::primitives::move_::move_items;
use crate::primitives::rename::rename_op;
use crate::remote::{
    self, copy_local_file_to_remote, copy_local_tree_to_remote, copy_remote_file_to_local,
    copy_remote_file_to_remote, copy_remote_tree_to_local, copy_remote_tree_to_remote, count_local,
    count_remote, delete_remote, emit_initial_progress, mkdir_remote, open_remote, rename_remote,
    RemoteCounts,
};
use crate::undo::UndoEntry;

/// Dispatch `kind` to the appropriate backend-specific primitive.
///
/// Returns `Ok(Some(entry))` when the op registered an undoable
/// mutation, `Ok(None)` otherwise. Cancellations surface as
/// [`AtlasError::Cancelled`] so the queue can emit
/// [`OpEvent::Cancelled`] instead of `OpEvent::Failed`.
pub async fn execute_op(
    id: OpId,
    kind: OpKind,
    flags: Arc<AtomicU8>,
    event_tx: crossbeam_channel::Sender<OpEvent>,
    op_arc: Arc<parking_lot::Mutex<Operation>>,
    progress_interval: Duration,
) -> AtlasResult<Option<UndoEntry>> {
    match kind {
        OpKind::Copy {
            sources,
            dest_dir,
            policy,
        } => {
            execute_copy(
                id,
                sources,
                dest_dir,
                policy,
                flags,
                event_tx,
                op_arc,
                progress_interval,
            )
            .await?;
            Ok(None)
        }
        OpKind::Move {
            sources,
            dest_dir,
            policy,
        } => {
            execute_move(
                id,
                sources,
                dest_dir,
                policy,
                flags,
                event_tx,
                op_arc,
                progress_interval,
            )
            .await?;
            Ok(None)
        }
        OpKind::Delete { paths, to_trash } => {
            execute_delete(id, paths, to_trash, flags, event_tx, op_arc).await
        }
        OpKind::Rename { path, new_name } => execute_rename(path, new_name).await.map(Some),
        OpKind::Mkdir { path, parents } => execute_mkdir(path, parents).await.map(|()| None),
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_copy(
    id: OpId,
    sources: Vec<Location>,
    dest_dir: Location,
    policy: ConflictPolicy,
    flags: Arc<AtomicU8>,
    event_tx: crossbeam_channel::Sender<OpEvent>,
    op_arc: Arc<parking_lot::Mutex<Operation>>,
    progress_interval: Duration,
) -> AtlasResult<()> {
    // Fast path: everything local.
    if dest_dir.is_local() && sources.iter().all(Location::is_local) {
        let local_sources: Vec<_> = sources
            .iter()
            .map(|loc| {
                loc.as_local()
                    .expect("all-local guarantee just checked")
                    .to_path_buf()
            })
            .collect();
        let dest = dest_dir
            .as_local()
            .expect("local dest guarantee just checked")
            .to_path_buf();
        let op_arc_blk = Arc::clone(&op_arc);
        let event_tx_blk = event_tx.clone();
        let flags_blk = Arc::clone(&flags);
        return tokio::task::spawn_blocking(move || {
            copy_items(
                id,
                &local_sources,
                &dest,
                policy,
                &flags_blk,
                &event_tx_blk,
                &op_arc_blk,
                progress_interval,
            )
        })
        .await
        .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))?;
    }

    // Seed progress totals with an upfront enumeration.
    let mut totals = RemoteCounts::default();
    let mut per_source_totals = Vec::with_capacity(sources.len());
    for source in &sources {
        let counts = count_source(source).await?;
        per_source_totals.push(counts);
        totals.items = totals.items.saturating_add(counts.items);
        totals.bytes = totals.bytes.saturating_add(counts.bytes);
    }
    {
        let mut op = op_arc.lock();
        op.progress.items_total = totals.items;
        op.progress.bytes_total = totals.bytes;
        op.progress.items_done = 0;
        op.progress.bytes_done = 0;
        op.progress.current_path = None;
    }
    emit_initial_progress(id, &event_tx, &op_arc);

    // Ensure the destination directory exists locally.
    if let Some(local) = dest_dir.as_local() {
        let dest = local.to_path_buf();
        tokio::task::spawn_blocking(move || std::fs::create_dir_all(&dest))
            .await
            .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))?
            .map_err(|source| AtlasError::io(Some(local.to_path_buf()), source))?;
    }

    let _ = policy; // Cross-backend conflict handling currently resolves to overwrite.

    for source in sources.iter() {
        let file_name = source.file_name().ok_or_else(|| {
            AtlasError::InvalidPath(format!(
                "source has no file name: {}",
                source.display_path()
            ))
        })?;
        let target = dest_dir.join(&file_name);
        copy_single(id, source, &target, &flags, &event_tx, &op_arc).await?;
    }
    Ok(())
}

async fn count_source(source: &Location) -> AtlasResult<RemoteCounts> {
    match source {
        Location::Local(path) => count_local(path),
        Location::Remote(_, _) => {
            let handle = open_remote(source).await?;
            count_remote(&handle).await
        }
    }
}

async fn copy_single(
    id: OpId,
    source: &Location,
    target: &Location,
    flags: &Arc<AtomicU8>,
    event_tx: &crossbeam_channel::Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
) -> AtlasResult<()> {
    match (source, target) {
        (Location::Local(src_path), Location::Local(dst_path)) => {
            // Delegate back to the local primitive for full parity
            // (permissions, symlinks, per-item conflict prompts).
            let src_owned = src_path.clone();
            let dst_owned = dst_path.clone();
            let event_tx = event_tx.clone();
            let op_arc = Arc::clone(op_arc);
            let flags = Arc::clone(flags);
            tokio::task::spawn_blocking(move || {
                copy_items(
                    id,
                    &[src_owned],
                    dst_owned
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("/")),
                    ConflictPolicy::Overwrite,
                    &flags,
                    &event_tx,
                    &op_arc,
                    Duration::from_millis(100),
                )
            })
            .await
            .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))?
        }
        (Location::Local(src_path), Location::Remote(_, _)) => {
            let dst_handle = open_remote(target).await?;
            let meta = tokio::fs::metadata(src_path)
                .await
                .map_err(|source| AtlasError::io(Some(src_path.clone()), source))?;
            if meta.is_dir() {
                copy_local_tree_to_remote(id, src_path, &dst_handle, event_tx, op_arc, flags).await
            } else {
                copy_local_file_to_remote(
                    id,
                    src_path,
                    &dst_handle,
                    &dst_handle.root,
                    event_tx,
                    op_arc,
                    flags,
                )
                .await?;
                bump_items_done(op_arc);
                Ok(())
            }
        }
        (Location::Remote(_, _), Location::Local(dst_path)) => {
            let src_handle = open_remote(source).await?;
            let stat = src_handle
                .vm
                .stat(&src_handle.root)
                .await
                .map_err(|err| remote::translate_remote_error(&src_handle.display, err))?;
            if matches!(stat.mode(), atlas_remote::RemoteMode::Dir) {
                copy_remote_tree_to_local(id, &src_handle, dst_path, event_tx, op_arc, flags).await
            } else {
                copy_remote_file_to_local(
                    id,
                    &src_handle,
                    &src_handle.root,
                    dst_path,
                    Some(stat.content_length()),
                    event_tx,
                    op_arc,
                    flags,
                )
                .await?;
                bump_items_done(op_arc);
                Ok(())
            }
        }
        (Location::Remote(_, _), Location::Remote(_, _)) => {
            let src_handle = open_remote(source).await?;
            let dst_handle = open_remote(target).await?;
            let stat = src_handle
                .vm
                .stat(&src_handle.root)
                .await
                .map_err(|err| remote::translate_remote_error(&src_handle.display, err))?;
            if matches!(stat.mode(), atlas_remote::RemoteMode::Dir) {
                copy_remote_tree_to_remote(id, &src_handle, &dst_handle, event_tx, op_arc, flags)
                    .await
            } else {
                copy_remote_file_to_remote(
                    id,
                    &src_handle,
                    &src_handle.root,
                    &dst_handle,
                    &dst_handle.root,
                    Some(stat.content_length()),
                    event_tx,
                    op_arc,
                    flags,
                )
                .await?;
                bump_items_done(op_arc);
                Ok(())
            }
        }
    }
}

fn bump_items_done(op_arc: &Arc<parking_lot::Mutex<Operation>>) {
    let mut op = op_arc.lock();
    op.progress.items_done = op.progress.items_done.saturating_add(1);
    if op.progress.items_total < op.progress.items_done {
        op.progress.items_total = op.progress.items_done;
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_move(
    id: OpId,
    sources: Vec<Location>,
    dest_dir: Location,
    policy: ConflictPolicy,
    flags: Arc<AtomicU8>,
    event_tx: crossbeam_channel::Sender<OpEvent>,
    op_arc: Arc<parking_lot::Mutex<Operation>>,
    progress_interval: Duration,
) -> AtlasResult<()> {
    // Fast path: everything local.
    if dest_dir.is_local() && sources.iter().all(Location::is_local) {
        let local_sources: Vec<_> = sources
            .iter()
            .map(|loc| loc.as_local().expect("local").to_path_buf())
            .collect();
        let dest = dest_dir.as_local().expect("local").to_path_buf();
        let op_arc_blk = Arc::clone(&op_arc);
        let event_tx_blk = event_tx.clone();
        let flags_blk = Arc::clone(&flags);
        return tokio::task::spawn_blocking(move || {
            move_items(
                id,
                &local_sources,
                &dest,
                policy,
                &flags_blk,
                &event_tx_blk,
                &op_arc_blk,
                progress_interval,
            )
        })
        .await
        .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))?;
    }

    // Seed totals.
    let mut totals = RemoteCounts::default();
    for source in &sources {
        let counts = count_source(source).await?;
        totals.items = totals.items.saturating_add(counts.items);
        totals.bytes = totals.bytes.saturating_add(counts.bytes);
    }
    {
        let mut op = op_arc.lock();
        op.progress.items_total = totals.items;
        op.progress.bytes_total = totals.bytes;
        op.progress.items_done = 0;
        op.progress.bytes_done = 0;
        op.progress.current_path = None;
    }
    emit_initial_progress(id, &event_tx, &op_arc);

    if let Some(local) = dest_dir.as_local() {
        let dest = local.to_path_buf();
        tokio::task::spawn_blocking(move || std::fs::create_dir_all(&dest))
            .await
            .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))?
            .map_err(|source| AtlasError::io(Some(local.to_path_buf()), source))?;
    }

    let _ = policy;
    for source in sources.iter() {
        let file_name = source.file_name().ok_or_else(|| {
            AtlasError::InvalidPath(format!(
                "source has no file name: {}",
                source.display_path()
            ))
        })?;
        let target = dest_dir.join(&file_name);
        // Same-backend move → native rename.
        if source.same_backend_as(&target) && source.is_remote() {
            let src_handle = open_remote(source).await?;
            let dst_handle = open_remote(&target).await?;
            remote::move_remote_same_backend(&src_handle, &dst_handle.root).await?;
            {
                let mut op = op_arc.lock();
                op.progress.items_done = op.progress.items_done.saturating_add(1);
                op.progress.current_path = Some(std::path::PathBuf::from(source.display_path()));
            }
        } else {
            // Cross-backend move → copy + delete src.
            copy_single(id, source, &target, &flags, &event_tx, &op_arc).await?;
            delete_single(source, &flags, &op_arc).await?;
        }
    }
    Ok(())
}

async fn execute_delete(
    id: OpId,
    paths: Vec<Location>,
    to_trash: bool,
    flags: Arc<AtomicU8>,
    event_tx: crossbeam_channel::Sender<OpEvent>,
    op_arc: Arc<parking_lot::Mutex<Operation>>,
) -> AtlasResult<Option<UndoEntry>> {
    if paths.iter().all(Location::is_local) {
        let local_paths: Vec<_> = paths
            .iter()
            .map(|loc| loc.as_local().expect("local").to_path_buf())
            .collect();
        let op_arc_blk = Arc::clone(&op_arc);
        let event_tx_blk = event_tx.clone();
        let flags_blk = Arc::clone(&flags);
        let result = tokio::task::spawn_blocking(move || {
            delete_paths(
                id,
                &local_paths,
                to_trash,
                &flags_blk,
                &event_tx_blk,
                &op_arc_blk,
            )
        })
        .await
        .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))?;
        return result;
    }

    if to_trash {
        tracing::warn!("trash requested for remote path; hard-deleting instead");
    }

    // Count first for progress totals.
    let mut totals = RemoteCounts::default();
    for path in &paths {
        let counts = count_source(path).await?;
        totals.items = totals.items.saturating_add(counts.items);
        totals.bytes = totals.bytes.saturating_add(counts.bytes);
    }
    {
        let mut op = op_arc.lock();
        op.progress.items_total = totals.items;
        op.progress.bytes_total = totals.bytes;
        op.progress.items_done = 0;
        op.progress.bytes_done = 0;
    }
    emit_initial_progress(id, &event_tx, &op_arc);

    for path in paths.iter() {
        delete_single(path, &flags, &op_arc).await?;
    }
    Ok(None)
}

async fn delete_single(
    path: &Location,
    flags: &Arc<AtomicU8>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
) -> AtlasResult<()> {
    match path {
        Location::Local(local) => {
            let owned = local.clone();
            tokio::task::spawn_blocking(move || {
                let counts = count_path(&owned)?;
                if owned.is_dir() {
                    std::fs::remove_dir_all(&owned)
                        .map_err(|source| AtlasError::io(Some(owned.clone()), source))?;
                } else {
                    std::fs::remove_file(&owned)
                        .map_err(|source| AtlasError::io(Some(owned.clone()), source))?;
                }
                Ok::<_, AtlasError>(counts.items)
            })
            .await
            .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))?
            .map(|items| {
                let mut op = op_arc.lock();
                op.progress.items_done = op.progress.items_done.saturating_add(items);
            })
        }
        Location::Remote(_, _) => {
            let handle = open_remote(path).await?;
            delete_remote(&handle, flags, op_arc).await
        }
    }
}

async fn execute_rename(path: Location, new_name: String) -> AtlasResult<UndoEntry> {
    match path {
        Location::Local(local) => tokio::task::spawn_blocking(move || rename_op(&local, &new_name))
            .await
            .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))?,
        Location::Remote(_, _) => {
            let handle = open_remote(&path).await?;
            let new_full = rename_remote(&handle, &new_name).await?;
            // Remote renames aren't currently undoable — round-trip
            // back to the original path would need a fresh open with
            // the reverse rename. The returned UndoEntry uses PathBuf
            // synthesised from the remote URI display so the ops
            // panel can still show a stable label.
            Ok(UndoEntry::Rename {
                from: std::path::PathBuf::from(new_full),
                to: std::path::PathBuf::from(handle.display),
            })
        }
    }
}

async fn execute_mkdir(path: Location, parents: bool) -> AtlasResult<()> {
    match path {
        Location::Local(local) => tokio::task::spawn_blocking(move || mkdir_op(&local, parents))
            .await
            .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))?,
        Location::Remote(_, _) => {
            let handle = open_remote(&path).await?;
            mkdir_remote(&handle).await
        }
    }
}
