//! Action identifiers and registry for command-palette discovery.

use ahash::AHashMap;

/// A stable, namespaced string identifier for a user-facing action.
///
/// Convention: `"namespace::PascalCase"`, e.g. `"command_palette::Toggle"`.
///
/// The empty string (`""`) is the **null sentinel** used to suppress a
/// lower-layer binding (see [`crate::Keymap`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ActionId(pub String);

impl ActionId {
    /// Construct a new [`ActionId`].
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The null sentinel that suppresses lower-layer bindings.
    pub fn null() -> Self {
        Self(String::new())
    }

    /// Returns `true` if this is the null sentinel.
    pub fn is_null(&self) -> bool {
        self.0.is_empty()
    }

    /// The underlying string identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ActionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ActionId {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for ActionId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Metadata describing a user-facing action.
#[derive(Clone, Debug)]
pub struct ActionMeta {
    /// Stable action identifier.
    pub id: ActionId,
    /// Human-readable label shown in the command palette.
    pub title: String,
    /// Optional longer description.
    pub description: Option<String>,
    /// Contexts in which this action is valid (e.g. `["Pane", "Global"]`).
    pub contexts: Vec<String>,
}

/// A registry of all known actions, keyed by [`ActionId`].
///
/// Used by the command palette to enumerate available commands.
#[derive(Default)]
pub struct ActionRegistry {
    actions: AHashMap<ActionId, ActionMeta>,
}

impl ActionRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an action. Replaces any existing registration with the same id.
    pub fn register(&mut self, meta: ActionMeta) {
        self.actions.insert(meta.id.clone(), meta);
    }

    /// Look up an action by id.
    pub fn get(&self, id: &ActionId) -> Option<&ActionMeta> {
        self.actions.get(id)
    }

    /// Iterate over all registered actions (order unspecified).
    pub fn iter(&self) -> impl Iterator<Item = &ActionMeta> {
        self.actions.values()
    }

    /// Number of registered actions.
    pub fn len(&self) -> usize {
        self.actions.len()
    }

    /// Returns `true` if no actions are registered.
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_id_null() {
        let null = ActionId::null();
        assert!(null.is_null());
        assert!(!ActionId::new("foo::Bar").is_null());
    }

    #[test]
    fn test_registry_register_get() {
        let mut reg = ActionRegistry::new();
        reg.register(ActionMeta {
            id: ActionId::new("test::Action"),
            title: "Test Action".into(),
            description: None,
            contexts: vec!["Global".into()],
        });
        assert_eq!(reg.len(), 1);
        let meta = reg.get(&ActionId::new("test::Action")).unwrap();
        assert_eq!(meta.title, "Test Action");
    }

    #[test]
    fn test_registry_empty() {
        let reg = ActionRegistry::new();
        assert!(reg.is_empty());
    }
}
