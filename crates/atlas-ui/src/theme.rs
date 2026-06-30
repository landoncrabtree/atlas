//! Theme tokens and mode switching.
//!
//! This module re-exports all theming types from [`crate::theming`] for
//! backward compatibility and convenience.  The canonical definitions live in
//! the sub-modules under [`crate::theming`].
//!
//! [`ThemeMode`] is applied to the window at startup via
//! [`crate::shell::AppShell::set_theme`]; full token pushes use
//! [`crate::shell::AppShell::apply_theme`].

pub use crate::theming::tokens::ThemeMode;
pub use crate::theming::{
    Chrome, Color, Colors, ThemeDescriptor, ThemeError, ThemeEvent, ThemeLoader, ThemeSource,
    ThemeTokens, ThemeWatcher, Typography,
};
