//! Theme tokens and mode switching.
//!
//! [`ThemeMode`] is applied to the window at startup via
//! [`crate::shell::AppShell::set_theme`].

/// Light or dark color scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    /// Dark scheme (GitHub dark colors).
    #[default]
    Dark,
    /// Light scheme.
    Light,
}

impl ThemeMode {
    /// Returns `true` when `self` is [`ThemeMode::Dark`].
    #[must_use]
    pub fn is_dark(self) -> bool {
        self == Self::Dark
    }
}

/// Static design-token values exposed to Rust.
///
/// Most tokens live in the `Theme` Slint global; this struct mirrors the
/// subset that Rust code may need.
#[derive(Debug, Clone)]
pub struct ThemeTokens {
    /// Current color scheme.
    pub mode: ThemeMode,
}

impl ThemeTokens {
    /// Construct tokens for the given mode.
    #[must_use]
    pub fn new(mode: ThemeMode) -> Self {
        Self { mode }
    }
}
