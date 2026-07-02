//! Move primitive with same-filesystem rename and copy-delete fallback.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU8;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::conflict::{resolve_conflict, ConflictDecision, ConflictPolicy};
use crate::op::{OpEvent, OpId, Operation};
use crate::primitives::copy::{check_flags, copy_path, count_path, count_paths};

#[allow(clippy::too_many_arguments)]
pub(crate) fn move_items(
    id: OpId,
    sources: &[PathBuf],
    dest_dir: &Path,
    policy: ConflictPolicy,
    flags: &AtomicU8,
    event_tx: &crossbeam_channel::Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    progress_interval: Duration,
) -> atlas_core::Result<()> {
    let totals = count_paths(sources)?;
    {
        let mut op = op_arc.lock();
        op.progress.items_total = totals.items;
        op.progress.bytes_total = totals.bytes;
        op.progress.items_done = 0;
        op.progress.bytes_done = 0;
        op.progress.current_path = None;
    }

    fs::create_dir_all(dest_dir)
        .map_err(|source| atlas_core::AtlasError::io(Some(dest_dir.to_path_buf()), source))?;

    for source in sources {
        check_flags(flags)?;
        let file_name = source.file_name().ok_or_else(|| {
            atlas_core::AtlasError::InvalidPath(format!(
                "source has no file name: {}",
                source.display()
            ))
        })?;
        let initial_dest = dest_dir.join(file_name);
        let dest = if initial_dest.exists() {
            match resolve_conflict(id, source, &initial_dest, policy, event_tx)? {
                ConflictDecision::Skip => {
                    let counts = count_path(source)?;
                    let mut op = op_arc.lock();
                    op.progress.items_done = op.progress.items_done.saturating_add(counts.items);
                    op.progress.current_path = Some(source.clone());
                    continue;
                }
                ConflictDecision::Overwrite => initial_dest,
                ConflictDecision::RenameTo(path) => path,
                ConflictDecision::Cancel => return Err(atlas_core::AtlasError::Cancelled),
            }
        } else {
            initial_dest
        };

        if dest.exists() {
            remove_existing(&dest)?;
        }

        match fs::rename(source, &dest) {
            Ok(()) => mark_move_done(op_arc, source, &dest)?,
            Err(error) if is_cross_device(&error) => {
                move_via_copy_delete_impl(
                    id,
                    source,
                    &dest,
                    flags,
                    event_tx,
                    op_arc,
                    progress_interval,
                )?;
            }
            Err(error) => {
                return Err(atlas_core::AtlasError::io(Some(source.clone()), error));
            }
        }
    }

    let snapshot = {
        let op = op_arc.lock();
        op.progress.clone()
    };
    let _ = event_tx.send(OpEvent::Progress { id, snapshot });
    Ok(())
}

/// Test helper that exercises the move copy-delete fallback path directly.
#[doc(hidden)]
pub fn move_via_copy_delete_for_tests(source: &Path, dest: &Path) -> atlas_core::Result<()> {
    use atlas_core::Location;
    let (event_tx, _event_rx) = crossbeam_channel::unbounded();
    let op_arc = Arc::new(parking_lot::Mutex::new(Operation {
        id: 0,
        kind: crate::op::OpKind::Move {
            sources: vec![Location::local(source)],
            dest_dir: Location::local(
                dest.parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf(),
            ),
            policy: ConflictPolicy::Overwrite,
        },
        status: crate::op::OpStatus::Running,
        started_at: None,
        finished_at: None,
        progress: Default::default(),
        error: None,
        undo_token: None,
    }));
    let flags = AtomicU8::new(0);
    {
        let totals = count_paths(&[source.to_path_buf()])?;
        let mut op = op_arc.lock();
        op.progress.items_total = totals.items;
        op.progress.bytes_total = totals.bytes;
    }
    move_via_copy_delete_impl(0, source, dest, &flags, &event_tx, &op_arc, Duration::ZERO)
}

fn move_via_copy_delete_impl(
    id: OpId,
    source: &Path,
    dest: &Path,
    flags: &AtomicU8,
    event_tx: &crossbeam_channel::Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    progress_interval: Duration,
) -> atlas_core::Result<()> {
    let mut last_emit = Instant::now();
    copy_path(
        id,
        source,
        dest,
        ConflictPolicy::Overwrite,
        flags,
        event_tx,
        op_arc,
        &mut last_emit,
        progress_interval,
    )?;
    if source.is_dir() {
        fs::remove_dir_all(source).map_err(|source_err| {
            atlas_core::AtlasError::io(Some(source.to_path_buf()), source_err)
        })?;
    } else {
        fs::remove_file(source).map_err(|source_err| {
            atlas_core::AtlasError::io(Some(source.to_path_buf()), source_err)
        })?;
    }
    Ok(())
}

fn mark_move_done(
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    source: &Path,
    dest: &Path,
) -> atlas_core::Result<()> {
    let counts = count_path(dest)?;
    let mut op = op_arc.lock();
    op.progress.items_done = op.progress.items_done.saturating_add(counts.items);
    op.progress.bytes_done = op.progress.bytes_done.saturating_add(counts.bytes);
    op.progress.current_path = Some(source.to_path_buf());
    Ok(())
}

fn remove_existing(path: &Path) -> atlas_core::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| atlas_core::AtlasError::io(Some(path.to_path_buf()), source))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
            .map_err(|source| atlas_core::AtlasError::io(Some(path.to_path_buf()), source))
    } else {
        fs::remove_file(path)
            .map_err(|source| atlas_core::AtlasError::io(Some(path.to_path_buf()), source))
    }
}

fn is_cross_device(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::CrossesDevices {
        return true;
    }
    #[cfg(unix)]
    if error.raw_os_error() == Some(18) {
        return true;
    }
    false
}
