//! Action dispatcher — resolves chord sequences to registered Rust handlers.
//!
//! The [`Dispatcher`] bridges the keymap (chord→[`ActionId`]) with the
//! application side (action-id→handler). It has two entry points:
//!
//! - [`Dispatcher::handle_key`] — resolves a chord sequence through the
//!   keymap and invokes a registered handler. Used by the Slint `FocusScope`
//!   once the Slint-side key-routing lands (tracked as `gap-keymap-slint-routing`).
//! - [`Dispatcher::dispatch_action`] — invokes a registered handler by its
//!   [`ActionId`] directly. Used by the command palette `on_dispatch` callback.

use std::sync::Arc;

use ahash::AHashMap;
use parking_lot::RwLock;

use crate::{ActionId, ChordSequence, Keymap, ResolveResult};

type Handler = Box<dyn Fn() + Send + Sync>;

/// Routes [`ActionId`]s to registered `Fn()` handlers.
///
/// Thread-safe; cheaply cloned by sharing the inner [`Arc`].
///
/// # Example
///
/// ```rust
/// use atlas_keymap::{Dispatcher, Keymap};
///
/// let d = Dispatcher::new(Keymap::with_defaults());
/// d.register("command_palette::Toggle", || println!("palette!"));
/// let _ = d.dispatch_action(&"command_palette::Toggle".into());
/// ```
pub struct Dispatcher {
    keymap: RwLock<Keymap>,
    handlers: RwLock<AHashMap<ActionId, Handler>>,
}

impl Dispatcher {
    /// Create a new dispatcher wrapping `keymap`.
    #[must_use]
    pub fn new(keymap: Keymap) -> Arc<Self> {
        Arc::new(Self {
            keymap: RwLock::new(keymap),
            handlers: RwLock::new(AHashMap::new()),
        })
    }

    /// Replace the active keymap (e.g., after a config hot-reload).
    pub fn set_keymap(&self, keymap: Keymap) {
        *self.keymap.write() = keymap;
    }

    /// Register a handler for `id`. Replaces any existing handler.
    pub fn register(&self, id: impl Into<ActionId>, handler: impl Fn() + Send + Sync + 'static) {
        self.handlers.write().insert(id.into(), Box::new(handler));
    }

    /// Resolve `seq` in the given `contexts` through the keymap and invoke the
    /// corresponding handler if one is registered. Returns `true` if a handler
    /// was called.
    pub fn handle_key(&self, seq: &ChordSequence, contexts: &[String]) -> bool {
        let action = match self.keymap.read().resolve(seq, contexts) {
            ResolveResult::Matched(id) => id,
            _ => return false,
        };
        self.dispatch_action(&action)
    }

    /// Return the number of registered handlers.
    #[must_use]
    pub fn handler_count(&self) -> usize {
        self.handlers.read().len()
    }

    /// Directly invoke the registered handler for `id`. Returns `true` if a
    /// handler was registered and has been called.
    pub fn dispatch_action(&self, id: &ActionId) -> bool {
        if let Some(handler) = self.handlers.read().get(id) {
            handler();
            true
        } else {
            tracing::debug!(%id, "dispatcher: no handler registered for action");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn dispatch_action_calls_handler() {
        let d = Dispatcher::new(Keymap::with_defaults());
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = Arc::clone(&count);
        d.register("test::Action", move || {
            count2.fetch_add(1, Ordering::SeqCst);
        });
        assert!(d.dispatch_action(&ActionId::new("test::Action")));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dispatch_action_returns_false_for_unknown_id() {
        let d = Dispatcher::new(Keymap::with_defaults());
        assert!(!d.dispatch_action(&ActionId::new("unknown::Action")));
    }

    #[test]
    fn register_replaces_existing_handler() {
        let d = Dispatcher::new(Keymap::with_defaults());
        let count = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::clone(&count);
        let c3 = Arc::clone(&count);
        d.register("test::Replace", move || {
            c2.fetch_add(1, Ordering::SeqCst);
        });
        d.register("test::Replace", move || {
            c3.fetch_add(10, Ordering::SeqCst);
        });
        assert!(d.dispatch_action(&ActionId::new("test::Replace")));
        // Second handler replaced the first; value should be 10, not 1.
        assert_eq!(count.load(Ordering::SeqCst), 10);
    }

    #[test]
    fn handle_key_resolves_and_dispatches() {
        let km = Keymap::with_defaults();
        let d = Dispatcher::new(km);
        let called = Arc::new(AtomicUsize::new(0));
        let called2 = Arc::clone(&called);
        d.register("command_palette::Toggle", move || {
            called2.fetch_add(1, Ordering::SeqCst);
        });
        let seq = crate::ChordSequence::from_str("cmd-shift-p").unwrap();
        assert!(d.handle_key(&seq, &[]));
        assert_eq!(called.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn handle_key_returns_false_when_no_match() {
        let d = Dispatcher::new(Keymap::with_defaults());
        let seq = crate::ChordSequence::from_str("f12").unwrap();
        assert!(!d.handle_key(&seq, &[]));
    }

    #[test]
    fn set_keymap_replaces_bindings() {
        let d = Dispatcher::new(Keymap::empty());
        let count = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::clone(&count);
        d.register("test::NewAction", move || {
            c2.fetch_add(1, Ordering::SeqCst);
        });
        // No bindings yet — should not match.
        let seq = crate::ChordSequence::from_str("x").unwrap();
        assert!(!d.handle_key(&seq, &["Global".to_owned()]));

        // Install a keymap that binds x → test::NewAction in Global context.
        let mut km = Keymap::empty();
        km.add_layer(
            "test",
            vec![crate::Binding {
                sequence: seq.clone(),
                context: "Global".to_owned(),
                action: ActionId::new("test::NewAction"),
            }],
        );
        d.set_keymap(km);
        assert!(d.handle_key(&seq, &["Global".to_owned()]));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
