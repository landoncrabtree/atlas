//! Atlas keymap — layered, configurable chord-sequence keymap system.
//!
//! This crate is intentionally framework-agnostic. A thin adapter layer in
//! `atlas-ui` bridges our [`ActionId`] strings to gpui's action system.
//!
//! # Architecture
//!
//! - [`Chord`] / [`ChordSequence`]: key representations and VS Code-style parsing.
//! - [`ActionId`] / [`ActionRegistry`]: stable string action identifiers.
//! - [`Binding`]: associates a chord sequence + context with an action.
//! - [`Keymap`]: layered storage (default layer + user layer) with context-scoped lookup.
//! - [`loader`]: TOML serialization and deserialization of user keymaps.
//! - [`defaults`]: built-in bindings shipped with the application.

pub mod action;
pub mod binding;
pub mod chord;
pub mod context;
pub mod defaults;
pub mod keymap;
pub mod loader;

pub use action::{ActionId, ActionMeta, ActionRegistry};
pub use binding::Binding;
pub use chord::{Chord, ChordSequence, Key, Modifiers, NamedKey};
pub use keymap::{Keymap, ResolveResult};
pub use loader::{load_keymap_toml, save_keymap_toml};
