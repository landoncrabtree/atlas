//! Back/forward navigation history stack.

use atlas_core::Location;

/// A bounded back/forward navigation history stack.
///
/// Maintains a current location and two ordered stacks (back and forward).
/// Pushing a new location clears the forward stack.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackForwardStack {
    back: Vec<Location>,
    current: Option<Location>,
    forward: Vec<Location>,
    capacity: usize,
}

impl BackForwardStack {
    /// Create a new stack with the given maximum back-history capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            back: Vec::new(),
            current: None,
            forward: Vec::new(),
            capacity,
        }
    }

    /// The current location, if any.
    #[must_use]
    pub fn current(&self) -> Option<&Location> {
        self.current.as_ref()
    }

    /// Push a new location.
    ///
    /// The previous current is moved to the back stack. The forward stack is
    /// cleared. If the back stack exceeds `capacity`, the oldest entry is
    /// dropped.
    pub fn push(&mut self, location: Location) {
        if let Some(current) = self.current.take() {
            self.back.push(current);
            if self.capacity > 0 && self.back.len() > self.capacity {
                self.back.remove(0);
            }
        }
        self.forward.clear();
        self.current = Some(location);
    }

    /// Navigate back; returns the new current location or `None` if no back history.
    pub fn back(&mut self) -> Option<Location> {
        let prev = self.back.pop()?;
        if let Some(current) = self.current.take() {
            self.forward.push(current);
        }
        self.current = Some(prev.clone());
        Some(prev)
    }

    /// Navigate forward; returns the new current location or `None` if no forward history.
    pub fn forward(&mut self) -> Option<Location> {
        let next = self.forward.pop()?;
        if let Some(current) = self.current.take() {
            self.back.push(current);
        }
        self.current = Some(next.clone());
        Some(next)
    }

    /// Returns `true` if back navigation is possible.
    #[must_use]
    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    /// Returns `true` if forward navigation is possible.
    #[must_use]
    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Number of entries in the back stack (excluding current).
    #[must_use]
    pub fn len(&self) -> usize {
        self.back.len()
    }

    /// Returns `true` if the back stack is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.back.is_empty()
    }

    /// A snapshot of the back stack, oldest-first.
    #[must_use]
    pub fn back_history(&self) -> Vec<Location> {
        self.back.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(path: &str) -> Location {
        Location::local(path)
    }

    #[test]
    fn push_sets_current() {
        let mut s = BackForwardStack::new(10);
        s.push(loc("/a"));
        assert_eq!(s.current(), Some(&loc("/a")));
    }

    #[test]
    fn back_returns_previous() {
        let mut s = BackForwardStack::new(10);
        s.push(loc("/a"));
        s.push(loc("/b"));
        let prev = s.back();
        assert_eq!(prev, Some(loc("/a")));
        assert_eq!(s.current(), Some(&loc("/a")));
    }

    #[test]
    fn push_clears_forward() {
        let mut s = BackForwardStack::new(10);
        s.push(loc("/a"));
        s.push(loc("/b"));
        s.back();
        s.push(loc("/c"));
        assert!(!s.can_go_forward());
        assert_eq!(s.current(), Some(&loc("/c")));
    }

    #[test]
    fn forward_after_back() {
        let mut s = BackForwardStack::new(10);
        s.push(loc("/a"));
        s.push(loc("/b"));
        s.back();
        let fwd = s.forward();
        assert_eq!(fwd, Some(loc("/b")));
        assert_eq!(s.current(), Some(&loc("/b")));
    }

    #[test]
    fn capacity_trims_oldest() {
        let mut s = BackForwardStack::new(2);
        s.push(loc("/a"));
        s.push(loc("/b"));
        s.push(loc("/c"));
        s.push(loc("/d"));
        assert_eq!(s.back.len(), 2);
        assert_eq!(s.back[0], loc("/b"));
    }

    #[test]
    fn can_go_back_and_forward() {
        let mut s = BackForwardStack::new(10);
        assert!(!s.can_go_back());
        assert!(!s.can_go_forward());
        s.push(loc("/a"));
        s.push(loc("/b"));
        assert!(s.can_go_back());
        assert!(!s.can_go_forward());
        s.back();
        assert!(!s.can_go_back());
        assert!(s.can_go_forward());
    }
}
