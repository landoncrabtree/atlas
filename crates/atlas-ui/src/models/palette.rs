//! Command palette model — query, results, and visibility state.

/// A single command-palette result entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteResult {
    /// Primary display text.
    pub title: String,
    /// Secondary detail line.
    pub subtitle: String,
    /// Opaque identifier dispatched on confirmation.
    pub action_id: String,
}

/// Full command-palette state.
#[derive(Debug, Clone, Default)]
pub struct PaletteModel {
    /// Whether the palette overlay is shown.
    pub visible: bool,
    /// Current text in the query field.
    pub query: String,
    /// Filtered result list.
    pub results: Vec<PaletteResult>,
    /// Keyboard-highlighted result index.
    pub selected: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_default_is_invisible() {
        let palette = PaletteModel::default();
        assert!(!palette.visible);
        assert!(palette.query.is_empty());
        assert!(palette.results.is_empty());
        assert_eq!(palette.selected, 0);
    }
}
