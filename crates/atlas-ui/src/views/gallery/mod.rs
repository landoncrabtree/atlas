//! Gallery view controller and supporting types.
//!
//! The [`GalleryController`] adapts a [`atlas_fs::LocationViewModel`] to the
//! large-preview Gallery view, maintaining a focused entry, thumbnail strip,
//! preview image, and metadata sidebar.

pub mod controller;
pub mod metadata;
pub mod thumbs;

pub use controller::GalleryController;
