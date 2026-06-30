//! Delete primitive supporting trash and hard deletion.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicU8;
use std::sync::Arc;

use anyhow::anyhow;

use crate::op::{OpEvent, OpId, Operation};
use crate::primitives::copy::{check_flags, count_path, count_paths};
use crate::undo::UndoEntry;

pub(crate) fn delete_paths(
    id: OpId,
    paths: &[PathBuf],
    to_trash: bool,
    flags: &AtomicU8,
    event_tx: &crossbeam_channel::Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
) -> atlas_core::Result<Option<UndoEntry>> {
    let totals = count_paths(paths)?;
    {
        let mut op = op_arc.lock();
        op.progress.items_total = totals.items;
        op.progress.bytes_total = totals.bytes;
        op.progress.items_done = 0;
        op.progress.bytes_done = 0;
        op.progress.current_path = None;
    }

    if to_trash {
        for path in paths {
            check_flags(flags)?;
            let counts = count_path(path)?;
            {
                let mut op = op_arc.lock();
                op.progress.items_done = op.progress.items_done.saturating_add(counts.items);
                op.progress.bytes_done = op.progress.bytes_done.saturating_add(counts.bytes);
                op.progress.current_path = Some(path.clone());
            }
        }
        trash::delete_all(paths).map_err(|error| atlas_core::AtlasError::Other(anyhow!(error)))?;
        let snapshot = {
            let op = op_arc.lock();
            op.progress.clone()
        };
        let _ = event_tx.send(OpEvent::Progress { id, snapshot });
        return Ok(Some(UndoEntry::Trash {
            paths: paths.to_vec(),
        }));
    }

    for path in paths {
        check_flags(flags)?;
        let counts = count_path(path)?;
        if path.is_dir() {
            fs::remove_dir_all(path)
                .map_err(|source| atlas_core::AtlasError::io(Some(path.clone()), source))?;
        } else {
            fs::remove_file(path)
                .map_err(|source| atlas_core::AtlasError::io(Some(path.clone()), source))?;
        }
        let snapshot = {
            let mut op = op_arc.lock();
            op.progress.items_done = op.progress.items_done.saturating_add(counts.items);
            op.progress.bytes_done = op.progress.bytes_done.saturating_add(counts.bytes);
            op.progress.current_path = Some(path.clone());
            op.progress.clone()
        };
        let _ = event_tx.send(OpEvent::Progress { id, snapshot });
    }

    Ok(None)
}
