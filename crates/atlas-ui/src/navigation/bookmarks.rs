//! Named filesystem bookmarks backed by `atlas_config::Bookmark`.

use std::path::PathBuf;

use atlas_core::path::expand_tilde;
use parking_lot::RwLock;

/// A named sidebar bookmark.
#[derive(Debug, Clone)]
pub struct Bookmark {
    /// Human-readable label shown in the sidebar.
    pub name: String,
    /// Absolute path (tilde-expanded when resolved via [`BookmarkStore::resolve`]).
    pub path: PathBuf,
}

/// Thread-safe store of named filesystem bookmarks.
///
/// Backed by a `RwLock<Vec<Bookmark>>` for concurrent reads. Load the initial
/// set from config with [`BookmarkStore::from_config`]; sync back to config
/// with [`BookmarkStore::to_config`].
pub struct BookmarkStore {
    inner: RwLock<Vec<Bookmark>>,
}

impl BookmarkStore {
    /// Construct a store from a `atlas_config::Bookmark` slice.
    #[must_use]
    pub fn from_config(bookmarks: &[atlas_config::Bookmark]) -> Self {
        let inner = bookmarks
            .iter()
            .map(|b| Bookmark {
                name: b.name.clone(),
                path: b.path.clone(),
            })
            .collect();
        Self {
            inner: RwLock::new(inner),
        }
    }

    /// Return a snapshot of all bookmarks.
    #[must_use]
    pub fn list(&self) -> Vec<Bookmark> {
        self.inner.read().clone()
    }

    /// Add a bookmark, replacing any existing bookmark with the same name.
    pub fn add(&self, bookmark: Bookmark) {
        let mut guard = self.inner.write();
        if let Some(existing) = guard.iter_mut().find(|b| b.name == bookmark.name) {
            *existing = bookmark;
        } else {
            guard.push(bookmark);
        }
    }

    /// Remove the bookmark with the given `name`.
    ///
    /// Returns `true` if a bookmark was found and removed.
    pub fn remove(&self, name: &str) -> bool {
        let mut guard = self.inner.write();
        let before = guard.len();
        guard.retain(|b| b.name != name);
        guard.len() < before
    }

    /// Resolve a bookmark name to its tilde-expanded path.
    ///
    /// Returns `None` if no bookmark with the given name exists.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<PathBuf> {
        self.inner
            .read()
            .iter()
            .find(|b| b.name == name)
            .map(|b| expand_tilde(&b.path))
    }

    /// Serialize to `atlas_config::Bookmark` entries for saving to config.
    #[must_use]
    pub fn to_config(&self) -> Vec<atlas_config::Bookmark> {
        self.inner
            .read()
            .iter()
            .map(|b| atlas_config::Bookmark {
                name: b.name.clone(),
                path: b.path.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> BookmarkStore {
        BookmarkStore::from_config(&[])
    }

    #[test]
    fn add_and_list() {
        let store = make_store();
        store.add(Bookmark {
            name: "home".to_owned(),
            path: PathBuf::from("/home/alice"),
        });
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "home");
    }

    #[test]
    fn add_replaces_same_name() {
        let store = make_store();
        store.add(Bookmark {
            name: "docs".to_owned(),
            path: PathBuf::from("/old"),
        });
        store.add(Bookmark {
            name: "docs".to_owned(),
            path: PathBuf::from("/new"),
        });
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, PathBuf::from("/new"));
    }

    #[test]
    fn remove_existing() {
        let store = make_store();
        store.add(Bookmark {
            name: "tmp".to_owned(),
            path: PathBuf::from("/tmp"),
        });
        assert!(store.remove("tmp"));
        assert!(store.list().is_empty());
    }

    #[test]
    fn remove_missing_returns_false() {
        let store = make_store();
        assert!(!store.remove("nonexistent"));
    }

    #[test]
    fn resolve_returns_path() {
        let store = make_store();
        store.add(Bookmark {
            name: "src".to_owned(),
            path: PathBuf::from("/usr/src"),
        });
        assert_eq!(store.resolve("src"), Some(PathBuf::from("/usr/src")));
    }

    #[test]
    fn resolve_expands_tilde() {
        let store = make_store();
        store.add(Bookmark {
            name: "home".to_owned(),
            path: PathBuf::from("~/Documents"),
        });
        let resolved = store.resolve("home").expect("bookmark should resolve");
        assert!(!resolved.starts_with("~"));
    }

    #[test]
    fn resolve_missing_is_none() {
        let store = make_store();
        assert!(store.resolve("missing").is_none());
    }

    #[test]
    fn to_config_roundtrips() {
        let store = make_store();
        store.add(Bookmark {
            name: "x".to_owned(),
            path: PathBuf::from("/x"),
        });
        let cfg = store.to_config();
        assert_eq!(cfg.len(), 1);
        assert_eq!(cfg[0].name, "x");
    }
}
