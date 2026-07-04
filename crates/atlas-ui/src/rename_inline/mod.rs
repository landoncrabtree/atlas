//! Inline rename controller — small module powering the
//! `InlineRenameCell` component wired into every view mode
//! (Details / Grid / Miller / Gallery).
//!
//! # Overview
//!
//! The user activates a rename in one of three ways:
//!
//! 1. F2 key (`fs::Rename` action) on the focused entry.
//! 2. Right-click → **Rename** in the context menu.
//! 3. (Slow-click / Finder timing trigger — deferred; noted in the
//!    PR body follow-ups.)
//!
//! In every case the shell delegates to
//! [`RenameInlineController::open`] with the target
//! [`atlas_core::Location`], the pane id, the row/entry index, and
//! the current entry name. The controller stashes a
//! [`RenameSession`], pushes the initial buffer / stem-selection
//! into Slint (both the top-level rename properties AND the pane-id
//! /entry-index pair that tells each view which row to swap for the
//! [`InlineRenameCell`]), and lets keystrokes / commits flow via
//! [`RenameInlineController::edited`] and
//! [`RenameInlineController::submit`].
//!
//! # Validation
//!
//! [`validate_name`] runs on every keystroke and on commit. It
//! catches empty strings, path separators, reserved names, and
//! per-OS illegal characters + length caps. Sibling-collision checks
//! are deferred to commit time and routed through the operations
//! controller so the shared conflict modal (`AtlasConflictModal`
//! landed with PR #14) does the user-facing prompt.
//!
//! # Threading
//!
//! Everything runs on the Slint event loop. The commit call submits
//! into `OpsController::submit_rename`, which owns its own worker
//! + cancellation token, so this module never blocks the UI thread.

pub mod controller;
pub mod session;
pub mod validation;

pub use controller::RenameInlineController;
pub use session::{stem_range, RenameSession};
pub use validation::{validate_name, RenameValidation};
