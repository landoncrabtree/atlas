//! Cross-platform shims for OS-native UI details that live outside Slint's
//! model:
//!
//! * [`titlebar_theme`] — tint the native title bar to match Atlas's
//!   active light/dark theme.
//! * [`open_with`] — invoke the platform-native *Open With…* application
//!   picker for a local file.

pub mod open_with;
pub mod titlebar_theme;
