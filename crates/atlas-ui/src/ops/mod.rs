//! File operations UI layer — controller, row models, and format helpers.
//!
//! The [`OpsController`] owns an [`atlas_ops::OperationQueue`], drains
//! [`atlas_ops::OpEvent`]s from a background thread, keeps an in-memory list
//! of [`models::OpRow`]s (including terminal rows until the user dismisses
//! them), and pushes the updated list into the Slint `AtlasWindow` via
//! [`slint::invoke_from_event_loop`].
//!
//! # Conflict resolution (MVP)
//!
//! Both Copy and Move default to [`atlas_ops::ConflictPolicy::RenameWithSuffix`]
//! (non-destructive). If the queue ever raises an
//! [`atlas_ops::OpEvent::Conflict`] with a `Prompt` responder (i.e. a future
//! caller explicitly requested prompting), this module auto-resolves with
//! [`atlas_ops::ConflictDecision::Skip`] and emits a `tracing::warn!`.
//! A full conflict-resolution modal is a post-MVP follow-up.

pub mod controller;
pub mod format;
pub mod models;

pub use controller::OpsController;
pub use models::OpRow;
