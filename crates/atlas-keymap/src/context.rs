//! Context (named scope) types.
//!
//! A context is a named string scope such as `"Pane"`, `"Palette"`, or
//! `"Global"`. Bindings are only active when their context is present in the
//! active context stack.

/// The special context string that matches in every context stack.
pub const GLOBAL_CONTEXT: &str = "Global";

/// Returns `true` if the given binding context is satisfied by the active
/// context stack.
///
/// A binding context is satisfied when:
/// - it equals `"Global"`, OR
/// - it is present anywhere in `active_contexts`.
pub fn context_matches(binding_context: &str, active_contexts: &[String]) -> bool {
    binding_context == GLOBAL_CONTEXT || active_contexts.iter().any(|c| c == binding_context)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_global_matches_always() {
        assert!(context_matches("Global", &[]));
        assert!(context_matches("Global", &["Pane".into()]));
    }

    #[test]
    fn test_specific_context() {
        assert!(context_matches(
            "Pane",
            &["Pane".into(), "BulkRename".into()]
        ));
        assert!(!context_matches("Pane", &["Palette".into()]));
        assert!(!context_matches("Pane", &[]));
    }
}
