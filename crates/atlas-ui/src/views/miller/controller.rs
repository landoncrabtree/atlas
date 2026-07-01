//! [`MillerController`] — drives the Slint Miller columns view.
//!
//! The controller manages a stack of [`Column`]s, each backed by an
//! [`atlas_fs::InMemoryLocationViewModel`] and a background subscription
//! thread.  When the user selects a directory row in column *N* the stack is
//! truncated to *N+1* columns and a new column is opened for the child
//! directory.  File selection just updates the focused index without extending
//! the stack.
//!
//! All Slint property updates are marshalled through
//! [`slint::invoke_from_event_loop`] so this controller is safe to use from
//! any thread.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread::JoinHandle,
};

use atlas_fs::{
    Entry, EntryKind, InMemoryLocationViewModel, LocationViewModel, OpenOptions, ViewModelEvent,
};
use crossbeam_channel::{unbounded, Sender};
use parking_lot::{Mutex, RwLock};
use slint::{ModelRc, SharedString, VecModel};

use crate::{
    actions::{ActionSink, UiAction},
    views::{
        details::{format_relative_time, format_size},
        miller::column::Column,
    },
    AtlasWindow, EntryRowItem, MillerColData,
};

// ── Internal types ────────────────────────────────────────────────────────────

struct ColumnSub {
    column: Arc<Column>,
    handle: JoinHandle<()>,
    stop_tx: Sender<()>,
}

// ── Controller ────────────────────────────────────────────────────────────────

/// Drives the Slint Miller columns view.
///
/// Construct via [`MillerController::new`] and share behind an [`Arc`].
/// Call [`MillerController::attach_window`] once the Slint window is available,
/// then [`MillerController::set_root`] to navigate to an initial directory.
pub struct MillerController {
    /// Active column stack (index 0 = leftmost).
    columns: RwLock<Vec<ColumnSub>>,
    /// Index of the column that currently holds focus.
    focused_column: AtomicUsize,
    /// Weak reference to the Slint window — set by `attach_window`.
    window: RwLock<slint::Weak<AtlasWindow>>,
    /// Shared action sink for emitting navigation / open-file actions.
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
}

impl MillerController {
    /// Construct a new controller.
    ///
    /// The controller starts with no columns and no window attached.
    /// Call [`Self::attach_window`] and then [`Self::set_root`] to begin.
    #[must_use]
    pub fn new(actions: Arc<Mutex<Box<dyn ActionSink>>>) -> Arc<Self> {
        Arc::new(Self {
            columns: RwLock::new(Vec::new()),
            focused_column: AtomicUsize::new(0),
            window: RwLock::new(slint::Weak::default()),
            actions,
        })
    }

    /// Attach the Slint window so that property updates can be sent.
    ///
    /// May be called from any thread before or after [`Self::set_root`].
    pub fn attach_window(&self, window: slint::Weak<AtlasWindow>) {
        *self.window.write() = window;
    }

    /// Root the Miller stack at `path`.
    ///
    /// Stops and drops all existing columns, then opens a fresh column for
    /// `path`.  Pushes the updated state to the Slint UI.
    pub fn set_root(self: &Arc<Self>, path: PathBuf) {
        self.stop_all_columns();
        self.push_new_column(path);
        self.focused_column.store(0, Ordering::Relaxed);
        self.push_ui_metadata();
    }

    /// Handle a row click in `column`.
    ///
    /// - Truncates the stack to `column + 1`.
    /// - If `row` is a directory, opens a new column for it and shifts focus
    ///   to the new column.
    /// - If `row` is a file, updates the focused index and keeps focus on
    ///   `column`.
    /// - Pushes updated state to the Slint UI.
    pub fn select_row(self: &Arc<Self>, column: usize, row: usize) {
        // Truncate any columns to the right.
        self.stop_columns_after(column);

        // Update the focused index for the clicked column.
        {
            let cols = self.columns.read();
            if let Some(sub) = cols.get(column) {
                sub.column.focused.store(row, Ordering::Relaxed);
            }
        }

        // Check whether the selected entry is a directory, and if so get its path.
        let child_path: Option<PathBuf> = {
            let cols = self.columns.read();
            cols.get(column).and_then(|sub| {
                let entries = sub.column.entries.read();
                entries.get(row).and_then(|e| {
                    if e.kind.is_dir() {
                        Some(e.path.clone())
                    } else {
                        None
                    }
                })
            })
        };

        if let Some(path) = child_path {
            self.push_new_column(path);
            let new_idx = self.columns.read().len().saturating_sub(1);
            self.focused_column.store(new_idx, Ordering::Relaxed);
        } else {
            self.focused_column.store(column, Ordering::Relaxed);
        }

        // Refresh the clicked column's entry display (focused row changed).
        self.push_column_entries_to_ui(column);
        self.push_ui_metadata();
    }

    /// Move the focused row within the focused column by `delta` rows.
    ///
    /// Movement is clamped to the valid range; the focused column is not
    /// changed.  Per the MVP spec, auto-opening child directories on focus
    /// movement is **not** implemented — require an explicit right-arrow /
    /// `activate_focused` to descend.
    pub fn move_focus(self: &Arc<Self>, delta: isize) {
        let focused_col = self.focused_column.load(Ordering::Relaxed);
        let (len, current) = {
            let cols = self.columns.read();
            let Some(sub) = cols.get(focused_col) else {
                return;
            };
            let len = sub.column.entries.read().len();
            let current = sub.column.focused.load(Ordering::Relaxed);
            (len, current)
        };
        if len == 0 {
            return;
        }
        let current_i = if current == usize::MAX { 0 } else { current };
        let next = (current_i as isize + delta).clamp(0, (len as isize) - 1) as usize;
        {
            let cols = self.columns.read();
            if let Some(sub) = cols.get(focused_col) {
                sub.column.focused.store(next, Ordering::Relaxed);
            }
        }
        self.push_column_entries_to_ui(focused_col);
    }

    /// Move focus to an adjacent column (`delta = -1` = left, `+1` = right).
    ///
    /// Moving right opens a child column if the focused row is a directory.
    /// Moving left closes the rightmost column and shifts focus one step left.
    pub fn move_column(self: &Arc<Self>, delta: isize) {
        let focused_col = self.focused_column.load(Ordering::Relaxed);
        let col_count = self.columns.read().len();

        if delta > 0 {
            // Try to open the child column for the focused row.
            let child_path: Option<PathBuf> = {
                let cols = self.columns.read();
                cols.get(focused_col).and_then(|sub| {
                    let focused = sub.column.focused.load(Ordering::Relaxed);
                    let entries = sub.column.entries.read();
                    entries.get(focused).and_then(|e| {
                        if e.kind.is_dir() {
                            Some(e.path.clone())
                        } else {
                            None
                        }
                    })
                })
            };
            if let Some(path) = child_path {
                // Truncate any existing children first.
                self.stop_columns_after(focused_col);
                self.push_new_column(path);
                let new_idx = self.columns.read().len().saturating_sub(1);
                self.focused_column.store(new_idx, Ordering::Relaxed);
                self.push_ui_metadata();
            }
        } else if delta < 0 && col_count > 1 {
            // Close the rightmost column and shift focus left.
            self.stop_columns_after(col_count.saturating_sub(2));
            let new_focused = col_count.saturating_sub(2);
            self.focused_column.store(new_focused, Ordering::Relaxed);
            self.push_ui_metadata();
        }
    }

    /// Activate the currently focused entry in the focused column.
    ///
    /// - Directory → dispatch [`UiAction::Navigate`] for pane 0.
    /// - File      → dispatch [`UiAction::OpenFile`] for pane 0.
    pub fn activate_focused(self: &Arc<Self>) {
        let focused_col = self.focused_column.load(Ordering::Relaxed);
        let entry: Option<Entry> = {
            let cols = self.columns.read();
            cols.get(focused_col).and_then(|sub| {
                let focused = sub.column.focused.load(Ordering::Relaxed);
                let entries = sub.column.entries.read();
                entries.get(focused).cloned()
            })
        };
        let Some(entry) = entry else { return };
        if entry.kind.is_dir() {
            self.actions.lock().dispatch(UiAction::Navigate {
                pane: 0,
                path: entry.path,
            });
        } else {
            self.actions.lock().dispatch(UiAction::OpenFile {
                pane: 0,
                path: entry.path,
            });
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Open a new column for `path`, spawn its subscription thread, and append
    /// it to the columns stack.  Pushes the initial entry list to the UI.
    fn push_new_column(self: &Arc<Self>, path: PathBuf) {
        let location = InMemoryLocationViewModel::open_live(
            path.clone(),
            OpenOptions {
                include_hidden: false,
                follow_symlinks: true,
                ..OpenOptions::default()
            },
        );

        let column = Column::new(path, Arc::clone(&location));
        let (stop_tx, stop_rx) = unbounded::<()>();
        let event_rx = location.subscribe();

        let col_idx = self.columns.read().len();
        let ctrl = Arc::clone(self);
        let col = Arc::clone(&column);

        let thread_name = format!("atlas-miller-col{col_idx}");
        let handle = match std::thread::Builder::new()
            .name(thread_name.clone())
            .spawn(move || {
                Self::run_subscription(ctrl, col, col_idx, event_rx, stop_rx);
            }) {
            Ok(h) => h,
            Err(err) => {
                tracing::error!(%err, col = col_idx, "failed to spawn miller subscription thread");
                return;
            }
        };

        self.columns.write().push(ColumnSub {
            column,
            handle,
            stop_tx,
        });

        // Do an initial refresh so something appears immediately even before
        // the first subscription event fires.
        self.push_column_entries_to_ui(col_idx);
    }

    /// Background loop for one column — updates entries on every view-model
    /// event and forwards the result to the Slint global.
    fn run_subscription(
        ctrl: Arc<Self>,
        col: Arc<Column>,
        col_idx: usize,
        event_rx: crossbeam_channel::Receiver<ViewModelEvent>,
        stop_rx: crossbeam_channel::Receiver<()>,
    ) {
        loop {
            crossbeam_channel::select! {
                recv(stop_rx) -> _ => break,
                recv(event_rx) -> event => {
                    let Ok(event) = event else { break };
                    match event {
                        ViewModelEvent::EntriesChanged | ViewModelEvent::Loaded => {
                            let entries = col.location.entries();
                            col.loaded.store(true, Ordering::Relaxed);
                            *col.entries.write() = entries;
                            ctrl.push_column_entries_to_ui(col_idx);
                        }
                        ViewModelEvent::Error(msg) => {
                            tracing::warn!(col = col_idx, %msg, "miller column view model error");
                        }
                    }
                }
            }
        }
    }

    /// Send stop signals to every column subscription thread and join them.
    fn stop_all_columns(&self) {
        let subs: Vec<ColumnSub> = {
            let mut cols = self.columns.write();
            cols.drain(..).collect()
        };
        Self::join_subs(subs);
    }

    /// Drain columns at indices `> col` and join their threads.
    fn stop_columns_after(&self, col: usize) {
        let subs: Vec<ColumnSub> = {
            let mut cols = self.columns.write();
            if col + 1 >= cols.len() {
                return;
            }
            cols.drain((col + 1)..).collect()
        };
        Self::join_subs(subs);
    }

    /// Send stop signals and join all subscription threads in `subs`.
    fn join_subs(subs: Vec<ColumnSub>) {
        for sub in subs {
            if let Err(err) = sub.stop_tx.send(()) {
                tracing::debug!(%err, "miller column subscription already stopped");
            }
            if let Err(err) = sub.handle.join() {
                tracing::warn!(?err, "miller column subscription thread panicked");
            }
        }
    }

    /// Push the entry list for column `col_idx` to the Slint window.
    ///
    /// Rebuilds the complete `[MillerColData]` array and schedules an
    /// `invoke_from_event_loop` to update the window.  No-ops silently if the
    /// window is not attached.
    fn push_column_entries_to_ui(&self, _col_idx: usize) {
        self.push_all_columns_to_ui();
    }

    /// Rebuild all column data and push to the Slint window.
    fn push_all_columns_to_ui(&self) {
        // Collect raw (Send-safe) data before the invoke_from_event_loop closure.
        // `ModelRc` uses `Rc` internally and is not `Send`, so we must not create
        // it before the closure — construct it inside the closure instead.
        let raw_cols: Vec<(SharedString, Vec<EntryRowItem>, i32, bool)> = {
            let cols = self.columns.read();
            cols.iter()
                .map(|sub| {
                    let entries = sub.column.entries.read();
                    let row_items: Vec<EntryRowItem> =
                        entries.iter().map(entry_to_row_item).collect();
                    let focused = sub.column.focused.load(Ordering::Relaxed);
                    let focused_i32 = if focused == usize::MAX || focused >= entries.len() {
                        -1_i32
                    } else {
                        focused as i32
                    };
                    (
                        col_title(&sub.column.path),
                        row_items,
                        focused_i32,
                        !sub.column.loaded.load(Ordering::Relaxed),
                    )
                })
                .collect()
        };
        let focused_col = self.focused_column.load(Ordering::Relaxed) as i32;
        let window = self.window.read().clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(w) = window.upgrade() else { return };
            let col_data: Vec<MillerColData> = raw_cols
                .into_iter()
                .map(|(title, entries, focused, loading)| MillerColData {
                    title,
                    entries: ModelRc::new(VecModel::from(entries)),
                    focused,
                    loading,
                })
                .collect();
            w.set_pane0_miller_columns(ModelRc::new(VecModel::from(col_data)));
            w.set_pane0_miller_focused_col(focused_col);
        });
    }

    /// Push `focused-column` to the Slint window (columns unchanged).
    fn push_ui_metadata(&self) {
        self.push_all_columns_to_ui();
    }

    // ── Test helpers ──────────────────────────────────────────────────────────

    /// Number of currently active columns (for tests).
    #[cfg(test)]
    pub(crate) fn column_count(&self) -> usize {
        self.columns.read().len()
    }

    /// Snapshot of the entries in column `col_idx` (for tests).
    #[cfg(test)]
    pub(crate) fn column_entries(&self, col_idx: usize) -> Vec<atlas_fs::Entry> {
        let cols = self.columns.read();
        cols.get(col_idx)
            .map(|sub| sub.column.entries.read().clone())
            .unwrap_or_default()
    }

    /// Whether the column at `col_idx` has finished its initial load.
    #[cfg(test)]
    pub(crate) fn column_loaded(&self, col_idx: usize) -> bool {
        let cols = self.columns.read();
        cols.get(col_idx)
            .is_some_and(|sub| sub.column.loaded.load(Ordering::Relaxed))
    }

    /// Current focused column index (for tests).
    #[cfg(test)]
    pub(crate) fn focused_col(&self) -> usize {
        self.focused_column.load(Ordering::Relaxed)
    }
}

impl Drop for MillerController {
    fn drop(&mut self) {
        let subs: Vec<ColumnSub> = self.columns.write().drain(..).collect();
        Self::join_subs(subs);
    }
}

// ── Slint global helpers ──────────────────────────────────────────────────────

/// Derive the display title for a column from its path.
fn col_title(path: &std::path::Path) -> SharedString {
    path.file_name()
        .map(|n| SharedString::from(n.to_string_lossy().as_ref()))
        .unwrap_or_else(|| SharedString::from(path.to_string_lossy().as_ref()))
}

// ── Entry conversion ──────────────────────────────────────────────────────────

/// Convert an [`atlas_fs::Entry`] to the Slint [`EntryRowItem`] struct.
fn entry_to_row_item(entry: &atlas_fs::Entry) -> EntryRowItem {
    let (is_dir, is_symlink, is_broken_symlink, kind_icon) = match &entry.kind {
        EntryKind::Dir => (true, false, false, "📁"),
        EntryKind::File => (false, false, false, "📄"),
        EntryKind::Symlink { broken, .. } => (false, true, *broken, "🔗"),
        EntryKind::Other => (false, false, false, "⚙️"),
    };

    let size_text = if is_dir {
        String::new()
    } else {
        format_size(entry.metadata.size)
    };
    let modified_text = entry
        .metadata
        .modified
        .map(format_relative_time)
        .unwrap_or_default();

    EntryRowItem {
        name: SharedString::from(entry.name.as_str()),
        kind_icon: SharedString::from(kind_icon),
        size_text: SharedString::from(size_text),
        modified_text: SharedString::from(modified_text),
        is_hidden: entry.metadata.is_hidden,
        is_dir,
        is_symlink,
        is_broken_symlink,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use tempfile::TempDir;

    fn make_actions() -> Arc<Mutex<Box<dyn ActionSink>>> {
        struct Noop;
        impl ActionSink for Noop {
            fn dispatch(&mut self, _: UiAction) {}
        }
        Arc::new(Mutex::new(Box::new(Noop)))
    }

    /// Spin-wait until `predicate` returns `true`, or panic after ~5 s.
    fn wait_until(predicate: impl Fn() -> bool) {
        for _ in 0..200 {
            if predicate() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("wait_until timed out");
    }

    /// Build a tempdir containing:
    ///   subdir_a/      (directory)
    ///   subdir_b/      (directory)
    ///   file_x.txt     (file)
    fn make_tree() -> TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("subdir_a")).expect("create subdir_a");
        fs::create_dir(dir.path().join("subdir_b")).expect("create subdir_b");
        fs::write(dir.path().join("file_x.txt"), b"hello").expect("write file_x");
        dir
    }

    #[test]
    fn set_root_opens_one_column() {
        let dir = make_tree();
        let ctrl = MillerController::new(make_actions());
        ctrl.set_root(dir.path().to_path_buf());

        wait_until(|| ctrl.column_loaded(0));

        assert_eq!(ctrl.column_count(), 1, "should have exactly 1 column");
        let entries = ctrl.column_entries(0);
        assert!(!entries.is_empty(), "root column should have entries");
        // Verify expected names are present.
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"subdir_a"));
        assert!(names.contains(&"subdir_b"));
        assert!(names.contains(&"file_x.txt"));
    }

    #[test]
    fn select_row_on_dir_opens_child_column() {
        let dir = make_tree();
        // Put a file inside subdir_a so the child column has entries.
        fs::write(dir.path().join("subdir_a").join("child.txt"), b"c").expect("write child");

        let ctrl = MillerController::new(make_actions());
        ctrl.set_root(dir.path().to_path_buf());
        wait_until(|| ctrl.column_loaded(0));

        // Find the row index for a directory entry.
        let dir_row = {
            let entries = ctrl.column_entries(0);
            entries
                .iter()
                .position(|e| e.kind.is_dir())
                .expect("at least one directory")
        };

        ctrl.select_row(0, dir_row);

        // A second column should now exist.
        wait_until(|| ctrl.column_count() == 2);
        assert_eq!(ctrl.column_count(), 2);
        assert_eq!(ctrl.focused_col(), 1);
    }

    #[test]
    fn select_row_on_file_does_not_open_column() {
        let dir = make_tree();
        let ctrl = MillerController::new(make_actions());
        ctrl.set_root(dir.path().to_path_buf());
        wait_until(|| ctrl.column_loaded(0));

        // Find a file row.
        let file_row = {
            let entries = ctrl.column_entries(0);
            entries
                .iter()
                .position(|e| !e.kind.is_dir())
                .expect("at least one file")
        };

        ctrl.select_row(0, file_row);

        // Column count must stay at 1.
        assert_eq!(ctrl.column_count(), 1);
        assert_eq!(ctrl.focused_col(), 0);
    }

    #[test]
    fn selecting_in_col_k_truncates_right() {
        let dir = make_tree();
        // subdir_a contains a nested directory.
        let nested = dir.path().join("subdir_a").join("nested");
        fs::create_dir(&nested).expect("create nested");
        fs::write(nested.join("deep.txt"), b"d").expect("write deep");

        let ctrl = MillerController::new(make_actions());
        ctrl.set_root(dir.path().to_path_buf());
        wait_until(|| ctrl.column_loaded(0));

        // Open subdir_a → 2 columns.
        let dir_row = ctrl
            .column_entries(0)
            .iter()
            .position(|e| e.name.as_str() == "subdir_a")
            .expect("subdir_a");
        ctrl.select_row(0, dir_row);
        wait_until(|| ctrl.column_count() == 2 && ctrl.column_loaded(1));

        // Open nested → 3 columns.
        let nested_row = ctrl
            .column_entries(1)
            .iter()
            .position(|e| e.kind.is_dir())
            .expect("nested dir");
        ctrl.select_row(1, nested_row);
        wait_until(|| ctrl.column_count() == 3);

        // Now select the file in col 1 — columns > 1 must be removed.
        let file_row = ctrl
            .column_entries(1)
            .iter()
            .position(|e| !e.kind.is_dir())
            .unwrap_or(0); // if no file, select row 0 (will find dir, that's fine for truncation)
        let _ = file_row; // used below only if there's a file
                          // Select a different row in col 0 to force col 1..N to truncate.
        ctrl.select_row(0, dir_row);
        // After selecting col 0 row, cols 1+ should be truncated and 1 new opened.
        wait_until(|| ctrl.column_count() <= 2);
        assert!(
            ctrl.column_count() <= 2,
            "columns after selected column should be gone"
        );
    }

    #[test]
    fn move_column_left_closes_rightmost() {
        let dir = make_tree();
        let ctrl = MillerController::new(make_actions());
        ctrl.set_root(dir.path().to_path_buf());
        wait_until(|| ctrl.column_loaded(0));

        // Open a child column.
        let dir_row = ctrl
            .column_entries(0)
            .iter()
            .position(|e| e.kind.is_dir())
            .expect("directory");
        ctrl.select_row(0, dir_row);
        wait_until(|| ctrl.column_count() == 2);

        // Move left — rightmost column should close.
        ctrl.move_column(-1);
        assert_eq!(ctrl.column_count(), 1, "move_column(-1) should close col 1");
        assert_eq!(ctrl.focused_col(), 0);
    }
}
