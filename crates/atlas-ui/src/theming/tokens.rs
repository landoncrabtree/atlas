//! [`ThemeTokens`] and its sub-structs.
//!
//! The full set of design tokens loaded from a TOML theme file.

use serde::{Deserialize, Serialize};

use super::color::Color;

fn color(hex: &str) -> Color {
    match Color::from_hex_str(hex) {
        Ok(color) => color,
        Err(error) => panic!("invalid built-in theme color {hex}: {error}"),
    }
}

fn default_panel_bg_elevated() -> Color {
    color("#1e232b")
}

fn default_fg_faint() -> Color {
    color("#5a6478")
}

fn default_border_strong() -> Color {
    color("#2b3341")
}

fn default_accent_soft() -> Color {
    color("#4a9eff26")
}

fn default_hover_bg() -> Color {
    color("#ffffff0d")
}

fn default_addressbar_h_px() -> f32 {
    30.0
}

fn default_row_h_compact_px() -> f32 {
    24.0
}

fn default_row_h_default_px() -> f32 {
    30.0
}

fn default_row_h_spacious_px() -> f32 {
    38.0
}

fn default_radius_xs_px() -> f32 {
    4.0
}

fn default_radius_lg_px() -> f32 {
    10.0
}

fn default_radius_xl_px() -> f32 {
    14.0
}

fn default_space_1_px() -> f32 {
    4.0
}

fn default_space_2_px() -> f32 {
    8.0
}

fn default_space_3_px() -> f32 {
    12.0
}

fn default_space_4_px() -> f32 {
    16.0
}

fn default_space_5_px() -> f32 {
    20.0
}

fn default_space_6_px() -> f32 {
    24.0
}

fn default_space_8_px() -> f32 {
    32.0
}

fn default_space_10_px() -> f32 {
    40.0
}

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
pub struct Colors {
    /// Main window background.
    pub bg: Color,
    /// Sidebar / panel background.
    pub panel_bg: Color,
    /// Elevated panel and modal background.
    #[serde(default = "default_panel_bg_elevated")]
    pub panel_bg_elevated: Color,
    /// Primary foreground text color.
    pub fg: Color,
    /// Secondary / muted foreground text color.
    pub fg_muted: Color,
    /// Tertiary / faint foreground text color.
    #[serde(default = "default_fg_faint")]
    pub fg_faint: Color,
    /// Border / separator color.
    pub border: Color,
    /// Stronger border / emphasized separator color.
    #[serde(default = "default_border_strong")]
    pub border_strong: Color,
    /// Accent highlight color (links, selection rings, etc.).
    pub accent: Color,
    /// Text on top of accent-colored backgrounds.
    pub accent_fg: Color,
    /// Soft accent fill for selected or active background washes.
    #[serde(default = "default_accent_soft")]
    pub accent_soft: Color,
    /// Text-selection background.
    pub selection_bg: Color,
    /// Text-selection foreground.
    pub selection_fg: Color,
    /// Hover-state background.
    #[serde(default = "default_hover_bg")]
    pub hover_bg: Color,
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
pub struct Chrome {
    /// Title bar height in logical pixels.
    pub titlebar_h_px: f32,
    /// Status bar height in logical pixels.
    pub statusbar_h_px: f32,
    /// Tab strip height in logical pixels.
    pub tab_h_px: f32,
    /// Address bar height in logical pixels.
    #[serde(default = "default_addressbar_h_px")]
    pub addressbar_h_px: f32,
    /// Compact row height in logical pixels.
    #[serde(default = "default_row_h_compact_px")]
    pub row_h_compact_px: f32,
    /// Default row height in logical pixels.
    #[serde(default = "default_row_h_default_px")]
    pub row_h_default_px: f32,
    /// Spacious row height in logical pixels.
    #[serde(default = "default_row_h_spacious_px")]
    pub row_h_spacious_px: f32,
    /// Extra-small border radius in logical pixels.
    #[serde(default = "default_radius_xs_px")]
    pub radius_xs_px: f32,
    /// Small border radius in logical pixels.
    pub radius_sm_px: f32,
    /// Medium border radius in logical pixels.
    pub radius_md_px: f32,
    /// Large border radius in logical pixels.
    #[serde(default = "default_radius_lg_px")]
    pub radius_lg_px: f32,
    /// Extra-large border radius in logical pixels.
    #[serde(default = "default_radius_xl_px")]
    pub radius_xl_px: f32,
    /// Legacy extra-small spacing unit in logical pixels.
    #[serde(default = "default_space_1_px")]
    pub spacing_xs_px: f32,
    /// Legacy small spacing unit in logical pixels.
    #[serde(default = "default_space_2_px")]
    pub spacing_sm_px: f32,
    /// Legacy medium spacing unit in logical pixels.
    #[serde(default = "default_space_3_px")]
    pub spacing_md_px: f32,
    /// Legacy large spacing unit in logical pixels.
    #[serde(default = "default_space_4_px")]
    pub spacing_lg_px: f32,
    /// 4 px spacing unit.
    #[serde(default = "default_space_1_px")]
    pub space_1_px: f32,
    /// 8 px spacing unit.
    #[serde(default = "default_space_2_px")]
    pub space_2_px: f32,
    /// 12 px spacing unit.
    #[serde(default = "default_space_3_px")]
    pub space_3_px: f32,
    /// 16 px spacing unit.
    #[serde(default = "default_space_4_px")]
    pub space_4_px: f32,
    /// 20 px spacing unit.
    #[serde(default = "default_space_5_px")]
    pub space_5_px: f32,
    /// 24 px spacing unit.
    #[serde(default = "default_space_6_px")]
    pub space_6_px: f32,
    /// 32 px spacing unit.
    #[serde(default = "default_space_8_px")]
    pub space_8_px: f32,
    /// 40 px spacing unit.
    #[serde(default = "default_space_10_px")]
    pub space_10_px: f32,
}
