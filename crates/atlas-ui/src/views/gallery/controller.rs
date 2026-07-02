//! Gallery view controller.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
};

use ahash::AHashMap;
use atlas_fs::{Entry, EntryKind, LocationViewModel, ViewModelEvent};
use atlas_thumbs::{can_thumbnail, SqliteCache};
use crossbeam_channel::{unbounded, Sender};
use parking_lot::{Mutex, RwLock};

use crate::{
    actions::ActionSink,
    models::split::PaneId,
    shell::AppShell,
    theming::icons::icon_for,
    views::{
        details::{format_relative_time, format_size},
        gallery::{
            metadata::{self, Metadata},
            thumbs::{
                DecodedPixels, GalleryThumbEvent, GalleryThumbRequester, GalleryThumbTarget,
                PREVIEW_TARGET_DIM, STRIP_TARGET_DIM,
            },
        },
        grid::controller::entry_to_row_item,
    },
    EntryRowItem, MetadataFields,
};

const NO_FOCUS: usize = usize::MAX;

struct SubscriptionState {
    handle: std::thread::JoinHandle<()>,
    stop_tx: Sender<()>,
}

#[derive(Clone, Debug, Default)]
struct UiMetadataFields {
    name: String,
    path: String,
    size_text: String,
    modified_text: String,
    kind: String,
    dimensions: String,
}

/// Drives the Slint Gallery view from a [`atlas_fs::LocationViewModel`] stream.
pub struct GalleryController {
    pane_id: PaneId,
    location: RwLock<Option<Arc<dyn LocationViewModel>>>,
    entries: RwLock<Vec<Entry>>,
    focused: AtomicUsize,
    strip_thumbs: RwLock<Vec<Option<DecodedPixels>>>,
    preview: RwLock<Option<DecodedPixels>>,
    preview_cache: RwLock<AHashMap<PathBuf, DecodedPixels>>,
    preview_path: RwLock<Option<PathBuf>>,
    preview_loading: AtomicBool,
    preview_fallback_glyph: RwLock<String>,
    metadata: RwLock<UiMetadataFields>,
    metadata_generation: AtomicU64,
    thumb_requester: Arc<GalleryThumbRequester>,
    subscription: Mutex<Option<SubscriptionState>>,
    shell: std::sync::Weak<AppShell>,
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
}

impl GalleryController {
    /// Construct a new controller for `pane_id`.
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
        Arc::new_cyclic(|weak: &std::sync::Weak<Self>| {
            let weak_ctrl = weak.clone();
            let requester = GalleryThumbRequester::new(
                cache,
                format!("atlas-gallery-thumbs-pane{}", pane_id.0),
                Arc::new(move |event| {
                    if let Some(controller) = weak_ctrl.upgrade() {
                        controller.handle_thumb_event(event);
                    }
                }),
                worker_count,
                max_cache_bytes,
                thumbs_enabled,
                max_file_bytes,
            );

            Self {
                pane_id,
                location: RwLock::new(None),
                entries: RwLock::new(Vec::new()),
                focused: AtomicUsize::new(NO_FOCUS),
                strip_thumbs: RwLock::new(Vec::new()),
                preview: RwLock::new(None),
                preview_cache: RwLock::new(AHashMap::new()),
                preview_path: RwLock::new(None),
                preview_loading: AtomicBool::new(false),
                preview_fallback_glyph: RwLock::new(String::new()),
                metadata: RwLock::new(UiMetadataFields::default()),
                metadata_generation: AtomicU64::new(0),
                thumb_requester: requester,
                subscription: Mutex::new(None),
                shell,
                actions,
            }
        })
    }

    /// Replace the current location and begin streaming its entries.
    pub fn set_location(self: &Arc<Self>, location: Arc<dyn LocationViewModel>) {
        self.stop_subscription();
        self.thumb_requester.reset();

        *self.location.write() = Some(Arc::clone(&location));
        *self.entries.write() = Vec::new();
        *self.strip_thumbs.write() = Vec::new();
        *self.preview.write() = None;
        self.preview_cache.write().clear();
        *self.preview_path.write() = None;
        *self.preview_fallback_glyph.write() = String::new();
        *self.metadata.write() = UiMetadataFields::default();
        self.preview_loading.store(false, Ordering::Relaxed);
        self.focused.store(NO_FOCUS, Ordering::Relaxed);
        self.metadata_generation.fetch_add(1, Ordering::Relaxed);

        let rx = location.subscribe();
        let (stop_tx, stop_rx) = unbounded();
        let controller = Arc::clone(self);
        match std::thread::Builder::new()
            .name(format!("atlas-gallery-pane{}", self.pane_id.0))
            .spawn(move || controller.run_subscription(rx, stop_rx))
        {
            Ok(handle) => {
                *self.subscription.lock() = Some(SubscriptionState { handle, stop_tx });
            }
            Err(error) => {
                tracing::error!(pane = self.pane_id.0, %error, "failed to spawn gallery subscription thread");
            }
        }

        self.refresh_from_location();
    }

    /// Focus the entry at `index`.
    pub fn entry_clicked(self: &Arc<Self>, index: usize) {
        self.set_focused(index);
    }

    /// Return a clone of the [`Entry`] at `index` without mutating focus
    /// state.  Companion to [`Self::entry_clicked`] used by the
    /// context-menu shell handler so it can build a full `ContextTarget`
    /// directly from the right-clicked cell (needs `entry.kind`, not just
    /// the path).  Matches the semantic contract in the convergence rule:
    /// build ContextTarget from `(pane_location, entry)`.
    #[must_use]
    pub fn entry_at(&self, index: usize) -> Option<atlas_fs::Entry> {
        self.entries.read().get(index).cloned()
    }

    /// Request the strip thumbnail for `index`.
    pub fn strip_visible(self: &Arc<Self>, index: usize) {
        let entries = self.entries.read();
        let Some(entry) = entries.get(index) else {
            return;
        };
        if entry.kind.is_dir() || !can_thumbnail(&entry.path) {
            return;
        }
        self.thumb_requester.request(
            entry.path.clone(),
            STRIP_TARGET_DIM,
            GalleryThumbTarget::Strip(index),
        );
    }

    /// Ensure the preview thumbnail for `index` is requested.
    pub fn preview_visible(self: &Arc<Self>, index: usize) {
        self.request_preview_for_index(index);
    }

    /// Activate the focused entry when it is a directory.
    pub fn activate_focused(&self) {
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
            self.actions
                .lock()
                .dispatch(crate::actions::UiAction::Navigate { pane: slot, path });
        }
    }

    /// Move focus one entry to the left.
    pub fn prev_image(self: &Arc<Self>) {
        self.move_focus(-1);
    }

    /// Move focus one entry to the right.
    pub fn next_image(self: &Arc<Self>) {
        self.move_focus(1);
    }

    /// Move focus by `delta`, clamping at directory bounds.
    pub fn move_focus(self: &Arc<Self>, delta: isize) {
        let len = self.entries.read().len();
        if len == 0 {
            return;
        }
        let current = self.focused.load(Ordering::Relaxed);
        let current = if current == NO_FOCUS { 0 } else { current };
        let next = current
            .saturating_add_signed(delta)
            .min(len.saturating_sub(1));
        self.set_focused(next);
    }

    fn set_focused(self: &Arc<Self>, index: usize) {
        let len = self.entries.read().len();
        if index >= len {
            return;
        }

        self.focused.store(index, Ordering::Relaxed);
        self.push_focus_to_ui();
        self.update_preview_state(index);
        self.update_metadata_state(index);
    }

    fn stop_subscription(&self) {
        let state = self.subscription.lock().take();
        if let Some(SubscriptionState { handle, stop_tx }) = state {
            if let Err(error) = stop_tx.send(()) {
                tracing::debug!(pane = self.pane_id.0, %error, "gallery subscription already stopped");
            }
            if let Err(error) = handle.join() {
                tracing::warn!(
                    pane = self.pane_id.0,
                    ?error,
                    "gallery subscription thread panicked"
                );
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
                        ViewModelEvent::EntriesChanged | ViewModelEvent::Loaded => self.refresh_from_location(),
                        ViewModelEvent::Error(message) => {
                            tracing::warn!(pane = self.pane_id.0, %message, "gallery location error");
                        }
                    }
                }
            }
        }
    }

    fn refresh_from_location(self: &Arc<Self>) {
        let snapshot = {
            let location = self.location.read();
            location.as_deref().map(LocationViewModel::entries)
        };
        let Some(entries) = snapshot else { return };

        let len = entries.len();
        let row_items: Vec<EntryRowItem> = entries.iter().map(entry_to_row_item).collect();
        *self.entries.write() = entries.clone();
        *self.strip_thumbs.write() = vec![None; len];
        *self.preview.write() = None;
        self.preview_cache.write().clear();
        *self.preview_path.write() = None;
        self.preview_loading.store(false, Ordering::Relaxed);
        *self.preview_fallback_glyph.write() = String::new();
        *self.metadata.write() = UiMetadataFields::default();

        self.push_rows_to_ui(row_items);
        self.push_strip_to_ui();
        self.push_preview_to_ui();
        self.push_metadata_to_ui();

        for (index, entry) in entries.iter().enumerate() {
            if !entry.kind.is_dir() && can_thumbnail(&entry.path) {
                self.thumb_requester.request(
                    entry.path.clone(),
                    STRIP_TARGET_DIM,
                    GalleryThumbTarget::Strip(index),
                );
            }
        }

        if len == 0 {
            self.focused.store(NO_FOCUS, Ordering::Relaxed);
            self.push_focus_to_ui();
            return;
        }

        let focused = self.focused.load(Ordering::Relaxed);
        let initial_focus = if focused == NO_FOCUS || focused >= len {
            0
        } else {
            focused
        };
        self.set_focused(initial_focus);
    }

    fn update_preview_state(self: &Arc<Self>, index: usize) {
        let Some(entry) = self.entries.read().get(index).cloned() else {
            return;
        };
        let fallback = fallback_glyph(&entry);
        if entry.kind.is_dir() || !can_thumbnail(&entry.path) {
            *self.preview.write() = None;
            *self.preview_path.write() = None;
            *self.preview_fallback_glyph.write() = fallback.to_string();
            self.preview_loading.store(false, Ordering::Relaxed);
            self.push_preview_to_ui();
            return;
        }

        *self.preview_path.write() = Some(entry.path.clone());
        *self.preview_fallback_glyph.write() = String::new();

        if let Some(decoded) = self.preview_cache.read().get(&entry.path).cloned() {
            *self.preview.write() = Some(decoded);
            self.preview_loading.store(false, Ordering::Relaxed);
            self.push_preview_to_ui();
            return;
        }

        *self.preview.write() = None;
        self.preview_loading.store(true, Ordering::Relaxed);
        self.push_preview_to_ui();
        self.thumb_requester.request(
            entry.path.clone(),
            PREVIEW_TARGET_DIM,
            GalleryThumbTarget::Preview(entry.path.clone()),
        );
    }

    fn request_preview_for_index(&self, index: usize) {
        let entries = self.entries.read();
        let Some(entry) = entries.get(index) else {
            return;
        };
        if entry.kind.is_dir() || !can_thumbnail(&entry.path) {
            return;
        }
        if self.preview_cache.read().contains_key(&entry.path) {
            return;
        }
        self.thumb_requester.request(
            entry.path.clone(),
            PREVIEW_TARGET_DIM,
            GalleryThumbTarget::Preview(entry.path.clone()),
        );
    }

    fn preload_next_preview(&self, current_index: usize) {
        let next_index = current_index.saturating_add(1);
        self.request_preview_for_index(next_index);
    }

    fn update_metadata_state(self: &Arc<Self>, index: usize) {
        let Some(entry) = self.entries.read().get(index).cloned() else {
            return;
        };
        *self.metadata.write() = metadata_placeholder(&entry);
        self.push_metadata_to_ui();

        let generation = self.metadata_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let weak = Arc::downgrade(self);
        let path = entry.path.clone();
        let kind = entry.kind.clone();
        if let Err(error) = std::thread::Builder::new()
            .name(format!("atlas-gallery-metadata-pane{}", self.pane_id.0))
            .spawn(move || {
                let Ok(meta) = std::fs::metadata(&path) else {
                    tracing::debug!(path = ?path, "gallery metadata stat failed");
                    return;
                };
                let extracted = metadata::extract(&path, &meta, kind);
                if let Some(controller) = weak.upgrade() {
                    controller.apply_metadata_result(generation, &path, extracted);
                }
            })
        {
            tracing::warn!(pane = self.pane_id.0, %error, "failed to spawn gallery metadata thread");
        }
    }

    fn apply_metadata_result(&self, generation: u64, path: &PathBuf, metadata: Metadata) {
        if self.metadata_generation.load(Ordering::Relaxed) != generation {
            return;
        }
        let current_path = self.current_focused_path();
        if current_path.as_ref() != Some(path) {
            return;
        }

        *self.metadata.write() = metadata_to_ui(metadata);
        self.push_metadata_to_ui();
    }

    fn handle_thumb_event(&self, event: GalleryThumbEvent) {
        let mut push_strip = false;
        let mut push_preview = false;
        let current_focus = self.focused.load(Ordering::Relaxed);

        for target in event.targets {
            match target {
                GalleryThumbTarget::Strip(index) => {
                    let matches = self
                        .entries
                        .read()
                        .get(index)
                        .is_some_and(|entry| entry.path == event.path);
                    if !matches {
                        continue;
                    }
                    let Some(pixels) = event.pixels.clone() else {
                        continue;
                    };
                    if let Some(slot) = self.strip_thumbs.write().get_mut(index) {
                        *slot = Some(pixels);
                        push_strip = true;
                    }
                }
                GalleryThumbTarget::Preview(path) => {
                    if path != event.path {
                        continue;
                    }

                    if let Some(pixels) = event.pixels.clone() {
                        self.preview_cache
                            .write()
                            .insert(path.clone(), pixels.clone());
                        let current_preview = self.preview_path.read().clone();
                        if current_preview.as_ref() == Some(&path) {
                            *self.preview.write() = Some(pixels);
                            *self.preview_fallback_glyph.write() = String::new();
                            self.preview_loading.store(false, Ordering::Relaxed);
                            push_preview = true;
                        }
                    } else {
                        let current_preview = self.preview_path.read().clone();
                        if current_preview.as_ref() == Some(&path) {
                            *self.preview.write() = None;
                            *self.preview_fallback_glyph.write() = self
                                .current_focused_entry()
                                .map(|entry| fallback_glyph(&entry).to_string())
                                .unwrap_or_default();
                            self.preview_loading.store(false, Ordering::Relaxed);
                            push_preview = true;
                        }
                    }
                }
            }
        }

        if push_strip {
            self.push_strip_to_ui();
        }
        if push_preview {
            self.push_preview_to_ui();
            if current_focus != NO_FOCUS {
                self.preload_next_preview(current_focus);
            }
        }
    }

    fn current_focused_entry(&self) -> Option<Entry> {
        let focused = self.focused.load(Ordering::Relaxed);
        if focused == NO_FOCUS {
            return None;
        }
        self.entries.read().get(focused).cloned()
    }

    fn current_focused_path(&self) -> Option<PathBuf> {
        self.current_focused_entry().map(|entry| entry.path)
    }

    fn push_rows_to_ui(&self, row_items: Vec<EntryRowItem>) {
        if let Some(shell) = self.shell.upgrade() {
            shell.publish_details_rows(self.pane_id, row_items);
        }
    }

    fn push_strip_to_ui(&self) {
        let decoded = self.strip_thumbs.read().clone();
        if let Some(shell) = self.shell.upgrade() {
            shell.publish_gallery_strip_thumbs(self.pane_id, decoded);
        }
    }

    fn push_focus_to_ui(&self) {
        let focused = self.focused.load(Ordering::Relaxed);
        let focused = if focused == NO_FOCUS {
            -1
        } else {
            focused as i32
        };
        if let Some(shell) = self.shell.upgrade() {
            shell.publish_gallery_focused_index(self.pane_id, focused);
        }
    }

    fn push_preview_to_ui(&self) {
        let preview = self.preview.read().clone();
        let loading = self.preview_loading.load(Ordering::Relaxed);
        let fallback = self.preview_fallback_glyph.read().clone();
        if let Some(shell) = self.shell.upgrade() {
            shell.publish_gallery_preview(self.pane_id, preview, loading, fallback);
        }
    }

    fn push_metadata_to_ui(&self) {
        let metadata = self.metadata.read().clone();
        let metadata = MetadataFields {
            name: metadata.name.into(),
            path: metadata.path.into(),
            size_text: metadata.size_text.into(),
            modified_text: metadata.modified_text.into(),
            kind: metadata.kind.into(),
            dimensions: metadata.dimensions.into(),
        };
        if let Some(shell) = self.shell.upgrade() {
            shell.publish_gallery_metadata(self.pane_id, metadata);
        }
    }
}

impl Drop for GalleryController {
    fn drop(&mut self) {
        self.stop_subscription();
    }
}

fn fallback_glyph(entry: &Entry) -> char {
    icon_for(entry).glyph
}

fn metadata_placeholder(entry: &Entry) -> UiMetadataFields {
    UiMetadataFields {
        name: entry.name.clone(),
        path: entry.path.to_string_lossy().into_owned(),
        size_text: if entry.kind.is_dir() {
            "—".to_owned()
        } else {
            format_size(entry.metadata.size)
        },
        modified_text: entry
            .metadata
            .modified
            .map(format_relative_time)
            .unwrap_or_else(|| "—".to_owned()),
        kind: match entry.kind {
            EntryKind::File => "File",
            EntryKind::Dir => "Directory",
            EntryKind::Symlink { .. } => "Symlink",
            EntryKind::Other => "Other",
        }
        .to_owned(),
        dimensions: String::new(),
    }
}

fn metadata_to_ui(metadata: Metadata) -> UiMetadataFields {
    UiMetadataFields {
        name: metadata.name,
        path: metadata.path,
        size_text: metadata.size_text,
        modified_text: metadata.modified_text,
        kind: metadata.kind,
        dimensions: metadata
            .dimensions
            .map(|(width, height)| format!("{width} × {height}"))
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_fs::{InMemoryLocationViewModel, OpenOptions};
    use image::RgbaImage;
    use std::time::Duration;
    use tempfile::TempDir;

    struct NoopSink;
    impl ActionSink for NoopSink {
        fn dispatch(&mut self, _action: crate::actions::UiAction) {}
    }

    fn make_controller(dir: &TempDir) -> Arc<GalleryController> {
        let actions: Arc<Mutex<Box<dyn ActionSink>>> = Arc::new(Mutex::new(Box::new(NoopSink)));
        let cache_dir = dir.path().join(".cache");
        std::fs::create_dir_all(&cache_dir).expect("create cache dir");
        let cache = Arc::new(
            SqliteCache::open(&cache_dir.join("thumbs.db")).expect("open thumbnail cache"),
        );
        GalleryController::new(
            PaneId(0),
            std::sync::Weak::new(),
            actions,
            cache,
            0,
            500 * 1024 * 1024,
            true,
            u64::MAX,
        )
    }

    fn wait_for<F: Fn() -> bool>(predicate: F, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn make_image_dir() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        for index in 0..5 {
            let path = dir.path().join(format!("img{index}.png"));
            RgbaImage::new(16 + index, 16 + index)
                .save(&path)
                .expect("save png");
        }
        dir
    }

    #[test]
    fn set_location_defaults_focus_and_strip_slots() {
        let dir = make_image_dir();
        let controller = make_controller(&dir);
        let vm_typed = InMemoryLocationViewModel::open(dir.path(), OpenOptions::default());
        assert!(wait_for(|| vm_typed.len() == 5, Duration::from_secs(5)));
        let vm: Arc<dyn LocationViewModel> = vm_typed;

        controller.set_location(vm);

        let ready = wait_for(
            || {
                controller.focused.load(Ordering::Relaxed) == 0
                    && controller.strip_thumbs.read().len() == 5
            },
            Duration::from_secs(5),
        );
        assert!(ready, "gallery controller should load entries");
    }

    #[test]
    fn move_focus_stops_at_bounds() {
        let dir = make_image_dir();
        let controller = make_controller(&dir);
        let vm_typed = InMemoryLocationViewModel::open(dir.path(), OpenOptions::default());
        assert!(wait_for(|| vm_typed.len() == 5, Duration::from_secs(5)));
        let vm: Arc<dyn LocationViewModel> = vm_typed;
        controller.set_location(vm);
        assert!(wait_for(
            || controller.entries.read().len() == 5,
            Duration::from_secs(5)
        ));

        controller.move_focus(1);
        assert_eq!(controller.focused.load(Ordering::Relaxed), 1);

        controller.set_focused(4);
        controller.move_focus(1);
        assert_eq!(controller.focused.load(Ordering::Relaxed), 4);
    }
}
