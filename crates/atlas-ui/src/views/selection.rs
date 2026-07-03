//! Shared multi-selection model for pane views.
//!
//! Motivation — before this module existed, `views::details::Selection`
//! and `views::grid::GridSelection` were byte-identical duplicates
//! (see the `TODO` in `views/grid/controller.rs` pre-refactor:
//! "if Selection methods are made pub upstream, remove this
//! duplicate"). The methods on `details::Selection` were `pub(crate)`
//! so grid could not re-use them without cross-module private-access
//! gymnastics, so grid forked its own copy — one bug fix now had to
//! land twice.
//!
//! Convergence — [`SelectionMask`] lives here with all methods
//! declared `pub`. Details and Grid controllers both hold a
//! `RwLock<SelectionMask>` in place of their previous per-view struct.
//! Adding a new view that wants multi-select support is a one-line
//! `selection: RwLock<SelectionMask>` field — no re-implementation.
//!
//! # Semantics
//!
//! * `mask[i] = true` means entry `i` is currently selected.
//! * `anchor` is the pivot for shift-range selection. Set by
//!   [`select_single`](SelectionMask::select_single) and
//!   [`toggle`](SelectionMask::toggle); consumed by
//!   [`select_range`](SelectionMask::select_range) via the caller
//!   (typically stored on the caller as `anchor.unwrap_or(new_index)`).
//!
//! # Non-goals
//!
//! * This is NOT a general "SelectionModel trait" — Miller's column-
//!   stack has a fundamentally different notion of selection (focused
//!   column + focused row per column) and Gallery is single-focus.
//!   Extracting only the shared shape (a flat mask + anchor) keeps
//!   the API honest — Details and Grid are the two views that share
//!   this exact model. Extending to Miller/Gallery is deferred to
//!   v0.3, when the multi-column selection story is designed
//!   properly rather than shoehorned onto a flat mask.

/// Multi-selection state for a linear list of entries.
///
/// Shared by [`crate::views::details::DetailsController`] and
/// [`crate::views::grid::GridController`]. See the module docs for
/// rationale and non-goals.
#[derive(Debug, Default)]
pub struct SelectionMask {
    /// Per-entry selection flags; same length as the current entries snapshot.
    pub mask: Vec<bool>,
    /// Anchor index for shift-range selection.
    pub anchor: Option<usize>,
}

impl SelectionMask {
    /// Drop every flag and forget the anchor.
    pub fn clear(&mut self) {
        self.mask.fill(false);
        self.anchor = None;
    }

    /// Resize the mask so `mask.len() == len`. New slots default to `false`.
    pub fn resize(&mut self, len: usize) {
        self.mask.resize(len, false);
    }

    /// Clear every flag then select `index`. `index` becomes the anchor.
    pub fn select_single(&mut self, index: usize) {
        self.clear();
        if index < self.mask.len() {
            self.mask[index] = true;
        }
        self.anchor = Some(index);
    }

    /// Flip the flag at `index`. `index` becomes the anchor.
    pub fn toggle(&mut self, index: usize) {
        if index < self.mask.len() {
            self.mask[index] = !self.mask[index];
        }
        self.anchor = Some(index);
    }

    /// Replace the mask with exactly `from..=to` selected (inclusive on
    /// both ends). `from` becomes the anchor. Order of the two arguments
    /// does not matter — the range is normalized.
    pub fn select_range(&mut self, from: usize, to: usize) {
        if self.mask.is_empty() {
            self.anchor = Some(to);
            return;
        }
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        let hi_clamped = hi.min(self.mask.len().saturating_sub(1));
        self.mask.fill(false);
        for slot in &mut self.mask[lo..=hi_clamped] {
            *slot = true;
        }
        self.anchor = Some(from);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(len: usize) -> SelectionMask {
        let mut s = SelectionMask::default();
        s.resize(len);
        s
    }

    #[test]
    fn select_single_clears_others_and_sets_anchor() {
        let mut s = make(5);
        s.mask[0] = true;
        s.mask[2] = true;
        s.select_single(3);
        assert_eq!(s.mask, vec![false, false, false, true, false]);
        assert_eq!(s.anchor, Some(3));
    }

    #[test]
    fn toggle_flips_and_sets_anchor() {
        let mut s = make(4);
        s.toggle(2);
        assert!(s.mask[2]);
        assert_eq!(s.anchor, Some(2));
        s.toggle(2);
        assert!(!s.mask[2]);
        assert_eq!(s.anchor, Some(2));
    }

    #[test]
    fn select_range_forward_inclusive() {
        let mut s = make(6);
        s.select_range(1, 4);
        assert!(!s.mask[0]);
        assert!(s.mask[1..=4].iter().all(|&b| b));
        assert!(!s.mask[5]);
        assert_eq!(s.anchor, Some(1));
    }

    #[test]
    fn select_range_reverse_normalizes() {
        let mut s = make(6);
        s.select_range(4, 1);
        assert!(!s.mask[0]);
        assert!(s.mask[1..=4].iter().all(|&b| b));
        assert!(!s.mask[5]);
        // Anchor is `from` — the caller's pivot — not the min of the pair.
        assert_eq!(s.anchor, Some(4));
    }

    #[test]
    fn select_range_clamps_hi_end() {
        let mut s = make(3);
        s.select_range(0, 99);
        assert!(s.mask.iter().all(|&b| b));
    }

    #[test]
    fn clear_resets_mask_and_anchor() {
        let mut s = make(3);
        s.select_single(1);
        s.clear();
        assert!(s.mask.iter().all(|&b| !b));
        assert!(s.anchor.is_none());
    }
}
