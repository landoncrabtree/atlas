//! Details view controller and supporting types.
//!
//! The [`DetailsController`] subscribes to a [`atlas_fs::LocationViewModel`],
//! streams entry data into Slint via [`slint::invoke_from_event_loop`],
//! and maintains selection and focus state.

pub mod columns;
pub mod controller;
pub mod format;

pub use columns::{ColumnKind, ColumnSpec};
pub use controller::{DetailsController, Selection};
pub use format::{format_relative_time, format_size};
