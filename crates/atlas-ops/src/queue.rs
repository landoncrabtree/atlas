//! Operation queue implementation with worker threads and event streaming.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

use crate::op::{OpEvent, OpId, OpKind, OpStatus, Operation, FLAG_CANCEL, FLAG_PAUSE};
use crate::primitives::copy::copy_items;
use crate::primitives::delete::delete_paths;
use crate::primitives::mkdir::mkdir_op;
use crate::primitives::move_::move_items;
use crate::primitives::rename::rename_op;
use crate::undo::{UndoEntry, UndoStack};

/// Runtime options for [`OperationQueue`].
pub struct QueueOptions {
    /// Number of worker threads.
    pub workers: usize,
    /// Minimum interval between progress events.
    pub progress_interval: Duration,
}

impl Default for QueueOptions {
    fn default() -> Self {
        Self {
            workers: std::thread::available_parallelism()
                .map(|count| count.get())
                .unwrap_or(4),
            progress_interval: Duration::from_millis(100),
        }
    }
}

struct WorkItem {
    id: OpId,
    kind: OpKind,
    flags: Arc<AtomicU8>,
}

struct Inner {
    next_id: AtomicU64,
    state: DashMap<OpId, Arc<parking_lot::Mutex<Operation>>>,
    flags_map: DashMap<OpId, Arc<AtomicU8>>,
    event_tx: crossbeam_channel::Sender<OpEvent>,
    undo_stack: Arc<UndoStack>,
    progress_interval: Duration,
}

/// Multi-worker queue for filesystem operations.
pub struct OperationQueue {
    work_tx: crossbeam_channel::Sender<WorkItem>,
    inner: Arc<Inner>,
    handles: Vec<std::thread::JoinHandle<()>>,
}

impl OperationQueue {
    /// Starts a queue and returns it together with its event receiver.
    #[must_use]
    pub fn start(opts: QueueOptions) -> (Self, crossbeam_channel::Receiver<OpEvent>) {
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (work_tx, work_rx) = crossbeam_channel::unbounded::<WorkItem>();
        let inner = Arc::new(Inner {
            next_id: AtomicU64::new(1),
            state: DashMap::new(),
            flags_map: DashMap::new(),
            event_tx,
            undo_stack: Arc::new(UndoStack::new(50)),
            progress_interval: opts.progress_interval,
        });
        let worker_count = opts.workers.max(1);
        let handles = (0..worker_count)
            .map(|_| {
                let work_rx = work_rx.clone();
                let inner = inner.clone();
                std::thread::spawn(move || worker_loop(work_rx, inner))
            })
            .collect();
        (
            Self {
                work_tx,
                inner,
                handles,
            },
            event_rx,
        )
    }

    /// Queues a new operation and returns its id.
    pub fn submit(&self, kind: OpKind) -> OpId {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let kind = normalize_kind(kind);
        let descriptor = kind.descriptor();
        let flags = Arc::new(AtomicU8::new(0));
        let operation = Operation {
            id,
            kind: kind.clone(),
            status: OpStatus::Queued,
            started_at: None,
            finished_at: None,
            progress: Default::default(),
            error: None,
            undo_token: None,
        };
        let op_arc = Arc::new(parking_lot::Mutex::new(operation));
        self.inner.state.insert(id, op_arc);
        self.inner.flags_map.insert(id, flags.clone());
        let _ = self.inner.event_tx.send(OpEvent::Queued {
            id,
            kind: descriptor,
        });
        let _ = self.work_tx.send(WorkItem { id, kind, flags });
        id
    }

    /// Requests cancellation of an operation.
    pub fn cancel(&self, id: OpId) {
        if let Some(flags) = self.inner.flags_map.get(&id) {
            flags.fetch_or(FLAG_CANCEL, Ordering::Relaxed);
        }
    }

    /// Pauses an operation at its next check point.
    pub fn pause(&self, id: OpId) {
        if let Some(flags) = self.inner.flags_map.get(&id) {
            flags.fetch_or(FLAG_PAUSE, Ordering::Relaxed);
        }
        if let Some(op) = self.inner.state.get(&id) {
            let mut op = op.lock();
            if matches!(op.status, OpStatus::Queued | OpStatus::Running) {
                op.status = OpStatus::Paused;
            }
        }
    }

    /// Resumes a paused operation.
    pub fn resume(&self, id: OpId) {
        if let Some(flags) = self.inner.flags_map.get(&id) {
            flags.fetch_and(!FLAG_PAUSE, Ordering::Relaxed);
        }
        if let Some(op) = self.inner.state.get(&id) {
            let mut op = op.lock();
            if op.status == OpStatus::Paused {
                op.status = OpStatus::Running;
            }
        }
    }

    /// Returns a snapshot of all known operations.
    #[must_use]
    pub fn list(&self) -> Vec<Operation> {
        let mut operations = self
            .inner
            .state
            .iter()
            .map(|entry| entry.value().lock().clone())
            .collect::<Vec<_>>();
        operations.sort_by_key(|op| op.id);
        operations
    }

    /// Returns a snapshot of a single operation if present.
    #[must_use]
    pub fn get(&self, id: OpId) -> Option<Operation> {
        self.inner
            .state
            .get(&id)
            .map(|entry| entry.value().lock().clone())
    }

    /// Stops the queue and joins worker threads.
    pub fn shutdown(self) {
        drop(self.work_tx);
        for handle in self.handles {
            let _ = handle.join();
        }
    }

    /// Returns the shared undo stack.
    #[must_use]
    pub fn undo_stack(&self) -> &Arc<UndoStack> {
        &self.inner.undo_stack
    }
}

fn normalize_kind(kind: OpKind) -> OpKind {
    match kind {
        OpKind::Copy {
            sources,
            dest_dir,
            policy,
        } => OpKind::Copy {
            sources: sources
                .into_iter()
                .map(atlas_core::path::expand_tilde)
                .collect(),
            dest_dir: atlas_core::path::expand_tilde(dest_dir),
            policy,
        },
        OpKind::Move {
            sources,
            dest_dir,
            policy,
        } => OpKind::Move {
            sources: sources
                .into_iter()
                .map(atlas_core::path::expand_tilde)
                .collect(),
            dest_dir: atlas_core::path::expand_tilde(dest_dir),
            policy,
        },
        OpKind::Delete { paths, to_trash } => OpKind::Delete {
            paths: paths
                .into_iter()
                .map(atlas_core::path::expand_tilde)
                .collect(),
            to_trash,
        },
        OpKind::Rename { path, new_name } => OpKind::Rename {
            path: atlas_core::path::expand_tilde(path),
            new_name,
        },
        OpKind::Mkdir { path, parents } => OpKind::Mkdir {
            path: atlas_core::path::expand_tilde(path),
            parents,
        },
    }
}

fn worker_loop(work_rx: crossbeam_channel::Receiver<WorkItem>, inner: Arc<Inner>) {
    while let Ok(item) = work_rx.recv() {
        let op_arc = match inner.state.get(&item.id) {
            Some(record) => record.value().clone(),
            None => continue,
        };

        {
            let mut op = op_arc.lock();
            op.status = if item.flags.load(Ordering::Relaxed) & FLAG_PAUSE != 0 {
                OpStatus::Paused
            } else {
                OpStatus::Running
            };
            op.started_at = Some(std::time::Instant::now());
        }
        let _ = inner.event_tx.send(OpEvent::Started { id: item.id });

        let result = execute_item(&item, &inner, &op_arc);

        let mut op = op_arc.lock();
        op.finished_at = Some(std::time::Instant::now());
        match result {
            Ok(undo_entry) => {
                op.status = OpStatus::Done;
                if let Some(entry) = undo_entry {
                    let token = inner.undo_stack.push(entry);
                    op.undo_token = Some(token);
                }
                drop(op);
                let _ = inner.event_tx.send(OpEvent::Completed { id: item.id });
            }
            Err(atlas_core::AtlasError::Cancelled) => {
                op.status = OpStatus::Cancelled;
                drop(op);
                let _ = inner.event_tx.send(OpEvent::Cancelled { id: item.id });
            }
            Err(error) => {
                let error_string = error.to_string();
                let partial_progress = op.progress.clone();
                op.status = OpStatus::Failed;
                op.error = Some(error_string.clone());
                drop(op);
                let _ = inner.event_tx.send(OpEvent::Failed {
                    id: item.id,
                    error: error_string,
                    partial_progress,
                });
            }
        }
    }
}

fn execute_item(
    item: &WorkItem,
    inner: &Inner,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
) -> atlas_core::Result<Option<UndoEntry>> {
    match &item.kind {
        OpKind::Copy {
            sources,
            dest_dir,
            policy,
        } => {
            copy_items(
                item.id,
                sources,
                dest_dir,
                *policy,
                &item.flags,
                &inner.event_tx,
                op_arc,
                inner.progress_interval,
            )?;
            Ok(None)
        }
        OpKind::Move {
            sources,
            dest_dir,
            policy,
        } => {
            move_items(
                item.id,
                sources,
                dest_dir,
                *policy,
                &item.flags,
                &inner.event_tx,
                op_arc,
                inner.progress_interval,
            )?;
            Ok(None)
        }
        OpKind::Delete { paths, to_trash } => delete_paths(
            item.id,
            paths,
            *to_trash,
            &item.flags,
            &inner.event_tx,
            op_arc,
        ),
        OpKind::Rename { path, new_name } => Ok(Some(rename_op(path, new_name)?)),
        OpKind::Mkdir { path, parents } => {
            mkdir_op(path, *parents)?;
            Ok(None)
        }
    }
}
