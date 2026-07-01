//! [`TreeController`] — lazy-expanded directory tree view controller.
//!
//! Maintains a `HashMap<PathBuf, Node>` as the internal representation of the
//! tree. The visible flat list pushed to Slint is built by DFS-walking expanded
//! nodes; this is cheap because only visible nodes are visited.
//!
//! Expanding a node that has not been loaded yet spawns a background thread
//! that calls [`atlas_fs::list_directory`] (one level at a time) and writes
//! the results back. Subsequent expands/collapses of already-loaded nodes are
//! instant (no I/O).

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

use ahash::AHashMap;
use atlas_fs::{sort_in_place, Entry, EntryKind, ListEvent, ListRequest, SortSpec};
use parking_lot::{Mutex, RwLock};
use slint::{ModelRc, SharedString, VecModel};

use crate::{
    actions::{ActionSink, UiAction},
    AtlasWindow, EntryRowItem, TreeNode,
};

use super::node::Node;

/// Sentinel value meaning "no focused / selected row".
const NO_IDX: usize = usize::MAX;

/// Drives the Slint Tree view for one pane.
///
/// Construct with [`TreeController::new`] (window can be attached later via
/// [`TreeController::attach_window`]). Navigate by calling
/// [`TreeController::set_root`]; expand/collapse individual nodes with
/// [`TreeController::expand`] / [`TreeController::collapse`] /
/// [`TreeController::toggle`].
///
/// The controller is `Send + Sync` and safe to share across threads via `Arc`.
pub struct TreeController {
    /// Pane index (0 or 1) this controller drives.
    pane: usize,
    /// Root path of the current tree.
    root: RwLock<Option<PathBuf>>,
    /// All known nodes keyed by canonical path.
    nodes: RwLock<AHashMap<PathBuf, Node>>,
    /// Focused row in the current visible list (`NO_IDX` when unset).
    focused: AtomicUsize,
    /// Selected row in the current visible list (`NO_IDX` when unset).
    selected: AtomicUsize,
    /// Whether hidden entries appear in the visible list.
    show_hidden: AtomicBool,
    /// Weak reference to the Slint window (absent during tests).
    window: RwLock<Option<slint::Weak<AtlasWindow>>>,
    /// Shared action sink for navigation.
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
}

impl TreeController {
    /// Create a new controller for `pane`.
    ///
    /// Call [`attach_window`](Self::attach_window) before triggering any
    /// navigation to enable UI pushes.
    #[must_use]
    pub fn new(pane: usize, actions: Arc<Mutex<Box<dyn ActionSink>>>) -> Arc<Self> {
        Arc::new(Self {
            pane,
            root: RwLock::new(None),
            nodes: RwLock::new(AHashMap::new()),
            focused: AtomicUsize::new(NO_IDX),
            selected: AtomicUsize::new(NO_IDX),
            show_hidden: AtomicBool::new(false),
            window: RwLock::new(None),
            actions,
        })
    }

    /// Attach the Slint window handle so property pushes reach the UI.
    pub fn attach_window(&self, window: slint::Weak<AtlasWindow>) {
        *self.window.write() = Some(window);
    }

    /// Reset the tree to `path` as the new root and kick off the first-level load.
    ///
    /// The root is immediately marked `expanded = true`. A background thread
    /// loads its children and pushes the updated model when done.
    pub fn set_root(self: &Arc<Self>, path: PathBuf) {
        {
            let mut root_guard = self.root.write();
            let mut nodes = self.nodes.write();
            *root_guard = Some(path.clone());
            nodes.clear();
            self.focused.store(NO_IDX, Ordering::Relaxed);
            self.selected.store(NO_IDX, Ordering::Relaxed);

            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());

            nodes.insert(
                path.clone(),
                Node {
                    path: path.clone(),
                    is_dir: true,
                    is_symlink: false,
                    is_broken_symlink: false,
                    name,
                    is_hidden: false,
                    expanded: true,
                    loaded: false,
                    loading: false,
                    children: Vec::new(),
                },
            );
        }

        self.push_visible_nodes_to_ui();
        self.load_children(path);
    }

    /// Toggle visibility of hidden entries and rebuild the visible list.
    pub fn set_show_hidden(&self, show: bool) {
        self.show_hidden.store(show, Ordering::Relaxed);
        self.push_visible_nodes_to_ui();
    }

    /// Expand the node at `path`.
    ///
    /// If already loaded, just flips the `expanded` flag. Otherwise, marks the
    /// node as `loading` and spawns a background thread to fetch children.
    pub fn expand(self: &Arc<Self>, path: &Path) {
        let need_load = {
            let mut nodes = self.nodes.write();
            let Some(node) = nodes.get_mut(path) else {
                return;
            };
            node.expanded = true;
            if !node.loaded && !node.loading {
                node.loading = true;
                true
            } else {
                false
            }
        };

        self.push_visible_nodes_to_ui();
        if need_load {
            self.load_children(path.to_path_buf());
        }
    }

    /// Collapse the node at `path`.
    ///
    /// Children remain in the map so a subsequent expand is instant.
    pub fn collapse(&self, path: &Path) {
        {
            let mut nodes = self.nodes.write();
            if let Some(node) = nodes.get_mut(path) {
                node.expanded = false;
            }
        }
        self.push_visible_nodes_to_ui();
    }

    /// Toggle expand/collapse for the node at `path`.
    pub fn toggle(self: &Arc<Self>, path: &Path) {
        let expanded = self.nodes.read().get(path).is_some_and(|n| n.expanded);
        if expanded {
            self.collapse(path);
        } else {
            self.expand(path);
        }
    }

    /// Update selection for the visible-list row at index `i`.
    ///
    /// Uses simple single-selection for tree MVP. `ctrl` toggles the current
    /// row. `shift` is accepted but treated as plain click for now.
    pub fn select_index(self: &Arc<Self>, i: usize, ctrl: bool, _shift: bool) {
        let visible_len = self.build_visible_nodes().len();
        if i >= visible_len {
            return;
        }
        if ctrl {
            let cur = self.selected.load(Ordering::Relaxed);
            self.selected
                .store(if cur == i { NO_IDX } else { i }, Ordering::Relaxed);
        } else {
            self.selected.store(i, Ordering::Relaxed);
        }
        self.focused.store(i, Ordering::Relaxed);
        self.push_visible_nodes_to_ui();
    }

    /// Navigate into (or open) the currently focused node.
    ///
    /// Directories dispatch a [`UiAction::Navigate`]; files are a no-op for MVP.
    pub fn activate_focused(self: &Arc<Self>) {
        let focused = self.focused.load(Ordering::Relaxed);
        if focused == NO_IDX {
            return;
        }
        let visible = self.build_visible_nodes();
        let Some(row) = visible.get(focused) else {
            return;
        };
        let path = PathBuf::from(row.node_id.as_str());
        if self.nodes.read().get(&path).is_some_and(|n| n.is_dir) {
            self.actions.lock().dispatch(UiAction::Navigate {
                pane: self.pane,
                path,
            });
        }
    }

    /// Move focus by `delta` rows, clamped to the valid range.
    pub fn move_focus(self: &Arc<Self>, delta: isize) {
        let visible_len = self.build_visible_nodes().len();
        if visible_len == 0 {
            return;
        }
        let current = self.focused.load(Ordering::Relaxed);
        let current_i = if current == NO_IDX {
            0_isize
        } else {
            current as isize
        };
        let next = (current_i + delta).clamp(0, (visible_len as isize) - 1) as usize;
        self.focused.store(next, Ordering::Relaxed);
        self.push_visible_nodes_to_ui();
    }

    /// Flatten the tree into a visible-node list by DFS from the root.
    ///
    /// Respects `expanded` / `collapsed` state and the `show_hidden` flag.
    pub(crate) fn build_visible_nodes(&self) -> Vec<TreeNode> {
        let root = self.root.read().clone();
        let Some(root_path) = root else {
            return vec![];
        };
        let nodes = self.nodes.read();
        let show_hidden = self.show_hidden.load(Ordering::Relaxed);

        let mut result = Vec::new();
        dfs(&nodes, &root_path, 0, show_hidden, &mut result);
        result
    }

    /// Spawn a background thread to fetch one level of children for `path`.
    fn load_children(self: &Arc<Self>, path: PathBuf) {
        let ctrl = Arc::clone(self);
        match std::thread::Builder::new()
            .name(format!("atlas-tree-p{}-load", self.pane))
            .spawn(move || ctrl.do_load_children(path))
        {
            Ok(_) => {}
            Err(error) => {
                tracing::error!(
                    pane = self.pane,
                    %error,
                    "failed to spawn tree load thread"
                );
                // Clear loading flag so expand can be retried.
                // (The path was moved, so nothing to do here.)
            }
        }
    }

    fn do_load_children(self: Arc<Self>, path: PathBuf) {
        let rx = atlas_fs::list_directory(ListRequest {
            path: path.clone(),
            follow_symlinks: false,
            // Always fetch all entries; `build_visible_nodes` filters hidden.
            include_hidden: true,
        });

        let mut all_entries: Vec<Entry> = Vec::new();
        for event in rx {
            match event {
                ListEvent::Batch(entries) => all_entries.extend(entries),
                ListEvent::Error {
                    path: err_path,
                    error,
                } => {
                    tracing::warn!(
                        pane = self.pane,
                        path = %err_path.display(),
                        %error,
                        "tree child load error"
                    );
                }
                ListEvent::Done => break,
            }
        }

        // Sort: dirs first, names ascending (natural, case-insensitive).
        let spec = SortSpec::default();
        sort_in_place(&mut all_entries, &spec);

        {
            let mut nodes = self.nodes.write();

            let child_paths: Vec<PathBuf> = all_entries.iter().map(|e| e.path.clone()).collect();

            if let Some(parent) = nodes.get_mut(&path) {
                parent.loaded = true;
                parent.loading = false;
                parent.children = child_paths;
            }

            // Insert stub nodes for each child (without overwriting existing ones
            // in case a child was already expanded).
            for entry in &all_entries {
                nodes.entry(entry.path.clone()).or_insert_with(|| {
                    let (is_symlink, is_broken_symlink) = match &entry.kind {
                        EntryKind::Symlink { broken, .. } => (true, *broken),
                        _ => (false, false),
                    };
                    Node::stub(
                        entry.path.clone(),
                        entry.kind.is_dir(),
                        is_symlink,
                        is_broken_symlink,
                        entry.name.clone(),
                        entry.metadata.is_hidden,
                    )
                });
            }
        }

        self.push_visible_nodes_to_ui();
    }

    fn push_visible_nodes_to_ui(&self) {
        let visible = self.build_visible_nodes();
        let focused = self.focused.load(Ordering::Relaxed);
        let selected = self.selected.load(Ordering::Relaxed);
        let focused_i32 = if focused == NO_IDX {
            -1_i32
        } else {
            focused as i32
        };
        let selected_i32 = if selected == NO_IDX {
            -1_i32
        } else {
            selected as i32
        };
        let pane = self.pane;
        let window_opt = self.window.read().clone();
        let Some(window) = window_opt else { return };

        let _ = slint::invoke_from_event_loop(move || {
            let Some(w) = window.upgrade() else { return };
            let model = ModelRc::new(VecModel::from(visible));
            if pane == 0 {
                w.set_pane0_tree_nodes(model);
                w.set_pane0_tree_focused_index(focused_i32);
                w.set_pane0_tree_selected_index(selected_i32);
            } else {
                w.set_pane1_tree_nodes(model);
                w.set_pane1_tree_focused_index(focused_i32);
                w.set_pane1_tree_selected_index(selected_i32);
            }
        });
    }

    /// Expose the internal node map for testing.
    #[cfg(test)]
    pub fn nodes_snapshot(&self) -> AHashMap<PathBuf, Node> {
        self.nodes.read().clone()
    }
}

// ── DFS helper ───────────────────────────────────────────────────────────────

/// Recursively collect visible nodes from `path` downward into `result`.
fn dfs(
    nodes: &AHashMap<PathBuf, Node>,
    path: &Path,
    depth: usize,
    show_hidden: bool,
    result: &mut Vec<TreeNode>,
) {
    let Some(node) = nodes.get(path) else { return };
    if !show_hidden && node.is_hidden {
        return;
    }

    let kind_icon = if node.is_symlink {
        "🔗"
    } else if node.is_dir {
        "📁"
    } else {
        "📄"
    };

    let entry = EntryRowItem {
        name: SharedString::from(node.name.as_str()),
        kind_icon: SharedString::from(kind_icon),
        size_text: SharedString::default(),
        modified_text: SharedString::default(),
        is_hidden: node.is_hidden,
        is_dir: node.is_dir,
        is_symlink: node.is_symlink,
        is_broken_symlink: node.is_broken_symlink,
    };

    result.push(TreeNode {
        name: SharedString::from(node.name.as_str()),
        entry,
        depth: depth as i32,
        is_expandable: node.is_expandable(),
        is_expanded: node.expanded,
        is_loading: node.loading,
        node_id: SharedString::from(node.path.to_string_lossy().as_ref()),
    });

    if node.expanded {
        let children = node.children.clone();
        for child_path in &children {
            dfs(nodes, child_path, depth + 1, show_hidden, result);
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    struct NoopSink;
    impl ActionSink for NoopSink {
        fn dispatch(&mut self, _: UiAction) {}
    }

    fn make_controller() -> Arc<TreeController> {
        let actions: Arc<Mutex<Box<dyn ActionSink>>> = Arc::new(Mutex::new(Box::new(NoopSink)));
        TreeController::new(0, actions)
    }

    fn wait_for<F: Fn() -> bool>(f: F, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn make_temp_tree() -> TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("sub_a")).unwrap();
        std::fs::create_dir(dir.path().join("sub_b")).unwrap();
        std::fs::write(dir.path().join("file.txt"), b"hello").unwrap();
        std::fs::write(dir.path().join(".hidden"), b"").unwrap();
        std::fs::create_dir(dir.path().join("sub_a").join("grandchild")).unwrap();
        dir
    }

    #[test]
    fn set_root_creates_root_node() {
        let ctrl = make_controller();
        let dir = make_temp_tree();
        ctrl.set_root(dir.path().to_path_buf());

        let snap = ctrl.nodes_snapshot();
        assert!(snap.contains_key(dir.path()), "root node must exist");
        assert!(
            snap[dir.path()].expanded,
            "root is auto-expanded on set_root"
        );
    }

    #[test]
    fn set_root_loads_children() {
        let ctrl = make_controller();
        let dir = make_temp_tree();
        ctrl.set_root(dir.path().to_path_buf());

        let loaded = wait_for(
            || ctrl.nodes_snapshot()[dir.path()].loaded,
            Duration::from_secs(5),
        );
        assert!(loaded, "root must be loaded within timeout");

        let snap = ctrl.nodes_snapshot();
        // sub_a, sub_b, file.txt, .hidden should all be in map
        let child_count = snap
            .values()
            .filter(|n| n.path.parent() == Some(dir.path()))
            .count();
        assert!(
            child_count >= 3,
            "at least 3 children expected, got {child_count}"
        );
    }

    #[test]
    fn expand_subdir_loads_grandchildren() {
        let ctrl = make_controller();
        let dir = make_temp_tree();
        ctrl.set_root(dir.path().to_path_buf());

        // Wait for root to load first.
        wait_for(
            || ctrl.nodes_snapshot()[dir.path()].loaded,
            Duration::from_secs(5),
        );

        let sub_a = dir.path().join("sub_a");
        ctrl.expand(&sub_a);

        let loaded = wait_for(
            || ctrl.nodes_snapshot().get(&sub_a).is_some_and(|n| n.loaded),
            Duration::from_secs(5),
        );
        assert!(loaded, "sub_a must be loaded after expand");

        let snap = ctrl.nodes_snapshot();
        let grandchild = sub_a.join("grandchild");
        assert!(
            snap.contains_key(&grandchild),
            "grandchild must appear in node map"
        );
    }

    #[test]
    fn collapse_hides_children_but_keeps_in_map() {
        let ctrl = make_controller();
        let dir = make_temp_tree();
        ctrl.set_root(dir.path().to_path_buf());

        wait_for(
            || ctrl.nodes_snapshot()[dir.path()].loaded,
            Duration::from_secs(5),
        );
        let sub_a = dir.path().join("sub_a");
        ctrl.expand(&sub_a);
        wait_for(
            || ctrl.nodes_snapshot().get(&sub_a).is_some_and(|n| n.loaded),
            Duration::from_secs(5),
        );

        ctrl.collapse(&sub_a);

        let snap = ctrl.nodes_snapshot();
        assert!(!snap[&sub_a].expanded, "sub_a must be collapsed");

        let grandchild = sub_a.join("grandchild");
        assert!(
            snap.contains_key(&grandchild),
            "grandchild node must remain in map after collapse"
        );

        // Visible nodes should NOT include grandchild.
        let visible = ctrl.build_visible_nodes();
        let has_grand = visible
            .iter()
            .any(|n| n.node_id.as_str() == grandchild.to_string_lossy());
        assert!(
            !has_grand,
            "grandchild must not appear in visible list when sub_a is collapsed"
        );
    }

    #[test]
    fn build_visible_nodes_depth_ordering() {
        let ctrl = make_controller();
        let dir = make_temp_tree();
        ctrl.set_root(dir.path().to_path_buf());

        wait_for(
            || ctrl.nodes_snapshot()[dir.path()].loaded,
            Duration::from_secs(5),
        );

        let visible = ctrl.build_visible_nodes();
        // Root must be the first node at depth 0.
        assert!(!visible.is_empty());
        assert_eq!(visible[0].depth, 0, "root must be at depth 0");

        // All children of root must be at depth 1.
        for node in visible.iter().skip(1) {
            assert_eq!(node.depth, 1, "immediate children at depth 1");
        }
    }

    #[test]
    fn show_hidden_false_filters_dotfiles() {
        let ctrl = make_controller();
        let dir = make_temp_tree();
        ctrl.set_show_hidden(false);
        ctrl.set_root(dir.path().to_path_buf());

        wait_for(
            || ctrl.nodes_snapshot()[dir.path()].loaded,
            Duration::from_secs(5),
        );

        let visible = ctrl.build_visible_nodes();
        // Use the `is_hidden` flag, not the name string — the temp root itself
        // may have a dot-prefixed name on some platforms.
        let has_hidden = visible.iter().any(|n| n.entry.is_hidden);
        assert!(
            !has_hidden,
            ".hidden must not appear when show_hidden = false"
        );
    }

    #[test]
    fn show_hidden_true_shows_dotfiles() {
        let ctrl = make_controller();
        let dir = make_temp_tree();
        ctrl.set_show_hidden(true);
        ctrl.set_root(dir.path().to_path_buf());

        wait_for(
            || ctrl.nodes_snapshot()[dir.path()].loaded,
            Duration::from_secs(5),
        );

        let visible = ctrl.build_visible_nodes();
        let has_hidden = visible.iter().any(|n| n.entry.is_hidden);
        assert!(has_hidden, ".hidden must appear when show_hidden = true");
    }

    #[test]
    fn move_focus_clamps_at_bounds() {
        let ctrl = make_controller();
        let dir = make_temp_tree();
        ctrl.set_root(dir.path().to_path_buf());

        wait_for(
            || ctrl.nodes_snapshot()[dir.path()].loaded,
            Duration::from_secs(5),
        );

        ctrl.move_focus(1);
        ctrl.move_focus(-100);
        // Must not panic; focus clamped to 0.
        assert_eq!(ctrl.focused.load(Ordering::Relaxed), 0);
    }
}
