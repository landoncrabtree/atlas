//! Atlas keymap — layered, configurable chord-sequence keymap system.
//!
//! This crate is intentionally framework-agnostic. A thin adapter layer in
//! `atlas-ui` bridges our [`ActionId`] strings to the Slint UI's key events.
//!
//! # Architecture
//!
//! - [`Chord`] / [`ChordSequence`]: key representations and VS Code-style parsing.
//! - [`ActionId`] / [`ActionRegistry`]: stable string action identifiers.
//! - [`Binding`]: associates a chord sequence + context with an action.
//! - [`Keymap`]: layered storage (default layer + user layer) with context-scoped lookup.
//! - [`Dispatcher`]: routes resolved [`ActionId`]s to registered `Fn()` handlers.
//! - [`loader`]: TOML serialization and deserialization of user keymaps.
//! - [`defaults`]: built-in bindings shipped with the application.

pub mod action;
pub mod binding;
pub mod chord;
pub mod context;
pub mod defaults;
pub mod dispatcher;
pub mod keymap;
pub mod loader;

pub use action::{ActionId, ActionMeta, ActionRegistry};
pub use binding::Binding;
pub use chord::{Chord, ChordSequence, Key, Modifiers, NamedKey, PrettyPlatform};
pub use dispatcher::Dispatcher;
pub use keymap::{Keymap, ResolveResult};
pub use loader::{
    default_keymap_toml_string, default_keymap_toml_string_for, load_keymap_toml,
    save_keymap_toml, write_default_keymap_to,
};
