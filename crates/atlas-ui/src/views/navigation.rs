//! Shared directional-navigation surface for every pane view.
//!
//! Motivation — before this module existed, each of `pane::MoveLeft`,
//! `pane::MoveRight`, `pane::MoveUp`, `pane::MoveDown` was a separate
//! closure in `build_dispatcher` with its own `match view_mode {
//! Details => …, Grid => …, Gallery => …, Miller => … }` arm. Adding a
//! new view meant editing four places; changing what "Left" means in
//! Grid meant re-reading four scattered blocks to keep them consistent.
//!
//! Convergence — this module centralises "what does Direction X mean in
//! view Y?" in a single lookup table via [`ViewNavAction::for_mode`].
//! The dispatcher becomes a tiny loop that registers all four action
//! IDs against the same closure body: look up the pane's view mode,
//! resolve the direction to an action, execute the action against the
//! pane's controllers. Adding a new view mode means adding one arm to
//! the match — nothing else changes.
//!
//! # Contract
//!
//! [`ViewNavAction`] is a small enum describing what a directional
//! keypress should do in the current view. It intentionally does NOT
//! carry closures or controllers — the dispatcher decides how to
//! execute each variant against the shell + per-view controllers. This
//! keeps the enum `Copy` / testable in isolation and prevents `views/`
//! from depending on shell internals.
//!
//! # Non-goals
//!
//! * This is NOT a general-purpose keymap dispatch. Chord parsing,
//!   modifier handling, and context-stack resolution stay in
//!   `atlas-keymap`. This module only answers *"if the dispatcher has
//!   already decided this is Left/Right/Up/Down for a Pane, what
//!   should happen given the current view mode?"*
//! * Shift-extend variants stay separate — [`ExtendDown`](crate::views::navigation::ViewNavAction)
//!   was considered here but shift-extend is only meaningful for
//!   Details/Grid today, so it stays in a separate dispatcher closure
//!   (see `pane::ExtendUp` / `pane::ExtendDown` in `build_dispatcher`).

use crate::models::ViewMode;

/// Which arrow-key direction the user pressed.
///
/// Matches the four Slint `KeyEvent.text` bindings (`h`/`j`/`k`/`l`,
/// `w`/`a`/`s`/`d`, and the four named arrow keys) once they have
/// been resolved through the keymap dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// What a directional keypress means in the current view.
///
/// The dispatcher executes each variant against the appropriate
/// controller / shell method — see `build_dispatcher::register_move_dir`
/// in `atlas-app` for the mapping.
///
/// # Variants
///
/// * [`MoveFocus`] — 2D grid move (`Grid` view). Positive `dx` = right,
///   positive `dy` = down. May wrap between rows depending on the
///   caller's semantics (Grid Left/Right wraps; Grid Up/Down does not).
/// * [`MoveIndex`] — linear focus move by `delta` entries. Positive =
///   forward (down / next). Used by `Details`, `Miller`, and `Gallery`
///   for the axes where the view is linear.
/// * [`GoUp`] — pop out to the parent directory. Same behavior as
///   `pane::GoUp`. Details/Miller `Left` and Details/Miller shortcut
///   `Backspace` / `,` all resolve to this.
/// * [`ViewEntry`] — activate the focused entry (`fs::View` semantics:
///   `cd` into directories, open files with the OS default). Details/
///   Miller `Right` and shortcut `Enter` / `.` resolve to this.
/// * [`Noop`] — direction is meaningless in the current view (Gallery
///   Up/Down). The dispatcher still consumes the key event so the
///   fallback path doesn't route it to an unrelated handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewNavAction {
    MoveFocus { dx: isize, dy: isize },
    MoveIndex { delta: isize },
    GoUp,
    ViewEntry,
    Noop,
}

impl ViewNavAction {
    /// Resolve `(view_mode, direction)` → `ViewNavAction`.
    ///
    /// This is THE single place where per-view directional semantics
    /// live. Adding a new view mode means adding one match arm; changing
    /// Grid Left/Right semantics is a one-line edit here.
    ///
    /// # Semantics table (mirror of `docs/keymap.md`)
    ///
    /// | View    | Left        | Right       | Up          | Down        |
    /// |---------|-------------|-------------|-------------|-------------|
    /// | Details | GoUp        | ViewEntry   | MoveIndex-1 | MoveIndex+1 |
    /// | Miller  | GoUp        | ViewEntry   | MoveIndex-1 | MoveIndex+1 |
    /// | Grid    | dx=-1 wrap  | dx=+1 wrap  | dy=-1       | dy=+1       |
    /// | Gallery | MoveIndex-1 | MoveIndex+1 | Noop        | Noop        |
    ///
    /// Grid row-wrap is applied by [`GridController::move_focus_wrapping`]
    /// (not this enum) — the enum stays a pure data mapping.
    #[must_use]
    pub fn for_mode(mode: ViewMode, dir: Direction) -> Self {
        match (mode, dir) {
            // ── Details / Miller — linear list with hierarchical Left/Right ──
            (ViewMode::Details | ViewMode::Miller, Direction::Left) => Self::GoUp,
            (ViewMode::Details | ViewMode::Miller, Direction::Right) => Self::ViewEntry,
            (ViewMode::Details | ViewMode::Miller, Direction::Up) => Self::MoveIndex { delta: -1 },
            (ViewMode::Details | ViewMode::Miller, Direction::Down) => Self::MoveIndex { delta: 1 },

            // ── Grid — true 2D navigation with row-wrap on horizontal ──
            (ViewMode::Grid, Direction::Left) => Self::MoveFocus { dx: -1, dy: 0 },
            (ViewMode::Grid, Direction::Right) => Self::MoveFocus { dx: 1, dy: 0 },
            (ViewMode::Grid, Direction::Up) => Self::MoveFocus { dx: 0, dy: -1 },
            (ViewMode::Grid, Direction::Down) => Self::MoveFocus { dx: 0, dy: 1 },

            // ── Gallery — horizontal strip; vertical axis is meaningless ──
            (ViewMode::Gallery, Direction::Left) => Self::MoveIndex { delta: -1 },
            (ViewMode::Gallery, Direction::Right) => Self::MoveIndex { delta: 1 },
            (ViewMode::Gallery, Direction::Up | Direction::Down) => Self::Noop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_left_is_go_up() {
        assert_eq!(
            ViewNavAction::for_mode(ViewMode::Details, Direction::Left),
            ViewNavAction::GoUp
        );
    }

    #[test]
    fn details_right_is_view_entry() {
        assert_eq!(
            ViewNavAction::for_mode(ViewMode::Details, Direction::Right),
            ViewNavAction::ViewEntry
        );
    }

    #[test]
    fn miller_matches_details_semantics() {
        for dir in [Direction::Left, Direction::Right, Direction::Up, Direction::Down] {
            assert_eq!(
                ViewNavAction::for_mode(ViewMode::Miller, dir),
                ViewNavAction::for_mode(ViewMode::Details, dir),
                "Miller diverged from Details for {dir:?}"
            );
        }
    }

    #[test]
    fn grid_left_right_are_horizontal_column_moves() {
        assert_eq!(
            ViewNavAction::for_mode(ViewMode::Grid, Direction::Left),
            ViewNavAction::MoveFocus { dx: -1, dy: 0 }
        );
        assert_eq!(
            ViewNavAction::for_mode(ViewMode::Grid, Direction::Right),
            ViewNavAction::MoveFocus { dx: 1, dy: 0 }
        );
    }

    #[test]
    fn grid_up_down_are_vertical_row_moves() {
        assert_eq!(
            ViewNavAction::for_mode(ViewMode::Grid, Direction::Up),
            ViewNavAction::MoveFocus { dx: 0, dy: -1 }
        );
        assert_eq!(
            ViewNavAction::for_mode(ViewMode::Grid, Direction::Down),
            ViewNavAction::MoveFocus { dx: 0, dy: 1 }
        );
    }

    #[test]
    fn gallery_horizontal_is_index_prev_next() {
        assert_eq!(
            ViewNavAction::for_mode(ViewMode::Gallery, Direction::Left),
            ViewNavAction::MoveIndex { delta: -1 }
        );
        assert_eq!(
            ViewNavAction::for_mode(ViewMode::Gallery, Direction::Right),
            ViewNavAction::MoveIndex { delta: 1 }
        );
    }

    #[test]
    fn gallery_vertical_is_noop() {
        assert_eq!(
            ViewNavAction::for_mode(ViewMode::Gallery, Direction::Up),
            ViewNavAction::Noop
        );
        assert_eq!(
            ViewNavAction::for_mode(ViewMode::Gallery, Direction::Down),
            ViewNavAction::Noop
        );
    }
}
