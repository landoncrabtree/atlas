//! [`ThemeTokens`] and its sub-structs.
//!
//! The full set of design tokens loaded from a TOML theme file.

use serde::{Deserialize, Serialize};

use super::color::Color;

/// The color scheme hint for the OS / window manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// Dark scheme.
    #[default]
    Dark,
    /// Light scheme.
    Light,
}

impl ThemeMode {
    /// Returns `true` when `self` is [`ThemeMode::Dark`].
    pub fn is_dark(self) -> bool {
        self == Self::Dark
    }
}

/// Complete design-token specification for one Atlas theme.
///
/// Loaded from a TOML file; two built-in defaults are embedded in the binary.
/// Use [`super::defaults::default_dark`] / [`super::defaults::default_light`]
/// to obtain them without touching the filesystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeTokens {
    /// Machine-readable identifier, e.g. `"atlas-dark"`.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Color-scheme hint for the OS.
    pub mode: ThemeMode,
    /// Color palette.
    pub colors: Colors,
    /// Typography settings.
    pub typography: Typography,
    /// Window chrome geometry.
    pub chrome: Chrome,
}

/// Full color palette for a theme.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Colors {
    /// Main window background.
    pub bg: Color,
    /// Sidebar / panel background.
    pub panel_bg: Color,
    /// Primary foreground text color.
    pub fg: Color,
    /// Secondary / muted foreground text color.
    pub fg_muted: Color,
    /// Border / separator color.
    pub border: Color,
    /// Accent highlight color (links, selection rings, etc.).
    pub accent: Color,
    /// Text on top of accent-colored backgrounds.
    pub accent_fg: Color,
    /// Text-selection background.
    pub selection_bg: Color,
    /// Text-selection foreground.
    pub selection_fg: Color,
    /// Error / destructive indicator color.
    pub error: Color,
    /// Success indicator color.
    pub success: Color,
    /// Warning indicator color.
    pub warning: Color,
}

/// Typography settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Typography {
    /// Primary UI font family name.
    pub font_family: String,
    /// Monospace font family for the preview pane and code views.
    pub monospace_family: String,
    /// Body font size in points.
    pub font_size_pt: f32,
}

/// Window chrome geometry (pixel values, logical/device-independent).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chrome {
    /// Title bar height in logical pixels.
    pub titlebar_h_px: f32,
    /// Status bar height in logical pixels.
    pub statusbar_h_px: f32,
    /// Tab strip height in logical pixels.
    pub tab_h_px: f32,
    /// Small border radius in logical pixels.
    pub radius_sm_px: f32,
    /// Medium border radius in logical pixels.
    pub radius_md_px: f32,
    /// Extra-small spacing unit in logical pixels.
    pub spacing_xs_px: f32,
    /// Small spacing unit in logical pixels.
    pub spacing_sm_px: f32,
    /// Medium spacing unit in logical pixels.
    pub spacing_md_px: f32,
    /// Large spacing unit in logical pixels.
    pub spacing_lg_px: f32,
}
