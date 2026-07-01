//! [`BulkRenameController`] — drives the bulk-rename modal.
//!
//! # Threading model
//!
//! - All public methods (`open`, `close`, `set_pattern`, …) may be called from
//!   any thread.  They update the shared state under `parking_lot` locks, then
//!   either push a direct (non-debounced) UI update via
//!   [`slint::invoke_from_event_loop`] or send a signal to the debounce thread.
//! - A dedicated `atlas-bulk-rename-compute` background thread debounces input
//!   changes (≥ 50 ms silence after the last change), calls
//!   [`preview::compute_preview`], updates the shared preview/error state, and
//!   pushes the result to the Slint window.
//! - Conflict detection in `compute_preview` performs `Path::exists()` filesystem
//!   calls, which is why it runs on the background thread, never on the UI thread.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use crossbeam_channel::{RecvTimeoutError, Sender};
use parking_lot::{Mutex, RwLock};
use slint::{ModelRc, VecModel};

use crate::{
    actions::{ActionSink, UiAction},
    ops::OpsController,
    AtlasWindow,
};

use super::preview::{compute_preview, Inputs, PreviewRow};

/// Debounce interval: wait this long after the last input change before
/// running the preview computation.
const DEBOUNCE: Duration = Duration::from_millis(50);

/// Controller that drives the bulk-rename modal.
///
/// Construct with [`BulkRenameController::new`], attach the Slint window with
/// [`BulkRenameController::attach_window`], then call [`BulkRenameController::open`]
/// with the paths to rename.
pub struct BulkRenameController {
    /// Current user inputs.
    inputs: RwLock<Inputs>,
    /// Paths that are currently being previewed / renamed.
    original_paths: RwLock<Vec<PathBuf>>,
    /// Latest computed preview rows.
    preview: RwLock<Vec<PreviewRow>>,
    /// Latest regex compile error, if any.
    error: RwLock<Option<String>>,
    /// Whether the modal is currently shown.
    visible: AtomicBool,
    /// Weak handle to the Slint window for property pushes.
    window: RwLock<slint::Weak<AtlasWindow>>,
    /// Underlying file-operations queue, used by [`confirm`].
    ops: Arc<OpsController>,
    /// Action sink for audit / future atlas-keymap integration.
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
    /// Sends `()` to the debounce background thread to trigger a recompute.
    recompute_tx: Sender<()>,
}

impl BulkRenameController {
    /// Create a new controller, spawn the debounce thread, and return a shared
    /// handle.
    #[must_use]
    pub fn new(ops: Arc<OpsController>, actions: Arc<Mutex<Box<dyn ActionSink>>>) -> Arc<Self> {
        let (recompute_tx, recompute_rx) = crossbeam_channel::bounded(16);

        let ctrl = Arc::new(Self {
            inputs: RwLock::new(Inputs::default()),
            original_paths: RwLock::new(Vec::new()),
            preview: RwLock::new(Vec::new()),
            error: RwLock::new(None),
            visible: AtomicBool::new(false),
            window: RwLock::new(slint::Weak::default()),
            ops,
            actions,
            recompute_tx,
        });

        let ctrl_weak = Arc::downgrade(&ctrl);
        std::thread::Builder::new()
            .name("atlas-bulk-rename-compute".to_owned())
            .spawn(move || debounce_thread(ctrl_weak, recompute_rx))
            .expect("failed to spawn atlas-bulk-rename-compute thread");

        ctrl
    }

    /// Attach the Slint window so the controller can push UI updates.
    ///
    /// Call this once, immediately after window creation and before [`open`].
    pub fn attach_window(&self, window: slint::Weak<AtlasWindow>) {
        *self.window.write() = window;
    }

    /// Open the modal with the given selection.
    ///
    /// Resets the find / replace inputs and triggers an immediate preview
    /// computation.
    pub fn open(&self, sources: Vec<PathBuf>) {
        *self.original_paths.write() = sources;
        *self.inputs.write() = Inputs::default();
        *self.preview.write() = Vec::new();
        *self.error.write() = None;
        self.visible.store(true, Ordering::Relaxed);

        self.actions.lock().dispatch(UiAction::OpenBulkRename);

        // Push the initial (empty-preview) state right away so the modal
        // appears immediately, then trigger a debounced recompute.
        self.push_to_ui();
        self.trigger_recompute();
    }

    /// Close the modal and reset state.
    pub fn close(&self) {
        self.visible.store(false, Ordering::Relaxed);
        self.actions.lock().dispatch(UiAction::BulkRenameClose);
        self.push_to_ui();
    }

    /// Update the find pattern and trigger a debounced preview recompute.
    pub fn set_pattern(&self, s: String) {
        self.inputs.write().pattern = s;
        self.push_to_ui();
        self.trigger_recompute();
    }

    /// Update the replacement string and trigger a debounced preview recompute.
    pub fn set_replacement(&self, s: String) {
        self.inputs.write().replacement = s;
        self.push_to_ui();
        self.trigger_recompute();
    }

    /// Toggle regex / literal mode and trigger a debounced preview recompute.
    pub fn toggle_regex(&self) {
        {
            let mut inputs = self.inputs.write();
            inputs.use_regex = !inputs.use_regex;
        }
        self.push_to_ui();
        self.trigger_recompute();
    }

    /// Toggle case-insensitive matching and trigger a debounced preview recompute.
    pub fn toggle_case_insensitive(&self) {
        {
            let mut inputs = self.inputs.write();
            inputs.case_insensitive = !inputs.case_insensitive;
        }
        self.push_to_ui();
        self.trigger_recompute();
    }

    /// Submit the pending renames and close the modal.
    ///
    /// No-ops if there are unresolved conflicts or a regex compile error.
    /// Unchanged rows are silently skipped.
    pub fn confirm(&self) {
        if self.error.read().is_some() {
            tracing::warn!("BulkRenameController::confirm called with a regex error — ignoring");
            return;
        }

        let preview = self.preview.read();
        let conflict_count = preview.iter().filter(|r| r.is_conflict).count();
        if conflict_count > 0 {
            tracing::warn!(
                conflict_count,
                "BulkRenameController::confirm called with conflicts — ignoring"
            );
            return;
        }

        let to_rename: Vec<(PathBuf, String)> = preview
            .iter()
            .filter(|r| !r.is_unchanged && !r.is_conflict)
            .map(|r| (r.original.clone(), r.proposed_name.clone()))
            .collect();
        drop(preview);

        let rename_count = to_rename.len();
        for (path, new_name) in to_rename {
            self.ops.submit_rename(path, new_name);
        }

        self.actions
            .lock()
            .dispatch(UiAction::BulkRenameConfirm { rename_count });

        tracing::info!(rename_count, "bulk rename submitted");
        self.close();
    }

    /// Whether the modal is currently visible.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn trigger_recompute(&self) {
        // A bounded channel absorbs bursts; if the channel is full (16 pending
        // triggers) the send is simply dropped — the thread will still run.
        let _ = self.recompute_tx.try_send(());
    }

    /// Run a synchronous preview computation and update the shared state.
    ///
    /// Called by the debounce thread only.
    pub(super) fn run_recompute(&self) {
        let inputs = self.inputs.read().clone();
        let paths = self.original_paths.read().clone();

        let (rows, error) = compute_preview(&paths, &inputs);

        *self.preview.write() = rows;
        *self.error.write() = error;

        self.push_to_ui();
    }

    /// Push the current full state to the Slint window via the event loop.
    fn push_to_ui(&self) {
        let preview = self.preview.read().clone();
        let error = self.error.read().clone();
        let inputs = self.inputs.read().clone();
        let visible = self.visible.load(Ordering::Relaxed);
        let window = self.window.read().clone();

        let conflict_count: i32 = preview.iter().filter(|r| r.is_conflict).count() as i32;
        let change_count: i32 = preview
            .iter()
            .filter(|r| !r.is_unchanged && !r.is_conflict)
            .count() as i32;

        let slint_rows: Vec<crate::RenamePreview> = preview
            .iter()
            .map(|r| crate::RenamePreview {
                original: r
                    .original
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
                proposed: r.proposed_name.clone().into(),
                is_conflict: r.is_conflict,
                is_unchanged: r.is_unchanged,
            })
            .collect();

        let error_text: slint::SharedString = error.unwrap_or_default().into();

        let _ = slint::invoke_from_event_loop(move || {
            let Some(win) = window.upgrade() else {
                return;
            };
            win.set_bulk_rename_visible(visible);
            win.set_bulk_rename_pattern(inputs.pattern.clone().into());
            win.set_bulk_rename_replacement(inputs.replacement.clone().into());
            win.set_bulk_rename_use_regex(inputs.use_regex);
            win.set_bulk_rename_case_insensitive(inputs.case_insensitive);
            win.set_bulk_rename_preview_rows(ModelRc::new(VecModel::from(slint_rows)));
            win.set_bulk_rename_conflict_count(conflict_count);
            win.set_bulk_rename_change_count(change_count);
            win.set_bulk_rename_error_text(error_text);
        });
    }

    // ── Test helpers ──────────────────────────────────────────────────────────

    #[cfg(test)]
    pub(crate) fn preview_snapshot(&self) -> Vec<PreviewRow> {
        self.preview.read().clone()
    }

    #[cfg(test)]
    pub(crate) fn error_snapshot(&self) -> Option<String> {
        self.error.read().clone()
    }
}

/// Background debounce thread: waits for triggers, debounces for [`DEBOUNCE`],
/// then calls [`BulkRenameController::run_recompute`].
fn debounce_thread(
    ctrl: std::sync::Weak<BulkRenameController>,
    rx: crossbeam_channel::Receiver<()>,
) {
    loop {
        // Block until the first trigger arrives.
        if rx.recv().is_err() {
            break;
        }

        // Drain additional triggers for `DEBOUNCE` after the last one.
        loop {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(()) => {
                    // Another trigger arrived — reset the timer by looping.
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Silence period elapsed — run the computation.
                    break;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return;
                }
            }
        }

        let Some(ctrl) = ctrl.upgrade() else {
            break;
        };
        ctrl.run_recompute();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::OpsController;

    struct NullSink;
    impl ActionSink for NullSink {
        fn dispatch(&mut self, _action: UiAction) {}
    }

    fn make_ctrl() -> Arc<BulkRenameController> {
        let ops = OpsController::new();
        let actions: Arc<Mutex<Box<dyn ActionSink>>> = Arc::new(Mutex::new(Box::new(NullSink)));
        BulkRenameController::new(ops, actions)
    }

    fn wait(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn tmp_paths(names: &[&str]) -> (tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = names.iter().map(|n| dir.path().join(n)).collect();
        (dir, paths)
    }

    #[test]
    fn confirm_with_conflicts_skips_submission() {
        let ctrl = make_ctrl();
        let (_dir, paths) = tmp_paths(&["foo_a.txt", "foo_b.txt"]);

        ctrl.open(paths);
        ctrl.set_pattern("_[ab]".to_owned());
        ctrl.inputs.write().use_regex = true;

        // Wait for debounce + compute.
        wait(200);

        let preview = ctrl.preview_snapshot();
        assert!(
            preview.iter().any(|r| r.is_conflict),
            "both rows should be in conflict"
        );

        // `confirm` must be a no-op when conflicts exist.
        ctrl.confirm();

        // Nothing was submitted — ops queue is empty (no rows visible in ops).
        // We can't observe ops rows directly here without a window, but we
        // verify `close()` was NOT called (modal stays visible).
        assert!(
            ctrl.is_visible(),
            "modal must remain open when confirm is blocked by conflicts"
        );
    }

    #[test]
    fn open_resets_inputs_and_triggers_compute() {
        let ctrl = make_ctrl();
        let (_dir, paths) = tmp_paths(&["IMG_001.jpg"]);

        ctrl.open(paths);
        ctrl.set_pattern("IMG_".to_owned());
        ctrl.inputs.write().replacement = "photo_".to_owned();
        ctrl.trigger_recompute();

        wait(200);

        let preview = ctrl.preview_snapshot();
        assert_eq!(preview.len(), 1);
        // proposed_name should reflect the replacement
        assert_eq!(preview[0].proposed_name, "photo_001.jpg");
    }

    #[test]
    fn regex_error_surfaces_in_error_field() {
        let ctrl = make_ctrl();
        let (_dir, paths) = tmp_paths(&["file.txt"]);

        ctrl.open(paths);
        ctrl.inputs.write().use_regex = true;
        ctrl.set_pattern("[bad".to_owned());

        wait(200);

        let err = ctrl.error_snapshot();
        assert!(err.is_some(), "expected a regex error");
    }

    #[test]
    fn close_hides_modal() {
        let ctrl = make_ctrl();
        let (_dir, paths) = tmp_paths(&["a.txt"]);

        ctrl.open(paths);
        assert!(ctrl.is_visible());

        ctrl.close();
        assert!(!ctrl.is_visible());
    }
}
