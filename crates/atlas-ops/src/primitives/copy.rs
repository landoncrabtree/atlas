//! Recursive copy primitive with progress, pause, cancellation, and conflicts.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use walkdir::WalkDir;

use crate::conflict::{resolve_conflict, ConflictDecision, ConflictPolicy};
use crate::op::{OpEvent, OpId, Operation, ProgressSnapshot, FLAG_CANCEL, FLAG_PAUSE};

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Counts {
    pub(crate) items: u64,
    pub(crate) bytes: u64,
}

enum DestinationResolution {
    Skip,
    Path(PathBuf),
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn copy_items(
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

    let mut last_emit = Instant::now();
    for source in sources {
        check_flags(flags)?;
        let file_name = source.file_name().ok_or_else(|| {
            atlas_core::AtlasError::InvalidPath(format!(
                "source has no file name: {}",
                source.display()
            ))
        })?;
        let default_dest = dest_dir.join(file_name);
        let source_meta = fs::symlink_metadata(source)
            .map_err(|source_err| atlas_core::AtlasError::io(Some(source.clone()), source_err))?;

        if source_meta.is_dir() && default_dest.exists() {
            match resolve_conflict(id, source, &default_dest, policy, event_tx)? {
                ConflictDecision::Skip => {
                    skip_path_progress(
                        id,
                        source,
                        op_arc,
                        event_tx,
                        &mut last_emit,
                        progress_interval,
                    )?;
                    continue;
                }
                ConflictDecision::Overwrite => {}
                ConflictDecision::RenameTo(path) => {
                    copy_path(
                        id,
                        source,
                        &path,
                        policy,
                        flags,
                        event_tx,
                        op_arc,
                        &mut last_emit,
                        progress_interval,
                    )?;
                    continue;
                }
                ConflictDecision::Cancel => return Err(atlas_core::AtlasError::Cancelled),
            }
        }

        copy_path(
            id,
            source,
            &default_dest,
            policy,
            flags,
            event_tx,
            op_arc,
            &mut last_emit,
            progress_interval,
        )?;
    }

    emit_progress(id, op_arc, event_tx, &mut last_emit, Duration::ZERO, true);
    Ok(())
}

pub(crate) fn count_paths(paths: &[PathBuf]) -> atlas_core::Result<Counts> {
    let mut counts = Counts::default();
    for path in paths {
        counts = counts_paths_sum(counts, count_path(path)?);
    }
    Ok(counts)
}

pub(crate) fn count_path(path: &Path) -> atlas_core::Result<Counts> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| atlas_core::AtlasError::io(Some(path.to_path_buf()), source))?;
    if metadata.is_dir() {
        let mut counts = Counts { items: 0, bytes: 0 };
        for entry in WalkDir::new(path).follow_links(false) {
            let entry = entry.map_err(|error| {
                atlas_core::AtlasError::io(
                    Some(path.to_path_buf()),
                    std::io::Error::other(error.to_string()),
                )
            })?;
            let entry_meta = entry.metadata().map_err(|source| {
                atlas_core::AtlasError::io(Some(entry.path().to_path_buf()), source.into())
            })?;
            counts.items += 1;
            if entry_meta.is_file() {
                counts.bytes += entry_meta.len();
            }
        }
        Ok(counts)
    } else {
        Ok(Counts {
            items: 1,
            bytes: if metadata.is_file() {
                metadata.len()
            } else {
                0
            },
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn copy_path(
    id: OpId,
    source: &Path,
    dest: &Path,
    policy: ConflictPolicy,
    flags: &AtomicU8,
    event_tx: &crossbeam_channel::Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    last_emit: &mut Instant,
    progress_interval: Duration,
) -> atlas_core::Result<()> {
    check_flags(flags)?;
    let metadata = fs::symlink_metadata(source)
        .map_err(|source_err| atlas_core::AtlasError::io(Some(source.to_path_buf()), source_err))?;

    if metadata.file_type().is_symlink() {
        return copy_symlink(
            id,
            source,
            dest,
            policy,
            flags,
            event_tx,
            op_arc,
            last_emit,
            progress_interval,
        );
    }

    if metadata.is_dir() {
        copy_directory(
            id,
            source,
            dest,
            policy,
            flags,
            event_tx,
            op_arc,
            last_emit,
            progress_interval,
        )
    } else {
        copy_file(
            id,
            source,
            dest,
            policy,
            flags,
            event_tx,
            op_arc,
            last_emit,
            progress_interval,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_directory(
    id: OpId,
    source: &Path,
    dest: &Path,
    policy: ConflictPolicy,
    flags: &AtomicU8,
    event_tx: &crossbeam_channel::Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    last_emit: &mut Instant,
    progress_interval: Duration,
) -> atlas_core::Result<()> {
    if dest.exists() && !dest.is_dir() {
        match resolve_conflict(id, source, dest, policy, event_tx)? {
            ConflictDecision::Skip => {
                skip_path_progress(id, source, op_arc, event_tx, last_emit, progress_interval)?;
                return Ok(());
            }
            ConflictDecision::Overwrite => remove_existing(dest)?,
            ConflictDecision::RenameTo(path) => {
                return copy_directory(
                    id,
                    source,
                    &path,
                    policy,
                    flags,
                    event_tx,
                    op_arc,
                    last_emit,
                    progress_interval,
                );
            }
            ConflictDecision::Cancel => return Err(atlas_core::AtlasError::Cancelled),
        }
    }

    if !dest.exists() {
        fs::create_dir_all(dest).map_err(|source_err| {
            atlas_core::AtlasError::io(Some(dest.to_path_buf()), source_err)
        })?;
    }
    preserve_permissions(source, dest)?;
    finish_item_progress(
        id,
        op_arc,
        event_tx,
        last_emit,
        progress_interval,
        source,
        0,
    );

    for entry in WalkDir::new(source).min_depth(1).follow_links(false) {
        check_flags(flags)?;
        let entry = entry.map_err(|error| {
            atlas_core::AtlasError::io(
                Some(source.to_path_buf()),
                std::io::Error::other(error.to_string()),
            )
        })?;
        let entry_path = entry.path();
        let relative = entry_path.strip_prefix(source).map_err(|_| {
            atlas_core::AtlasError::InvalidPath(format!(
                "failed to strip prefix {} from {}",
                source.display(),
                entry_path.display()
            ))
        })?;
        let target = dest.join(relative);
        copy_path(
            id,
            entry_path,
            &target,
            policy,
            flags,
            event_tx,
            op_arc,
            last_emit,
            progress_interval,
        )?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn copy_file(
    id: OpId,
    source: &Path,
    dest: &Path,
    policy: ConflictPolicy,
    flags: &AtomicU8,
    event_tx: &crossbeam_channel::Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    last_emit: &mut Instant,
    progress_interval: Duration,
) -> atlas_core::Result<()> {
    let final_dest = match resolve_destination(id, source, dest, policy, event_tx)? {
        DestinationResolution::Skip => {
            finish_item_progress(
                id,
                op_arc,
                event_tx,
                last_emit,
                progress_interval,
                source,
                0,
            );
            return Ok(());
        }
        DestinationResolution::Path(path) => path,
    };
    let Some(parent) = final_dest.parent() else {
        return Err(atlas_core::AtlasError::InvalidPath(format!(
            "destination has no parent: {}",
            final_dest.display()
        )));
    };
    fs::create_dir_all(parent)
        .map_err(|source_err| atlas_core::AtlasError::io(Some(parent.to_path_buf()), source_err))?;
    if final_dest.exists() {
        remove_existing(&final_dest)?;
    }

    let input = File::open(source)
        .map_err(|source_err| atlas_core::AtlasError::io(Some(source.to_path_buf()), source_err))?;
    let output = File::create(&final_dest)
        .map_err(|source_err| atlas_core::AtlasError::io(Some(final_dest.clone()), source_err))?;
    let mut reader = BufReader::new(input);
    let mut writer = BufWriter::new(output);
    let mut buf = vec![0_u8; 1024 * 1024];

    update_current_path(op_arc, source);
    emit_progress(id, op_arc, event_tx, last_emit, progress_interval, false);

    loop {
        check_flags(flags)?;
        let read = reader.read(&mut buf).map_err(|source_err| {
            atlas_core::AtlasError::io(Some(source.to_path_buf()), source_err)
        })?;
        if read == 0 {
            break;
        }
        writer.write_all(&buf[..read]).map_err(|source_err| {
            atlas_core::AtlasError::io(Some(final_dest.clone()), source_err)
        })?;
        add_bytes_progress(op_arc, read as u64, source);
        emit_progress(id, op_arc, event_tx, last_emit, progress_interval, false);
    }

    writer
        .flush()
        .map_err(|source_err| atlas_core::AtlasError::io(Some(final_dest.clone()), source_err))?;
    preserve_permissions(source, &final_dest)?;
    finish_item_progress(
        id,
        op_arc,
        event_tx,
        last_emit,
        progress_interval,
        source,
        0,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn copy_symlink(
    id: OpId,
    source: &Path,
    dest: &Path,
    policy: ConflictPolicy,
    _flags: &AtomicU8,
    event_tx: &crossbeam_channel::Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    last_emit: &mut Instant,
    progress_interval: Duration,
) -> atlas_core::Result<()> {
    let final_dest = match resolve_destination(id, source, dest, policy, event_tx)? {
        DestinationResolution::Skip => {
            finish_item_progress(
                id,
                op_arc,
                event_tx,
                last_emit,
                progress_interval,
                source,
                0,
            );
            return Ok(());
        }
        DestinationResolution::Path(path) => path,
    };
    let Some(parent) = final_dest.parent() else {
        return Err(atlas_core::AtlasError::InvalidPath(format!(
            "destination has no parent: {}",
            final_dest.display()
        )));
    };
    fs::create_dir_all(parent)
        .map_err(|source_err| atlas_core::AtlasError::io(Some(parent.to_path_buf()), source_err))?;
    if final_dest.exists() {
        remove_existing(&final_dest)?;
    }

    #[cfg(unix)]
    {
        let target = fs::read_link(source).map_err(|source_err| {
            atlas_core::AtlasError::io(Some(source.to_path_buf()), source_err)
        })?;
        symlink(&target, &final_dest).map_err(|source_err| {
            atlas_core::AtlasError::io(Some(final_dest.clone()), source_err)
        })?;
    }

    #[cfg(windows)]
    {
        tracing::warn!(path = %source.display(), "skipping symlink recreation on Windows");
    }

    finish_item_progress(
        id,
        op_arc,
        event_tx,
        last_emit,
        progress_interval,
        source,
        0,
    );
    Ok(())
}

fn resolve_destination(
    id: OpId,
    source: &Path,
    dest: &Path,
    policy: ConflictPolicy,
    event_tx: &crossbeam_channel::Sender<OpEvent>,
) -> atlas_core::Result<DestinationResolution> {
    if !dest.exists() {
        return Ok(DestinationResolution::Path(dest.to_path_buf()));
    }

    match resolve_conflict(id, source, dest, policy, event_tx)? {
        ConflictDecision::Skip => Ok(DestinationResolution::Skip),
        ConflictDecision::Overwrite => Ok(DestinationResolution::Path(dest.to_path_buf())),
        ConflictDecision::RenameTo(path) => Ok(DestinationResolution::Path(path)),
        ConflictDecision::Cancel => Err(atlas_core::AtlasError::Cancelled),
    }
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

fn skip_path_progress(
    id: OpId,
    source: &Path,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    event_tx: &crossbeam_channel::Sender<OpEvent>,
    last_emit: &mut Instant,
    progress_interval: Duration,
) -> atlas_core::Result<()> {
    let counts = count_path(source)?;
    finish_item_progress(
        id,
        op_arc,
        event_tx,
        last_emit,
        progress_interval,
        source,
        counts.items.saturating_sub(1),
    );
    Ok(())
}

fn finish_item_progress(
    id: OpId,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    event_tx: &crossbeam_channel::Sender<OpEvent>,
    last_emit: &mut Instant,
    progress_interval: Duration,
    current_path: &Path,
    extra_items: u64,
) {
    {
        let mut op = op_arc.lock();
        op.progress.items_done = op.progress.items_done.saturating_add(1 + extra_items);
        op.progress.current_path = Some(current_path.to_path_buf());
    }
    emit_progress(id, op_arc, event_tx, last_emit, progress_interval, false);
}

fn add_bytes_progress(
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    bytes: u64,
    current_path: &Path,
) {
    let mut op = op_arc.lock();
    op.progress.bytes_done = op.progress.bytes_done.saturating_add(bytes);
    op.progress.current_path = Some(current_path.to_path_buf());
}

fn update_current_path(op_arc: &Arc<parking_lot::Mutex<Operation>>, current_path: &Path) {
    let mut op = op_arc.lock();
    op.progress.current_path = Some(current_path.to_path_buf());
}

fn emit_progress(
    id: OpId,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    event_tx: &crossbeam_channel::Sender<OpEvent>,
    last_emit: &mut Instant,
    progress_interval: Duration,
    force: bool,
) {
    let now = Instant::now();
    if !force && !progress_interval.is_zero() && now.duration_since(*last_emit) < progress_interval
    {
        return;
    }
    *last_emit = now;
    let snapshot = {
        let op = op_arc.lock();
        ProgressSnapshot {
            items_total: op.progress.items_total,
            items_done: op.progress.items_done,
            bytes_total: op.progress.bytes_total,
            bytes_done: op.progress.bytes_done,
            current_path: op.progress.current_path.clone(),
        }
    };
    let _ = event_tx.send(OpEvent::Progress { id, snapshot });
}

fn preserve_permissions(source: &Path, dest: &Path) -> atlas_core::Result<()> {
    #[cfg(unix)]
    {
        let mode = fs::metadata(source)
            .map_err(|source_err| {
                atlas_core::AtlasError::io(Some(source.to_path_buf()), source_err)
            })?
            .permissions()
            .mode();
        fs::set_permissions(dest, fs::Permissions::from_mode(mode)).map_err(|source_err| {
            atlas_core::AtlasError::io(Some(dest.to_path_buf()), source_err)
        })?;
    }

    #[cfg(not(unix))]
    {
        let _ = source;
        let _ = dest;
    }

    Ok(())
}

pub(crate) fn check_flags(flags: &AtomicU8) -> atlas_core::Result<()> {
    loop {
        let current = flags.load(Ordering::Relaxed);
        if current & FLAG_CANCEL != 0 {
            return Err(atlas_core::AtlasError::Cancelled);
        }
        if current & FLAG_PAUSE != 0 {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        return Ok(());
    }
}

pub(crate) fn counts_paths_sum(lhs: Counts, rhs: Counts) -> Counts {
    Counts {
        items: lhs.items.saturating_add(rhs.items),
        bytes: lhs.bytes.saturating_add(rhs.bytes),
    }
}
