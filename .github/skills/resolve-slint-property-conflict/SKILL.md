---
name: resolve-slint-property-conflict
description: Diagnosis and fix pattern for Slint `changed <property>` handlers that never fire. Use when a Slint property visibly updates but its `changed` handler does not run, or when Rust-side state fails to mirror a Slint property.
---

Slint's `changed <property>` handler fires only when the property is **declared on the element that owns the handler**. When a property is *forwarded* through a chain of components (child exposes `input-focused`; parent binds to child; grandparent reads through the parent), only the leaf element's `changed` handler sees the update — the intermediate declarations do not automatically trigger `changed` upstream.

This regularly produces a bug shape like: "the UI updates correctly, but my Rust state never gets the callback."

## Symptom checklist

- A Slint property visibly reflects the correct value in the UI (bound expressions evaluate to the new value, conditional elements show/hide correctly).
- The `changed <property>` handler in one of the ancestors does not fire.
- Rust-side state that was supposed to be updated via a callback in that handler stays stale.
- The chain involves component composition (child → parent → root).

## The fix pattern

Add a **local mirror property** on the element that owns the `changed` handler, bind it from the source property, and put the `changed` handler on the *mirror*. Slint fires `changed` reliably on locally-declared properties.

Reference implementations in the tree:

- Per-pane text-input focus is bubbled up to the root `FocusScope` via `text-focus-pane-id`. Each `Pane` element declares its own `text-input-focused` bool and the root has a `changed text-input-focused => { … }` handler on the `Pane` (not on some outer wrapper).
- The Connect modal exposes `input-focused: bool` and the parent `atlas.slint` mirrors it into a root-level `connect-modal-input-focused` bool via `changed input-focused => { root.connect-modal-input-focused = self.input-focused; }`. That mirror then joins the disjunction that drives `keymap-bypass-active`.

## When to reach for this pattern

- A modal's TextInput focus state needs to reach the root's chord-routing bypass logic.
- A view's per-row selection state needs to feed a shell-level property.
- A pane's per-tab active index needs to bubble up to workspace-level saved-state.

## Anti-pattern

Adding `changed` handlers to alias/two-way bindings and hoping they fire on the "far side" of the chain. They will not. Use the mirror pattern.

## Cross-references

- [`ui-composition.instructions.md`](../../instructions/ui-composition.instructions.md) §5 — the ONE canonical modal chord-routing pattern, which is where this shape appears most often.
- `assets/ui/atlas.slint` — search for `changed text-input-focused` and `changed input-focused` for two working examples.
