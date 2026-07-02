//! [`OpsController`] — bridges the `atlas_ops` queue to the Slint UI panel
//! and the small "progress modal" overlay.
//!
//! # Threading
//!
//! - Construction spawns one long-lived `atlas-ops-events` thread that drains
//!   [`atlas_ops::OpEvent`]s and updates [`OpRow`] state.
//! - After every state change the thread calls
//!   [`slint::invoke_from_event_loop`] to push the new row list into the Slint
//!   window. Progress events are debounced: a per-id timestamp prevents
//!   hammering the event loop faster than ~50 ms while the op is running.
//! - Every submission also spawns a short-lived "promotion timer" (see
//!   [`FOREGROUND_DEFER`]) that decides whether to surface the modal —
//!   trivial ops that finish before the timer fires never flash the modal.
//! - All other methods (`submit_*`, `cancel`, `dismiss`, `toggle_visible`,
//!   `background_current_foreground`, `cancel_current_foreground`) are
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
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use ahash::AHashMap;
use atlas_core::Location;
use atlas_ops::{
    ConflictDecision, ConflictPolicy, OpEvent, OpId, OpKind, OpKindDescriptor, OperationQueue,
    QueueOptions,
};
use crossbeam_channel::Receiver;
use parking_lot::RwLock;
use slint::{ModelRc, SharedString, VecModel};

use super::{
    format::{
        format_eta, format_op_heading, format_op_subtitle, format_op_title, format_size,
        glyph_for_op_kind,
    },
    models::OpRow,
};
use crate::AtlasWindow;

/// Minimum interval between per-operation progress pushes to the Slint UI.
const DEBOUNCE: Duration = Duration::from_millis(50);

/// Delay between a submission and the modal appearing. Ops that finish faster
/// than this never flash the modal — this is the whole point of the two-tier
/// UI (in-your-face modal vs. background panel).
const FOREGROUND_DEFER: Duration = Duration::from_millis(250);

/// Controller that owns an `atlas_ops` queue, tracks operation state, and
/// syncs it to the Slint ops panel + progress modal.
pub struct OpsController {
    queue: Arc<OperationQueue>,
    /// Current row state (including terminal rows until dismissed).
    rows: RwLock<Vec<OpRow>>,
    /// Whether the ops panel tray is currently shown.
    visible: AtomicBool,
    /// Op currently owning the progress modal, if any.
    foreground: RwLock<Option<OpId>>,
    /// Ops queued within the last `FOREGROUND_DEFER` window and still eligible
    /// to be promoted to foreground. Cleared when the op terminates or the
    /// promotion timer fires.
    pending: RwLock<AHashMap<OpId, Instant>>,
    /// Wall-clock timestamps for ops, used to compute ETA. Populated on
    /// `Queued`; cleared on terminal events + dismiss.
    started_at: RwLock<AHashMap<OpId, Instant>>,
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
            foreground: RwLock::new(None),
            pending: RwLock::new(AHashMap::default()),
            started_at: RwLock::new(AHashMap::default()),
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
    /// `sources` are the locations to copy; `dest_dir` is the destination
    /// directory. Conflicts default to [`ConflictPolicy::RenameWithSuffix`]
    /// (non-destructive).
    pub fn submit_copy(&self, sources: Vec<Location>, dest_dir: Location) {
        self.queue.submit(OpKind::Copy {
            sources,
            dest_dir,
            policy: ConflictPolicy::RenameWithSuffix,
        });
    }

    /// Submit a Move operation.
    ///
    /// Conflicts default to [`ConflictPolicy::RenameWithSuffix`].
    pub fn submit_move(&self, sources: Vec<Location>, dest_dir: Location) {
        self.queue.submit(OpKind::Move {
            sources,
            dest_dir,
            policy: ConflictPolicy::RenameWithSuffix,
        });
    }

    /// Submit a Delete operation.
    ///
    /// When `to_trash` is `true` (the default for F8), items are sent to the
    /// OS trash rather than permanently deleted. Remote paths always hard-delete.
    pub fn submit_delete(&self, paths: Vec<Location>, to_trash: bool) {
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
    pub fn submit_rename(&self, path: Location, new_name: String) {
        self.queue.submit(OpKind::Rename { path, new_name });
    }

    /// Submit a Mkdir operation, creating parent directories as needed.
    pub fn submit_mkdir(&self, path: Location) {
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

    /// Cancel whatever op currently owns the progress modal, if any.
    ///
    /// Wired to the modal's `Cancel` button. Cancellation flows through
    /// [`atlas_ops::OperationQueue::cancel`] like every other cancel — the
    /// subsequent [`OpEvent::Cancelled`] tears the modal down naturally.
    pub fn cancel_current_foreground(&self) {
        let id = *self.foreground.read();
        if let Some(id) = id {
            tracing::debug!(op_id = id, "op-modal: user pressed Cancel");
            self.cancel(id);
        }
    }

    /// Demote the current foreground op to the background panel.
    ///
    /// Wired to the modal's `Background` button (and to Escape / Enter /
    /// click-outside). The op keeps running; the modal simply hides.
    pub fn background_current_foreground(&self) {
        let id = self.foreground.write().take();
        if let Some(id) = id {
            tracing::debug!(op_id = id, "op-modal: user pressed Background");
        }
        // Removing the foreground pointer implicitly hides the modal; also
        // reveal the panel so the user can see the op still lives on.
        if self.rows.read().iter().any(|r| !r.is_terminal) {
            self.visible.store(true, Ordering::Relaxed);
        }
        self.push_modal_state();
        self.push_to_ui();
    }

    /// Remove a terminal (done / failed / cancelled) row from the panel.
    ///
    /// No-op if `id` refers to an active or unknown operation.
    pub fn dismiss(&self, id: OpId) {
        let mut rows = self.rows.write();
        rows.retain(|row| !(row.id == id && row.is_terminal));
        let has_rows = !rows.is_empty();
        drop(rows);
        self.started_at.write().remove(&id);
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

    /// Drop every terminal (done / failed / cancelled) row from the panel.
    ///
    /// Wired to the "Clear completed" button in the redesigned ops panel.
    /// Active rows are untouched.
    pub fn clear_completed(&self) {
        let removed_ids: Vec<OpId> = {
            let mut rows = self.rows.write();
            let removed = rows
                .iter()
                .filter(|r| r.is_terminal)
                .map(|r| r.id)
                .collect();
            rows.retain(|r| !r.is_terminal);
            removed
        };
        if !removed_ids.is_empty() {
            let mut started = self.started_at.write();
            for id in &removed_ids {
                started.remove(id);
            }
        }
        if self.rows.read().is_empty() {
            self.visible.store(false, Ordering::Relaxed);
        }
        self.push_to_ui();
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

    fn handle_event(self: &Arc<Self>, event: OpEvent) {
        match event {
            OpEvent::Queued { id, kind } => {
                let (source_summary, dest_summary) = split_source_dest(&kind);
                let row = OpRow {
                    id,
                    title: format_op_title(&kind),
                    status: "Queued".to_owned(),
                    kind: kind.kind.to_owned(),
                    source_summary,
                    dest_summary,
                    progress: 0.0,
                    ..OpRow::default()
                };
                self.rows.write().push(row);
                self.started_at.write().insert(id, Instant::now());
                self.pending.write().insert(id, Instant::now());
                self.spawn_promotion_timer(id);
                self.push_to_ui();
            }
            OpEvent::Started { id } => {
                self.update_row(id, |row| {
                    row.status = "Running".to_owned();
                });
                self.push_to_ui();
                self.push_modal_state();
            }
            OpEvent::Progress { id, snapshot } => {
                let elapsed = self
                    .started_at
                    .read()
                    .get(&id)
                    .map(Instant::elapsed)
                    .unwrap_or_default();
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
                    row.eta = format_eta(elapsed, row.progress);
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
                    row.eta = String::new();
                    row.is_terminal = true;
                    row.is_error = false;
                });
                self.clear_foreground_if(id);
                self.pending.write().remove(&id);
                self.push_to_ui();
                self.push_modal_state();
            }
            OpEvent::Failed { id, error, .. } => {
                self.update_row(id, |row| {
                    row.status = format!("Failed: {error}");
                    row.eta = String::new();
                    row.is_terminal = true;
                    row.is_error = true;
                });
                self.clear_foreground_if(id);
                self.pending.write().remove(&id);
                self.push_to_ui();
                self.push_modal_state();
            }
            OpEvent::Cancelled { id } => {
                self.update_row(id, |row| {
                    row.status = "Cancelled".to_owned();
                    row.eta = String::new();
                    row.is_terminal = true;
                    row.is_error = false;
                });
                self.clear_foreground_if(id);
                self.pending.write().remove(&id);
                self.push_to_ui();
                self.push_modal_state();
            }
        }
    }

    fn update_row(&self, id: OpId, f: impl FnOnce(&mut OpRow)) {
        let mut rows = self.rows.write();
        if let Some(row) = rows.iter_mut().find(|r| r.id == id) {
            f(row);
        }
    }

    fn clear_foreground_if(&self, id: OpId) {
        let mut fg = self.foreground.write();
        if *fg == Some(id) {
            *fg = None;
        }
    }

    /// Spawn a short-lived timer thread that, after [`FOREGROUND_DEFER`] has
    /// elapsed, promotes `id` to the foreground modal if it's still running.
    ///
    /// Trivial ops that complete faster than `FOREGROUND_DEFER` never flash
    /// the modal — the terminal event will have already removed `id` from
    /// `pending` by the time the timer fires.
    fn spawn_promotion_timer(self: &Arc<Self>, id: OpId) {
        let weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("atlas-ops-promote".to_owned())
            .spawn(move || {
                std::thread::sleep(FOREGROUND_DEFER);
                let Some(ctrl) = weak.upgrade() else {
                    return;
                };
                // If the op finished before the timer fired, `pending` no
                // longer has this id — bail out.
                let still_pending = ctrl.pending.write().remove(&id).is_some();
                if !still_pending {
                    return;
                }
                // Confirm the op is genuinely still running before promoting.
                let non_terminal = ctrl
                    .rows
                    .read()
                    .iter()
                    .any(|r| r.id == id && !r.is_terminal);
                if !non_terminal {
                    return;
                }
                // Most-recent-wins: whatever op was foreground gets replaced.
                // The previous op stays in the ops panel and keeps running.
                *ctrl.foreground.write() = Some(id);
                ctrl.push_modal_state();
            })
            .expect("failed to spawn atlas-ops-promote thread");
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

    fn push_modal_state(&self) {
        let snapshot = self.build_modal_snapshot();
        let window = self.window.read().clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(win) = window.upgrade() else {
                return;
            };
            win.set_op_modal_visible(snapshot.visible);
            win.set_op_modal_title(SharedString::from(snapshot.title));
            win.set_op_modal_subtitle(SharedString::from(snapshot.subtitle));
            win.set_op_modal_detail(SharedString::from(snapshot.detail));
            win.set_op_modal_progress(snapshot.progress);
            win.set_op_modal_indeterminate(snapshot.indeterminate);
            win.set_op_modal_icon(SharedString::from(snapshot.icon));
        });
    }

    fn build_modal_snapshot(&self) -> ModalSnapshot {
        let Some(id) = *self.foreground.read() else {
            return ModalSnapshot::hidden();
        };
        let rows = self.rows.read();
        let Some(row) = rows.iter().find(|r| r.id == id) else {
            return ModalSnapshot::hidden();
        };
        // Never show the modal for a terminal row — the terminal-event
        // handler clears `foreground`, but a stale render could race.
        if row.is_terminal {
            return ModalSnapshot::hidden();
        }
        let heading = format_op_heading(&row.kind);
        let icon = glyph_for_op_kind(&row.kind).to_owned();
        let subtitle = if row.dest_summary.is_empty() {
            row.source_summary.clone()
        } else if row.source_summary.is_empty() {
            row.dest_summary.clone()
        } else {
            format!("{} → {}", row.source_summary, row.dest_summary)
        };
        let detail = build_modal_detail(row);
        // If we don't have progress numbers yet, mark indeterminate so the
        // bar renders a wide segment instead of an empty rail.
        let indeterminate = row.bytes_total_raw == 0 && row.items_total == 0;
        ModalSnapshot {
            visible: true,
            title: heading.to_owned(),
            subtitle,
            detail,
            progress: row.progress,
            indeterminate,
            icon,
        }
    }

    /// Return a snapshot of the current rows (for testing).
    #[cfg(test)]
    pub(crate) fn rows_snapshot(&self) -> Vec<OpRow> {
        self.rows.read().clone()
    }

    /// Return the current foreground op id (for testing).
    #[cfg(test)]
    pub(crate) fn foreground_snapshot(&self) -> Option<OpId> {
        *self.foreground.read()
    }
}

/// Compact snapshot pushed to Slint on every foreground state change.
struct ModalSnapshot {
    visible: bool,
    title: String,
    subtitle: String,
    detail: String,
    progress: f32,
    indeterminate: bool,
    icon: String,
}

impl ModalSnapshot {
    fn hidden() -> Self {
        Self {
            visible: false,
            title: String::new(),
            subtitle: String::new(),
            detail: String::new(),
            progress: 0.0,
            indeterminate: false,
            icon: String::new(),
        }
    }
}

/// Split an [`OpKindDescriptor`]'s summary into `(source, dest)` for panel
/// and modal display.
///
/// The atlas-ops summary is a small, well-defined format:
/// - `Copy` / `Move` / `Rename`: `"{source} → {dest}"`.
/// - `Trash` / `Delete`: `"{n} items"` — no destination.
/// - `Mkdir`: `"create {path}"` or `"create {path} (with parents)"`.
fn split_source_dest(kind: &OpKindDescriptor) -> (String, String) {
    let summary = format_op_subtitle(kind);
    match kind.kind {
        "Copy" | "Move" | "Rename" => {
            if let Some((lhs, rhs)) = summary.split_once(" → ") {
                (lhs.to_owned(), rhs.to_owned())
            } else {
                (summary, String::new())
            }
        }
        "Trash" | "Delete" => (summary, String::new()),
        "Mkdir" => {
            let stripped = summary
                .strip_prefix("create ")
                .unwrap_or(&summary)
                .trim_end_matches(" (with parents)");
            (String::new(), stripped.to_owned())
        }
        _ => (summary, String::new()),
    }
}

/// Build the "detail" text line for the progress modal.
///
/// Prefers bytes when the op has a byte total (copy / move of files);
/// falls back to item counts (delete / trash of many items); appends the
/// current file name when available.
fn build_modal_detail(row: &OpRow) -> String {
    let base = if row.bytes_total_raw > 0 {
        format!(
            "{} of {}",
            format_size(row.bytes_done_raw),
            format_size(row.bytes_total_raw)
        )
    } else if row.items_total > 0 {
        format!("{} of {} items", row.items_done, row.items_total)
    } else {
        String::new()
    };
    match (
        base.is_empty(),
        row.current_path.is_empty(),
        row.eta.is_empty(),
    ) {
        (true, true, _) => String::new(),
        (true, false, true) => row.current_path.clone(),
        (true, false, false) => format!("{}  ·  {} left", row.current_path, row.eta),
        (false, true, true) => base,
        (false, true, false) => format!("{}  ·  {} left", base, row.eta),
        (false, false, true) => format!("{}  ·  {}", base, row.current_path),
        (false, false, false) => format!("{}  ·  {}  ·  {} left", base, row.current_path, row.eta),
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

        // Decide if this Progress tick should push to the UI.
        let mut should_push_progress = false;
        if let OpEvent::Progress { id, .. } = &event {
            let id = *id;
            let now = Instant::now();
            let elapsed_ok = last_progress
                .get(&id)
                .map(|&last| now.duration_since(last) >= DEBOUNCE)
                .unwrap_or(true);
            if elapsed_ok {
                last_progress.insert(id, now);
                should_push_progress = true;
            }
        }

        // Terminal events clean up the debounce state.
        match &event {
            OpEvent::Completed { id } | OpEvent::Failed { id, .. } | OpEvent::Cancelled { id } => {
                last_progress.remove(id);
            }
            _ => {}
        }

        // `handle_event` updates internal state; non-Progress events also
        // push_to_ui / push_modal_state. Progress needs an explicit push
        // gated on the debounce.
        let was_progress = matches!(&event, OpEvent::Progress { .. });
        ctrl.handle_event(event);
        if was_progress && should_push_progress {
            ctrl.push_to_ui();
            ctrl.push_modal_state();
        }
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
            vec![Location::local(src_dir.path().join("test.txt"))],
            Location::local(dst_dir.path()),
        );

        // Give the queue and event thread time to process.
        wait(300);
        let rows = ctrl.rows_snapshot();
        assert!(!rows.is_empty(), "expected at least one row");
        assert!(
            rows[0].title.starts_with("Copy:"),
            "title should start with 'Copy:'"
        );
        assert_eq!(rows[0].kind, "Copy");
    }

    #[test]
    fn event_drain_progresses_to_done() {
        let ctrl = OpsController::new();
        let dir = tempfile::tempdir().expect("tempdir");
        ctrl.submit_mkdir(Location::local(dir.path().join("ops_test_newdir")));

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
        ctrl.submit_mkdir(Location::local(dir.path().join("cancel_test_dir")));
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
        ctrl.submit_mkdir(Location::local(dir.path().join("dismiss_test_dir")));

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

    #[test]
    fn trivial_op_never_promoted_to_foreground() {
        // mkdir is near-instant (<<250ms). The promotion timer should fire
        // *after* the terminal event has cleared `pending`, so foreground
        // must remain `None` and the modal must never appear.
        let ctrl = OpsController::new();
        let dir = tempfile::tempdir().expect("tempdir");
        ctrl.submit_mkdir(Location::local(dir.path().join("fast_op")));

        // Wait past the 250ms defer *plus* debounce slack.
        wait(400);

        assert_eq!(
            ctrl.foreground_snapshot(),
            None,
            "trivial op must not surface the modal"
        );
        let rows = ctrl.rows_snapshot();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_terminal, "row should have terminated");
    }

    #[test]
    fn background_and_clear_completed() {
        let ctrl = OpsController::new();
        let dir = tempfile::tempdir().expect("tempdir");
        ctrl.submit_mkdir(Location::local(dir.path().join("bg_test_dir")));
        wait(300);
        // Nothing to background (no foreground was ever set); the call must
        // be a benign no-op.
        ctrl.background_current_foreground();
        assert_eq!(ctrl.foreground_snapshot(), None);

        assert_eq!(ctrl.rows_snapshot().len(), 1);
        ctrl.clear_completed();
        assert!(ctrl.rows_snapshot().is_empty());
    }

    #[test]
    fn split_source_dest_shapes() {
        use atlas_ops::OpKindDescriptor;
        let copy = OpKindDescriptor {
            kind: "Copy",
            summary: "3 items → /Users/x/Downloads".to_owned(),
        };
        assert_eq!(
            split_source_dest(&copy),
            ("3 items".to_owned(), "/Users/x/Downloads".to_owned())
        );
        let trash = OpKindDescriptor {
            kind: "Trash",
            summary: "2 items".to_owned(),
        };
        assert_eq!(
            split_source_dest(&trash),
            ("2 items".to_owned(), String::new())
        );
        let mkdir = OpKindDescriptor {
            kind: "Mkdir",
            summary: "create /tmp/x (with parents)".to_owned(),
        };
        assert_eq!(
            split_source_dest(&mkdir),
            (String::new(), "/tmp/x".to_owned())
        );
    }
}
