//! Context-menu capability model.
//!
//! Every right-click event flows through a two-step model:
//!
//! 1. The view collects a [`ContextTarget`] describing what the user
//!    right-clicked: `location`, `entry_kind`, whether the target's
//!    mount is writable, and the `backend_kind`.
//! 2. [`crate::shell::AppShell::context_capabilities_for`] resolves
//!    the target into a [`ContextCapabilities`] bitset ("what
//!    actions apply to *this* entry?"). The result drives Slint
//!    `visible:` bindings on each static `MenuItem`, so the menu
//!    only shows actions that make sense for the target.
//!
//! # Adding a new capability
//!
//! 1. Add a `bool` field to [`ContextCapabilities`].
//! 2. Extend the resolver in
//!    `AppShell::context_capabilities_for` to compute it from the
//!    target.
//! 3. Add a `context-menu-can-<name>: bool` property on the Slint
//!    root and a matching `set_context_menu_can_<name>` push in
//!    `AppShell::open_context_menu`.
//! 4. Add a `MenuItem` in `atlas.slint` with a
//!    `visible: root.context-menu-can-<name>` binding.
//!
//! Plugins (v0.6+) will hook the resolver behind a trait so
//! per-context items can be contributed dynamically. See the
//! `TODO(plugins)` marker on
//! `AppShell::context_capabilities_for` for the seam.

pub mod capabilities;

pub use capabilities::{ContextCapabilities, ContextTarget};
