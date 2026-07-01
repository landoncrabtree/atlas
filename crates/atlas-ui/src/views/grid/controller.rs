//! [`GridController`] drives the Slint Grid view from a
//! [`atlas_fs::LocationViewModel`] event stream.
//!
//! Mirrors the structure of [`crate::views::details::DetailsController`]:
//! a background subscription thread listens for `ViewModelEvent`s, converts
//! entries to [`crate::EntryRowItem`] slices, and pushes them to Slint via
//! [`slint::invoke_from_event_loop`].
//!
//! Thumbnail requests are delegated to [`super::thumbs::ThumbRequester`],
//! which deduplicates in-flight requests and routes decoded images back to
//! the correct cell indices on the UI thread.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use atlas_fs::{Entry, EntryKind, LocationViewModel, ViewModelEvent};
use atlas_thumbs::{can_thumbnail, SqliteCache};
use crossbeam_channel::{unbounded, Sender};
use parking_lot::{Mutex, RwLock};
use slint::SharedString;

use crate::{
    actions::{ActionSink, UiAction},
    models::split::PaneId,
    shell::AppShell,
    views::details::{format_relative_time, format_size},
    EntryRowItem,
};

use super::thumbs::{ThumbRequester, DEFAULT_TARGET_DIM};

/// Sentinel meaning "no focused index".
const NO_FOCUS: usize = usize::MAX;

// ── Local selection type ──────────────────────────────────────────────────────
//
// `crate::views::details::Selection` is pub but its methods (`resize`,
// `select_single`, `toggle`, `select_range`) are private.  We define an
// equivalent struct here rather than reach into details internals.
// TODO: if `Selection` methods are made pub upstream, remove this duplicate.

/// Selection state for the Grid view.
#[derive(Debug, Default)]
pub struct GridSelection {
    /// Per-entry selection flags; same length as the current entries snapshot.
    pub mask: Vec<bool>,
    /// Anchor index for shift-range selection.
    pub anchor: Option<usize>,
}

impl GridSelection {
    fn clear(&mut self) {
        self.mask.fill(false);
        self.anchor = None;
    }

    pub(crate) fn resize(&mut self, len: usize) {
        self.mask.resize(len, false);
    }

    pub(crate) fn select_single(&mut self, index: usize) {
        self.clear();
        if index < self.mask.len() {
            self.mask[index] = true;
        }
        self.anchor = Some(index);
    }

    pub(crate) fn toggle(&mut self, index: usize) {
        if index < self.mask.len() {
            self.mask[index] = !self.mask[index];
        }
        self.anchor = Some(index);
    }

    pub(crate) fn select_range(&mut self, from: usize, to: usize) {
        if self.mask.is_empty() {
            self.anchor = Some(to);
            return;
        }
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        let hi_clamped = hi.min(self.mask.len().saturating_sub(1));
        self.mask.fill(false);
        for slot in &mut self.mask[lo..=hi_clamped] {
            *slot = true;
        }
        self.anchor = Some(from);
    }
}

// ── Controller ────────────────────────────────────────────────────────────────

struct SubscriptionState {
    handle: std::thread::JoinHandle<()>,
    stop_tx: Sender<()>,
}

/// Drives the Slint Grid view from a [`atlas_fs::LocationViewModel`] stream.
///
/// Construct via [`GridController::new`] and share behind an [`Arc`]. Call
/// [`GridController::set_location`] to navigate; the previous subscription
/// thread is stopped gracefully before the new one starts.
pub struct GridController {
    pane_id: PaneId,
    location: RwLock<Option<Arc<dyn LocationViewModel>>>,
    entries: RwLock<Vec<Entry>>,
    selection: RwLock<GridSelection>,
    focused: AtomicUsize,
    /// Current column count; updated by the Slint `columns-changed` callback.
    columns: AtomicUsize,
    thumb_requester: Arc<ThumbRequester>,
    subscription: Mutex<Option<SubscriptionState>>,
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
    shell: std::sync::Weak<AppShell>,
}

impl GridController {
    /// Construct a new controller for the given pane id.
    ///
    /// Accepts a shared [`SqliteCache`] for thumbnail persistence and starts
    /// the drain background thread inside [`ThumbRequester`].
    ///
    /// `worker_count` / `max_cache_bytes` are forwarded to [`ThumbRequester`];
    /// pass `0` / `500 * 1024 * 1024` for defaults.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pane_id: PaneId,
        shell: std::sync::Weak<AppShell>,
        actions: Arc<Mutex<Box<dyn ActionSink>>>,
        cache: Arc<SqliteCache>,
        worker_count: usize,
        max_cache_bytes: u64,
        thumbs_enabled: bool,
        max_file_bytes: u64,
    ) -> Arc<Self> {
        let thumb_requester = ThumbRequester::new(
            cache,
            pane_id,
            shell.clone(),
            worker_count,
            max_cache_bytes,
            thumbs_enabled,
            max_file_bytes,
        );
        Arc::new(Self {
            pane_id,
            location: RwLock::new(None),
            entries: RwLock::new(Vec::new()),
            selection: RwLock::new(GridSelection::default()),
            focused: AtomicUsize::new(NO_FOCUS),
            columns: AtomicUsize::new(6),
            thumb_requester,
            subscription: Mutex::new(None),
            actions,
            shell,
        })
    }

    /// Replace the current location and begin streaming its entries.
    pub fn set_location(self: &Arc<Self>, location: Arc<dyn LocationViewModel>) {
        self.stop_subscription();

        {
            *self.entries.write() = Vec::new();
            *self.selection.write() = GridSelection::default();
            self.focused.store(NO_FOCUS, Ordering::Relaxed);
        }

        *self.location.write() = Some(Arc::clone(&location));

        let rx = location.subscribe();
        let (stop_tx, stop_rx) = unbounded();
        let ctrl = Arc::clone(self);

        match std::thread::Builder::new()
            .name(format!("atlas-grid-pane{}", self.pane_id.0))
            .spawn(move || ctrl.run_subscription(rx, stop_rx))
        {
            Ok(handle) => {
                *self.subscription.lock() = Some(SubscriptionState { handle, stop_tx });
            }
            Err(error) => {
                tracing::error!(pane = self.pane_id.0, %error, "failed to spawn grid subscription thread");
            }
        }

        self.refresh_from_location();
    }

    /// Update the current column count from the Slint `columns-changed` callback.
    pub fn set_columns(&self, cols: usize) {
        self.columns.store(cols.max(1), Ordering::Relaxed);
    }

    /// Update selection for a clicked entry, then push state to Slint.
    pub fn select_index(self: &Arc<Self>, index: usize, ctrl: bool, shift: bool) {
        let len = self.entries.read().len();
        if index >= len {
            return;
        }
        let anchor = self.selection.read().anchor;

        {
            let mut sel = self.selection.write();
            sel.resize(len);
            if shift {
                let from = anchor.unwrap_or(index);
                sel.select_range(from, index);
            } else if ctrl {
                sel.toggle(index);
            } else {
                sel.select_single(index);
            }
        }

        if !shift {
            self.focused.store(index, Ordering::Relaxed);
        }

        self.push_selection_to_ui();
    }

    /// Activate (navigate into) the currently focused entry if it is a directory.
    pub fn activate_focused(self: &Arc<Self>) {
        let focused = self.focused.load(Ordering::Relaxed);
        if focused == NO_FOCUS {
            return;
        }
        // Extract the path under a short-lived read lock so we don't hold
        // the lock while dispatching (Navigate re-enters set_location which
        // needs the write lock; parking_lot is non-reentrant → deadlock).
        let target = {
            let entries = self.entries.read();
            entries
                .get(focused)
                .filter(|entry| entry.kind.is_dir())
                .map(|entry| entry.path.clone())
        };
        if let Some(path) = target {
            let slot = self
                .shell
                .upgrade()
                .and_then(|s| s.slint_slot_for(self.pane_id))
                .unwrap_or(0);
            self.actions.lock().dispatch(UiAction::Navigate {
                pane: slot,
                path,
            });
        }
    }

    /// Move focus by `(delta_row, delta_col)` in grid coordinates.
    ///
    /// Uses the current `columns` count to map between linear index and 2-D
    /// position. Focus stops at grid edges (does not wrap).
    pub fn move_focus(self: &Arc<Self>, delta_row: isize, delta_col: isize) {
        let len = self.entries.read().len();
        if len == 0 {
            return;
        }

        let cols = self.columns.load(Ordering::Relaxed).max(1);
        let current = self.focused.load(Ordering::Relaxed);
        let current_idx = if current == NO_FOCUS { 0 } else { current };

        let new_idx = grid_move(current_idx, delta_row, delta_col, cols, len);
        self.focused.store(new_idx, Ordering::Relaxed);
        self.push_selection_to_ui();
    }

    /// Move focus AND single-select the new cell — keyboard-navigation
    /// parity with left-click.
    pub fn move_and_select(self: &Arc<Self>, delta_row: isize, delta_col: isize) {
        let len = self.entries.read().len();
        if len == 0 {
            return;
        }
        let cols = self.columns.load(Ordering::Relaxed).max(1);
        let current = self.focused.load(Ordering::Relaxed);
        let current_idx = if current == NO_FOCUS { 0 } else { current };
        let new_idx = grid_move(current_idx, delta_row, delta_col, cols, len);
        {
            let mut sel = self.selection.write();
            sel.resize(len);
            sel.select_single(new_idx);
        }
        self.focused.store(new_idx, Ordering::Relaxed);
        self.push_selection_to_ui();
    }

    /// Move focus and extend the range selection from anchor
    /// (Shift+Arrow / Shift+j/k).
    pub fn extend_selection(self: &Arc<Self>, delta_row: isize, delta_col: isize) {
        let len = self.entries.read().len();
        if len == 0 {
            return;
        }
        let cols = self.columns.load(Ordering::Relaxed).max(1);
        let current = self.focused.load(Ordering::Relaxed);
        let current_idx = if current == NO_FOCUS { 0 } else { current };
        let new_idx = grid_move(current_idx, delta_row, delta_col, cols, len);
        let anchor = self.selection.read().anchor.unwrap_or(new_idx);
        {
            let mut sel = self.selection.write();
            sel.resize(len);
            sel.select_range(anchor, new_idx);
        }
        self.focused.store(new_idx, Ordering::Relaxed);
        self.push_selection_to_ui();
    }

    /// Called by the Slint `thumbnail-visible` callback; enqueues a thumbnail
    /// request for the entry at `cell_index` if it is thumbnailable.
    pub fn thumbnail_visible(self: &Arc<Self>, cell_index: usize) {
        let entries = self.entries.read();
        let Some(entry) = entries.get(cell_index) else {
            return;
        };
        if entry.kind.is_dir() || !can_thumbnail(&entry.path) {
            return;
        }
        let path = entry.path.clone();
        drop(entries);
        self.thumb_requester
            .request(path, DEFAULT_TARGET_DIM, cell_index);
    }

    fn stop_subscription(&self) {
        let state = self.subscription.lock().take();
        if let Some(SubscriptionState { handle, stop_tx }) = state {
            if let Err(e) = stop_tx.send(()) {
                tracing::debug!(pane = self.pane_id.0, %e, "grid subscription already stopped");
            }
            if let Err(e) = handle.join() {
                tracing::warn!(pane = self.pane_id.0, ?e, "grid subscription thread panicked");
            }
        }
    }

    fn run_subscription(
        self: Arc<Self>,
        rx: crossbeam_channel::Receiver<ViewModelEvent>,
        stop_rx: crossbeam_channel::Receiver<()>,
    ) {
        loop {
            crossbeam_channel::select! {
                recv(stop_rx) -> _ => break,
                recv(rx) -> event => {
                    let Ok(event) = event else { break };
                    match event {
                        ViewModelEvent::EntriesChanged | ViewModelEvent::Loaded => {
                            self.refresh_from_location();
                        }
                        ViewModelEvent::Error(msg) => {
                            tracing::warn!(pane = self.pane_id.0, %msg, "grid location error");
                        }
                    }
                }
            }
        }
    }

    fn refresh_from_location(&self) {
        let snapshot = {
            let loc = self.location.read();
            loc.as_deref().map(LocationViewModel::entries)
        };
        let Some(entries) = snapshot else { return };

        let len = entries.len();
        let row_items: Vec<EntryRowItem> = entries.iter().map(entry_to_row_item).collect();

        // Reset thumbnail state for the new entry count.
        self.thumb_requester.reset(len);

        // Eagerly request thumbnails for all thumbnailable entries. In a fully
        // virtualised v0.2 grid these requests would come from the Slint
        // `thumbnail-visible` callback as rows scroll into view.
        for (i, entry) in entries.iter().enumerate() {
            if !entry.kind.is_dir() && can_thumbnail(&entry.path) {
                self.thumb_requester
                    .request(entry.path.clone(), DEFAULT_TARGET_DIM, i);
            }
        }

        *self.entries.write() = entries;

        let (mask, _anchor) = {
            let mut sel = self.selection.write();
            sel.resize(len);
            if let Some(a) = sel.anchor {
                if a >= len {
                    sel.anchor = None;
                }
            }
            (sel.mask.clone(), sel.anchor.map_or(-1_i32, |v| v as i32))
        };

        let focused = self.focused.load(Ordering::Relaxed);
        let focused_i32 = if focused == NO_FOCUS || focused >= len {
            self.focused.store(NO_FOCUS, Ordering::Relaxed);
            -1_i32
        } else {
            focused as i32
        };

        // snapshot() returns (Vec<Option<DecodedPixels>>, Vec<bool>).
        // Conversion to slint::Image happens inside push_pane_data_to_slint.
        let (decoded_snap, has_snap) = self.thumb_requester.snapshot();

        if let Some(shell) = self.shell.upgrade() {
            // Details rows are shared for the grid view label strip.
            shell.publish_details_rows(self.pane_id, row_items);
            shell.publish_grid_selected_mask(self.pane_id, mask);
            shell.publish_grid_focused_index(self.pane_id, focused_i32);
            shell.publish_grid_thumbs(self.pane_id, decoded_snap);
            shell.publish_grid_has_thumbs(self.pane_id, has_snap);
        }
    }

    fn push_selection_to_ui(&self) {
        let mask = self.selection.read().mask.clone();
        let focused = self.focused.load(Ordering::Relaxed);
        let focused_i32 = if focused == NO_FOCUS {
            -1_i32
        } else {
            focused as i32
        };

        if let Some(shell) = self.shell.upgrade() {
            shell.publish_grid_selected_mask(self.pane_id, mask);
            shell.publish_grid_focused_index(self.pane_id, focused_i32);
        }
    }
}

impl Drop for GridController {
    fn drop(&mut self) {
        self.stop_subscription();
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Convert an [`atlas_fs::Entry`] to the Slint [`EntryRowItem`] struct.
pub(crate) fn entry_to_row_item(entry: &Entry) -> EntryRowItem {
    let (is_dir, is_symlink, is_broken_symlink, kind_icon) = match &entry.kind {
        EntryKind::Dir => (true, false, false, "▸"),
        EntryKind::File => (false, false, false, "·"),
        EntryKind::Symlink { broken, .. } => (false, true, *broken, "↳"),
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

/// Compute the next focused linear index after a 2-D grid move.
///
/// `cols` is the number of columns; `len` is total entry count.
/// Movement stops at grid edges (no wrapping).
pub(crate) fn grid_move(
    current: usize,
    delta_row: isize,
    delta_col: isize,
    cols: usize,
    len: usize,
) -> usize {
    if len == 0 {
        return 0;
    }
    let cols = cols.max(1);
    let current_row = (current / cols) as isize;
    let current_col = (current % cols) as isize;
    let row_count = len.div_ceil(cols) as isize;
    let new_row = (current_row + delta_row).clamp(0, row_count - 1);
    let new_col = (current_col + delta_col).clamp(0, (cols as isize) - 1);
    let candidate = (new_row as usize) * cols + new_col as usize;
    candidate.min(len - 1)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_selection(len: usize) -> GridSelection {
        let mut s = GridSelection::default();
        s.resize(len);
        s
    }

    // ── Selection ────────────────────────────────────────────────────────────

    #[test]
    fn select_single_clears_others() {
        let mut s = make_selection(5);
        s.select_single(2);
        assert!(s.mask[2]);
        assert!(!s.mask[0]);
        assert_eq!(s.anchor, Some(2));
    }

    #[test]
    fn ctrl_toggle_adds_and_removes() {
        let mut s = make_selection(5);
        s.select_single(1);
        s.toggle(3);
        assert!(s.mask[1] && s.mask[3]);
        s.toggle(1);
        assert!(!s.mask[1]);
    }

    #[test]
    fn shift_range_forward() {
        let mut s = make_selection(10);
        s.select_single(2);
        s.select_range(2, 5);
        assert!(s.mask[2..=5].iter().all(|&b| b));
        assert!(!s.mask[6]);
    }

    // ── grid_move ────────────────────────────────────────────────────────────

    #[test]
    fn move_down_one_row() {
        // 10 entries, 4 cols: idx 1 (row 0, col 1) → idx 5 (row 1, col 1)
        assert_eq!(grid_move(1, 1, 0, 4, 10), 5);
    }

    #[test]
    fn move_right_one_col() {
        // idx 2 (row 0, col 2) → idx 3 (row 0, col 3)
        assert_eq!(grid_move(2, 0, 1, 4, 10), 3);
    }

    #[test]
    fn move_stops_at_left_edge() {
        assert_eq!(grid_move(0, 0, -1, 4, 10), 0);
    }

    #[test]
    fn move_stops_at_top_edge() {
        assert_eq!(grid_move(2, -1, 0, 4, 10), 2);
    }

    #[test]
    fn move_stops_at_bottom_edge() {
        // 10 entries, 4 cols: last row starts at idx 8 (row 2, col 0)
        assert_eq!(grid_move(8, 1, 0, 4, 10), 8);
    }

    #[test]
    fn move_clamps_to_last_entry_in_partial_row() {
        // 11 entries, 4 cols: last row has idx 8..=10
        // from idx 3 (row 0, col 3), down 2 rows → (row 2, col 3) = 11 → clamped to 10
        assert_eq!(grid_move(3, 2, 0, 4, 11), 10);
    }

    // ── Dedup logic (pure) ───────────────────────────────────────────────────

    #[test]
    fn dedup_in_flight_same_key_collects_indices() {
        use ahash::{AHashMap, AHashSet};
        use std::path::PathBuf;

        let mut in_flight: AHashSet<(PathBuf, u32)> = AHashSet::new();
        let mut pending: AHashMap<(PathBuf, u32), Vec<usize>> = AHashMap::new();

        let path = PathBuf::from("/img/photo.jpg");
        let key = (path.clone(), 256_u32);

        // First request: mark in-flight and add cell 0
        in_flight.insert(key.clone());
        pending.entry(key.clone()).or_default().push(0);

        // Second request for same path+dim: already in-flight, only append index
        if in_flight.contains(&key) {
            pending.entry(key.clone()).or_default().push(1);
        } else {
            in_flight.insert(key.clone());
            pending.entry(key.clone()).or_default().push(1);
        }

        assert_eq!(in_flight.len(), 1, "should not double-insert in_flight");
        assert_eq!(
            pending.get(&key).map(|v| v.len()),
            Some(2),
            "both cell indices must be pending"
        );
    }
}
