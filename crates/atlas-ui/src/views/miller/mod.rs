//! Miller columns view controller and supporting types.
//!
//! The [`MillerController`] manages a horizontal stack of directory columns
//! (Finder/NeXT style).  Each column owns a live
//! [`atlas_fs::LocationViewModel`] subscription thread that pushes updates
//! into the Slint [`MillerState`] global via
//! [`slint::invoke_from_event_loop`].
//!
//! The concrete [`atlas_fs::LocationViewModel`] backing each column is chosen
//! by a [`controller::LocationOpener`] plumbed in by the shell — this lets a
//! Miller pane rooted at a remote SFTP location descend into remote
//! subdirectories without falling back to the local filesystem.

pub mod column;
pub mod controller;

pub use controller::{LocalLocationOpener, LocationOpener, MillerController, RemoteLocationOpener};
