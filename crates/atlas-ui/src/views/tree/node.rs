//! Internal tree node type.
//!
//! Stores the state of a single filesystem entry in the tree, keyed by its
//! canonical path inside the [`crate::views::tree::TreeController`] node map.

use std::path::PathBuf;

/// An internal node in the lazy-expanded directory tree.
///
/// The tree uses a `HashMap<PathBuf, Node>` rather than a recursive structure
/// so that flattening into a visible list stays O(visible) regardless of the
/// total node count.
#[derive(Debug, Clone)]
pub struct Node {
    /// Canonical filesystem path for this entry.
    pub path: PathBuf,
    /// `true` when the entry is a directory (expandable).
    pub is_dir: bool,
    /// `true` when the entry is a symbolic link.
    pub is_symlink: bool,
    /// `true` when the symlink target does not exist.
    pub is_broken_symlink: bool,
    /// Display name (last component of `path`).
    pub name: String,
    /// Whether the entry is normally hidden (dot-file or system-hidden attribute).
    pub is_hidden: bool,
    /// Whether the node is currently expanded in the tree.
    pub expanded: bool,
    /// Whether children have been fetched at least once.
    pub loaded: bool,
    /// Whether a background fetch is currently in-flight for this node.
    pub loading: bool,
    /// Ordered child paths (empty until loaded; dirs first, names ascending).
    pub children: Vec<PathBuf>,
}

impl Node {
    /// Construct a stub node with no children loaded.
    #[must_use]
    pub fn stub(
        path: PathBuf,
        is_dir: bool,
        is_symlink: bool,
        is_broken_symlink: bool,
        name: String,
        is_hidden: bool,
    ) -> Self {
        Self {
            path,
            is_dir,
            is_symlink,
            is_broken_symlink,
            name,
            is_hidden,
            expanded: false,
            loaded: false,
            loading: false,
            children: Vec::new(),
        }
    }

    /// Returns `true` if this node can have children (is a directory).
    #[must_use]
    pub fn is_expandable(&self) -> bool {
        self.is_dir
    }
}
