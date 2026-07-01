//! Bulk rename module — live regex-preview modal for renaming multiple files.
//!
//! # Overview
//!
//! The entry point is [`BulkRenameController`].  Wire it into [`AppShell`] by
//! calling [`BulkRenameController::new`] with an [`OpsController`] and the
//! shared [`ActionSink`], then call [`BulkRenameController::attach_window`]
//! with the Slint window handle.
//!
//! Open the modal by calling [`BulkRenameController::open`] with the current
//! pane's selected paths.  A background thread debounces input changes (50 ms)
//! and computes the preview; results are pushed to the Slint window via
//! [`slint::invoke_from_event_loop`].
//!
//! [`AppShell`]: crate::shell::AppShell
//! [`OpsController`]: crate::ops::OpsController
//! [`ActionSink`]: crate::actions::ActionSink

pub mod controller;
pub mod error;
pub mod preview;

pub use controller::BulkRenameController;
pub use error::RenameError;
pub use preview::{compute_preview, Inputs, PreviewRow};
