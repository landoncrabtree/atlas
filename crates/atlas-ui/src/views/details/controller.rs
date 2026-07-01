//! The [`DetailsController`] drives the Slint Details view from a
//! [`atlas_fs::LocationViewModel`] event stream.
//!
//! The controller owns a background subscription thread that listens for
//! [`atlas_fs::ViewModelEvent`]s and pushes formatted [`crate::EntryRowItem`]
//! batches into the Slint window via [`slint::invoke_from_event_loop`].

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread::JoinHandle,
};

use atlas_fs::{Entry, EntryKind, LocationViewModel, SortKey, SortOrder, SortSpec, ViewModelEvent};
use crossbeam_channel::{select, unbounded, Receiver, Sender};
use parking_lot::{Mutex, RwLock};
use slint::{ModelRc, SharedString, VecModel};

use crate::{
    actions::{ActionSink, UiAction},
    views::details::{
        columns::{default_columns, ColumnKind, ColumnSpec},
        format::{format_relative_time, format_size},
    },
    AtlasWindow, EntryRowItem,
};

/// Sentinel value meaning "no focused index".
const NO_FOCUS: usize = usize::MAX;

/// Selection state for the Details view.
#[derive(Debug, Default)]
pub struct Selection {
    /// Per-entry selection flags; same length as the current entries snapshot.
    pub mask: Vec<bool>,
    /// Anchor index for shift-range selection.
    pub anchor: Option<usize>,
}

impl Selection {
    pub(crate) fn clear(&mut self) {
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

struct SubscriptionState {
    handle: JoinHandle<()>,
    stop_tx: Sender<()>,
}

/// Drives the Slint Details view from a [`atlas_fs::LocationViewModel`] event stream.
///
/// Construct once and share behind an [`Arc`]. Call [`DetailsController::set_location`]
/// to navigate to a new directory; the previous background thread is stopped
/// gracefully before the new one starts.
pub struct DetailsController {
    /// Currently observed location, guarded for cross-thread access.
    location: RwLock<Option<Arc<dyn LocationViewModel>>>,
    /// Cached entry snapshot (same order as the view model's sorted view).
    entries: RwLock<Vec<Entry>>,
    /// Selection state.
    selection: RwLock<Selection>,
    /// Focused row index (`NO_FOCUS` when unset).
    focused: AtomicUsize,
    /// Column definitions, including sort state.
    columns: RwLock<Vec<ColumnSpec>>,
    /// Which pane (0 or 1) this controller drives.
    pane: usize,
    /// Shared action sink for emitting navigation actions.
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
    /// Weak reference to the Slint window for property updates.
    window: slint::Weak<AtlasWindow>,
    /// Background subscription thread state.
    subscription: Mutex<Option<SubscriptionState>>,
}

impl DetailsController {
    /// Create a new controller for the given pane index.
    #[must_use]
    pub fn new(
        pane: usize,
        window: slint::Weak<AtlasWindow>,
        actions: Arc<Mutex<Box<dyn ActionSink>>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            location: RwLock::new(None),
            entries: RwLock::new(Vec::new()),
            selection: RwLock::new(Selection::default()),
            focused: AtomicUsize::new(NO_FOCUS),
            columns: RwLock::new(default_columns()),
            pane,
            actions,
            window,
            subscription: Mutex::new(None),
        })
    }

    /// Replace the current location and start streaming its entries.
    pub fn set_location(self: &Arc<Self>, location: Arc<dyn LocationViewModel>) {
        self.stop_subscription();

        {
            *self.entries.write() = Vec::new();
            *self.selection.write() = Selection::default();
            self.focused.store(NO_FOCUS, Ordering::Relaxed);
        }
        *self.location.write() = Some(Arc::clone(&location));

        let rx = location.subscribe();
        let (stop_tx, stop_rx) = unbounded();
        let controller = Arc::clone(self);

        match std::thread::Builder::new()
            .name(format!("atlas-details-pane{}", self.pane))
            .spawn(move || controller.run_subscription(rx, stop_rx))
        {
            Ok(handle) => {
                *self.subscription.lock() = Some(SubscriptionState { handle, stop_tx });
            }
            Err(error) => {
                tracing::error!(pane = self.pane, %error, "failed to spawn details subscription thread");
            }
        }

        self.refresh_from_location();
    }

    /// Apply a sort on the given column kind.
    pub fn apply_sort(self: &Arc<Self>, kind: ColumnKind, order: SortOrder) {
        let sort_key = match kind {
            ColumnKind::Name => SortKey::Name,
            ColumnKind::Size => SortKey::Size,
            ColumnKind::Modified => SortKey::Modified,
            ColumnKind::Kind => SortKey::Kind,
            ColumnKind::Extension => SortKey::Extension,
        };

        let spec = SortSpec {
            key: sort_key,
            order,
            dirs_first: true,
            natural: true,
            case_insensitive: true,
        };

        {
            let mut columns = self.columns.write();
            for column in &mut *columns {
                column.sort = if column.kind == kind {
                    Some(order)
                } else {
                    None
                };
            }
        }

        if let Some(location) = self.location.read().as_deref() {
            location.set_sort(spec);
        }
        self.push_columns_to_ui();
    }

    /// Handle a column header click: toggle or set sort on that column.
    pub fn header_clicked(self: &Arc<Self>, col_index: usize) {
        let (kind, current_sort) = {
            let columns = self.columns.read();
            let Some(column) = columns.get(col_index) else {
                return;
            };
            (column.kind, column.sort)
        };

        let order = match current_sort {
            Some(SortOrder::Asc) => SortOrder::Desc,
            _ => SortOrder::Asc,
        };

        self.apply_sort(kind, order);
    }

    /// Update the selection model for a clicked row.
    pub fn select_index(self: &Arc<Self>, index: usize, ctrl: bool, shift: bool) {
        let len = self.entries.read().len();
        if index >= len {
            return;
        }
        let anchor = self.selection.read().anchor;

        {
            let mut selection = self.selection.write();
            selection.resize(len);
            if shift {
                let from = anchor.unwrap_or(index);
                selection.select_range(from, index);
            } else if ctrl {
                selection.toggle(index);
            } else {
                selection.select_single(index);
            }
        }

        if !shift {
            self.focused.store(index, Ordering::Relaxed);
        }

        self.push_selection_to_ui();
    }

    /// Activate the currently focused row.
    pub fn activate_focused(self: &Arc<Self>) {
        let focused_index = self.focused.load(Ordering::Relaxed);
        if focused_index == NO_FOCUS {
            return;
        }

        // Extract the target under a short-lived read lock so we don't hold
        // the lock while dispatching. Dispatch may synchronously trigger
        // `set_location`, which needs the corresponding write lock —
        // parking_lot::RwLock is non-reentrant, so holding a read across the
        // dispatch would deadlock the UI thread.
        let target = {
            let entries = self.entries.read();
            entries
                .get(focused_index)
                .filter(|entry| entry.kind.is_dir())
                .map(|entry| entry.path.clone())
        };

        if let Some(path) = target {
            self.actions.lock().dispatch(UiAction::Navigate {
                pane: self.pane,
                path,
            });
        }
    }

    /// Move focus by `delta` rows, clamped to the valid range.
    pub fn move_focus(self: &Arc<Self>, delta: i64) {
        let len = self.entries.read().len();
        if len == 0 {
            return;
        }

        let current = self.focused.load(Ordering::Relaxed);
        let current_i64 = if current == NO_FOCUS {
            0
        } else {
            current as i64
        };
        let next = (current_i64 + delta).clamp(0, (len as i64) - 1) as usize;
        self.focused.store(next, Ordering::Relaxed);
        self.push_selection_to_ui();
    }

    fn stop_subscription(&self) {
        let subscription = self.subscription.lock().take();
        if let Some(SubscriptionState { handle, stop_tx }) = subscription {
            if let Err(error) = stop_tx.send(()) {
                tracing::debug!(pane = self.pane, %error, "details subscription already stopped");
            }
            if let Err(error) = handle.join() {
                tracing::warn!(
                    pane = self.pane,
                    ?error,
                    "details subscription thread panicked"
                );
            }
        }
    }

    fn run_subscription(self: Arc<Self>, rx: Receiver<ViewModelEvent>, stop_rx: Receiver<()>) {
        loop {
            select! {
                recv(stop_rx) -> _ => break,
                recv(rx) -> event => {
                    let Ok(event) = event else {
                        break;
                    };
                    match event {
                        ViewModelEvent::EntriesChanged | ViewModelEvent::Loaded => {
                            self.refresh_from_location();
                        }
                        ViewModelEvent::Error(message) => {
                            tracing::warn!(pane = self.pane, %message, "location view model error");
                        }
                    }
                }
            }
        }
    }

    fn refresh_from_location(&self) {
        let snapshot = {
            let location = self.location.read();
            location.as_deref().map(LocationViewModel::entries)
        };
        let Some(entries) = snapshot else {
            return;
        };

        let len = entries.len();
        let row_items: Vec<EntryRowItem> = entries.iter().map(entry_to_row_item).collect();

        {
            let mut stored = self.entries.write();
            *stored = entries;
        }

        let (mask, anchor) = {
            let mut selection = self.selection.write();
            selection.resize(len);
            if let Some(anchor) = selection.anchor {
                if anchor >= len {
                    selection.anchor = None;
                }
            }
            (
                selection.mask.clone(),
                selection.anchor.map_or(-1, |value| value as i32),
            )
        };

        let focused = self.focused.load(Ordering::Relaxed);
        let focused_i32 = if focused == NO_FOCUS || focused >= len {
            self.focused.store(NO_FOCUS, Ordering::Relaxed);
            -1
        } else {
            focused as i32
        };

        let column_specs: Vec<crate::ColumnSpec> = self
            .columns
            .read()
            .iter()
            .map(ColumnSpec::to_slint)
            .collect();

        let pane = self.pane;
        let window = self.window.clone();

        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = window.upgrade() else {
                return;
            };

            let rows_model = ModelRc::new(VecModel::from(row_items));
            let columns_model = ModelRc::new(VecModel::from(column_specs));
            let mask_model = ModelRc::new(VecModel::from(mask));

            if pane == 0 {
                window.set_pane0_details_rows(rows_model);
                window.set_pane0_details_columns(columns_model);
                window.set_pane0_details_selected_mask(mask_model);
                window.set_pane0_details_selected_anchor(anchor);
                window.set_pane0_details_focused_index(focused_i32);
            } else {
                window.set_pane1_details_rows(rows_model);
                window.set_pane1_details_columns(columns_model);
                window.set_pane1_details_selected_mask(mask_model);
                window.set_pane1_details_selected_anchor(anchor);
                window.set_pane1_details_focused_index(focused_i32);
            }
        });
    }

    fn push_columns_to_ui(&self) {
        let column_specs: Vec<crate::ColumnSpec> = self
            .columns
            .read()
            .iter()
            .map(ColumnSpec::to_slint)
            .collect();
        let pane = self.pane;
        let window = self.window.clone();

        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            let columns_model = ModelRc::new(VecModel::from(column_specs));
            if pane == 0 {
                window.set_pane0_details_columns(columns_model);
            } else {
                window.set_pane1_details_columns(columns_model);
            }
        });
    }

    fn push_selection_to_ui(&self) {
        let mask = self.selection.read().mask.clone();
        let anchor = self
            .selection
            .read()
            .anchor
            .map_or(-1, |value| value as i32);
        let focused = self.focused.load(Ordering::Relaxed);
        let focused_i32 = if focused == NO_FOCUS {
            -1
        } else {
            focused as i32
        };

        let pane = self.pane;
        let window = self.window.clone();

        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            let mask_model = ModelRc::new(VecModel::from(mask));

            if pane == 0 {
                window.set_pane0_details_selected_mask(mask_model);
                window.set_pane0_details_selected_anchor(anchor);
                window.set_pane0_details_focused_index(focused_i32);
            } else {
                window.set_pane1_details_selected_mask(mask_model);
                window.set_pane1_details_selected_anchor(anchor);
                window.set_pane1_details_focused_index(focused_i32);
            }
        });
    }
}

impl Drop for DetailsController {
    fn drop(&mut self) {
        self.stop_subscription();
    }
}

/// Convert an [`atlas_fs::Entry`] to the Slint-generated [`crate::EntryRowItem`] struct.
#[must_use]
fn entry_to_row_item(entry: &Entry) -> EntryRowItem {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_selection(len: usize) -> Selection {
        let mut selection = Selection::default();
        selection.resize(len);
        selection
    }

    #[test]
    fn selection_single_clears_others() {
        let mut selection = make_selection(5);
        selection.select_single(2);
        assert!(selection.mask[2]);
        assert!(!selection.mask[0]);
        assert!(!selection.mask[4]);
        assert_eq!(selection.anchor, Some(2));
    }

    #[test]
    fn selection_ctrl_toggles() {
        let mut selection = make_selection(5);
        selection.select_single(0);
        selection.toggle(2);
        assert!(selection.mask[0]);
        assert!(selection.mask[2]);
        selection.toggle(0);
        assert!(!selection.mask[0]);
    }

    #[test]
    fn selection_shift_range_forward() {
        let mut selection = make_selection(10);
        selection.select_single(2);
        selection.select_range(2, 5);
        assert!(selection.mask[2]);
        assert!(selection.mask[3]);
        assert!(selection.mask[4]);
        assert!(selection.mask[5]);
        assert!(!selection.mask[6]);
    }

    #[test]
    fn selection_shift_range_backward() {
        let mut selection = make_selection(10);
        selection.select_range(5, 2);
        assert!(selection.mask[2]);
        assert!(selection.mask[3]);
        assert!(selection.mask[4]);
        assert!(selection.mask[5]);
    }

    #[test]
    fn move_focus_clamps_at_zero() {
        let current = 0_i64;
        let delta = -5_i64;
        let len = 10_usize;
        let next = (current + delta).clamp(0, (len as i64) - 1) as usize;
        assert_eq!(next, 0);
    }

    #[test]
    fn move_focus_clamps_at_end() {
        let current = 8_i64;
        let delta = 10_i64;
        let len = 10_usize;
        let next = (current + delta).clamp(0, (len as i64) - 1) as usize;
        assert_eq!(next, 9);
    }
}
