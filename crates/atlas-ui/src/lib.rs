#![allow(unreachable_pub)]
#![allow(clippy::todo)]

//! Atlas UI crate — Slint component tree, pure-Rust models, and the
//! [`shell::AppShell`] adapter that bridges them.
//!
//! Slint compilation lives in atlas-ui so AppShell can reference generated
//! types directly. Atlas-app stays as a thin binary wrapper. Future: split
//! into a separate slint-ui crate if multiple binaries need the same UI.

slint::include_modules!();

pub mod actions;
pub mod focus;
pub mod models;
pub mod shell;
pub mod theme;
