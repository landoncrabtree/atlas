//! Lazy-expanded directory Tree view — controller and internal node type.
//!
//! The [`TreeController`] maintains a `HashMap<PathBuf, Node>` as its internal
//! representation. The currently-visible flat list is built by DFS-walking
//! expanded nodes on every state change, then pushed to the Slint window via
//! [`slint::invoke_from_event_loop`].

pub mod controller;
pub mod node;

pub use controller::TreeController;
pub use node::Node;
