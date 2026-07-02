//! Operation queue implementation with worker threads and event streaming.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use atlas_core::Location;
use dashmap::DashMap;

use crate::execute::execute_op;
use crate::op::{OpEvent, OpId, OpKind, OpStatus, Operation, FLAG_CANCEL, FLAG_PAUSE};
use crate::runtime::shared_runtime_handle;
use crate::undo::UndoStack;

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

pub(crate) struct Inner {
    next_id: AtomicU64,
    state: DashMap<OpId, Arc<parking_lot::Mutex<Operation>>>,
    flags_map: DashMap<OpId, Arc<AtomicU8>>,
    pub(crate) event_tx: crossbeam_channel::Sender<OpEvent>,
    undo_stack: Arc<UndoStack>,
    pub(crate) progress_interval: Duration,
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

fn normalize_location(loc: Location) -> Location {
    match loc {
        Location::Local(path) => Location::Local(atlas_core::path::expand_tilde(path)),
        remote @ Location::Remote(_, _) => remote,
    }
}

fn normalize_locations(locs: Vec<Location>) -> Vec<Location> {
    locs.into_iter().map(normalize_location).collect()
}

fn normalize_kind(kind: OpKind) -> OpKind {
    match kind {
        OpKind::Copy {
            sources,
            dest_dir,
            policy,
        } => OpKind::Copy {
            sources: normalize_locations(sources),
            dest_dir: normalize_location(dest_dir),
            policy,
        },
        OpKind::Move {
            sources,
            dest_dir,
            policy,
        } => OpKind::Move {
            sources: normalize_locations(sources),
            dest_dir: normalize_location(dest_dir),
            policy,
        },
        OpKind::Delete { paths, to_trash } => OpKind::Delete {
            paths: normalize_locations(paths),
            to_trash,
        },
        OpKind::Rename { path, new_name } => OpKind::Rename {
            path: normalize_location(path),
            new_name,
        },
        OpKind::Mkdir { path, parents } => OpKind::Mkdir {
            path: normalize_location(path),
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

        // Every op goes through `execute_op`. Locally-native ops still run
        // synchronously inside a `spawn_blocking` — the tokio runtime lets us
        // treat local and remote paths uniformly without dedicating a second
        // pool. Remote / cross-backend work uses `atlas-remote` directly.
        let handle = shared_runtime_handle();
        let flags = Arc::clone(&item.flags);
        let event_tx = inner.event_tx.clone();
        let op_arc_clone = Arc::clone(&op_arc);
        let progress_interval = inner.progress_interval;
        let id = item.id;
        let kind = item.kind.clone();
        let result = handle.block_on(async move {
            execute_op(id, kind, flags, event_tx, op_arc_clone, progress_interval).await
        });

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
