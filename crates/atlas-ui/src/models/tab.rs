//! Tab model — represents a single browser-like tab within a pane.

/// A single tab inside a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabModel {
    /// Display title shown in the tab strip.
    pub title: String,
    /// Whether the tab has unsaved or in-progress state.
    pub dirty: bool,
}

impl TabModel {
    /// Create a clean (non-dirty) tab with the given title.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            dirty: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_model_new() {
        let tab = TabModel::new("Downloads");
        assert_eq!(tab.title, "Downloads");
        assert!(!tab.dirty);
    }
}
