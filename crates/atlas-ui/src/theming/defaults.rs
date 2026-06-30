//! Built-in `atlas-dark` and `atlas-light` themes, embedded at compile time.
//!
//! The TOML sources live in `assets/themes/` and are included via
//! [`include_str!`] so the binary ships with sane defaults even when
//! no theme files are present on disk.

use super::tokens::ThemeTokens;

/// Raw TOML for the built-in dark theme.
const DARK_TOML: &str = include_str!("../../../../assets/themes/atlas-dark.toml");
/// Raw TOML for the built-in light theme.
const LIGHT_TOML: &str = include_str!("../../../../assets/themes/atlas-light.toml");

/// Return the built-in dark theme.
///
/// Parsed from the embedded `atlas-dark.toml`; panics only if the embedded
/// TOML is invalid (a programmer error, not a runtime error).
pub fn default_dark() -> ThemeTokens {
    toml::from_str(DARK_TOML).expect("embedded atlas-dark.toml must be valid TOML")
}

/// Return the built-in light theme.
///
/// Parsed from the embedded `atlas-light.toml`; panics only if the embedded
/// TOML is invalid (a programmer error, not a runtime error).
pub fn default_light() -> ThemeTokens {
    toml::from_str(LIGHT_TOML).expect("embedded atlas-light.toml must be valid TOML")
}

/// Return an iterator over all built-in themes (`atlas-dark` first).
pub fn defaults() -> impl IntoIterator<Item = ThemeTokens> {
    [default_dark(), default_light()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_deserializes() {
        let theme = default_dark();
        assert_eq!(theme.id, "atlas-dark");
        assert!(theme.mode.is_dark());
    }

    #[test]
    fn light_deserializes() {
        let theme = default_light();
        assert_eq!(theme.id, "atlas-light");
        assert!(!theme.mode.is_dark());
    }
}
