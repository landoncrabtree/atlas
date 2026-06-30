//! Data-driven theming subsystem for Atlas.
//!
//! # Architecture
//!
//! - [`color`] — the [`Color`] value type with hex/array (de)serialization.
//! - [`tokens`] — [`ThemeTokens`] and its sub-structs ([`Colors`], [`Typography`], [`Chrome`]).
//! - [`defaults`] — embedded `atlas-dark` and `atlas-light` built-in themes.
//! - [`loader`] — [`ThemeLoader`] resolves theme IDs to [`ThemeTokens`].
//! - [`watcher`] — [`ThemeWatcher`] hot-reloads the active theme on file change.

pub mod color;
pub mod defaults;
pub mod loader;
pub mod tokens;
pub mod watcher;

pub use color::Color;
pub use loader::{ThemeDescriptor, ThemeLoader, ThemeSource};
pub use tokens::{Chrome, Colors, ThemeMode, ThemeTokens, Typography};
pub use watcher::{ThemeError, ThemeEvent, ThemeWatcher};
