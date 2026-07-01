//! Split-tree primitives for the multi-pane workspace model.

use serde::{Deserialize, Serialize};

/// Stable identifier for a workspace pane.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PaneId(pub u32);

/// Direction of a binary split in the layout tree.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    /// First child occupies the left side, second the right side.
    Horizontal,
    /// First child occupies the top side, second the bottom side.
    Vertical,
}

/// Cardinal direction used for pane-to-pane focus movement.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Cardinal {
    /// Move focus to the nearest pane on the left.
    Left,
    /// Move focus to the nearest pane on the right.
    Right,
    /// Move focus to the nearest pane above.
    Up,
    /// Move focus to the nearest pane below.
    Down,
}

/// Rectangle in logical coordinates.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rect {
    /// Left origin.
    pub x: f32,
    /// Top origin.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl Rect {
    /// Create a rectangle at the origin with the provided size.
    #[must_use]
    pub fn from_size(width: f32, height: f32) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width,
            height,
        }
    }

    /// Returns the shared edge length on the perpendicular axis.
    #[must_use]
    pub fn edges_overlap(self, other: Self, side: Cardinal) -> f32 {
        let (a0, a1, b0, b1) = match side {
            Cardinal::Left | Cardinal::Right => (
                self.y,
                self.y + self.height,
                other.y,
                other.y + other.height,
            ),
            Cardinal::Up | Cardinal::Down => {
                (self.x, self.x + self.width, other.x, other.x + other.width)
            }
        };

        (a1.min(b1) - a0.max(b0)).max(0.0)
    }
}

/// Result of removing a pane leaf from a split layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseOutcome {
    /// Removed pane identifier.
    pub removed: PaneId,
    /// Pane that should receive focus after the close.
    pub new_focused: PaneId,
}

/// Binary split tree describing an arbitrary pane layout.
#[derive(Debug, Clone)]
pub enum SplitLayout {
    /// Leaf node containing a pane id.
    Leaf(PaneId),
    /// Internal split node.
    Split {
        /// Split axis.
        direction: SplitDirection,
        /// First child's fraction of the split axis.
        ratio: f32,
        /// First child subtree.
        first: Box<SplitLayout>,
        /// Second child subtree.
        second: Box<SplitLayout>,
    },
}

impl SplitLayout {
    /// Construct a single-leaf layout.
    #[must_use]
    pub fn single(id: PaneId) -> Self {
        Self::Leaf(id)
    }

    /// Return every leaf in depth-first order.
    #[must_use]
    pub fn all_leaves(&self) -> Vec<PaneId> {
        match self {
            Self::Leaf(id) => vec![*id],
            Self::Split { first, second, .. } => {
                let mut leaves = first.all_leaves();
                leaves.extend(second.all_leaves());
                leaves
            }
        }
    }

    /// Compute the rectangle for every pane leaf within `bounds`.
    #[must_use]
    pub fn layout_rects(&self, bounds: Rect) -> Vec<(PaneId, Rect)> {
        let mut rects = Vec::with_capacity(self.leaf_count());
        self.collect_layout_rects(bounds, &mut rects);
        rects
    }

    /// Find the nearest focusable neighbour of `from` in `dir`.
    #[must_use]
    pub fn focus_direction(&self, from: PaneId, dir: Cardinal, bounds: Rect) -> Option<PaneId> {
        const TOLERANCE: f32 = 0.5;

        let rects = self.layout_rects(bounds);
        let from_rect = rects
            .iter()
            .find_map(|(id, rect)| (*id == from).then_some(*rect))?;

        let mut best: Option<(PaneId, f32, f32)> = None;
        for (id, rect) in rects {
            if id == from || !is_candidate_on_side(from_rect, rect, dir, TOLERANCE) {
                continue;
            }

            let overlap = from_rect.edges_overlap(rect, dir);
            let distance = parallel_distance(from_rect, rect, dir);
            let replace = match best {
                None => true,
                Some((_, best_overlap, best_distance)) => {
                    overlap > best_overlap || (overlap == best_overlap && distance < best_distance)
                }
            };
            if replace {
                best = Some((id, overlap, distance));
            }
        }

        best.map(|(id, _, _)| id)
    }

    /// Split `id`, inserting `new_id` as the second child.
    #[allow(clippy::result_unit_err)]
    pub fn split_leaf(
        &mut self,
        id: PaneId,
        dir: SplitDirection,
        new_id: PaneId,
    ) -> Result<(), ()> {
        match self {
            Self::Leaf(current) if *current == id => {
                *self = Self::Split {
                    direction: dir,
                    ratio: 0.5,
                    first: Box::new(Self::Leaf(id)),
                    second: Box::new(Self::Leaf(new_id)),
                };
                Ok(())
            }
            Self::Leaf(_) => Err(()),
            Self::Split { first, second, .. } => first
                .split_leaf(id, dir, new_id)
                .or_else(|()| second.split_leaf(id, dir, new_id)),
        }
    }

    /// Remove `id`, collapsing the containing split into the sibling subtree.
    pub fn close_leaf(&mut self, id: PaneId) -> Option<CloseOutcome> {
        if matches!(self, Self::Leaf(_)) {
            return None;
        }

        let old = std::mem::replace(self, Self::Leaf(PaneId(0)));
        match try_close_leaf(old, id) {
            Ok((new_layout, outcome)) => {
                *self = new_layout;
                Some(outcome)
            }
            Err(original) => {
                *self = original;
                None
            }
        }
    }

    /// Return the number of pane leaves.
    #[must_use]
    pub fn leaf_count(&self) -> usize {
        match self {
            Self::Leaf(_) => 1,
            Self::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    /// Returns `true` when the layout contains at least one split node.
    #[must_use]
    pub fn is_split(&self) -> bool {
        matches!(self, Self::Split { .. })
    }

    fn collect_layout_rects(&self, bounds: Rect, rects: &mut Vec<(PaneId, Rect)>) {
        match self {
            Self::Leaf(id) => rects.push((*id, bounds)),
            Self::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let ratio = ratio.clamp(0.05, 0.95);
                let (first_rect, second_rect) = match direction {
                    SplitDirection::Horizontal => {
                        let first_width = bounds.width * ratio;
                        (
                            Rect {
                                x: bounds.x,
                                y: bounds.y,
                                width: first_width,
                                height: bounds.height,
                            },
                            Rect {
                                x: bounds.x + first_width,
                                y: bounds.y,
                                width: bounds.width - first_width,
                                height: bounds.height,
                            },
                        )
                    }
                    SplitDirection::Vertical => {
                        let first_height = bounds.height * ratio;
                        (
                            Rect {
                                x: bounds.x,
                                y: bounds.y,
                                width: bounds.width,
                                height: first_height,
                            },
                            Rect {
                                x: bounds.x,
                                y: bounds.y + first_height,
                                width: bounds.width,
                                height: bounds.height - first_height,
                            },
                        )
                    }
                };
                first.collect_layout_rects(first_rect, rects);
                second.collect_layout_rects(second_rect, rects);
            }
        }
    }

    fn first_leaf(&self) -> PaneId {
        match self {
            Self::Leaf(id) => *id,
            Self::Split { first, .. } => first.first_leaf(),
        }
    }
}

fn is_candidate_on_side(from: Rect, other: Rect, dir: Cardinal, tolerance: f32) -> bool {
    match dir {
        Cardinal::Left => other.x + other.width <= from.x + tolerance,
        Cardinal::Right => other.x >= from.x + from.width - tolerance,
        Cardinal::Up => other.y + other.height <= from.y + tolerance,
        Cardinal::Down => other.y >= from.y + from.height - tolerance,
    }
}

fn parallel_distance(from: Rect, other: Rect, dir: Cardinal) -> f32 {
    match dir {
        Cardinal::Left => from.x - (other.x + other.width),
        Cardinal::Right => other.x - (from.x + from.width),
        Cardinal::Up => from.y - (other.y + other.height),
        Cardinal::Down => other.y - (from.y + from.height),
    }
}

fn try_close_leaf(
    node: SplitLayout,
    id: PaneId,
) -> Result<(SplitLayout, CloseOutcome), SplitLayout> {
    match node {
        SplitLayout::Leaf(lid) => Err(SplitLayout::Leaf(lid)),
        SplitLayout::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            if let SplitLayout::Leaf(lid) = first.as_ref() {
                if *lid == id {
                    let new_focused = second.first_leaf();
                    return Ok((
                        *second,
                        CloseOutcome {
                            removed: id,
                            new_focused,
                        },
                    ));
                }
            }

            if let SplitLayout::Leaf(lid) = second.as_ref() {
                if *lid == id {
                    let new_focused = first.first_leaf();
                    return Ok((
                        *first,
                        CloseOutcome {
                            removed: id,
                            new_focused,
                        },
                    ));
                }
            }

            match try_close_leaf(*first, id) {
                Ok((new_first, outcome)) => Ok((
                    SplitLayout::Split {
                        direction,
                        ratio,
                        first: Box::new(new_first),
                        second,
                    },
                    outcome,
                )),
                Err(orig_first) => match try_close_leaf(*second, id) {
                    Ok((new_second, outcome)) => Ok((
                        SplitLayout::Split {
                            direction,
                            ratio,
                            first: Box::new(orig_first),
                            second: Box::new(new_second),
                        },
                        outcome,
                    )),
                    Err(orig_second) => Err(SplitLayout::Split {
                        direction,
                        ratio,
                        first: Box::new(orig_first),
                        second: Box::new(orig_second),
                    }),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(id: PaneId, x: f32, y: f32, width: f32, height: f32) -> (PaneId, Rect) {
        (
            id,
            Rect {
                x,
                y,
                width,
                height,
            },
        )
    }

    fn grid_layout() -> SplitLayout {
        SplitLayout::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(SplitLayout::Split {
                direction: SplitDirection::Vertical,
                ratio: 0.5,
                first: Box::new(SplitLayout::Leaf(PaneId(1))),
                second: Box::new(SplitLayout::Leaf(PaneId(3))),
            }),
            second: Box::new(SplitLayout::Split {
                direction: SplitDirection::Vertical,
                ratio: 0.5,
                first: Box::new(SplitLayout::Leaf(PaneId(2))),
                second: Box::new(SplitLayout::Leaf(PaneId(4))),
            }),
        }
    }

    #[test]
    fn single() {
        let layout = SplitLayout::single(PaneId(1));
        assert_eq!(layout.leaf_count(), 1);
        assert_eq!(layout.all_leaves(), vec![PaneId(1)]);
    }

    #[test]
    fn split_leaf_on_leaf_creates_split() {
        let mut layout = SplitLayout::single(PaneId(1));
        let result = layout.split_leaf(PaneId(1), SplitDirection::Horizontal, PaneId(2));
        assert_eq!(result, Ok(()));
        assert!(layout.is_split());
        assert_eq!(layout.leaf_count(), 2);
        assert_eq!(layout.all_leaves(), vec![PaneId(1), PaneId(2)]);
    }

    #[test]
    fn deep_split_preserves_depth_first_leaf_order() {
        let mut layout = SplitLayout::single(PaneId(1));
        let _ = layout.split_leaf(PaneId(1), SplitDirection::Horizontal, PaneId(2));
        let _ = layout.split_leaf(PaneId(1), SplitDirection::Vertical, PaneId(3));
        let _ = layout.split_leaf(PaneId(2), SplitDirection::Horizontal, PaneId(4));
        assert_eq!(
            layout.all_leaves(),
            vec![PaneId(1), PaneId(3), PaneId(2), PaneId(4)]
        );
        assert_eq!(layout.leaf_count(), 4);
    }

    #[test]
    fn close_leaf_collapses_two_leaf_layout() {
        let mut layout = SplitLayout::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(SplitLayout::Leaf(PaneId(1))),
            second: Box::new(SplitLayout::Leaf(PaneId(2))),
        };

        let outcome = layout.close_leaf(PaneId(1));
        assert_eq!(
            outcome,
            Some(CloseOutcome {
                removed: PaneId(1),
                new_focused: PaneId(2),
            })
        );
        assert_eq!(layout.all_leaves(), vec![PaneId(2)]);
    }

    #[test]
    fn close_leaf_preserves_remaining_nested_leaves() {
        let mut layout = grid_layout();
        let outcome = layout.close_leaf(PaneId(2));
        assert_eq!(
            outcome,
            Some(CloseOutcome {
                removed: PaneId(2),
                new_focused: PaneId(4),
            })
        );
        assert_eq!(layout.all_leaves(), vec![PaneId(1), PaneId(3), PaneId(4)]);
        assert_eq!(layout.leaf_count(), 3);
    }

    #[test]
    fn close_leaf_refuses_single_leaf() {
        let mut layout = SplitLayout::single(PaneId(1));
        assert_eq!(layout.close_leaf(PaneId(1)), None);
    }

    #[test]
    fn layout_rects_single_leaf_returns_bounds() {
        let bounds = Rect {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 80.0,
        };
        let layout = SplitLayout::single(PaneId(7));
        assert_eq!(layout.layout_rects(bounds), vec![(PaneId(7), bounds)]);
    }

    #[test]
    fn layout_rects_horizontal_split_halves_width() {
        let layout = SplitLayout::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(SplitLayout::Leaf(PaneId(1))),
            second: Box::new(SplitLayout::Leaf(PaneId(2))),
        };
        assert_eq!(
            layout.layout_rects(Rect::from_size(200.0, 100.0)),
            vec![
                rect(PaneId(1), 0.0, 0.0, 100.0, 100.0),
                rect(PaneId(2), 100.0, 0.0, 100.0, 100.0),
            ]
        );
    }

    #[test]
    fn layout_rects_vertical_split_halves_height() {
        let layout = SplitLayout::Split {
            direction: SplitDirection::Vertical,
            ratio: 0.5,
            first: Box::new(SplitLayout::Leaf(PaneId(1))),
            second: Box::new(SplitLayout::Leaf(PaneId(2))),
        };
        assert_eq!(
            layout.layout_rects(Rect::from_size(200.0, 100.0)),
            vec![
                rect(PaneId(1), 0.0, 0.0, 200.0, 50.0),
                rect(PaneId(2), 0.0, 50.0, 200.0, 50.0),
            ]
        );
    }

    #[test]
    fn focus_direction_moves_across_two_by_two_grid() {
        let layout = grid_layout();
        let bounds = Rect::from_size(200.0, 200.0);

        assert_eq!(
            layout.focus_direction(PaneId(1), Cardinal::Right, bounds),
            Some(PaneId(2))
        );
        assert_eq!(
            layout.focus_direction(PaneId(1), Cardinal::Down, bounds),
            Some(PaneId(3))
        );
        assert_eq!(
            layout.focus_direction(PaneId(2), Cardinal::Left, bounds),
            Some(PaneId(1))
        );
        assert_eq!(
            layout.focus_direction(PaneId(2), Cardinal::Down, bounds),
            Some(PaneId(4))
        );
    }
}
