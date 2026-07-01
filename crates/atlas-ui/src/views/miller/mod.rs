//! Miller columns view controller and supporting types.
//!
//! The [`MillerController`] manages a horizontal stack of directory columns
//! (Finder/NeXT style).  Each column owns a live
//! [`atlas_fs::InMemoryLocationViewModel`] subscription thread that pushes
//! updates into the Slint [`MillerState`] global via
//! [`slint::invoke_from_event_loop`].

pub mod column;
pub mod controller;

pub use controller::MillerController;
