//! Layered keymap: default bindings overlaid by user bindings.
//!
//! # Layer ordering
//!
//! Layers are ordered by insertion time; later-added layers have higher priority.
//! Within a layer, the last binding for the same `(sequence, context)` wins.
//!
//! A binding whose [`ActionId`] is the null sentinel (empty string) in a
//! higher layer **suppresses** the lower-layer binding.
//!
//! # Context matching
//!
//! A binding is active when its context is present in the caller's active
//! context stack, or when it equals `"Global"`.

use crate::{
    action::ActionId, binding::Binding, chord::ChordSequence, context::context_matches,
    defaults::default_bindings, loader::load_keymap_toml,
};
use atlas_core::Result;

/// The outcome of a keymap resolution attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum ResolveResult {
    /// The sequence matched a binding; here is the action.
    Matched(ActionId),
    /// The sequence is a valid prefix of at least one longer binding.
    Prefix,
    /// No binding matches and no binding has this as a prefix.
    NoMatch,
}

/// A named layer of bindings.
#[derive(Debug, Clone)]
struct Layer {
    name: String,
    bindings: Vec<Binding>,
}

/// A layered keymap that maps chord sequences in a context to action identifiers.
///
/// The keymap maintains an ordered list of named layers. Later layers have higher
/// priority. Within a layer the last binding for the same `(sequence, context)`
/// wins.
pub struct Keymap {
    /// Ordered lowest-priority first.
    layers: Vec<Layer>,
}

impl Keymap {
    /// Create an empty keymap with no layers.
    pub fn empty() -> Self {
        Self { layers: Vec::new() }
    }

    /// Create a keymap pre-populated with the default bindings layer.
    pub fn with_defaults() -> Self {
        let mut km = Self::empty();
        km.add_layer("default", default_bindings());
        km
    }

    /// Add (or replace) a named layer. Later calls = higher priority.
    ///
    /// If a layer with `name` already exists it is replaced in-place,
    /// preserving its original priority position.
    pub fn add_layer(&mut self, name: &str, bindings: Vec<Binding>) {
        if let Some(layer) = self.layers.iter_mut().find(|layer| layer.name == name) {
            layer.bindings = bindings;
        } else {
            self.layers.push(Layer {
                name: name.to_owned(),
                bindings,
            });
        }
    }

    /// Remove a named layer. No-op if the layer does not exist.
    pub fn remove_layer(&mut self, name: &str) {
        self.layers.retain(|layer| layer.name != name);
    }

    /// Return the names of all layers in priority order (lowest first).
    pub fn layers(&self) -> Vec<String> {
        self.layers.iter().map(|layer| layer.name.clone()).collect()
    }

    /// Resolve a chord sequence against the active context stack.
    ///
    /// Higher layers override lower layers; within a layer the last matching
    /// binding wins. A suppression binding (null `ActionId`) in a higher layer
    /// prevents lower-layer actions from firing.
    pub fn resolve(&self, seq: &ChordSequence, contexts: &[String]) -> ResolveResult {
        for layer in self.layers.iter().rev() {
            let matched = layer.bindings.iter().rev().find(|binding| {
                binding.sequence == *seq && context_matches(&binding.context, contexts)
            });

            if let Some(binding) = matched {
                if binding.is_suppression() {
                    return ResolveResult::NoMatch;
                }
                return ResolveResult::Matched(binding.action.clone());
            }
        }

        let is_prefix = self.layers.iter().any(|layer| {
            layer.bindings.iter().any(|binding| {
                context_matches(&binding.context, contexts)
                    && binding.sequence.len() > seq.len()
                    && binding.sequence.0.starts_with(&seq.0)
            })
        });

        if is_prefix {
            ResolveResult::Prefix
        } else {
            ResolveResult::NoMatch
        }
    }

    /// All bindings reachable in the given context stack, in priority order
    /// (highest-priority first). Suppressed bindings are excluded.
    pub fn bindings_for_contexts(&self, contexts: &[String]) -> Vec<&Binding> {
        use std::collections::HashSet;

        let mut seen: HashSet<(&ChordSequence, &str)> = HashSet::new();
        let mut result = Vec::new();

        for layer in self.layers.iter().rev() {
            for binding in layer.bindings.iter().rev() {
                if !context_matches(&binding.context, contexts) {
                    continue;
                }
                let key = (&binding.sequence, binding.context.as_str());
                if !seen.insert(key) || binding.is_suppression() {
                    continue;
                }
                result.push(binding);
            }
        }

        result
    }

    /// Return the highest-priority chord sequence bound to `action_id` in
    /// any of the given contexts, or `None` if no binding exists. Useful for
    /// rendering "action → shortcut" hints (e.g. the bottom shortcut footer).
    #[must_use]
    pub fn chord_for_action(
        &self,
        action_id: &ActionId,
        contexts: &[String],
    ) -> Option<ChordSequence> {
        self.bindings_for_contexts(contexts)
            .into_iter()
            .find(|binding| &binding.action == action_id)
            .map(|binding| binding.sequence.clone())
    }

    /// Apply the `user` layer from a parsed list of bindings.
    pub fn set_user_bindings(&mut self, bindings: Vec<Binding>) {
        self.add_layer("user", bindings);
    }

    /// Load user bindings from TOML text and apply as the `user` layer.
    pub fn apply_user_toml(&mut self, toml_text: &str) -> Result<()> {
        let bindings = load_keymap_toml(toml_text)?;
        self.set_user_bindings(bindings);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(s: &str) -> ChordSequence {
        ChordSequence::from_str(s).unwrap()
    }

    fn binding(sequence: &str, context: &str, action: &str) -> Binding {
        Binding {
            sequence: seq(sequence),
            context: context.to_owned(),
            action: ActionId::new(action),
        }
    }

    fn suppress(sequence: &str, context: &str) -> Binding {
        Binding {
            sequence: seq(sequence),
            context: context.to_owned(),
            action: ActionId::null(),
        }
    }

    #[test]
    fn test_basic_resolution() {
        let mut km = Keymap::empty();
        km.add_layer("default", vec![binding("j", "Pane", "pane::MoveDown")]);
        let contexts = vec!["Pane".to_owned()];
        assert_eq!(
            km.resolve(&seq("j"), &contexts),
            ResolveResult::Matched(ActionId::new("pane::MoveDown"))
        );
    }

    #[test]
    fn test_user_overrides_default() {
        let mut km = Keymap::empty();
        km.add_layer("default", vec![binding("j", "Pane", "pane::MoveDown")]);
        km.add_layer("user", vec![binding("j", "Pane", "custom::Action")]);
        let contexts = vec!["Pane".to_owned()];
        assert_eq!(
            km.resolve(&seq("j"), &contexts),
            ResolveResult::Matched(ActionId::new("custom::Action"))
        );
    }

    #[test]
    fn test_suppression() {
        let mut km = Keymap::empty();
        km.add_layer("default", vec![binding("j", "Pane", "pane::MoveDown")]);
        km.add_layer("user", vec![suppress("j", "Pane")]);
        let contexts = vec!["Pane".to_owned()];
        assert_eq!(km.resolve(&seq("j"), &contexts), ResolveResult::NoMatch);
    }

    #[test]
    fn test_prefix_detection() {
        let mut km = Keymap::empty();
        km.add_layer("default", vec![binding("g g", "Pane", "pane::MoveToTop")]);
        let contexts = vec!["Pane".to_owned()];
        assert_eq!(km.resolve(&seq("g"), &contexts), ResolveResult::Prefix);
        assert_eq!(
            km.resolve(&seq("g g"), &contexts),
            ResolveResult::Matched(ActionId::new("pane::MoveToTop"))
        );
    }

    #[test]
    fn test_no_match() {
        let km = Keymap::empty();
        assert_eq!(km.resolve(&seq("z"), &[]), ResolveResult::NoMatch);
    }

    #[test]
    fn test_context_filter() {
        let mut km = Keymap::empty();
        km.add_layer("default", vec![binding("j", "Pane", "pane::MoveDown")]);
        assert_eq!(
            km.resolve(&seq("j"), &["Palette".into()]),
            ResolveResult::NoMatch
        );
        assert_eq!(
            km.resolve(&seq("j"), &["Pane".into()]),
            ResolveResult::Matched(ActionId::new("pane::MoveDown"))
        );
    }

    #[test]
    fn test_global_context() {
        let mut km = Keymap::empty();
        km.add_layer(
            "default",
            vec![binding("cmd-shift-p", "Global", "command_palette::Toggle")],
        );
        assert_eq!(
            km.resolve(&seq("cmd-shift-p"), &[]),
            ResolveResult::Matched(ActionId::new("command_palette::Toggle"))
        );
        assert_eq!(
            km.resolve(&seq("cmd-shift-p"), &["Pane".into()]),
            ResolveResult::Matched(ActionId::new("command_palette::Toggle"))
        );
    }

    #[test]
    fn test_layer_replace() {
        let mut km = Keymap::empty();
        km.add_layer("default", vec![binding("j", "Pane", "old::Action")]);
        km.add_layer("default", vec![binding("j", "Pane", "new::Action")]);
        assert_eq!(km.layers().len(), 1);
        assert_eq!(
            km.resolve(&seq("j"), &["Pane".into()]),
            ResolveResult::Matched(ActionId::new("new::Action"))
        );
    }

    #[test]
    fn test_with_defaults_parses() {
        let km = Keymap::with_defaults();
        assert!(km.layers().contains(&"default".to_owned()));
    }
}
