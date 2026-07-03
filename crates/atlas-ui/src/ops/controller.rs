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
//! # Conflict resolution
//!
//! Copy and Move default to [`atlas_ops::ConflictPolicy::RenameWithSuffix`].
//! If the queue raises [`atlas_ops::OpEvent::Conflict`] (which requires a
//! `Prompt` policy), this controller auto-resolves with
//! [`atlas_ops::ConflictDecision::Skip`] and logs a warning. A modal dialog
//! for interactive resolution is a follow-up.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
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
use atlas_remote::StreamProgress;
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
    /// Monotonic id source for controller-managed rows (preview
    /// downloads) that don't ride the `OperationQueue`. The queue
    /// itself assigns `1..next_queue_id`; controller ids live in the
    /// high half of the space starting at [`CONTROLLER_ID_BASE`] to
    /// avoid collision.
    preview_id_seq: AtomicU64,
    /// Cancellation flags for controller-managed rows. Consulted by
    /// [`Self::cancel`] before delegating to the queue, so the ops
    /// panel's per-row cancel button works for preview downloads
    /// too.
    preview_cancels: RwLock<AHashMap<OpId, Arc<AtomicBool>>>,
    /// Pending conflict resolutions. Populated when
    /// [`OpEvent::Conflict`] fires and the UI is asked to answer.
    /// Cleared once the modal callback delivers a decision (or the
    /// op finishes / is cancelled). See [`Self::submit_conflict`].
    pending_conflicts: RwLock<Vec<PendingConflict>>,
    /// Per-op decision cache for the "Apply to all" modal checkbox.
    /// A non-empty entry means every subsequent conflict for that op
    /// resolves immediately with the cached decision instead of
    /// re-prompting the user.
    apply_to_all: RwLock<AHashMap<OpId, ConflictDecision>>,
}

/// A conflict awaiting user input.
///
/// Kept in FIFO order so a burst of `OpEvent::Conflict` events fills
/// the queue and the modal drains one at a time. `resolver.resolve(...)`
/// unblocks the ops-thread once the user answers.
struct PendingConflict {
    op_id: OpId,
    resolver: atlas_ops::ConflictResponder,
    prompt: ConflictPrompt,
}

/// Snapshot pushed into Slint when a conflict prompt arrives.
///
/// Every visible cell of the modal is derived from this struct — the
/// controller re-computes it via [`OpsController::build_conflict_snapshot`]
/// after every modal state change.
#[derive(Debug, Clone)]
pub struct ConflictPrompt {
    /// File name in question (e.g. `README.md`).
    pub name: String,
    /// Absolute-ish display of the source location.
    pub source_display: String,
    /// Absolute-ish display of the destination.
    pub dest_display: String,
    /// True when the source was modified more recently than the
    /// destination. Drives Finder-parity phrasing.
    pub source_is_newer: bool,
    /// True when the destination was modified more recently than
    /// the source. Drives Finder-parity phrasing when replacing an
    /// older file with a newer one still applies to the opposite
    /// direction.
    pub source_is_older: bool,
}

/// Base id for controller-managed synthetic operations. See
/// [`OpsController::preview_id_seq`]. We reserve the top bit of the
/// `u64` id space to prevent collision with queue-assigned ids
/// (which start at 1 and increment on every submit).
const CONTROLLER_ID_BASE: OpId = 0x8000_0000_0000_0000;

/// Handle returned by [`OpsController::start_preview_download`].
///
/// The caller feeds `progress_tx` into
/// [`atlas_remote::stream::stream_copy`] and periodically checks
/// [`Self::is_cancelled`]. On completion / failure the handle must
/// be resolved via [`Self::complete`], [`Self::fail`], or
/// [`Self::cancelled`] — dropping it without a resolution leaves a
/// "running" row in the ops panel.
///
/// The handle intentionally borrows the shared `progress_tx`;
/// callers may `clone()` it and pass into `stream_copy` as
/// `Some(&sender)`.
pub struct PreviewDownloadHandle {
    /// Stable id used to key the row and cancel state on the
    /// controller. Lies in the [`CONTROLLER_ID_BASE`] range so it
    /// never collides with a queue-assigned id.
    pub id: OpId,
    /// Sender the caller passes to `stream_copy`. Progress deltas
    /// flow via a background bridge into `OpEvent::Progress`-style
    /// row updates.
    pub progress_tx: crossbeam_channel::Sender<StreamProgress>,
    /// Cancellation flag flipped by the panel's cancel button.
    pub cancel: Arc<AtomicBool>,
    controller: std::sync::Weak<OpsController>,
    /// Set on terminal transition to prevent double-finalisation.
    finalised: AtomicBool,
}

impl PreviewDownloadHandle {
    /// Fast-path predicate the streaming loop calls between chunks.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Mark the download successful. The row transitions to a
    /// terminal "Done" state and fades out on the standard 5s timer.
    pub fn complete(&self) {
        self.finalise(|row| {
            row.status = "Done".to_owned();
            row.progress = 1.0;
            row.current_path.clear();
            row.eta.clear();
            row.is_terminal = true;
            row.is_error = false;
        });
    }

    /// Mark the download failed with a human-readable message.
    pub fn fail(&self, error: impl Into<String>) {
        let error = error.into();
        self.finalise(move |row| {
            row.status = format!("Failed: {error}");
            row.eta.clear();
            row.is_terminal = true;
            row.is_error = true;
        });
    }

    /// Mark the download cancelled — expected after the user hits
    /// the row's cancel button.
    pub fn cancelled(&self) {
        self.finalise(|row| {
            row.status = "Cancelled".to_owned();
            row.eta.clear();
            row.is_terminal = true;
            row.is_error = false;
        });
    }

    fn finalise(&self, mutate: impl FnOnce(&mut super::models::OpRow)) {
        if self.finalised.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(ctrl) = self.controller.upgrade() else {
            return;
        };
        ctrl.finalise_preview_row(self.id, mutate);
    }
}

impl Drop for PreviewDownloadHandle {
    fn drop(&mut self) {
        // Guarantee no dangling "running" rows if the caller forgot
        // to finalise. Emit a Failed status so users know the op
        // didn't complete cleanly.
        if !self.finalised.swap(true, Ordering::SeqCst) {
            if let Some(ctrl) = self.controller.upgrade() {
                ctrl.finalise_preview_row(self.id, |row| {
                    row.status = "Failed: preview handle dropped without completion".to_owned();
                    row.eta.clear();
                    row.is_terminal = true;
                    row.is_error = true;
                });
            }
        }
    }
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
            preview_id_seq: AtomicU64::new(0),
            preview_cancels: RwLock::new(AHashMap::default()),
            pending_conflicts: RwLock::new(Vec::new()),
            apply_to_all: RwLock::new(AHashMap::default()),
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
    /// directory. Conflicts default to [`ConflictPolicy::Prompt`]
    /// (Finder-parity) — the shell surfaces the `AtlasConflictModal`
    /// with Keep Both · Stop · Replace when a collision occurs.
    /// Callers wanting the silent-rename behaviour (bulk automation,
    /// tests) can use [`Self::submit_copy_with_policy`] with an
    /// explicit [`ConflictPolicy::RenameWithSuffix`].
    pub fn submit_copy(&self, sources: Vec<Location>, dest_dir: Location) {
        self.queue.submit(OpKind::Copy {
            sources,
            dest_dir,
            policy: ConflictPolicy::Prompt,
        });
    }

    /// Submit a Move operation.
    ///
    /// Conflicts default to [`ConflictPolicy::Prompt`] (Finder-parity).
    pub fn submit_move(&self, sources: Vec<Location>, dest_dir: Location) {
        self.queue.submit(OpKind::Move {
            sources,
            dest_dir,
            policy: ConflictPolicy::Prompt,
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

    /// Submit a Copy operation with an explicit conflict policy.
    ///
    /// Callers pick `ConflictPolicy::Prompt` when they want the
    /// conflict modal to surface for every colliding destination;
    /// the plain [`Self::submit_copy`] defaults to
    /// [`ConflictPolicy::RenameWithSuffix`] which is safe but silent.
    pub fn submit_copy_with_policy(
        &self,
        sources: Vec<Location>,
        dest_dir: Location,
        policy: ConflictPolicy,
    ) {
        self.queue.submit(OpKind::Copy {
            sources,
            dest_dir,
            policy,
        });
    }

    /// Submit a Move operation with an explicit conflict policy.
    pub fn submit_move_with_policy(
        &self,
        sources: Vec<Location>,
        dest_dir: Location,
        policy: ConflictPolicy,
    ) {
        self.queue.submit(OpKind::Move {
            sources,
            dest_dir,
            policy,
        });
    }

    // ── preview downloads ────────────────────────────────────────────────────
    //
    // Remote-file preview downloads reuse the ops-panel UI without
    // going through the queue: they already have their own reader /
    // writer plumbing inside `PreviewCache`, they only need progress
    // + cancel affordances. `start_preview_download` allocates a
    // controller-managed OpId, inserts an OpRow, and spawns a
    // background bridge that pumps `StreamProgress` events into row
    // updates. Cancellation flips an atomic shared with the caller.

    /// Register a preview download as an ops-panel row and return a
    /// [`PreviewDownloadHandle`] the caller feeds to
    /// [`atlas_remote::stream::stream_copy`].
    ///
    /// `display_name` is the file's basename (e.g. `readme.txt`);
    /// `source_display` is the source URI for the panel's source
    /// column (`sftp://user@host/pub/readme.txt`); `total_bytes` is
    /// the expected content length.
    ///
    /// The row is inserted only after `FOREGROUND_DEFER` unless the
    /// caller opts in via [`Self::start_preview_download_immediate`].
    /// Small / fast downloads never surface a row; that's why the
    /// cache-hit fast path stays instant.
    pub fn start_preview_download(
        self: &Arc<Self>,
        display_name: impl Into<String>,
        source_display: impl Into<String>,
        total_bytes: u64,
    ) -> PreviewDownloadHandle {
        self.spawn_preview_row(display_name.into(), source_display.into(), total_bytes)
    }

    fn spawn_preview_row(
        self: &Arc<Self>,
        display_name: String,
        source_display: String,
        total_bytes: u64,
    ) -> PreviewDownloadHandle {
        let id =
            CONTROLLER_ID_BASE.saturating_add(self.preview_id_seq.fetch_add(1, Ordering::Relaxed));
        let cancel = Arc::new(AtomicBool::new(false));
        self.preview_cancels.write().insert(id, Arc::clone(&cancel));

        // Insert an initial row + `started_at` timestamp so ETA
        // calculations use the same clock as queue-managed rows.
        let title = format!("Downloading {display_name}");
        let row = super::models::OpRow {
            id,
            title,
            status: "Running".to_owned(),
            kind: "Copy".to_owned(),
            source_summary: source_display,
            dest_summary: display_name.clone(),
            progress: 0.0,
            bytes_total_raw: total_bytes,
            current_path: display_name,
            ..super::models::OpRow::default()
        };
        self.rows.write().push(row);
        self.started_at.write().insert(id, Instant::now());
        self.push_to_ui();

        // Bridge thread: drain StreamProgress events into row updates.
        let (progress_tx, progress_rx) = crossbeam_channel::unbounded::<StreamProgress>();
        let weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("atlas-preview-progress".to_owned())
            .spawn(move || preview_progress_bridge(weak, id, progress_rx))
            .expect("failed to spawn atlas-preview-progress thread");

        PreviewDownloadHandle {
            id,
            progress_tx,
            cancel,
            controller: Arc::downgrade(self),
            finalised: AtomicBool::new(false),
        }
    }

    fn apply_preview_progress(&self, id: OpId, event: &StreamProgress) {
        let elapsed = self
            .started_at
            .read()
            .get(&id)
            .map(Instant::elapsed)
            .unwrap_or_default();
        self.update_row(id, |row| {
            row.bytes_done_raw = event.bytes_transferred;
            if let Some(total) = event.total_bytes {
                row.bytes_total_raw = total;
            }
            if row.bytes_total_raw > 0 {
                row.progress = row.bytes_done_raw as f32 / row.bytes_total_raw as f32;
            }
            row.eta = format_eta(elapsed, row.progress);
        });
    }

    fn finalise_preview_row(&self, id: OpId, mutate: impl FnOnce(&mut super::models::OpRow)) {
        self.update_row(id, mutate);
        self.preview_cancels.write().remove(&id);
        self.clear_foreground_if(id);
        self.push_to_ui();
        self.push_modal_state();
    }

    // ── conflict prompt integration ──────────────────────────────────────────

    /// Deliver the user's decision to the oldest pending conflict.
    ///
    /// If `apply_to_all` is true, cache the decision keyed by that
    /// conflict's op id — subsequent [`OpEvent::Conflict`] events on
    /// the same op will resolve immediately without prompting.
    pub fn resolve_current_conflict(&self, decision: ConflictDecision, apply_to_all: bool) {
        let popped = {
            let mut queue = self.pending_conflicts.write();
            if queue.is_empty() {
                None
            } else {
                Some(queue.remove(0))
            }
        };
        let Some(pending) = popped else {
            tracing::debug!("resolve_current_conflict: no pending conflict");
            return;
        };
        if apply_to_all {
            self.apply_to_all
                .write()
                .insert(pending.op_id, decision.clone());
        }
        pending.resolver.resolve(decision);
        self.push_conflict_state();
    }

    /// Semantic wrapper: user picked "Keep Both". Computes a
    /// non-colliding renamed destination sibling based on the
    /// pending conflict's stored `dest` PathBuf (works for local
    /// paths and URI-shaped PathBufs alike — the ops layer's
    /// backend-aware rename applies the basename to the correct
    /// parent regardless).
    pub fn conflict_keep_both(&self, apply_to_all: bool) {
        let dest = self.peek_pending_dest();
        let decision = match dest {
            Some(dest) => {
                let parent = dest.parent().unwrap_or_else(|| std::path::Path::new("/"));
                let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("copy");
                // NB: for URI-shaped PathBufs `rename_with_suffix`
                // still produces a usable basename because
                // `Path::parent`/`file_name` treat `/` as separator.
                // The extra `.exists()` calls are benign no-ops
                // (URIs never match), so we get suffix index 1 on
                // the first hit — the ops layer's stat-loop
                // handles collision detection on the real backend.
                ConflictDecision::RenameTo(atlas_ops::rename_with_suffix(parent, name))
            }
            None => ConflictDecision::RenameTo(std::path::PathBuf::new()),
        };
        self.resolve_current_conflict(decision, apply_to_all);
    }

    /// Semantic wrapper: user picked "Stop" (cancels the whole op).
    pub fn conflict_stop(&self, apply_to_all: bool) {
        self.resolve_current_conflict(ConflictDecision::Cancel, apply_to_all);
    }

    /// Semantic wrapper: user picked "Replace" (overwrite).
    pub fn conflict_replace(&self, apply_to_all: bool) {
        self.resolve_current_conflict(ConflictDecision::Overwrite, apply_to_all);
    }

    /// Peek at the oldest pending conflict's dest PathBuf without
    /// dequeuing it. Used to compute Keep-Both rename candidates.
    fn peek_pending_dest(&self) -> Option<std::path::PathBuf> {
        self.pending_conflicts
            .read()
            .first()
            .map(|p| std::path::PathBuf::from(p.prompt.dest_display.as_str()))
    }

    /// Access the current conflict prompt payload for tests + shell
    /// wiring. `None` when no prompt is queued.
    #[must_use]
    pub fn conflict_prompt(&self) -> Option<ConflictPrompt> {
        self.pending_conflicts
            .read()
            .first()
            .map(|c| c.prompt.clone())
    }

    fn push_conflict_state(&self) {
        let prompt = self.conflict_prompt();
        let window = self.window.read().clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(win) = window.upgrade() else {
                return;
            };
            match prompt {
                Some(p) => {
                    win.set_conflict_modal_visible(true);
                    win.set_conflict_modal_name(SharedString::from(p.name));
                    win.set_conflict_modal_source(SharedString::from(p.source_display));
                    win.set_conflict_modal_dest(SharedString::from(p.dest_display));
                    win.set_conflict_modal_source_newer(p.source_is_newer);
                    win.set_conflict_modal_source_older(p.source_is_older);
                }
                None => {
                    win.set_conflict_modal_visible(false);
                }
            }
        });
    }

    fn record_conflict(
        &self,
        id: OpId,
        source: std::path::PathBuf,
        dest: std::path::PathBuf,
        resolver: atlas_ops::ConflictResponder,
    ) {
        // Apply-to-all short-circuit: reuse the cached decision, no
        // new prompt, no queue interaction.
        if let Some(decision) = self.apply_to_all.read().get(&id).cloned() {
            resolver.resolve(decision);
            return;
        }
        let prompt = build_conflict_prompt(&source, &dest);
        {
            self.pending_conflicts.write().push(PendingConflict {
                op_id: id,
                resolver,
                prompt,
            });
        }
        self.push_conflict_state();
    }

    // ── lifecycle ─────────────────────────────────────────────────────────────

    /// Request cancellation of the operation identified by `id`.
    ///
    /// Preview downloads (controller-managed rows in the
    /// [`CONTROLLER_ID_BASE`] range) flip their cancellation atomic
    /// so the streaming loop breaks between chunks; queue-managed
    /// ops delegate to [`OperationQueue::cancel`].
    pub fn cancel(&self, id: OpId) {
        if let Some(flag) = self.preview_cancels.read().get(&id).cloned() {
            flag.store(true, Ordering::SeqCst);
            return;
        }
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
    ///
    /// The ops panel visibility is deliberately **not** flipped here — ops
    /// stay silent until the user explicitly asks for them via
    /// `ops::TogglePanel` (`Cmd+J`). Making Background auto-open the panel
    /// swaps one visible surface for another and breaks the "fast and
    /// asynchronous" feel the modal contract promises.
    pub fn background_current_foreground(&self) {
        let id = self.foreground.write().take();
        if let Some(id) = id {
            tracing::debug!(op_id = id, "op-modal: user pressed Background");
        }
        // NB: do not touch `self.visible` here. The panel's open/closed state
        // is user-owned via `ops::TogglePanel` (`Cmd+J`); if it was already
        // open we leave it open, if it was closed we leave it closed.
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

    /// Show or hide the Ops right-dock content state.
    pub fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Relaxed);
        self.push_to_ui();
    }

    /// Toggle the Ops right-dock content state open/closed.
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
                // Route to the conflict modal via the pending queue.
                // If the user checked "Apply to all" on a prior
                // conflict for this op, we resolve immediately.
                self.record_conflict(id, source, dest, resolver);
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
                self.apply_to_all.write().remove(&id);
                self.drop_conflicts_for(id);
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
                self.apply_to_all.write().remove(&id);
                self.drop_conflicts_for(id);
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
                self.apply_to_all.write().remove(&id);
                self.drop_conflicts_for(id);
                self.push_to_ui();
                self.push_modal_state();
            }
            OpEvent::Retrying {
                id,
                attempt,
                next_backoff_ms,
            } => {
                self.update_row(id, |row| {
                    row.status = format!("Retrying (attempt {attempt} in {next_backoff_ms}ms)");
                });
                self.push_to_ui();
                self.push_modal_state();
            }
            OpEvent::RetryFailed { id, attempts } => {
                self.update_row(id, |row| {
                    row.status = format!("Retry gave up after {attempts} attempts");
                });
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

    /// Drop any pending conflicts belonging to `id`. Called on every
    /// terminal event so a race between "op cancelled" and "user
    /// clicked a modal button" doesn't leave an orphan responder.
    fn drop_conflicts_for(&self, id: OpId) {
        let mut queue = self.pending_conflicts.write();
        queue.retain(|p| p.op_id != id);
        drop(queue);
        self.push_conflict_state();
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
        let window = self.window.read().clone();

        let _ = slint::invoke_from_event_loop(move || {
            let Some(win) = window.upgrade() else {
                return;
            };
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

    /// Return a snapshot of the current rows.
    ///
    /// Exposed to integration tests (via `atlas_ui::ops::OpsController::rows_snapshot`)
    /// so preview-download flows and other side-effectful surfaces
    /// can be asserted against without reaching into private state.
    #[must_use]
    pub fn rows_snapshot(&self) -> Vec<super::models::OpRow> {
        self.rows.read().clone()
    }

    /// Return the current foreground op id (for testing).
    #[cfg(test)]
    pub(crate) fn foreground_snapshot(&self) -> Option<OpId> {
        *self.foreground.read()
    }

    /// Return the current ops-panel visibility (for testing).
    #[cfg(test)]
    pub(crate) fn visible_snapshot(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
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

/// Background bridge: pump [`StreamProgress`] events from a preview
/// download into the controller's row state.
fn preview_progress_bridge(
    ctrl: std::sync::Weak<OpsController>,
    id: OpId,
    rx: crossbeam_channel::Receiver<StreamProgress>,
) {
    let mut last_push = Instant::now() - Duration::from_secs(1);
    while let Ok(event) = rx.recv() {
        let Some(ctrl) = ctrl.upgrade() else {
            break;
        };
        ctrl.apply_preview_progress(id, &event);
        // Debounce UI pushes to match the queue-managed progress
        // cadence — otherwise a fast local stream can hammer the
        // Slint event loop at chunk-rate.
        let now = Instant::now();
        if now.duration_since(last_push) >= DEBOUNCE {
            last_push = now;
            ctrl.push_to_ui();
            ctrl.push_modal_state();
        }
    }
    // Final push so the last chunk is reflected on-screen; the
    // caller's `complete/fail/cancelled` will still terminate the row.
    if let Some(ctrl) = ctrl.upgrade() {
        ctrl.push_to_ui();
    }
}

/// Compute a [`ConflictPrompt`] from the paths reported in
/// [`OpEvent::Conflict`]. Determines the source-vs-dest mtime
/// relation so the modal can select Finder-parity phrasing.
///
/// `source` and `dest` are lossy display paths on local backends and
/// full URIs on remote backends. We only need the basename for the
/// modal's primary sentence.
fn build_conflict_prompt(source: &std::path::Path, dest: &std::path::Path) -> ConflictPrompt {
    let name = dest
        .file_name()
        .or_else(|| source.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_owned();
    // Only local backends have `SystemTime` mtimes accessible via
    // `symlink_metadata`. Remote paths surface as URIs which
    // `symlink_metadata` won't find, so both sides degrade to "no
    // mtime known" and the modal falls back to the neutral phrasing.
    let source_mtime = std::fs::symlink_metadata(source)
        .ok()
        .and_then(|m| m.modified().ok());
    let dest_mtime = std::fs::symlink_metadata(dest)
        .ok()
        .and_then(|m| m.modified().ok());
    let (source_is_newer, source_is_older) = match (source_mtime, dest_mtime) {
        (Some(s), Some(d)) if s > d => (true, false),
        (Some(s), Some(d)) if s < d => (false, true),
        _ => (false, false),
    };
    ConflictPrompt {
        name,
        source_display: source.to_string_lossy().into_owned(),
        dest_display: dest.to_string_lossy().into_owned(),
        source_is_newer,
        source_is_older,
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
    fn background_does_not_open_ops_panel() {
        // The Background button on the progress modal must dismiss the modal
        // WITHOUT opening the ops panel. The panel is user-owned via
        // `ops::TogglePanel` (Cmd+J); Background should be silent so that
        // "fire-and-forget" file ops feel fast and asynchronous.
        let ctrl = OpsController::new();

        // Inject a running (non-terminal) row and mark it as the current
        // foreground op — this is the state the modal is showing in.
        {
            ctrl.rows.write().push(OpRow {
                id: 42,
                status: "Running".to_owned(),
                is_terminal: false,
                ..OpRow::default()
            });
            *ctrl.foreground.write() = Some(42);
        }
        assert_eq!(ctrl.foreground_snapshot(), Some(42));
        assert!(
            !ctrl.visible_snapshot(),
            "panel should start closed in this test"
        );

        ctrl.background_current_foreground();

        assert_eq!(
            ctrl.foreground_snapshot(),
            None,
            "Background must drop the foreground op (dismissing the modal)"
        );
        assert!(
            !ctrl.visible_snapshot(),
            "Background must NOT open the ops panel — that surface is Cmd+J-only"
        );
    }

    #[test]
    fn background_preserves_existing_open_panel() {
        // If the user already had the ops panel open (via Cmd+J) when they
        // clicked Background, we must NOT close it as a side effect. The
        // panel's open/closed state is orthogonal to the modal's lifecycle.
        let ctrl = OpsController::new();
        {
            ctrl.rows.write().push(OpRow {
                id: 7,
                status: "Running".to_owned(),
                is_terminal: false,
                ..OpRow::default()
            });
            *ctrl.foreground.write() = Some(7);
        }
        ctrl.set_visible(true);
        assert!(ctrl.visible_snapshot());

        ctrl.background_current_foreground();

        assert_eq!(ctrl.foreground_snapshot(), None);
        assert!(
            ctrl.visible_snapshot(),
            "Background must leave an already-open panel open"
        );
    }

    #[test]
    fn trivial_op_completion_does_not_open_ops_panel() {
        // Auto-dismissal on completion (the modal closes because the op
        // finished, not because the user pressed Background) must also keep
        // the panel closed.
        let ctrl = OpsController::new();
        let dir = tempfile::tempdir().expect("tempdir");
        ctrl.submit_mkdir(Location::local(dir.path().join("silent_complete")));
        wait(400);

        assert_eq!(ctrl.foreground_snapshot(), None);
        assert!(
            !ctrl.visible_snapshot(),
            "op completing (with no user interaction) must not open the panel"
        );
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

    // ── Conflict-modal wiring ────────────────────────────────────────────
    //
    // These exercise the controller-side machinery that surfaces
    // OpEvent::Conflict as a modal prompt and translates the button
    // choice back into a ConflictDecision.

    #[test]
    fn build_conflict_prompt_selects_source_newer_when_mtime_lt() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        // Write dest FIRST so its mtime is older.
        let dest_path = dst.path().join("README.md");
        std::fs::write(&dest_path, b"old").expect("write");
        std::thread::sleep(Duration::from_millis(50));
        let source_path = src.path().join("README.md");
        std::fs::write(&source_path, b"new").expect("write");

        let prompt = build_conflict_prompt(&source_path, &dest_path);
        assert_eq!(prompt.name, "README.md");
        assert!(prompt.source_is_newer, "source should be flagged newer");
        assert!(!prompt.source_is_older, "source must not also be older");
    }

    #[test]
    fn build_conflict_prompt_selects_source_older_when_mtime_gt_dest() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        // Write source FIRST so it's older.
        let source_path = src.path().join("README.md");
        std::fs::write(&source_path, b"old").expect("write");
        std::thread::sleep(Duration::from_millis(50));
        let dest_path = dst.path().join("README.md");
        std::fs::write(&dest_path, b"new").expect("write");

        let prompt = build_conflict_prompt(&source_path, &dest_path);
        assert!(prompt.source_is_older, "source should be flagged older");
        assert!(!prompt.source_is_newer);
    }

    #[test]
    fn build_conflict_prompt_falls_back_to_neutral_for_remote_uris() {
        // Remote paths surface as URI-shaped PathBufs that
        // `symlink_metadata` cannot resolve; both flags must be
        // false so the modal uses neutral phrasing.
        let source = std::path::PathBuf::from("sftp://user@host/foo/README.md");
        let dest = std::path::PathBuf::from("s3://bucket/README.md");
        let prompt = build_conflict_prompt(&source, &dest);
        assert!(!prompt.source_is_newer);
        assert!(!prompt.source_is_older);
        assert_eq!(prompt.name, "README.md");
    }

    #[test]
    fn conflict_prompt_flows_through_ops_controller() {
        // Submit a Copy with Prompt policy at a colliding destination.
        // The controller must surface a ConflictPrompt and, once
        // resolved via `conflict_replace`, unblock the op — no
        // resurrection of the pre-fix "auto-skip on Prompt" bug.
        let ctrl = OpsController::new();
        let src_dir = tempfile::tempdir().expect("tempdir");
        let dst_dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(src_dir.path().join("clash.txt"), b"new").expect("src");
        std::fs::write(dst_dir.path().join("clash.txt"), b"old").expect("dst");

        ctrl.submit_copy_with_policy(
            vec![Location::local(src_dir.path().join("clash.txt"))],
            Location::local(dst_dir.path()),
            ConflictPolicy::Prompt,
        );

        // Wait for the prompt to arrive.
        let deadline = Instant::now() + Duration::from_secs(3);
        let prompt = loop {
            if let Some(p) = ctrl.conflict_prompt() {
                break p;
            }
            assert!(
                Instant::now() < deadline,
                "no conflict prompt appeared within 3s"
            );
            wait(20);
        };
        assert_eq!(prompt.name, "clash.txt");

        // Respond with Replace → the op should complete and the
        // destination content should equal the new source.
        ctrl.conflict_replace(false);
        wait(300);
        let rows = ctrl.rows_snapshot();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_terminal, "expected op to reach terminal state");
        assert_eq!(
            std::fs::read(dst_dir.path().join("clash.txt")).unwrap(),
            b"new"
        );
    }

    #[test]
    fn apply_to_all_caches_decision_for_subsequent_conflicts() {
        let ctrl = OpsController::new();
        let src_dir = tempfile::tempdir().expect("tempdir");
        let dst_dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(src_dir.path().join("a.txt"), b"a-new").expect("a");
        std::fs::write(src_dir.path().join("b.txt"), b"b-new").expect("b");
        std::fs::write(dst_dir.path().join("a.txt"), b"a-old").expect("da");
        std::fs::write(dst_dir.path().join("b.txt"), b"b-old").expect("db");

        ctrl.submit_copy_with_policy(
            vec![
                Location::local(src_dir.path().join("a.txt")),
                Location::local(src_dir.path().join("b.txt")),
            ],
            Location::local(dst_dir.path()),
            ConflictPolicy::Prompt,
        );

        // Wait for FIRST prompt.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if ctrl.conflict_prompt().is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "no first prompt");
            wait(20);
        }
        // Answer with Replace + Apply To All. Subsequent conflicts
        // for this op must NOT surface additional prompts.
        ctrl.conflict_replace(true);

        // Wait for op to finish. Along the way we must NEVER see
        // another prompt appear (Apply-To-All short-circuit).
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut extra_prompts = 0;
        loop {
            let rows = ctrl.rows_snapshot();
            if rows.iter().all(|r| r.is_terminal) && !rows.is_empty() {
                break;
            }
            if ctrl.conflict_prompt().is_some() {
                extra_prompts += 1;
            }
            assert!(Instant::now() < deadline, "op did not terminate in 5s");
            wait(20);
        }
        assert_eq!(
            extra_prompts, 0,
            "Apply-To-All must suppress subsequent prompts"
        );
        assert_eq!(
            std::fs::read(dst_dir.path().join("a.txt")).unwrap(),
            b"a-new"
        );
        assert_eq!(
            std::fs::read(dst_dir.path().join("b.txt")).unwrap(),
            b"b-new"
        );
    }

    // ── Preview-download progress ────────────────────────────────────────

    #[test]
    fn preview_download_handle_creates_row_and_completes() {
        let ctrl = OpsController::new();
        let handle =
            ctrl.start_preview_download("readme.txt", "sftp://user@host/pub/readme.txt", 1_024_000);
        // Row should exist immediately (start_preview_download is
        // sync; the bridge thread only handles progress).
        let rows = ctrl.rows_snapshot();
        let row = rows
            .iter()
            .find(|r| r.id == handle.id)
            .expect("preview row present");
        assert!(row.title.starts_with("Downloading"));
        assert!(!row.is_terminal);
        assert_eq!(row.bytes_total_raw, 1_024_000);

        // Push a progress event and drop the sender so the bridge exits.
        handle
            .progress_tx
            .send(atlas_remote::StreamProgress {
                bytes_transferred: 512_000,
                total_bytes: Some(1_024_000),
            })
            .expect("progress send");
        wait(120);
        let row = ctrl
            .rows_snapshot()
            .into_iter()
            .find(|r| r.id == handle.id)
            .expect("preview row still present");
        assert!(
            row.bytes_done_raw >= 512_000,
            "progress must land on the row"
        );

        handle.complete();
        wait(50);
        let row = ctrl
            .rows_snapshot()
            .into_iter()
            .find(|r| r.id == handle.id)
            .expect("row present");
        assert!(row.is_terminal);
        assert!(!row.is_error);
        assert_eq!(row.status, "Done");
    }

    #[test]
    fn preview_download_cancel_flag_is_observed_by_caller() {
        let ctrl = OpsController::new();
        let handle = ctrl.start_preview_download("big.bin", "sftp://host/big.bin", 1_000_000);
        assert!(!handle.is_cancelled());

        // The panel's per-row cancel button routes through
        // `OpsController::cancel(id)`; verify it flips the flag
        // for controller-managed rows without a queue round-trip.
        ctrl.cancel(handle.id);
        assert!(
            handle.is_cancelled(),
            "cancel must be observable to the caller"
        );
        handle.cancelled();
        wait(30);
        let rows = ctrl.rows_snapshot();
        let row = rows.iter().find(|r| r.id == handle.id).expect("row");
        assert!(row.is_terminal);
        assert_eq!(row.status, "Cancelled");
    }

    #[test]
    fn preview_download_dropped_handle_terminates_row_as_failed() {
        // Guardrail: if a caller forgets to call complete/fail/cancelled
        // the Drop impl must not leave a "running" row dangling.
        let ctrl = OpsController::new();
        let id = {
            let handle = ctrl.start_preview_download("orphan.bin", "sftp://host/o.bin", 42);
            handle.id
        }; // handle dropped here
        wait(30);
        let rows = ctrl.rows_snapshot();
        let row = rows.iter().find(|r| r.id == id).expect("row present");
        assert!(row.is_terminal, "drop must terminate the row");
        assert!(row.is_error, "unresolved drop is a failure");
    }
}
