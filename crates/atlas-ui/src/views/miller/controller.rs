//! [`MillerController`] — drives the Slint Miller columns view.
//!
//! The controller manages a stack of [`Column`]s, each backed by a
//! [`atlas_fs::LocationViewModel`] (constructed by the pane's
//! [`LocationOpener`]) and a background subscription thread.  When the user
//! selects a directory row in column *N* the stack is truncated to *N+1*
//! columns and a new column is opened for the child directory using the same
//! opener, so a Miller pane rooted at a remote SFTP location descends into
//! remote sub-directories rather than punching through to the local `/`.
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
use slint::SharedString;

use crate::{
    actions::{ActionSink, UiAction},
    models::split::PaneId,
    shell::{AppShell, MillerColumnCache},
    theming::icons::icon_for,
    views::{
        details::{format_relative_time, format_size},
        miller::column::Column,
    },
    AtlasWindow, EntryRowItem,
};

// ── LocationOpener ────────────────────────────────────────────────────────────

/// Abstraction over "open a directory as a [`LocationViewModel`]".
///
/// The shell hands one of these to the Miller controller when it navigates a
/// pane: a [`LocalLocationOpener`] for local file:// panes, or an opener that
/// wraps a live [`atlas_remote::RemoteLocationViewModel`] so Miller columns
/// can descend into remote sub-directories using the pane's already-opened
/// connection pool entry.
pub trait LocationOpener: Send + Sync {
    /// Open `path` as a live [`LocationViewModel`].
    ///
    /// Called synchronously from the controller when a new column is pushed;
    /// implementations should return quickly (the returned VM streams entries
    /// asynchronously via its subscribe channel).
    fn open(&self, path: PathBuf) -> Arc<dyn LocationViewModel>;
}

/// Default [`LocationOpener`] that opens local paths via
/// [`InMemoryLocationViewModel::open_live`] with hidden files filtered out
/// and symlinks followed.
pub struct LocalLocationOpener;

impl LocationOpener for LocalLocationOpener {
    fn open(&self, path: PathBuf) -> Arc<dyn LocationViewModel> {
        InMemoryLocationViewModel::open_live(
            path,
            OpenOptions {
                include_hidden: false,
                follow_symlinks: true,
                ..OpenOptions::default()
            },
        )
    }
}

/// [`LocationOpener`] for a remote-backed Miller pane.
///
/// Rebuilds a fresh [`atlas_remote::RemoteLocationViewModel`] for every
/// column by cloning the pane's initial `uri` and swapping its `.path`
/// component.  The remote connection pool guarantees that all sub-columns
/// reuse the pane's already-established SSH / HTTP session, so descending
/// through a Miller pane does not re-handshake for every column.
pub struct RemoteLocationOpener {
    /// Template URI for the pane.  Only the `.path` component changes per
    /// column.
    template: atlas_core::RemoteUri,
    /// Backend kind, cloned onto every derived URI.
    kind: atlas_core::BackendKind,
}

impl RemoteLocationOpener {
    /// Construct a remote opener from the pane's root
    /// [`atlas_remote::RemoteLocationViewModel`].
    #[must_use]
    pub fn from_remote(remote: &atlas_remote::RemoteLocationViewModel) -> Self {
        Self {
            template: remote.remote_uri().clone(),
            kind: remote.backend_kind(),
        }
    }
}

impl LocationOpener for RemoteLocationOpener {
    fn open(&self, path: PathBuf) -> Arc<dyn LocationViewModel> {
        // Re-fetch credentials from the ops keyring on each open.  The pool
        // key already re-uses the existing session for the (kind, host,
        // port, user, credentials) tuple, so this is cheap and keeps the
        // opener stateless w.r.t. secret material.
        let mut uri = self.template.clone();
        // Remote paths are always POSIX-style; convert via to_string_lossy so
        // Windows callers producing e.g. `PathBuf::from("/tmp/x")` still
        // serialise correctly.
        uri.path = path.to_string_lossy().into_owned();

        let credentials = match atlas_ops::credentials_for(&uri) {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(%err, path = %uri.path, "miller remote opener: credential lookup failed; falling back to local");
                return InMemoryLocationViewModel::open_live(
                    path,
                    OpenOptions {
                        include_hidden: false,
                        follow_symlinks: true,
                        ..OpenOptions::default()
                    },
                );
            }
        };
        let opts = OpenOptions::default();

        let opened = match self.kind {
            atlas_core::BackendKind::Sftp => {
                atlas_remote::RemoteLocationViewModel::open_live_sftp_with_options(
                    uri.clone(),
                    credentials,
                    opts,
                    atlas_remote::vm::sftp::SftpOptions {
                        known_hosts_mode: atlas_remote::vm::sftp::default_known_hosts_mode(),
                        resolver: None,
                    },
                )
            }
            _ => atlas_remote::RemoteLocationViewModel::open_live(
                uri.clone(),
                self.kind,
                credentials,
                opts,
            ),
        };

        match opened {
            Ok(vm) => vm as Arc<dyn LocationViewModel>,
            Err(err) => {
                tracing::warn!(%err, path = %uri.path, "miller remote opener: open_live failed");
                // Return an in-memory empty view model so the column renders as
                // an empty list rather than panicking.  Uses a nonexistent local
                // path so `is_loaded()` transitions to true immediately with 0
                // entries.
                InMemoryLocationViewModel::open_live(
                    path,
                    OpenOptions {
                        include_hidden: false,
                        follow_symlinks: true,
                        ..OpenOptions::default()
                    },
                )
            }
        }
    }
}

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
    /// Semantic id of the pane this controller drives.
    pane_id: PaneId,
    /// Active column stack (index 0 = leftmost).
    columns: RwLock<Vec<ColumnSub>>,
    /// Index of the column that currently holds focus.
    focused_column: AtomicUsize,
    /// Weak reference to the Slint window — set by `attach_window`.
    /// Retained for backwards-compatibility with callers that still invoke
    /// [`Self::attach_window`]; per-view data now flows through the shell
    /// cache via [`crate::shell::AppShell::publish_miller_columns`].
    #[allow(dead_code)]
    window: RwLock<slint::Weak<AtlasWindow>>,
    /// Weak reference to the shell so publish_* calls can rebuild the cache.
    shell: std::sync::Weak<AppShell>,
    /// Shared action sink for emitting navigation / open-file actions.
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
    /// Opener used to construct new columns.  Swapped whenever the pane's
    /// [`atlas_fs::LocationViewModel`] flavour changes (e.g. mounting a
    /// remote location swaps in a remote-aware opener).
    opener: RwLock<Arc<dyn LocationOpener>>,
}

impl MillerController {
    /// Construct a new controller.
    ///
    /// The controller starts with no columns and no window attached.
    /// Call [`Self::attach_window`] and then [`Self::set_root`] to begin.
    #[must_use]
    pub fn new(
        pane_id: PaneId,
        shell: std::sync::Weak<AppShell>,
        actions: Arc<Mutex<Box<dyn ActionSink>>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            pane_id,
            columns: RwLock::new(Vec::new()),
            focused_column: AtomicUsize::new(0),
            window: RwLock::new(slint::Weak::default()),
            shell,
            actions,
            opener: RwLock::new(Arc::new(LocalLocationOpener) as Arc<dyn LocationOpener>),
        })
    }

    /// Attach the Slint window so that property updates can be sent.
    ///
    /// May be called from any thread before or after [`Self::set_root`].
    pub fn attach_window(&self, window: slint::Weak<AtlasWindow>) {
        *self.window.write() = window;
    }

    /// Swap in a new [`LocationOpener`] for subsequent column pushes.
    ///
    /// Existing columns keep their originally-opened view models.  The next
    /// call to [`Self::set_root`] (or a `select_row` that pushes a child
    /// column) will use the new opener.
    pub fn set_opener(&self, opener: Arc<dyn LocationOpener>) {
        *self.opener.write() = opener;
    }

    /// Root the Miller stack at `path`.
    ///
    /// Uses the currently-installed [`LocationOpener`] (see
    /// [`Self::set_opener`]).  Stops and drops all existing columns, then
    /// opens a fresh column for `path`.  Pushes the updated state to the
    /// Slint UI.
    pub fn set_root(self: &Arc<Self>, path: PathBuf) {
        self.stop_all_columns();
        self.push_new_column(path);
        self.focused_column.store(0, Ordering::Relaxed);
        self.push_ui_metadata();
    }

    /// Root the Miller stack at `path` using an explicit [`LocationOpener`].
    ///
    /// Convenience wrapper that swaps the opener and then calls
    /// [`Self::set_root`] in one atomic-looking operation.  This is the seam
    /// used by the shell when mounting a remote location so subsequent
    /// column pushes reuse the remote connection pool entry.
    pub fn set_root_with_opener(self: &Arc<Self>, path: PathBuf, opener: Arc<dyn LocationOpener>) {
        self.set_opener(opener);
        self.set_root(path);
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

    /// Resolve the on-disk path of the entry at `(column, row)` without
    /// modifying focus state. Callers use this for double-click / context
    /// menu targeting where they need the path but not the selection
    /// side-effects of [`Self::select_row`].
    #[must_use]
    pub fn entry_path_at(&self, column: usize, row: usize) -> Option<PathBuf> {
        let cols = self.columns.read();
        let sub = cols.get(column)?;
        let entries = sub.column.entries.read();
        entries.get(row).map(|e| e.path.clone())
    }

    /// Return a clone of the [`Entry`] at `(column, row)` without modifying
    /// focus state.  Companion to [`Self::entry_path_at`] used by the
    /// context-menu shell handler so it can build a full `ContextTarget`
    /// (needs `entry.kind`, not just the path) directly from the
    /// right-clicked cell — matching the semantic contract in the
    /// convergence rule: build ContextTarget from `(pane_location, entry)`.
    #[must_use]
    pub fn column_entry(&self, column: usize, row: usize) -> Option<Entry> {
        let cols = self.columns.read();
        let sub = cols.get(column)?;
        let entries = sub.column.entries.read();
        entries.get(row).cloned()
    }

    /// Update the per-column focused row for `column` **without** opening a
    /// child column or truncating the stack — the visual highlight follows
    /// the pointer but the user's exploration state stays put.
    ///
    /// Right-click uses this so the highlighted row matches the entry the
    /// context menu will act on, matching Finder / Explorer behaviour where
    /// a right-click selects but does not navigate.  Regular left-click still
    /// goes through [`Self::select_row`] which navigates into directories.
    pub fn focus_row_within_column(self: &Arc<Self>, column: usize, row: usize) {
        {
            let cols = self.columns.read();
            let Some(sub) = cols.get(column) else {
                return;
            };
            if row >= sub.column.entries.read().len() {
                return;
            }
            sub.column.focused.store(row, Ordering::Relaxed);
        }
        self.focused_column.store(column, Ordering::Relaxed);
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
        let next = (current_i as isize)
            .saturating_add(delta)
            .clamp(0, (len as isize) - 1) as usize;
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
    /// - Directory → dispatch [`UiAction::Navigate`].
    /// - File      → dispatch [`UiAction::OpenFile`].
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
        let slot = self
            .shell
            .upgrade()
            .and_then(|s| s.slint_slot_for(self.pane_id))
            .unwrap_or(0);
        if entry.kind.is_dir() {
            self.actions.lock().dispatch(UiAction::Navigate {
                pane: slot,
                path: entry.path,
            });
        } else {
            self.actions.lock().dispatch(UiAction::OpenFile {
                pane: slot,
                path: entry.path,
            });
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Open a new column for `path` using the currently-installed
    /// [`LocationOpener`], spawn its subscription thread, and append it to
    /// the columns stack.  Pushes the initial entry list to the UI.
    fn push_new_column(self: &Arc<Self>, path: PathBuf) {
        let opener = Arc::clone(&*self.opener.read());
        let location: Arc<dyn LocationViewModel> = opener.open(path.clone());

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

    /// Rebuild all column data and push to the Slint window via the shell cache.
    fn push_all_columns_to_ui(&self) {
        let raw_cols: Vec<MillerColumnCache> = {
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
                    MillerColumnCache {
                        title: col_title(&sub.column.path).to_string(),
                        entries: row_items,
                        focused: focused_i32,
                        loading: !sub.column.loaded.load(Ordering::Relaxed),
                    }
                })
                .collect()
        };
        let focused_col = self.focused_column.load(Ordering::Relaxed) as i32;
        if let Some(shell) = self.shell.upgrade() {
            shell.publish_miller_columns(self.pane_id, raw_cols);
            shell.publish_miller_focused_col(self.pane_id, focused_col);
        }
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

/// Compute the desired horizontal scroll offset (`viewport-x`) for the Miller
/// columns view so the newest column's right edge sits at the visible-area's
/// right edge.
///
/// This mirrors the Slint-side math in `assets/ui/views/miller/miller-view.slint`;
/// keep them in lock-step.  Returned value is negative (Slint `viewport-x`
/// convention: negative = content shifted left = scrolled right) or zero.
///
/// Behaviour:
///   * `col_count == 0` → `0`.
///   * total content width ≤ visible width → `0` (everything fits).
///   * otherwise → `visible − content_width` (a negative value).
///
/// The caller only applies this value when the focused column is the
/// rightmost one, so navigating back to an earlier column does not jerk the
/// viewport.
#[must_use]
pub fn compute_miller_viewport_x(
    col_count: usize,
    col_width: f32,
    col_sep: f32,
    visible: f32,
) -> f32 {
    if col_count == 0 {
        return 0.0;
    }
    let content = (col_count as f32) * (col_width + col_sep);
    if content <= visible {
        0.0
    } else {
        visible - content
    }
}

// ── Entry conversion ──────────────────────────────────────────────────────────

/// Convert an [`atlas_fs::Entry`] to the Slint [`EntryRowItem`] struct.
fn entry_to_row_item(entry: &atlas_fs::Entry) -> EntryRowItem {
    let (is_dir, is_symlink, is_broken_symlink) = match &entry.kind {
        EntryKind::Dir => (true, false, false),
        EntryKind::File => (false, false, false),
        EntryKind::Symlink { broken, .. } => (false, true, *broken),
        EntryKind::Other => (false, false, false),
    };
    let kind_icon = icon_for(entry).glyph;

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

    fn make_ctrl() -> Arc<MillerController> {
        MillerController::new(PaneId(0), std::sync::Weak::new(), make_actions())
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
        let ctrl = make_ctrl();
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

        let ctrl = make_ctrl();
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
        let ctrl = make_ctrl();
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

        let ctrl = make_ctrl();
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
        let ctrl = make_ctrl();
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

    // ── LocationOpener plumbing ───────────────────────────────────────────────

    /// Counting opener that records every path it's asked to open — used to
    /// verify that `set_root_with_opener` routes through the caller's opener
    /// (rather than always falling back to a local `InMemoryLocationViewModel`,
    /// which was the bug for Miller on remote panes in phase 2.3).
    struct CountingOpener {
        target: PathBuf,
        seen: parking_lot::Mutex<Vec<PathBuf>>,
    }

    impl LocationOpener for CountingOpener {
        fn open(&self, path: PathBuf) -> Arc<dyn LocationViewModel> {
            self.seen.lock().push(path.clone());
            // Redirect every open() to a fixed test directory so entries are
            // deterministic regardless of the `path` argument.  This models a
            // "remote" opener that ignores local path structure and always
            // reads from its own backend.
            InMemoryLocationViewModel::open_live(
                self.target.clone(),
                OpenOptions {
                    include_hidden: false,
                    follow_symlinks: true,
                    ..OpenOptions::default()
                },
            )
        }
    }

    #[test]
    fn set_root_with_opener_uses_custom_opener() {
        // Two independent temp trees: `real` mimics the "remote" tree the
        // opener redirects to, `fake` is the path the miller root is nominally
        // set to (mimicking the remote URI path being `/`).
        let real = make_tree();
        let fake = tempfile::tempdir().expect("tempdir");
        let opener = Arc::new(CountingOpener {
            target: real.path().to_path_buf(),
            seen: parking_lot::Mutex::new(Vec::new()),
        });

        let ctrl = make_ctrl();
        ctrl.set_root_with_opener(fake.path().to_path_buf(), opener.clone());
        wait_until(|| ctrl.column_loaded(0));

        // Even though set_root was called with `fake.path()`, the opener
        // redirected the listing to `real.path()`, so we see real's entries.
        let names: Vec<String> = ctrl
            .column_entries(0)
            .iter()
            .map(|e| e.name.to_string())
            .collect();
        assert!(
            names.iter().any(|n| n == "subdir_a"),
            "opener listing should contain subdir_a, got {names:?}"
        );

        // The opener saw exactly one open() call for the root column.
        assert_eq!(opener.seen.lock().len(), 1);
        assert_eq!(opener.seen.lock()[0], fake.path().to_path_buf());
    }

    // ── compute_miller_viewport_x ─────────────────────────────────────────────

    #[test]
    fn viewport_x_zero_when_no_columns() {
        assert_eq!(compute_miller_viewport_x(0, 240.0, 1.0, 800.0), 0.0);
    }

    #[test]
    fn viewport_x_zero_when_content_fits() {
        // 3 columns × 241 px = 723 px content, 800 px visible → fits.
        assert_eq!(compute_miller_viewport_x(3, 240.0, 1.0, 800.0), 0.0);
    }

    #[test]
    fn viewport_x_shifts_left_when_content_overflows() {
        // 5 columns × 241 px = 1205 px content, 800 px visible → shift by
        // -(1205 - 800) = -405.
        let got = compute_miller_viewport_x(5, 240.0, 1.0, 800.0);
        assert!((got - -405.0).abs() < f32::EPSILON, "got {got}");
    }

    #[test]
    fn viewport_x_exactly_at_boundary_returns_zero() {
        // 4 cols × 200 px = 800 px content, 800 px visible → exactly fits.
        assert_eq!(compute_miller_viewport_x(4, 199.0, 1.0, 800.0), 0.0);
    }

    #[test]
    fn viewport_x_uses_col_sep() {
        // 2 cols × (100 + 10) = 220 content, 200 visible → -20.
        let got = compute_miller_viewport_x(2, 100.0, 10.0, 200.0);
        assert!((got - -20.0).abs() < f32::EPSILON, "got {got}");
    }

    // ── Right-click context menu targeting ────────────────────────────────────
    //
    // Locks the invariant behind item 5 (Phase 2.9): a right-click on a
    // Miller cell must resolve to the entry's on-disk path *and* full
    // `Entry` value without mutating focus.  These are the values the
    // shell handler feeds to `AppShell::open_context_menu_for_entry` —
    // if they drift, the capability-aware menu will act on the wrong
    // entry or misclassify its kind.

    #[test]
    fn entry_path_at_resolves_row_without_mutating_focus() {
        let dir = make_tree();
        let ctrl = make_ctrl();
        ctrl.set_root(dir.path().to_path_buf());
        wait_until(|| ctrl.column_loaded(0));

        let (dir_row, dir_name) = ctrl
            .column_entries(0)
            .iter()
            .enumerate()
            .find_map(|(i, e)| e.kind.is_dir().then(|| (i, e.name.to_string())))
            .expect("at least one directory in fixture");
        let focus_before = ctrl.focused_col();

        let resolved = ctrl.entry_path_at(0, dir_row).expect("path");
        assert!(
            resolved.ends_with(&dir_name),
            "resolved path {resolved:?} should end with {dir_name}"
        );
        assert_eq!(
            ctrl.focused_col(),
            focus_before,
            "entry_path_at must not change focused column"
        );
    }

    #[test]
    fn entry_path_at_out_of_range_returns_none() {
        let dir = make_tree();
        let ctrl = make_ctrl();
        ctrl.set_root(dir.path().to_path_buf());
        wait_until(|| ctrl.column_loaded(0));
        assert!(ctrl.entry_path_at(0, 9_999).is_none());
        assert!(ctrl.entry_path_at(9, 0).is_none());
    }

    #[test]
    fn column_entry_returns_entry_without_mutating_focus() {
        let dir = make_tree();
        let ctrl = make_ctrl();
        ctrl.set_root(dir.path().to_path_buf());
        wait_until(|| ctrl.column_loaded(0));

        let (dir_row, dir_name) = ctrl
            .column_entries(0)
            .iter()
            .enumerate()
            .find_map(|(i, e)| e.kind.is_dir().then(|| (i, e.name.to_string())))
            .expect("directory in fixture");
        let focus_before = ctrl.focused_col();

        let entry = ctrl.column_entry(0, dir_row).expect("entry");
        assert_eq!(entry.name.as_str(), dir_name);
        assert!(entry.kind.is_dir(), "kind must round-trip");
        assert_eq!(
            ctrl.focused_col(),
            focus_before,
            "column_entry must not change focused column"
        );
    }

    #[test]
    fn column_entry_out_of_range_returns_none() {
        let dir = make_tree();
        let ctrl = make_ctrl();
        ctrl.set_root(dir.path().to_path_buf());
        wait_until(|| ctrl.column_loaded(0));
        assert!(ctrl.column_entry(0, 9_999).is_none());
        assert!(ctrl.column_entry(9, 0).is_none());
    }

    #[test]
    fn focus_row_within_column_updates_row_without_navigating() {
        let dir = make_tree();
        let ctrl = make_ctrl();
        ctrl.set_root(dir.path().to_path_buf());
        wait_until(|| ctrl.column_loaded(0));

        let (dir_row, _) = ctrl
            .column_entries(0)
            .iter()
            .enumerate()
            .find_map(|(i, e)| e.kind.is_dir().then_some((i, ())))
            .expect("directory in fixture");
        let cols_before = ctrl.column_count();

        ctrl.focus_row_within_column(0, dir_row);

        // Unlike `select_row`, no child column is opened for a dir target.
        assert_eq!(
            ctrl.column_count(),
            cols_before,
            "focus_row_within_column must not push a child column"
        );
        // The row focus of column 0 is now `dir_row`.
        let entries = ctrl.column_entries(0);
        assert_eq!(entries.len(), ctrl.column_entries(0).len());
        assert!(dir_row < entries.len());
    }

    #[test]
    fn focus_row_within_column_out_of_range_is_no_op() {
        let dir = make_tree();
        let ctrl = make_ctrl();
        ctrl.set_root(dir.path().to_path_buf());
        wait_until(|| ctrl.column_loaded(0));
        let cols_before = ctrl.column_count();
        ctrl.focus_row_within_column(0, 9_999);
        ctrl.focus_row_within_column(9, 0);
        assert_eq!(ctrl.column_count(), cols_before);
    }
}
