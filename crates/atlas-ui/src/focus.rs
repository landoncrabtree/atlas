//! Focus management: tracks which pane owns keyboard focus and whether the
//! command palette is intercepting input.

/// Tracks focus state across the workspace.
#[derive(Debug, Clone, Default)]
pub struct FocusManager {
    /// Index of the pane that currently holds focus.
    pub focused_pane: usize,
    /// Whether the command palette is open.
    pub palette_open: bool,
}

impl FocusManager {
    /// Cycle focus to the next pane.
    pub fn cycle(&mut self, pane_count: usize) {
        if pane_count > 1 {
            self.focused_pane = (self.focused_pane + 1) % pane_count;
        }
    }

    /// Open the command palette, marking it as the focus owner.
    pub fn open_palette(&mut self) {
        self.palette_open = true;
    }

    /// Close the command palette, returning focus to the last pane.
    pub fn close_palette(&mut self) {
        self.palette_open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_two_panes() {
        let mut focus = FocusManager::default();
        focus.cycle(2);
        assert_eq!(focus.focused_pane, 1);
        focus.cycle(2);
        assert_eq!(focus.focused_pane, 0);
    }

    #[test]
    fn cycle_one_pane_noop() {
        let mut focus = FocusManager::default();
        focus.cycle(1);
        assert_eq!(focus.focused_pane, 0);
    }

    #[test]
    fn palette_open_close() {
        let mut focus = FocusManager::default();
        focus.open_palette();
        assert!(focus.palette_open);
        focus.close_palette();
        assert!(!focus.palette_open);
    }
}
