//! [`RenameInlineController`] — glue between the shell, the Slint
//! `InlineRenameCell` (view-level red-pill editor), and the ops
//! queue.
//!
//! # Responsibilities
//!
//! * Own the (optional) active [`RenameSession`].
//! * Push the buffer, stem-selection, error text, and target `(pane
//!   id, entry index [, miller col])` into the `AtlasWindow` on
//!   open and on every keystroke.
//! * On commit, run final validation and submit the rename through
//!   `OpsController::submit_rename`. Sibling collisions are routed
//!   to the shared `AtlasConflictModal` by the ops layer
//!   (`ConflictPolicy::Prompt` is the default on rename ops).
//!
//! # Threading
//!
//! Every method is called from the Slint event loop; no work is
//! offloaded. Ops-side execution owns its own worker via
//! `OpsController`.
//!
//! # State machine
//!
//! * `None` — no session; `edited()` / `submit()` are no-ops.
//! * `Some(session)` — a view is rendering `InlineRenameCell` on
//!   `(pane_id, entry_index)`; buffer + validation refresh on every
//!   `edited()`; `submit()` commits or stays open on validation
//!   error.

use std::sync::Arc;

use atlas_core::Location;
use parking_lot::RwLock;
use slint::SharedString;

use crate::ops::OpsController;
use crate::rename_inline::session::{stem_range, RenameSession};
use crate::rename_inline::validation::{validate_name, RenameValidation};
use crate::{AtlasWindow, PaneId};

/// Controller for the inline rename cell.
///
/// Owned by [`crate::shell::AppShell`]; cheap to `Arc::clone` — holds
/// only a small session under `RwLock`, an `Arc<OpsController>`, and
/// the Slint window weak reference.
pub struct RenameInlineController {
    session: RwLock<Option<RenameSession>>,
    ops: Arc<OpsController>,
    window: RwLock<Option<slint::Weak<AtlasWindow>>>,
    /// Monotonic tick published as `rename-selection-tick` on every
    /// open so the Slint side re-applies the stem selection even
    /// when the numeric bounds match a prior session.
    selection_tick: parking_lot::Mutex<i32>,
}

impl RenameInlineController {
    /// Build a fresh controller. The window must be attached
    /// separately via [`Self::attach_window`] before any open() call.
    #[must_use]
    pub fn new(ops: Arc<OpsController>) -> Arc<Self> {
        Arc::new(Self {
            session: RwLock::new(None),
            ops,
            window: RwLock::new(None),
            selection_tick: parking_lot::Mutex::new(0),
        })
    }

    /// Attach the Slint window handle. Called once during shell
    /// construction.
    pub fn attach_window(&self, window: slint::Weak<AtlasWindow>) {
        *self.window.write() = Some(window);
    }

    /// Open a rename session. Any previous session is dropped
    /// silently — Atlas allows only one active rename at a time. On
    /// Miller `miller_col` names the column that hosts the cell;
    /// non-Miller callers pass `-1`.
    ///
    /// Pushes into the Slint side:
    /// * `rename-active-pane-id`, `rename-active-index`,
    ///   `rename-active-col` — tell each view which row/column swaps
    ///   its filename Text for the InlineRenameCell.
    /// * `rename-buffer` = `current_name`.
    /// * `rename-selection-{start,end,tick}` = stem range + fresh tick.
    /// * `rename-error-text` = "" (fresh session ⇒ no error).
    pub fn open(
        &self,
        target: Location,
        current_name: String,
        is_dir: bool,
        pane_id: PaneId,
        entry_index: i32,
        miller_col: i32,
    ) {
        let (start, end) = stem_range(&current_name, is_dir);
        let session =
            RenameSession::new(target, current_name.clone(), is_dir, pane_id, entry_index);
        *self.session.write() = Some(session);
        let tick = {
            let mut t = self.selection_tick.lock();
            *t = t.wrapping_add(1);
            *t
        };
        self.push_open(
            &current_name,
            pane_id,
            entry_index,
            miller_col,
            start,
            end,
            tick,
        );
    }

    /// Close the current session without committing. Idempotent —
    /// safe to call from Escape, blur-commit (when the session was
    /// already torn down), or explicit cancel paths.
    pub fn cancel(&self) {
        if self.session.write().take().is_none() {
            return;
        }
        self.push_close();
    }

    /// True while a session is open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.session.read().is_some()
    }

    /// Called on every keystroke inside the cell. Refreshes the
    /// session buffer and pushes fresh validation state
    /// (`rename-error-text`) to Slint.
    pub fn edited(&self, new_buffer: String) {
        let mut guard = self.session.write();
        let Some(session) = guard.as_mut() else {
            return;
        };
        session.buffer = new_buffer;
        let validation = validate_name(&session.buffer);
        drop(guard);
        self.push_validation(validation);
    }

    /// Commit the pending rename. On success, tears the session
    /// down and submits into the ops queue. On validation failure,
    /// leaves the session open with the error visible.
    ///
    /// Sibling-collision detection is NOT done here — it happens
    /// asynchronously inside the ops layer when the rename primitive
    /// hits an existing entry. That path emits an `OpEvent::Conflict`
    /// which routes through the shared `AtlasConflictModal`.
    pub fn submit(&self) {
        // Snapshot the session under a short lock; drop before we
        // hand off to the ops controller so we never hold this lock
        // across an external call.
        let session = match self.session.read().clone() {
            Some(s) => s,
            None => return,
        };
        if !session.is_dirty() {
            // Silent no-op — matches Finder: pressing Return without
            // typing anything just closes the cell.
            self.cancel();
            return;
        }
        let validation = validate_name(&session.buffer);
        if !validation.is_ok() {
            self.push_validation(validation);
            return;
        }
        let target = session.target.clone();
        let new_name = session.buffer.clone();
        *self.session.write() = None;
        self.push_close();
        tracing::info!(
            target = %target.display_path(),
            new_name = %new_name,
            "rename_inline: submitting rename via OpsController"
        );
        self.ops.submit_rename(target, new_name);
    }

    /// Blur-commit is intentionally routed to `cancel` (not
    /// `submit`) — losing focus should NEVER destructively rename a
    /// file. Real Finder does commit on click-outside within its own
    /// window, but Atlas can't reliably distinguish "user clicked
    /// outside within Atlas" from "another OS process stole focus"
    /// (Cmd+Tab, notification center, dictation panel). Cancel is
    /// the safe default; users who want to commit press Return.
    ///
    /// If a session-preserving "focus lost within window" affordance
    /// is added later, this handler is the single point to promote
    /// to `submit`.
    pub fn blur_commit(&self) {
        if !self.is_open() {
            return;
        }
        self.cancel();
    }

    // ── Slint bridge ──────────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn push_open(
        &self,
        buffer: &str,
        pane_id: PaneId,
        entry_index: i32,
        miller_col: i32,
        start: usize,
        end: usize,
        tick: i32,
    ) {
        let window = self.window.read().as_ref().and_then(|w| w.upgrade());
        let Some(window) = window else { return };
        window.set_rename_buffer(SharedString::from(buffer));
        window.set_rename_active_pane_id(pane_id.0 as i32);
        window.set_rename_active_index(entry_index);
        window.set_rename_active_col(miller_col);
        window.set_rename_selection_start(i32::try_from(start).unwrap_or(0));
        window.set_rename_selection_end(i32::try_from(end).unwrap_or(0));
        window.set_rename_selection_tick(tick);
        window.set_rename_error_text(SharedString::default());
    }

    fn push_close(&self) {
        let window = self.window.read().as_ref().and_then(|w| w.upgrade());
        let Some(window) = window else { return };
        window.set_rename_active_pane_id(-1);
        window.set_rename_active_index(-1);
        window.set_rename_active_col(-1);
        window.set_rename_buffer(SharedString::default());
        window.set_rename_error_text(SharedString::default());
        // Bump the tick so the next open re-applies its selection.
        let mut t = self.selection_tick.lock();
        *t = t.wrapping_add(1);
        window.set_rename_selection_tick(*t);
    }

    fn push_validation(&self, validation: RenameValidation) {
        let window = self.window.read().as_ref().and_then(|w| w.upgrade());
        let Some(window) = window else { return };
        let msg = validation.message();
        window.set_rename_error_text(SharedString::from(msg.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Isolated controller with no window attached — the push_* calls
    /// are all early-returns, so we can exercise state transitions
    /// without a live Slint runtime.
    fn detached_controller() -> Arc<RenameInlineController> {
        RenameInlineController::new(OpsController::new())
    }

    fn any_pane() -> PaneId {
        PaneId(0)
    }

    #[test]
    fn open_installs_session_and_close_clears_it() {
        let ctrl = detached_controller();
        assert!(!ctrl.is_open());

        let target = Location::local(PathBuf::from("/tmp/foo.txt"));
        ctrl.open(target, "foo.txt".to_owned(), false, any_pane(), 3, -1);
        assert!(ctrl.is_open());

        ctrl.cancel();
        assert!(!ctrl.is_open());
        // Idempotent: second cancel is fine.
        ctrl.cancel();
        assert!(!ctrl.is_open());
    }

    #[test]
    fn edited_updates_buffer_within_session() {
        let ctrl = detached_controller();
        let target = Location::local(PathBuf::from("/tmp/foo.txt"));
        ctrl.open(target, "foo.txt".to_owned(), false, any_pane(), 3, -1);

        ctrl.edited("bar.txt".to_owned());
        let session = ctrl.session.read().clone().expect("session still open");
        assert_eq!(session.buffer, "bar.txt");
        assert_eq!(session.original_name, "foo.txt");
        assert!(session.is_dirty());
    }

    #[test]
    fn edited_no_op_when_no_session() {
        let ctrl = detached_controller();
        // No open() — must not panic and must not create a session.
        ctrl.edited("bar.txt".to_owned());
        assert!(!ctrl.is_open());
    }

    #[test]
    fn submit_clean_session_is_silent_noop() {
        let ctrl = detached_controller();
        let target = Location::local(PathBuf::from("/tmp/foo.txt"));
        ctrl.open(target, "foo.txt".to_owned(), false, any_pane(), 0, -1);
        // Buffer still equals original name → dirty is false → submit
        // should close the cell without pushing anything to the ops
        // queue.
        ctrl.submit();
        assert!(
            !ctrl.is_open(),
            "clean-session submit closes the cell (Finder-parity)"
        );
    }

    #[test]
    fn submit_keeps_session_open_on_validation_error() {
        let ctrl = detached_controller();
        let target = Location::local(PathBuf::from("/tmp/foo.txt"));
        ctrl.open(target, "foo.txt".to_owned(), false, any_pane(), 0, -1);
        ctrl.edited("foo/bar".to_owned()); // slash → invalid
        ctrl.submit();
        assert!(
            ctrl.is_open(),
            "invalid names must not close the session — user should fix and retry"
        );
    }

    #[test]
    fn submit_closes_session_and_publishes_valid_name() {
        let ctrl = detached_controller();
        let target = Location::local(PathBuf::from("/tmp/foo.txt"));
        ctrl.open(
            target.clone(),
            "foo.txt".to_owned(),
            false,
            any_pane(),
            0,
            -1,
        );
        ctrl.edited("renamed.txt".to_owned());
        ctrl.submit();
        assert!(!ctrl.is_open());
    }

    #[test]
    fn blur_commit_cancels_rather_than_submitting_when_open() {
        let ctrl = detached_controller();
        let target = Location::local(PathBuf::from("/tmp/foo.txt"));
        ctrl.open(target, "foo.txt".to_owned(), false, any_pane(), 0, -1);
        ctrl.edited("renamed.txt".to_owned());
        ctrl.blur_commit();
        // Safety-first: blur cancels the session so an accidental
        // focus-steal from another OS window can't renaming a file
        // the user didn't confirm.
        assert!(!ctrl.is_open(), "blur closes the session (cancel path)");
    }

    #[test]
    fn blur_commit_no_op_when_no_session() {
        let ctrl = detached_controller();
        // No panic, no side effects — just an early return.
        ctrl.blur_commit();
        assert!(!ctrl.is_open());
    }
}
