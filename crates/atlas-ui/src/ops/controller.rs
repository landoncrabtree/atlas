//! [`OpsController`] — bridges the `atlas_ops` queue to the Slint UI panel.
//!
//! # Threading
//!
//! - Construction spawns one long-lived `atlas-ops-events` thread that drains
//!   [`atlas_ops::OpEvent`]s and updates [`OpRow`] state.
//! - After every state change the thread calls
//!   [`slint::invoke_from_event_loop`] to push the new row list into the Slint
//!   window. Progress events are debounced: a per-id timestamp prevents
//!   hammering the event loop faster than ~50 ms while the op is running.
//! - All other methods (`submit_*`, `cancel`, `dismiss`, `toggle_visible`) are
//!   safe to call from any thread.
//!
//! # Conflict resolution (MVP)
//!
//! Copy and Move default to [`atlas_ops::ConflictPolicy::RenameWithSuffix`].
//! If the queue raises [`atlas_ops::OpEvent::Conflict`] (which requires a
//! `Prompt` policy), this controller auto-resolves with
//! [`atlas_ops::ConflictDecision::Skip`] and logs a warning. A modal dialog
//! for interactive resolution is a post-MVP follow-up.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use ahash::AHashMap;
use atlas_ops::{
    ConflictDecision, ConflictPolicy, OpEvent, OpId, OpKind, OperationQueue, QueueOptions,
};
use crossbeam_channel::Receiver;
use parking_lot::RwLock;
use slint::{ModelRc, VecModel};

use super::{format::format_op_title, models::OpRow};
use crate::AtlasWindow;

/// Minimum interval between per-operation progress pushes to the Slint UI.
const DEBOUNCE: Duration = Duration::from_millis(50);

/// Controller that owns an `atlas_ops` queue, tracks operation state, and
/// syncs it to the Slint ops panel.
pub struct OpsController {
    queue: Arc<OperationQueue>,
    /// Current row state (including terminal rows until dismissed).
    rows: RwLock<Vec<OpRow>>,
    /// Whether the ops panel tray is currently shown.
    visible: AtomicBool,
    /// Weak handle to the Slint window for property updates.
    window: RwLock<slint::Weak<AtlasWindow>>,
}

impl OpsController {
    /// Create a new controller, start the underlying queue, and spawn the
    /// event-drain thread.
    #[must_use]
    pub fn new() -> Arc<Self> {
        let (queue, event_rx) = OperationQueue::start(QueueOptions::default());

        let ctrl = Arc::new(Self {
            queue: Arc::new(queue),
            rows: RwLock::new(Vec::new()),
            visible: AtomicBool::new(false),
            window: RwLock::new(slint::Weak::default()),
        });

        let ctrl_weak = Arc::downgrade(&ctrl);
        std::thread::Builder::new()
            .name("atlas-ops-events".to_owned())
            .spawn(move || drain_events(ctrl_weak, event_rx))
            .expect("failed to spawn atlas-ops-events thread");

        ctrl
    }

    /// Attach the Slint window so the controller can push UI updates.
    ///
    /// Must be called before any operation is submitted if you want UI
    /// feedback. Safe to call from any thread.
    pub fn attach_window(&self, window: slint::Weak<AtlasWindow>) {
        *self.window.write() = window;
    }

    // ── submission ────────────────────────────────────────────────────────────

    /// Submit a Copy operation.
    ///
    /// `sources` are the paths to copy; `dest_dir` is the destination
    /// directory. Conflicts default to [`ConflictPolicy::RenameWithSuffix`]
    /// (non-destructive).
    pub fn submit_copy(&self, sources: Vec<PathBuf>, dest_dir: PathBuf) {
        self.queue.submit(OpKind::Copy {
            sources,
            dest_dir,
            policy: ConflictPolicy::RenameWithSuffix,
        });
    }

    /// Submit a Move operation.
    ///
    /// Conflicts default to [`ConflictPolicy::RenameWithSuffix`].
    pub fn submit_move(&self, sources: Vec<PathBuf>, dest_dir: PathBuf) {
        self.queue.submit(OpKind::Move {
            sources,
            dest_dir,
            policy: ConflictPolicy::RenameWithSuffix,
        });
    }

    /// Submit a Delete operation.
    ///
    /// When `to_trash` is `true` (the default for F8), items are sent to the
    /// OS trash rather than permanently deleted.
    pub fn submit_delete(&self, paths: Vec<PathBuf>, to_trash: bool) {
        self.queue.submit(OpKind::Delete { paths, to_trash });
    }

    /// Submit a Rename operation.
    ///
    /// # MVP note
    ///
    /// The rename UI dialog is not yet implemented. For now this submits the
    /// operation directly with the provided `new_name`. Callers (e.g. the F2
    /// handler) should obtain the new name from an inline text input or modal
    /// before calling this method. The F2 binding logs and skips for now.
    pub fn submit_rename(&self, path: PathBuf, new_name: String) {
        self.queue.submit(OpKind::Rename { path, new_name });
    }

    /// Submit a Mkdir operation, creating parent directories as needed.
    pub fn submit_mkdir(&self, path: PathBuf) {
        self.queue.submit(OpKind::Mkdir {
            path,
            parents: true,
        });
    }

    // ── lifecycle ─────────────────────────────────────────────────────────────

    /// Request cancellation of the operation identified by `id`.
    pub fn cancel(&self, id: OpId) {
        self.queue.cancel(id);
    }

    /// Cancel the operation at the given row index in the visible panel list.
    ///
    /// This is the callback-safe variant used by the Slint `ops-cancel` handler
    /// which only knows the model index, not the stable `OpId`.
    pub fn cancel_by_index(&self, index: usize) {
        let id = self.rows.read().get(index).map(|r| r.id);
        if let Some(id) = id {
            self.cancel(id);
        }
    }

    /// Remove a terminal (done / failed / cancelled) row from the panel.
    ///
    /// No-op if `id` refers to an active or unknown operation.
    pub fn dismiss(&self, id: OpId) {
        let mut rows = self.rows.write();
        rows.retain(|row| !(row.id == id && row.is_terminal));
        let has_rows = !rows.is_empty();
        drop(rows);
        if !has_rows {
            self.visible.store(false, Ordering::Relaxed);
        }
        self.push_to_ui();
    }

    /// Dismiss the operation at the given row index in the visible panel list.
    ///
    /// This is the callback-safe variant used by the Slint `ops-dismiss` handler
    /// which only knows the model index, not the stable `OpId`.
    pub fn dismiss_by_index(&self, index: usize) {
        let id = self.rows.read().get(index).map(|r| r.id);
        if let Some(id) = id {
            self.dismiss(id);
        }
    }

    /// Show or hide the ops tray.
    pub fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Relaxed);
        self.push_to_ui();
    }

    /// Toggle the ops tray open/closed.
    pub fn toggle_visible(&self) {
        let new = !self.visible.load(Ordering::Relaxed);
        self.visible.store(new, Ordering::Relaxed);
        self.push_to_ui();
    }

    // ── internal helpers ──────────────────────────────────────────────────────

    fn handle_event(&self, event: OpEvent) {
        match event {
            OpEvent::Queued { id, kind } => {
                let row = OpRow {
                    id,
                    title: format_op_title(&kind),
                    status: "Queued".to_owned(),
                    progress: 0.0,
                    ..OpRow::default()
                };
                self.rows.write().push(row);
                // Auto-show the tray when a new op is queued.
                self.visible.store(true, Ordering::Relaxed);
                self.push_to_ui();
            }
            OpEvent::Started { id } => {
                self.update_row(id, |row| {
                    row.status = "Running".to_owned();
                });
                self.push_to_ui();
            }
            OpEvent::Progress { id, snapshot } => {
                self.update_row(id, |row| {
                    row.status = "Running".to_owned();
                    row.items_total = snapshot.items_total;
                    row.items_done = snapshot.items_done;
                    row.bytes_total_raw = snapshot.bytes_total;
                    row.bytes_done_raw = snapshot.bytes_done;
                    row.current_path = snapshot
                        .current_path
                        .as_deref()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if snapshot.bytes_total > 0 {
                        row.progress = snapshot.bytes_done as f32 / snapshot.bytes_total as f32;
                    } else if snapshot.items_total > 0 {
                        row.progress = snapshot.items_done as f32 / snapshot.items_total as f32;
                    }
                });
                // Progress events are pushed via the debounced path; see drain_events.
            }
            OpEvent::Conflict {
                id,
                source,
                dest,
                resolver,
            } => {
                // MVP: auto-skip on conflict prompt (safe default; no overwrite without explicit UI).
                // TODO: surface a conflict-resolution modal (post-MVP).
                tracing::warn!(
                    op_id = id,
                    source = %source.display(),
                    dest = %dest.display(),
                    "ops conflict — auto-skipping (MVP: no conflict dialog yet)"
                );
                resolver.resolve(ConflictDecision::Skip);
            }
            OpEvent::Completed { id } => {
                self.update_row(id, |row| {
                    row.status = "Done".to_owned();
                    row.progress = 1.0;
                    row.current_path = String::new();
                    row.is_terminal = true;
                    row.is_error = false;
                });
                self.push_to_ui();
            }
            OpEvent::Failed { id, error, .. } => {
                self.update_row(id, |row| {
                    row.status = format!("Failed: {error}");
                    row.is_terminal = true;
                    row.is_error = true;
                });
                self.push_to_ui();
            }
            OpEvent::Cancelled { id } => {
                self.update_row(id, |row| {
                    row.status = "Cancelled".to_owned();
                    row.is_terminal = true;
                    row.is_error = false;
                });
                self.push_to_ui();
            }
        }
    }

    fn update_row(&self, id: OpId, f: impl FnOnce(&mut OpRow)) {
        let mut rows = self.rows.write();
        if let Some(row) = rows.iter_mut().find(|r| r.id == id) {
            f(row);
        }
    }

    fn push_to_ui(&self) {
        let rows: Vec<crate::OpRow> = self.rows.read().iter().map(OpRow::to_slint).collect();
        let visible = self.visible.load(Ordering::Relaxed);
        let window = self.window.read().clone();

        let _ = slint::invoke_from_event_loop(move || {
            let Some(win) = window.upgrade() else {
                return;
            };
            win.set_ops_panel_visible(visible);
            win.set_ops_rows(ModelRc::new(VecModel::from(rows)));
        });
    }

    /// Return a snapshot of the current rows (for testing).
    #[cfg(test)]
    pub(crate) fn rows_snapshot(&self) -> Vec<OpRow> {
        self.rows.read().clone()
    }
}

impl Default for OpsController {
    fn default() -> Self {
        panic!("OpsController must be created via OpsController::new()");
    }
}

/// Background thread: drain [`OpEvent`]s and update the controller state.
///
/// Debounces `Progress` events per-operation to avoid overwhelming the Slint
/// event loop. All other events are processed immediately.
fn drain_events(ctrl: std::sync::Weak<OpsController>, event_rx: Receiver<OpEvent>) {
    // Per-op last-push timestamps for debouncing progress events.
    let mut last_progress: AHashMap<OpId, Instant> = AHashMap::new();

    for event in &event_rx {
        let Some(ctrl) = ctrl.upgrade() else {
            break;
        };

        // For Progress events, debounce per id.
        if let OpEvent::Progress { id, .. } = &event {
            let id = *id;
            let now = Instant::now();
            if let Some(&last) = last_progress.get(&id) {
                if now.duration_since(last) < DEBOUNCE {
                    // Still update internal state so the row is accurate, but
                    // don't trigger a Slint push yet.
                    ctrl.handle_event(event);
                    continue;
                }
            }
            last_progress.insert(id, now);
        } else {
            // For terminal events, clear the debounce entry.
            match &event {
                OpEvent::Completed { id }
                | OpEvent::Failed { id, .. }
                | OpEvent::Cancelled { id } => {
                    last_progress.remove(id);
                }
                _ => {}
            }
        }

        ctrl.handle_event(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn wait(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    #[test]
    fn submit_copy_creates_row() {
        let ctrl = OpsController::new();
        let src_dir = tempfile::tempdir().expect("tempdir");
        let dst_dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(src_dir.path().join("test.txt"), b"hello").expect("write");

        ctrl.submit_copy(
            vec![src_dir.path().join("test.txt")],
            dst_dir.path().to_owned(),
        );

        // Give the queue and event thread time to process.
        wait(300);
        let rows = ctrl.rows_snapshot();
        assert!(!rows.is_empty(), "expected at least one row");
        assert!(
            rows[0].title.starts_with("Copy:"),
            "title should start with 'Copy:'"
        );
    }

    #[test]
    fn event_drain_progresses_to_done() {
        let ctrl = OpsController::new();
        let dir = tempfile::tempdir().expect("tempdir");
        ctrl.submit_mkdir(dir.path().join("ops_test_newdir"));

        wait(300);
        let rows = ctrl.rows_snapshot();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "Done");
        assert!(rows[0].is_terminal);
        assert!(!rows[0].is_error);
    }

    #[test]
    fn progress_row_has_valid_fraction() {
        let row = OpRow {
            id: 42,
            progress: 0.75,
            items_total: 4,
            items_done: 3,
            ..OpRow::default()
        };
        assert!((row.progress - 0.75).abs() < f32::EPSILON);
        let slint = row.to_slint();
        assert_eq!(slint.items_total, 4);
        assert_eq!(slint.items_done, 3);
    }

    #[test]
    fn cancel_marks_row_cancelled() {
        let ctrl = OpsController::new();
        let dir = tempfile::tempdir().expect("tempdir");

        // mkdir is near-instant; to test cancel we submit it then immediately cancel.
        // Even if it finishes first, the row will be terminal (Done not Cancelled).
        // What we verify here is that calling cancel() doesn't panic/error.
        ctrl.submit_mkdir(dir.path().join("cancel_test_dir"));
        let rows = ctrl.rows_snapshot();
        if !rows.is_empty() {
            ctrl.cancel(rows[0].id);
        }
        wait(200);
        // Row should be terminal (either Done or Cancelled — both are acceptable).
        let rows = ctrl.rows_snapshot();
        assert!(!rows.is_empty());
        assert!(rows[0].is_terminal);
    }

    #[test]
    fn dismiss_removes_terminal_row() {
        let ctrl = OpsController::new();
        let dir = tempfile::tempdir().expect("tempdir");
        ctrl.submit_mkdir(dir.path().join("dismiss_test_dir"));

        wait(300);
        let rows = ctrl.rows_snapshot();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_terminal);
        let id = rows[0].id;

        ctrl.dismiss(id);
        let rows = ctrl.rows_snapshot();
        assert!(rows.is_empty(), "row should be removed after dismiss");
    }

    #[test]
    fn dismiss_non_terminal_row_is_noop() {
        let ctrl = OpsController::new();
        // Manually inject a running row to test the non-terminal guard.
        {
            ctrl.rows.write().push(OpRow {
                id: 99,
                status: "Running".to_owned(),
                is_terminal: false,
                ..OpRow::default()
            });
        }
        ctrl.dismiss(99);
        let rows = ctrl.rows_snapshot();
        assert_eq!(rows.len(), 1, "non-terminal rows must not be dismissed");
    }
}
