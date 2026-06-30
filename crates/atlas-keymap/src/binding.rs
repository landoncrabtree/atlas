//! A [`Binding`] associates a chord sequence and a context with an action.

use crate::{ActionId, ChordSequence};

/// Associates a chord sequence in a context with an action.
///
/// Setting [`action`] to [`ActionId::null()`] in a higher layer **suppresses**
/// the lower-layer binding (prevents it from firing).
///
/// [`action`]: Binding::action
#[derive(Clone, Debug)]
pub struct Binding {
    /// The chord sequence that triggers this binding.
    pub sequence: ChordSequence,
    /// The context scope in which this binding is active.
    /// `"Global"` matches every context stack.
    pub context: String,
    /// The action to invoke, or [`ActionId::null()`] to suppress.
    pub action: ActionId,
}

impl Binding {
    /// Create a new binding.
    pub fn new(sequence: ChordSequence, context: impl Into<String>, action: ActionId) -> Self {
        Self {
            sequence,
            context: context.into(),
            action,
        }
    }

    /// Returns `true` if this binding suppresses a lower-layer binding.
    pub fn is_suppression(&self) -> bool {
        self.action.is_null()
    }
}
