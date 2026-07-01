//! Grid view controller and supporting types.
//!
//! The [`GridController`] subscribes to a [`atlas_fs::LocationViewModel`],
//! streams entry data into Slint via [`slint::invoke_from_event_loop`],
//! maintains selection and focus state, and delegates thumbnail generation
//! to [`thumbs::ThumbRequester`].

pub mod controller;
pub mod thumbs;

pub use controller::GridController;
