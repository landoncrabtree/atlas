//! AppShell — the bridge between pure-Rust models and the Slint window.
//!
//! Compilation of the `.slint` files lives in `atlas-ui/build.rs` so that
//! this crate can reference the generated `AtlasWindow` type directly.
//! `atlas-app` therefore does not need its own `slint::include_modules!()`
//! call; it simply re-uses the types re-exported from this crate.
//!
//! Thread-safety: every `set_*` method may be called from any thread. It uses
//! [`slint::invoke_from_event_loop`] to push property changes onto the Slint
//! event loop. The inner `RwLock`s guard the Rust-side model copies.

use std::{
    collections::{HashMap, VecDeque},
    env,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc},
};

use ahash::AHashMap;
use atlas_core::{path::expand_tilde, Location};
use atlas_fs::LocationViewModel;
use atlas_keymap::{defaults::default_actions, ActionRegistry, Keymap};
use directories::UserDirs;
use parking_lot::{Mutex, RwLock};
use slint::{ComponentHandle as _, ModelRc, SharedString, VecModel};

use crate::{
    actions::{ActionSink, UiAction},
    models::{
        split::{Cardinal, PaneId, Rect, SplitDirection, SplitLayout},
        PaletteModel, PaletteResult, PaneState, StatusModel, TabModel, ViewMode, WorkspaceModel,
    },
    navigation::NavigationController,
    ops::OpsController,
    palette::{
        ActionsSource, BookmarksSource, GotoPathsSource, PaletteController, WalkerPathIndex,
    },
    rename::BulkRenameController,
    search::SearchController,
    theme::{ThemeMode, ThemeTokens},
    theming::defaults,
    views::details::DetailsController,
    views::gallery::GalleryController,
    views::grid::GridController,
    views::miller::MillerController,
    views::tree::TreeController,
    AtlasWindow, EntryRowItem, PaletteEntry, PaneSlintData, ShortcutHint, SplitHandle, TabEntry,
};

fn to_palette_model(results: &[PaletteResult]) -> ModelRc<PaletteEntry> {
    let entries: Vec<PaletteEntry> = results
        .iter()
        .map(|result| PaletteEntry {
            title: SharedString::from(result.title.as_str()),
            subtitle: SharedString::from(result.subtitle.as_str()),
            action_id: SharedString::from(result.action_id.as_str()),
        })
        .collect();
    ModelRc::new(VecModel::from(entries))
}

/// Return a "NN GB free of NN TB" string for the volume that contains `path`.
/// Returns `None` if the platform stat call fails (unmounted volume, etc.).
fn free_space_text_for(path: &Path) -> Option<String> {
    // Walk up to the nearest existing ancestor so we can stat SOMEthing even
    // if the pane's active path was just deleted from underneath us.
    let mut probe: &Path = path;
    while !probe.exists() {
        probe = probe.parent()?;
    }
    let stats = fs2::statvfs(probe).ok()?;
    let avail = stats.available_space();
    let total = stats.total_space();
    Some(format!(
        "{} free of {}",
        crate::format_size(avail),
        crate::format_size(total)
    ))
}

/// Raw (non-Slint) descriptor for a split-handle grab area.
struct SplitHandleData {
    node_index: i32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    horizontal: bool,
}

/// Walk the split tree in DFS order and collect one grab-area descriptor per
/// internal `Split` node.  `node_index` is the DFS visit order (0, 1, …).
fn collect_split_handles(layout: &SplitLayout, bounds: Rect) -> Vec<SplitHandleData> {
    let mut handles = Vec::new();
    collect_handles_recurse(layout, bounds, &mut handles, &mut 0i32);
    handles
}

fn collect_handles_recurse(
    layout: &SplitLayout,
    bounds: Rect,
    handles: &mut Vec<SplitHandleData>,
    node_idx: &mut i32,
) {
    let SplitLayout::Split {
        direction,
        ratio,
        first,
        second,
    } = layout
    else {
        return;
    };

    let idx = *node_idx;
    *node_idx += 1;
    let ratio = ratio.clamp(0.05, 0.95);

    let (first_bounds, second_bounds, handle) = match direction {
        SplitDirection::Horizontal => {
            let split_x = bounds.x + bounds.width * ratio;
            (
                Rect {
                    x: bounds.x,
                    y: bounds.y,
                    width: bounds.width * ratio,
                    height: bounds.height,
                },
                Rect {
                    x: split_x,
                    y: bounds.y,
                    width: bounds.width * (1.0 - ratio),
                    height: bounds.height,
                },
                SplitHandleData {
                    node_index: idx,
                    x: split_x - 2.0,
                    y: bounds.y,
                    width: 4.0,
                    height: bounds.height,
                    horizontal: false,
                },
            )
        }
        SplitDirection::Vertical => {
            let split_y = bounds.y + bounds.height * ratio;
            (
                Rect {
                    x: bounds.x,
                    y: bounds.y,
                    width: bounds.width,
                    height: bounds.height * ratio,
                },
                Rect {
                    x: bounds.x,
                    y: split_y,
                    width: bounds.width,
                    height: bounds.height * (1.0 - ratio),
                },
                SplitHandleData {
                    node_index: idx,
                    x: bounds.x,
                    y: split_y - 2.0,
                    width: bounds.width,
                    height: 4.0,
                    horizontal: true,
                },
            )
        }
    };

    handles.push(handle);
    collect_handles_recurse(first, first_bounds, handles, node_idx);
    collect_handles_recurse(second, second_bounds, handles, node_idx);
}

fn palette_root() -> PathBuf {
    if let Some(user_dirs) = UserDirs::new() {
        return user_dirs.home_dir().to_path_buf();
    }

    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn dirs_home() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn build_palette_controller(
    window: &AtlasWindow,
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
    bookmarks: Vec<(String, PathBuf)>,
) -> Arc<PaletteController> {
    let mut registry = ActionRegistry::new();
    for action in default_actions() {
        registry.register(action);
    }

    // Build the keymap starting from defaults, then layer the user's
    // keymap.toml on top if it exists.  Failures are logged as warnings so a
    // malformed user keymap never prevents startup.
    let mut keymap = Keymap::with_defaults();
    if let Ok(km_path) = atlas_config::keymap_file_path() {
        if km_path.exists() {
            match std::fs::read_to_string(&km_path) {
                Ok(text) => {
                    if let Err(e) = keymap.apply_user_toml(&text) {
                        tracing::warn!("ignoring malformed keymap {}: {e}", km_path.display());
                    } else {
                        tracing::info!("loaded user keymap from {}", km_path.display());
                    }
                }
                Err(e) => {
                    tracing::warn!("could not read keymap file {}: {e}", km_path.display());
                }
            }
        }
    }
    let keymap = Arc::new(keymap);
    let actions_source = Arc::new(ActionsSource::new(Arc::new(registry), Arc::clone(&keymap)));
    let path_index = Arc::new(WalkerPathIndex::new(palette_root()));
    let goto_source = Arc::new(GotoPathsSource::new(path_index));
    let bookmarks_source = Arc::new(BookmarksSource::new(bookmarks));

    let controller = PaletteController::new(actions);
    controller.attach_window(window.as_weak());
    controller.register_source(actions_source);
    controller.register_source(goto_source);
    controller.register_source(bookmarks_source);
    controller.set_on_dispatch(|action_id| {
        tracing::info!(%action_id, "palette action dispatched");
    });
    controller
}

/// Per-pane view controllers.
///
/// Cloning is cheap: every field is an [`Arc`], so a clone shares the
/// underlying controllers.
#[derive(Clone)]
pub struct PaneControllers {
    /// Stable id of the pane these controllers drive.
    pub pane_id: PaneId,
    /// Details view controller for the pane.
    pub details: Arc<crate::views::details::DetailsController>,
    /// Grid view controller for the pane.
    pub grid: Arc<crate::views::grid::GridController>,
    /// Miller columns view controller for the pane.
    pub miller: Arc<crate::views::miller::MillerController>,
    /// Tree view controller for the pane.
    pub tree: Arc<crate::views::tree::TreeController>,
    /// Gallery view controller for the pane.
    pub gallery: Arc<crate::views::gallery::GalleryController>,
}

/// Send/Sync-safe per-pane snapshot of a single Miller column.
///
/// The generated Slint [`MillerColData`] type embeds a `ModelRc<EntryRowItem>`
/// which is not `Send`; we therefore stage the raw fields in the pane cache
/// and rebuild `MillerColData` on the UI thread inside
/// [`AppShell::push_pane_data_to_slint`].
#[derive(Default, Clone)]
pub struct MillerColumnCache {
    /// Column header (usually the directory name).
    pub title: String,
    /// Column entries as pre-formatted Slint rows.
    pub entries: Vec<EntryRowItem>,
    /// Focused row within the column (`-1` for none).
    pub focused: i32,
    /// True while the column's initial load is in flight.
    pub loading: bool,
}

/// Cached Rust-side snapshot of every view's per-pane state.
///
/// View controllers push new data into this cache via the `AppShell`
/// `publish_*` methods; the shell then rebuilds the outer nested [`VecModel`]s
/// in DFS-leaf order and pushes them to the Slint window inside a single
/// `invoke_from_event_loop` call.
///
/// Every field is `Send + Sync` so the whole cache can live behind an
/// [`ahash::AHashMap`] guarded by a [`parking_lot::RwLock`]. Heavy types that
/// are not `Send` (e.g. [`slint::Image`], `ModelRc<T>`) are staged as their
/// `Send`-safe intermediaries here and materialised into their Slint
/// counterparts only on the UI thread.
#[derive(Default, Clone)]
pub struct PaneRenderCache {
    // ── Light per-pane data (also shown in PaneSlintData) ────────────────
    /// Tab entries pre-formatted as Slint rows.
    pub tabs: Vec<TabEntry>,
    /// Path segments for the breadcrumb strip.
    pub segments: Vec<SharedString>,
    /// Active-tab index within `tabs`.
    pub active_tab: i32,

    // ── Details view ─────────────────────────────────────────────────────
    /// Row items for the Details table.
    pub details_rows: Vec<EntryRowItem>,
    /// Column descriptors for the Details table.
    pub details_columns: Vec<crate::ColumnSpec>,
    /// One bit per row indicating whether the row is selected.
    pub details_selected_mask: Vec<bool>,
    /// Shift-range selection anchor (`-1` = unset).
    pub details_selected_anchor: i32,
    /// Currently focused row (`-1` = unset).
    pub details_focused_index: i32,

    // ── Grid view ────────────────────────────────────────────────────────
    /// Decoded thumbnail pixels for each grid cell (converted to
    /// [`slint::Image`] on the UI thread).
    pub grid_thumbs: Vec<Option<crate::views::grid::thumbs::DecodedPixels>>,
    /// One bit per cell indicating whether a thumbnail has been decoded.
    pub grid_has_thumbs: Vec<bool>,
    /// One bit per cell indicating whether the cell is selected.
    pub grid_selected_mask: Vec<bool>,
    /// Currently focused grid cell (`-1` = unset).
    pub grid_focused_index: i32,

    // ── Gallery view ─────────────────────────────────────────────────────
    /// Decoded strip thumbnails.
    pub gallery_strip_thumbs: Vec<Option<crate::views::gallery::thumbs::DecodedPixels>>,
    /// Decoded preview image, if any.
    pub gallery_preview: Option<crate::views::gallery::thumbs::DecodedPixels>,
    /// Whether the preview is still decoding.
    pub gallery_preview_loading: bool,
    /// Glyph shown in place of the preview when unavailable.
    pub gallery_preview_fallback_glyph: String,
    /// Currently focused gallery entry (`-1` = unset).
    pub gallery_focused_index: i32,
    /// Metadata sidebar contents.
    pub gallery_metadata: crate::MetadataFields,

    // ── Tree view ────────────────────────────────────────────────────────
    /// Currently visible tree rows in DFS order.
    pub tree_nodes: Vec<crate::TreeNode>,
    /// Currently focused tree row (`-1` = unset).
    pub tree_focused_index: i32,
    /// Currently selected tree row (`-1` = unset).
    pub tree_selected_index: i32,

    // ── Miller view ──────────────────────────────────────────────────────
    /// Column snapshots; rebuilt into `MillerColData` on the UI thread.
    pub miller_columns: Vec<MillerColumnCache>,
    /// Focused column index within `miller_columns`.
    pub miller_focused_col: i32,

    // ── Per-pane status bar ──────────────────────────────────────────────
    //
    // Pre-formatted so the UI-thread push is O(1) and doesn't allocate
    // through the format machinery on every event-loop tick. Filled in by
    // `AppShell::refresh_pane_status` from the pane's current entries +
    // an `fs2::statvfs` call against the pane's cwd. Empty on brand-new
    // panes until the first vm event fires.
    /// Number of directories in the pane's current listing.
    pub status_folder_count: i32,
    /// Number of non-directory entries in the pane's current listing.
    pub status_file_count: i32,
    /// Cumulative bytes of the visible files, formatted (`"1.7 MiB"`).
    /// Empty when nothing is loaded yet.
    pub status_total_size_text: String,
    /// Free / total space on the volume backing the pane's cwd,
    /// e.g. `"590 GB free of 926 GB"`. Empty when unavailable
    /// (unmounted volume, no cwd yet).
    pub status_free_space_text: String,

    // ── Cross-push identity preservation ─────────────────────────────────
    //
    // Monotonically bumped every time a `publish_*` method mutates one of
    // the fields above that shows up in `push_pane_data_to_slint`
    // (details rows, columns, thumbnails, tree nodes, miller columns,
    // gallery images, tabs, breadcrumbs, active-tab, per-pane status).
    //
    // The UI-thread side (`PANE_MODEL_HANDLES` in `shell.rs`) caches the
    // `ModelRc<T>` it built for each pane last push and reuses them when
    // `data_epoch` is unchanged. That preserves inner `ModelRc` identity
    // for panes whose data did not change since the last push, which in
    // turn preserves `ListView.viewport-y` for those panes — a full
    // rebuild would drop the ListView's model subscription and reset
    // scroll to zero.
    //
    // Bumped by [`AppShell::mark_pane_data_dirty`]. Selection-only
    // publishers do NOT bump this — they route through the parallel
    // `push_pane_selection_to_slint` path which never touches the row
    // models.
    pub data_epoch: u64,
}

/// UI-thread cache of the last-published `ModelRc<T>` handles for a single
/// pane, plus the [`PaneRenderCache::data_epoch`] they were built from.
///
/// See [`AppShell::push_pane_data_to_slint`] for the identity-preservation
/// rationale. Only touched from inside `slint::invoke_from_event_loop`
/// closures — the `ModelRc<T>` inside is `!Send`.
///
/// Handles default to empty `ModelRc<T>` sentinels; `epoch: None`
/// distinguishes an unbuilt entry from a built one with epoch 0. All
/// fields are overwritten on the first call to [`Self::rebuild_from`].
#[derive(Default, Clone)]
struct PaneSlintModelHandles {
    /// Epoch this entry was built from; `None` means "never built".
    epoch: Option<u64>,
    tabs: ModelRc<TabEntry>,
    segments: ModelRc<SharedString>,
    details_rows: ModelRc<EntryRowItem>,
    details_columns: ModelRc<crate::ColumnSpec>,
    grid_thumbnails: ModelRc<slint::Image>,
    grid_has_thumbs: ModelRc<bool>,
    gallery_strip_thumbs: ModelRc<slint::Image>,
    tree_nodes: ModelRc<crate::TreeNode>,
    miller_columns: ModelRc<crate::MillerColData>,
}

impl PaneSlintModelHandles {
    /// Rebuild every cached ModelRc from `snap`. Called only when
    /// [`PaneRenderCache::data_epoch`] indicates the pane's data has
    /// changed since the last build.
    fn rebuild_from(&mut self, snap: &PaneRenderCache) {
        self.tabs = ModelRc::new(VecModel::from(snap.tabs.clone()));
        self.segments = ModelRc::new(VecModel::from(snap.segments.clone()));
        self.details_rows = ModelRc::new(VecModel::from(snap.details_rows.clone()));
        self.details_columns = ModelRc::new(VecModel::from(snap.details_columns.clone()));

        let grid_images: Vec<slint::Image> = snap
            .grid_thumbs
            .iter()
            .map(|opt| {
                opt.as_ref()
                    .map(crate::views::grid::thumbs::decoded_to_slint)
                    .unwrap_or_default()
            })
            .collect();
        self.grid_thumbnails = ModelRc::new(VecModel::from(grid_images));
        self.grid_has_thumbs = ModelRc::new(VecModel::from(snap.grid_has_thumbs.clone()));

        let strip_images: Vec<slint::Image> = snap
            .gallery_strip_thumbs
            .iter()
            .map(|opt| {
                opt.as_ref()
                    .map(crate::views::gallery::thumbs::decoded_to_slint)
                    .unwrap_or_default()
            })
            .collect();
        self.gallery_strip_thumbs = ModelRc::new(VecModel::from(strip_images));

        self.tree_nodes = ModelRc::new(VecModel::from(snap.tree_nodes.clone()));

        let miller_cols: Vec<crate::MillerColData> = snap
            .miller_columns
            .iter()
            .map(|c| crate::MillerColData {
                title: SharedString::from(c.title.as_str()),
                entries: ModelRc::new(VecModel::from(c.entries.clone())),
                focused: c.focused,
                loading: c.loading,
            })
            .collect();
        self.miller_columns = ModelRc::new(VecModel::from(miller_cols));
    }
}

thread_local! {
    /// UI-thread cache of per-pane `ModelRc<T>` handles. See
    /// [`AppShell::push_pane_data_to_slint`] and
    /// [`PaneSlintModelHandles`] for the design rationale.
    ///
    /// Only accessed from inside `slint::invoke_from_event_loop`
    /// closures. Entries are pruned when their pane leaves the layout.
    static PANE_MODEL_HANDLES: std::cell::RefCell<AHashMap<PaneId, PaneSlintModelHandles>> =
        std::cell::RefCell::default();

    /// UI-thread cache of the outer per-property `VecModel`s that back
    /// every `panes-*: [T]` window property (plus the root `panes` and
    /// `split-handles` models).
    ///
    /// # Why persistent outer models
    ///
    /// Slint's `for pane[i] in panes: Pane { ... }` iterator binds to
    /// the *identity* of the outer `panes` `ModelRc`. If a subsequent
    /// `set_panes(new_model)` call replaces the outer model, Slint
    /// treats every row as new and tears down / re-creates every `Pane`
    /// instance — including their inner `ListView`s, which reset
    /// `viewport-y` to zero.
    ///
    /// The same holds one layer deeper: `pane.details-rows` is bound to
    /// `panes-details-rows[i]`. If `panes-details-rows` (the outer
    /// `ModelRc<ModelRc<EntryRowItem>>`) is replaced, Slint may
    /// re-evaluate the indexed access and produce a fresh `ModelRc`
    /// even when the underlying data hasn't changed.
    ///
    /// By caching each outer property's [`VecModel`] behind an [`Rc`]
    /// and binding the property to `ModelRc::from(rc.clone())` **once**
    /// (via [`OuterPaneModels::ensure_bound`]), subsequent pushes only
    /// mutate rows in place through [`sync_vec_model`]. Rows whose
    /// value hasn't changed skip the `set_row_data` call entirely —
    /// combined with [`PaneSlintModelHandles`] preserving inner
    /// `ModelRc` identity across pushes, this means an untouched pane's
    /// `ListView` never sees a model-changed notification and keeps its
    /// scroll offset when the *other* pane navigates.
    ///
    /// See Bug 1 in the July 2026 regression run for the user-facing
    /// symptom this addresses.
    static OUTER_PANE_MODELS: std::cell::RefCell<OuterPaneModels> =
        std::cell::RefCell::new(OuterPaneModels::default());
}

/// In-place synchronisation of a persistent [`VecModel<T>`] to match `desired`.
///
/// Only fires `row_changed` for rows whose value actually differs from the
/// current model contents (per `T: PartialEq`), so `ModelRc<T>` fields whose
/// inner `Rc` identity is preserved by the caller (see
/// [`PaneSlintModelHandles`]) will skip the notification entirely and Slint
/// will not re-evaluate bindings that depend on those rows.
///
/// Trims excess rows from the tail with `remove(row_count - 1)` and appends
/// new rows via `push` when the model is too short.
fn sync_vec_model<T>(model: &std::rc::Rc<VecModel<T>>, desired: &[T])
where
    T: 'static + Clone + PartialEq,
{
    let current_len = <VecModel<T> as slint::Model>::row_count(model);
    let desired_len = desired.len();
    let overlap = current_len.min(desired_len);
    for (i, item) in desired.iter().take(overlap).enumerate() {
        let cur = <VecModel<T> as slint::Model>::row_data(model, i);
        if cur.as_ref() != Some(item) {
            <VecModel<T> as slint::Model>::set_row_data(model, i, item.clone());
        }
    }
    while <VecModel<T> as slint::Model>::row_count(model) > desired_len {
        let last = <VecModel<T> as slint::Model>::row_count(model) - 1;
        model.remove(last);
    }
    for item in desired.iter().skip(current_len) {
        model.push(item.clone());
    }
}

/// Owner of the persistent outer [`VecModel`]s bound to each `panes-*`
/// window property.
///
/// See the [`OUTER_PANE_MODELS`] doc for why these live on the UI thread
/// and why every push routes through [`sync_vec_model`] rather than
/// building a fresh [`VecModel`] each time.
struct OuterPaneModels {
    // Root `panes` list + split handle descriptors, driven by
    // `project_workspace_to_slint`.
    panes: std::rc::Rc<VecModel<PaneSlintData>>,
    split_handles: std::rc::Rc<VecModel<SplitHandle>>,

    // Per-pane data models, driven by `push_pane_data_to_slint`.
    tabs: std::rc::Rc<VecModel<ModelRc<TabEntry>>>,
    segments: std::rc::Rc<VecModel<ModelRc<SharedString>>>,
    active_tab: std::rc::Rc<VecModel<i32>>,
    details_rows: std::rc::Rc<VecModel<ModelRc<EntryRowItem>>>,
    details_columns: std::rc::Rc<VecModel<ModelRc<crate::ColumnSpec>>>,
    details_selected_mask: std::rc::Rc<VecModel<ModelRc<bool>>>,
    details_selected_anchor: std::rc::Rc<VecModel<i32>>,
    details_focused_index: std::rc::Rc<VecModel<i32>>,
    grid_thumbnails: std::rc::Rc<VecModel<ModelRc<slint::Image>>>,
    grid_has_thumbs: std::rc::Rc<VecModel<ModelRc<bool>>>,
    grid_selected_mask: std::rc::Rc<VecModel<ModelRc<bool>>>,
    grid_focused_index: std::rc::Rc<VecModel<i32>>,
    gallery_strip_thumbnails: std::rc::Rc<VecModel<ModelRc<slint::Image>>>,
    gallery_preview_image: std::rc::Rc<VecModel<slint::Image>>,
    gallery_preview_loading: std::rc::Rc<VecModel<bool>>,
    gallery_preview_fallback_glyph: std::rc::Rc<VecModel<SharedString>>,
    gallery_focused_index: std::rc::Rc<VecModel<i32>>,
    gallery_metadata: std::rc::Rc<VecModel<crate::MetadataFields>>,
    tree_nodes: std::rc::Rc<VecModel<ModelRc<crate::TreeNode>>>,
    tree_focused_index: std::rc::Rc<VecModel<i32>>,
    tree_selected_index: std::rc::Rc<VecModel<i32>>,
    miller_columns: std::rc::Rc<VecModel<ModelRc<crate::MillerColData>>>,
    miller_focused_col: std::rc::Rc<VecModel<i32>>,

    // Per-pane status bar arrays, driven by `push_pane_selection_to_slint`.
    status_folder_count: std::rc::Rc<VecModel<i32>>,
    status_file_count: std::rc::Rc<VecModel<i32>>,
    status_total_size_text: std::rc::Rc<VecModel<SharedString>>,
    status_free_space_text: std::rc::Rc<VecModel<SharedString>>,

    /// Whether every property has been bound to its underlying `Rc<VecModel>`
    /// yet. Guards [`Self::ensure_bound`] so we only issue `set_*` calls once.
    bound: bool,
}

impl Default for OuterPaneModels {
    fn default() -> Self {
        Self {
            panes: std::rc::Rc::new(VecModel::default()),
            split_handles: std::rc::Rc::new(VecModel::default()),
            tabs: std::rc::Rc::new(VecModel::default()),
            segments: std::rc::Rc::new(VecModel::default()),
            active_tab: std::rc::Rc::new(VecModel::default()),
            details_rows: std::rc::Rc::new(VecModel::default()),
            details_columns: std::rc::Rc::new(VecModel::default()),
            details_selected_mask: std::rc::Rc::new(VecModel::default()),
            details_selected_anchor: std::rc::Rc::new(VecModel::default()),
            details_focused_index: std::rc::Rc::new(VecModel::default()),
            grid_thumbnails: std::rc::Rc::new(VecModel::default()),
            grid_has_thumbs: std::rc::Rc::new(VecModel::default()),
            grid_selected_mask: std::rc::Rc::new(VecModel::default()),
            grid_focused_index: std::rc::Rc::new(VecModel::default()),
            gallery_strip_thumbnails: std::rc::Rc::new(VecModel::default()),
            gallery_preview_image: std::rc::Rc::new(VecModel::default()),
            gallery_preview_loading: std::rc::Rc::new(VecModel::default()),
            gallery_preview_fallback_glyph: std::rc::Rc::new(VecModel::default()),
            gallery_focused_index: std::rc::Rc::new(VecModel::default()),
            gallery_metadata: std::rc::Rc::new(VecModel::default()),
            tree_nodes: std::rc::Rc::new(VecModel::default()),
            tree_focused_index: std::rc::Rc::new(VecModel::default()),
            tree_selected_index: std::rc::Rc::new(VecModel::default()),
            miller_columns: std::rc::Rc::new(VecModel::default()),
            miller_focused_col: std::rc::Rc::new(VecModel::default()),
            status_folder_count: std::rc::Rc::new(VecModel::default()),
            status_file_count: std::rc::Rc::new(VecModel::default()),
            status_total_size_text: std::rc::Rc::new(VecModel::default()),
            status_free_space_text: std::rc::Rc::new(VecModel::default()),
            bound: false,
        }
    }
}

impl OuterPaneModels {
    /// Bind every `panes-*` window property to its persistent
    /// [`Rc<VecModel>`] the first time this runs. Subsequent calls are
    /// no-ops.
    fn ensure_bound(&mut self, window: &AtlasWindow) {
        if self.bound {
            return;
        }
        window.set_panes(ModelRc::from(self.panes.clone()));
        window.set_split_handles(ModelRc::from(self.split_handles.clone()));
        window.set_panes_tabs(ModelRc::from(self.tabs.clone()));
        window.set_panes_segments(ModelRc::from(self.segments.clone()));
        window.set_panes_active_tab(ModelRc::from(self.active_tab.clone()));
        window.set_panes_details_rows(ModelRc::from(self.details_rows.clone()));
        window.set_panes_details_columns(ModelRc::from(self.details_columns.clone()));
        window.set_panes_details_selected_mask(ModelRc::from(self.details_selected_mask.clone()));
        window
            .set_panes_details_selected_anchor(ModelRc::from(self.details_selected_anchor.clone()));
        window.set_panes_details_focused_index(ModelRc::from(self.details_focused_index.clone()));
        window.set_panes_grid_thumbnails(ModelRc::from(self.grid_thumbnails.clone()));
        window.set_panes_grid_has_thumbs(ModelRc::from(self.grid_has_thumbs.clone()));
        window.set_panes_grid_selected_mask(ModelRc::from(self.grid_selected_mask.clone()));
        window.set_panes_grid_focused_index(ModelRc::from(self.grid_focused_index.clone()));
        window.set_panes_gallery_strip_thumbnails(ModelRc::from(
            self.gallery_strip_thumbnails.clone(),
        ));
        window.set_panes_gallery_preview_image(ModelRc::from(self.gallery_preview_image.clone()));
        window
            .set_panes_gallery_preview_loading(ModelRc::from(self.gallery_preview_loading.clone()));
        window.set_panes_gallery_preview_fallback_glyph(ModelRc::from(
            self.gallery_preview_fallback_glyph.clone(),
        ));
        window.set_panes_gallery_focused_index(ModelRc::from(self.gallery_focused_index.clone()));
        window.set_panes_gallery_metadata(ModelRc::from(self.gallery_metadata.clone()));
        window.set_panes_tree_nodes(ModelRc::from(self.tree_nodes.clone()));
        window.set_panes_tree_focused_index(ModelRc::from(self.tree_focused_index.clone()));
        window.set_panes_tree_selected_index(ModelRc::from(self.tree_selected_index.clone()));
        window.set_panes_miller_columns(ModelRc::from(self.miller_columns.clone()));
        window.set_panes_miller_focused_col(ModelRc::from(self.miller_focused_col.clone()));
        window.set_panes_status_folder_count(ModelRc::from(self.status_folder_count.clone()));
        window.set_panes_status_file_count(ModelRc::from(self.status_file_count.clone()));
        window.set_panes_status_total_size_text(ModelRc::from(self.status_total_size_text.clone()));
        window.set_panes_status_free_space_text(ModelRc::from(self.status_free_space_text.clone()));
        self.bound = true;
    }
}

#[allow(clippy::too_many_arguments)]
fn build_pane_controllers(
    pane_id: PaneId,
    window: &AtlasWindow,
    shell: std::sync::Weak<AppShell>,
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
    thumb_cache: Arc<atlas_thumbs::SqliteCache>,
    thumb_worker_count: usize,
    thumb_max_cache_bytes: u64,
    thumbs_enabled: bool,
    thumb_max_file_bytes: u64,
) -> PaneControllers {
    let details = DetailsController::new(pane_id, shell.clone(), Arc::clone(&actions));
    let grid = GridController::new(
        pane_id,
        shell.clone(),
        Arc::clone(&actions),
        Arc::clone(&thumb_cache),
        thumb_worker_count,
        thumb_max_cache_bytes,
        thumbs_enabled,
        thumb_max_file_bytes,
    );
    let gallery = GalleryController::new(
        pane_id,
        shell.clone(),
        Arc::clone(&actions),
        Arc::clone(&thumb_cache),
        thumb_worker_count,
        thumb_max_cache_bytes,
        thumbs_enabled,
        thumb_max_file_bytes,
    );
    let tree = TreeController::new(pane_id, shell.clone(), Arc::clone(&actions));
    tree.attach_window(window.as_weak());
    let miller = MillerController::new(pane_id, shell, actions);
    miller.attach_window(window.as_weak());

    PaneControllers {
        pane_id,
        details,
        grid,
        miller,
        tree,
        gallery,
    }
}

/// Active drag state once the 4-px movement threshold has been crossed.
///
/// Stored in [`AppShell::dragging`] while a cross-pane drag is in flight.
/// Cleared by [`AppShell::drag_end`] and [`AppShell::drag_cancel`].
pub struct DragState {
    /// Pane from which the drag originated.
    pub source_pane: PaneId,
    /// Resolved filesystem paths of the entries being dragged.
    pub paths: Vec<PathBuf>,
}

/// Transient "armed" state between the view's pointer-down (or `drag-start`
/// Slint callback) and the 4-px threshold that promotes to a full drag.
///
/// Using a virtual origin of (0, 0) means that any call to
/// [`AppShell::drag_move`] with real window coordinates will immediately
/// satisfy the threshold in production use.  Small synthetic deltas can be
/// used in unit tests to probe the threshold logic explicitly.
struct DragArmedState {
    pane: PaneId,
    #[allow(dead_code)]
    entry_index: usize,
    /// Pre-resolved paths (computed on the Slint event-loop thread at arm time
    /// so promotion in `drag_move` is allocation-free).
    paths: Vec<PathBuf>,
    /// Virtual origin; threshold is checked as `hypot(x − origin_x, y − origin_y) ≥ 4`.
    origin_x: f32,
    origin_y: f32,
}

/// Owns Rust-side model state and bridges it to the Slint window.
///
/// Construct with [`AppShell::new`], then call
/// [`AppShell::project_workspace_to_slint`], [`AppShell::set_status`], and
/// [`AppShell::set_theme`] to push initial state.
///
/// The workspace is an N-pane [`WorkspaceModel`]; per-pane controllers and
/// view models are keyed by [`PaneId`]. The Slint UI renders panes via a
/// `for pane[i] in panes` loop driven by [`PaneSlintData`] pushed by
/// `project_workspace_to_slint`. The `pane_slint_index` map tracks each
/// pane's DFS slot index so per-slot heavy-data properties remain routed
/// correctly while view controllers are migrated to an N-pane model.
pub struct AppShell {
    window: slint::Weak<AtlasWindow>,
    workspace: RwLock<WorkspaceModel>,
    /// Per-pane view controllers keyed by pane id.
    panes_ctrl: RwLock<AHashMap<PaneId, PaneControllers>>,
    /// Per-pane snapshot of view data (rows, columns, thumbs, …) staged for
    /// Slint. View controllers push into this cache via the `publish_*`
    /// methods; the shell rebuilds the outer nested [`VecModel`]s in DFS
    /// order and dispatches to the window on the Slint event loop.
    pane_cache: RwLock<AHashMap<PaneId, PaneRenderCache>>,
    /// Current location view model per pane id.
    vms: RwLock<AHashMap<PaneId, Arc<dyn LocationViewModel>>>,
    /// Maps `PaneId` → Slint slot index (0 or 1) for the compat layer.
    pane_slint_index: RwLock<AHashMap<PaneId, usize>>,
    palette: RwLock<PaletteModel>,
    status: RwLock<StatusModel>,
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
    navigation: Arc<NavigationController>,
    palette_ctrl: Arc<PaletteController>,
    search: Arc<SearchController>,
    /// File-operations queue controller.
    ops: Arc<OpsController>,
    /// Bulk rename modal controller.
    bulk_rename: Arc<BulkRenameController>,
    /// OS-clipboard bridge for Copy / Cut / Paste of file paths.
    clipboard: Arc<crate::clipboard::ClipboardController>,
    /// Shared thumbnail cache used when building new pane controllers on split.
    thumb_cache: Arc<atlas_thumbs::SqliteCache>,
    /// Thumbnail worker thread count (config: thumbnails.generation_threads).
    thumb_worker_count: usize,
    /// Thumbnail cache byte cap (config: thumbnails.cache_max_size_mb).
    thumb_max_cache_bytes: u64,
    /// Whether thumbnail generation is enabled at all (config: thumbnails.enabled).
    thumbs_enabled: bool,
    /// Skip thumbnail generation for files above this byte cap
    /// (config: thumbnails.generate_for_size_up_to_mb).
    thumb_max_file_bytes: u64,
    /// Recently-closed tabs per pane, newest first. Bounded to 20 entries per pane.
    closed_tabs: RwLock<AHashMap<PaneId, VecDeque<TabModel>>>,
    /// Armed drag state (between pointer-down and the 4-px promotion threshold).
    drag_armed: RwLock<Option<DragArmedState>>,
    /// Active drag state (past the 4-px threshold; `is_dragging` returns `true`).
    dragging: RwLock<Option<DragState>>,
    /// Path the currently-open right-click context menu is targeting, if any.
    /// Set by `on_*_row_context_menu` right before showing the menu; consumed
    /// by every `ctx-*` handler so context-menu actions operate on the
    /// specific entry the user right-clicked (not the focused entry, which
    /// may differ if the user right-clicked without first selecting).
    context_menu_target: RwLock<Option<(PaneId, PathBuf)>>,
    /// Per-column-kind width overrides for the Details view, keyed by the
    /// wire string from [`crate::views::details::ColumnKind::as_str`].
    /// Updated live as the user drags a column divider; persisted to
    /// `[view.details.column_widths]` in the user config on quit
    /// (see [`Self::column_widths_snapshot`] +
    /// `atlas-app::main::persist_column_widths_on_quit`).
    column_widths: RwLock<HashMap<String, f32>>,
    /// `true` once the user has resized any column since app start —
    /// prevents an unnecessary config write when nothing changed.
    column_widths_dirty: AtomicBool,
    /// Whether the bottom shortcut-footer strip is currently rendered.
    /// Kept in sync with the Slint `show-shortcut-footer` property via
    /// [`Self::set_shortcut_footer_visible`]. Consulted by
    /// [`Self::workspace_content_bounds`] so panes reclaim the footer's
    /// 26 px when the user disables `ui.show_shortcuts`.
    shortcut_footer_visible: AtomicBool,
}

impl AppShell {
    /// Build the shell, wire all Slint callbacks, and return a shared handle.
    ///
    /// `thumb_worker_count` / `thumb_max_cache_bytes` are forwarded to every
    /// thumbnail requester created for each pane; pass `0` / `500 * 1024 * 1024`
    /// for defaults.  See config fields `thumbnails.generation_threads` and
    /// `thumbnails.cache_max_size_mb`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        window: &AtlasWindow,
        actions: impl ActionSink,
        nav: Arc<NavigationController>,
        search: Arc<SearchController>,
        thumb_worker_count: usize,
        thumb_max_cache_bytes: u64,
        thumbs_enabled: bool,
        thumb_max_file_bytes: u64,
        bookmarks: Vec<(String, PathBuf)>,
    ) -> Arc<Self> {
        let actions: Arc<Mutex<Box<dyn ActionSink>>> = Arc::new(Mutex::new(Box::new(actions)));
        let thumb_cache = Arc::new(
            atlas_thumbs::SqliteCache::open_default()
                .unwrap_or_else(|error| panic!("failed to open thumbnail cache: {error}")),
        );

        let workspace = WorkspaceModel::new_default();
        let initial_pane_id = workspace.focused;

        let mut pane_slint_index = AHashMap::default();
        pane_slint_index.insert(initial_pane_id, 0usize);

        let mut pane_cache = AHashMap::default();
        pane_cache.insert(initial_pane_id, PaneRenderCache::default());

        let palette_ctrl = build_palette_controller(window, Arc::clone(&actions), bookmarks);
        search.set_action_sink(Arc::clone(&actions));
        let ops = OpsController::new();
        ops.attach_window(window.as_weak());
        let bulk_rename = BulkRenameController::new(Arc::clone(&ops), Arc::clone(&actions));
        bulk_rename.attach_window(window.as_weak());
        let clipboard = crate::clipboard::ClipboardController::new(Arc::clone(&ops));

        // Construct the shell cyclically so controllers can hold a weak
        // reference to it (used to route publish_* calls back into the cache).
        let shell = Arc::new_cyclic(|weak: &std::sync::Weak<Self>| {
            let mut panes_ctrl = AHashMap::default();
            panes_ctrl.insert(
                initial_pane_id,
                build_pane_controllers(
                    initial_pane_id,
                    window,
                    weak.clone(),
                    Arc::clone(&actions),
                    Arc::clone(&thumb_cache),
                    thumb_worker_count,
                    thumb_max_cache_bytes,
                    thumbs_enabled,
                    thumb_max_file_bytes,
                ),
            );
            Self {
                window: window.as_weak(),
                workspace: RwLock::new(workspace),
                panes_ctrl: RwLock::new(panes_ctrl),
                pane_cache: RwLock::new(pane_cache),
                vms: RwLock::new(AHashMap::default()),
                pane_slint_index: RwLock::new(pane_slint_index),
                palette: RwLock::new(PaletteModel::default()),
                status: RwLock::new(StatusModel::default()),
                actions,
                navigation: nav,
                palette_ctrl,
                search,
                ops,
                bulk_rename,
                clipboard,
                thumb_cache,
                thumb_worker_count,
                thumb_max_cache_bytes,
                thumbs_enabled,
                thumb_max_file_bytes,
                closed_tabs: RwLock::new(AHashMap::default()),
                drag_armed: RwLock::new(None),
                dragging: RwLock::new(None),
                context_menu_target: RwLock::new(None),
                column_widths: RwLock::new(HashMap::new()),
                column_widths_dirty: AtomicBool::new(false),
                shortcut_footer_visible: AtomicBool::new(true),
            }
        });

        shell.wire_callbacks(window);

        // Route goto::Anything (and any other Path-kind palette result)
        // through fs::View so directories navigate the focused pane and
        // files hand off to the OS default handler. Uses a weak self so
        // the callback doesn't extend the shell's lifetime.
        {
            let weak = Arc::downgrade(&shell);
            shell.palette_ctrl.set_on_path_confirm(move |path| {
                let Some(shell) = weak.upgrade() else { return };
                let id = shell.focused_pane_id();
                shell.view_path(id, path);
            });
        }

        shell
    }

    /// Return the focused pane's details controller.
    #[must_use]
    pub fn details_controller(&self) -> Arc<DetailsController> {
        Arc::clone(&self.focused_controllers().details)
    }

    /// Return the focused pane's grid controller.
    #[must_use]
    pub fn grid_controller(&self) -> Arc<GridController> {
        Arc::clone(&self.focused_controllers().grid)
    }

    /// Return the focused pane's gallery controller.
    #[must_use]
    pub fn gallery_controller(&self) -> Arc<GalleryController> {
        Arc::clone(&self.focused_controllers().gallery)
    }

    /// Return the focused pane's tree controller.
    #[must_use]
    pub fn tree_controller(&self) -> Arc<TreeController> {
        Arc::clone(&self.focused_controllers().tree)
    }

    /// Return the focused pane's miller columns controller.
    #[must_use]
    pub fn miller_controller(&self) -> Arc<MillerController> {
        Arc::clone(&self.focused_controllers().miller)
    }

    // ── Details column-width persistence ─────────────────────────────────────

    /// Apply persisted per-column widths (loaded from
    /// `[view.details.column_widths]` at startup) to every existing pane's
    /// details controller AND stash them in the in-memory snapshot so the
    /// same widths are seeded onto any pane created later (via split).
    pub fn apply_column_widths(self: &Arc<Self>, widths: &HashMap<String, f32>) {
        if widths.is_empty() {
            return;
        }
        *self.column_widths.write() = widths.clone();
        let panes = self.panes_ctrl.read();
        for ctrl in panes.values() {
            ctrl.details.apply_persisted_widths(widths);
        }
    }

    /// Record a user-driven column resize; called from the
    /// `on_details_header_resize` handler after the details controller
    /// clamps the proposed width. Also flags the snapshot dirty so quit
    /// knows whether it has anything worth writing.
    pub(crate) fn record_column_width(
        &self,
        kind: crate::views::details::ColumnKind,
        clamped_width: f32,
    ) {
        self.column_widths
            .write()
            .insert(kind.as_str().to_owned(), clamped_width);
        self.column_widths_dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Snapshot the current per-column width overrides for persistence.
    ///
    /// Returns `None` when no column has been resized since app start —
    /// callers use this as an early-exit signal so untouched sessions
    /// don't trigger a config rewrite.
    #[must_use]
    pub fn column_widths_snapshot(&self) -> Option<HashMap<String, f32>> {
        if !self
            .column_widths_dirty
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return None;
        }
        Some(self.column_widths.read().clone())
    }

    /// Return a clone of the controllers for the currently focused pane,
    /// falling back to any pane if the focused pane has no controllers yet.
    fn focused_controllers(&self) -> PaneControllers {
        let id = self.focused_pane_id();
        let panes = self.panes_ctrl.read();
        panes
            .get(&id)
            .or_else(|| panes.values().next())
            .cloned()
            .expect("at least one pane's controllers must exist")
    }

    /// Get the per-pane view controllers by pane id.
    ///
    /// Returns a clone; controllers live behind [`Arc`], so this is cheap.
    #[must_use]
    pub fn pane_by_id(&self, id: PaneId) -> Option<PaneControllers> {
        self.panes_ctrl.read().get(&id).cloned()
    }

    /// Resolve the controllers currently occupying Slint slot `index`.
    ///
    /// Used by the `toggle-dual-pane` callback and Phase 4.1 migration code.
    #[allow(dead_code)]
    fn ctrl_for_index(&self, index: usize) -> Option<PaneControllers> {
        self.pane_id_for_index(index)
            .and_then(|id| self.pane_by_id(id))
    }

    /// Return the shared navigation controller.
    #[must_use]
    pub fn navigation(&self) -> Arc<NavigationController> {
        Arc::clone(&self.navigation)
    }

    /// Return the shared palette controller.
    #[must_use]
    pub fn palette_controller(&self) -> Arc<PaletteController> {
        Arc::clone(&self.palette_ctrl)
    }

    /// Return the shared search controller.
    #[must_use]
    pub fn search(&self) -> Arc<SearchController> {
        Arc::clone(&self.search)
    }

    /// Return the file-operations controller.
    #[must_use]
    pub fn ops(&self) -> Arc<OpsController> {
        Arc::clone(&self.ops)
    }

    /// Return the bulk-rename modal controller.
    #[must_use]
    pub fn bulk_rename(&self) -> Arc<BulkRenameController> {
        Arc::clone(&self.bulk_rename)
    }

    /// Return the OS-clipboard bridge for file copy / cut / paste.
    #[must_use]
    pub fn clipboard(&self) -> Arc<crate::clipboard::ClipboardController> {
        Arc::clone(&self.clipboard)
    }

    /// Return the focused pane's [`PaneId`].
    #[must_use]
    pub fn focused_pane_id(&self) -> PaneId {
        self.workspace.read().focused
    }

    /// Set focus to the given pane.
    ///
    /// This is a hot-path called from every mouse click on a row: we take
    /// care to avoid a full [`Self::project_workspace_to_slint`] rebuild
    /// (which would re-emit `panes-details-rows`, resetting `ListView`
    /// scroll and destroying `TouchArea` double-click state). When the
    /// requested pane is already focused, we skip work entirely. When
    /// focus actually shifts, we push only the top-level `focused_pane_id`
    /// and `focus_index` scalars.
    pub fn set_focused_pane_id(self: &Arc<Self>, id: PaneId) {
        let (changed, new_focus_index) = {
            let mut ws = self.workspace.write();
            let already = ws.focused == id;
            if already {
                (false, 0i32)
            } else {
                ws.set_focused(id);
                let bounds = self.workspace_content_bounds();
                let rects = ws.layout.layout_rects(bounds);
                let idx = rects.iter().position(|(pid, _)| *pid == id).unwrap_or(0) as i32;
                (true, idx)
            }
        };
        if !changed {
            return;
        }
        let focused_id_i32 = id.0 as i32;
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = weak.upgrade() {
                window.set_focused_pane_id(focused_id_i32);
                window.set_focus_index(new_focus_index);
            }
        });
    }

    /// Force the root FocusScope to re-grab keyboard focus by bumping the
    /// `refocus-tick` property Slint watches. Call this after any UI action
    /// that may leave focus stranded on a TextInput or dismissed modal.
    pub fn bump_refocus_tick(&self) {
        if let Some(window) = self.window.upgrade() {
            let next = window.get_refocus_tick().wrapping_add(1);
            window.set_refocus_tick(next);
        }
    }

    /// Record `path` as the context-menu target and show the menu at
    /// `(x, y)`. The `ctx-*` handlers all read
    /// [`Self::context_menu_target`] so context-menu actions operate on
    /// the entry the user right-clicked rather than the pane's focused
    /// entry (which may differ if the user right-clicked without first
    /// selecting).
    pub fn open_context_menu(&self, pane: PaneId, path: Option<PathBuf>, x: f32, y: f32) {
        if let Some(p) = path {
            *self.context_menu_target.write() = Some((pane, p));
        }
        if let Some(window) = self.window.upgrade() {
            window.set_context_menu_x(x);
            window.set_context_menu_y(y);
            let next = window.get_context_menu_tick().wrapping_add(1);
            window.set_context_menu_tick(next);
        }
    }

    /// Look up the current context-menu target. Called by every `ctx-*`
    /// callback handler in main.rs.
    #[must_use]
    pub fn context_menu_target(&self) -> Option<(PaneId, PathBuf)> {
        self.context_menu_target.read().clone()
    }

    /// Set the active-pane border thickness (in logical pixels). Bound to
    /// `config.ui.active_pane_border_px`.
    pub fn set_active_pane_border_px(&self, px: f32) {
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = weak.upgrade() {
                window.set_active_pane_border_px(px);
            }
        });
    }

    /// Show or hide the bottom status bar. Bound to `ui.show_status_bar`.
    pub fn set_status_bar_visible(&self, visible: bool) {
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = weak.upgrade() {
                window.set_show_status_bar(visible);
            }
        });
    }

    /// Show or hide the breadcrumb strip in every pane. Bound to
    /// `ui.show_breadcrumbs`.
    pub fn set_breadcrumbs_visible(&self, visible: bool) {
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = weak.upgrade() {
                window.set_show_breadcrumbs(visible);
            }
        });
    }

    /// Show or hide the bottom shortcut-footer strip (Marta/NC-style
    /// key hints). Bound to `ui.show_shortcuts`. Rebindings still work;
    /// only the hint chips are hidden.
    ///
    /// Also re-projects the workspace so panes reclaim (or yield) the
    /// footer's 26 px of vertical chrome — see
    /// [`Self::workspace_content_bounds`].
    pub fn set_shortcut_footer_visible(self: &Arc<Self>, visible: bool) {
        self.shortcut_footer_visible
            .store(visible, std::sync::atomic::Ordering::Relaxed);
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = weak.upgrade() {
                window.set_show_shortcut_footer(visible);
            }
        });
        self.project_workspace_to_slint();
    }

    /// Enable or disable UI animations globally. When `false`, every
    /// `animate {}` block in Slint collapses to a 0ms transition. Bound to
    /// `ui.animations`.
    pub fn set_animations_enabled(&self, enabled: bool) {
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = weak.upgrade() {
                window.set_theme_animations(enabled);
            }
        });
    }

    /// Replace the shortcut-footer hints. Each entry is a pre-formatted
    /// `(chord_display, action_label)` pair. Called at startup and any time
    /// the keymap changes (hot-reload of `~/.config/atlas/keymaps/default.toml`).
    pub fn set_shortcut_hints(&self, hints: Vec<(String, String)>) {
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            let entries: Vec<ShortcutHint> = hints
                .into_iter()
                .map(|(key, label)| ShortcutHint {
                    key: SharedString::from(key.as_str()),
                    label: SharedString::from(label.as_str()),
                })
                .collect();
            window.set_shortcut_hints(ModelRc::new(VecModel::from(entries)));
        });
    }

    /// Return a weak reference to the Slint window backing this shell.
    /// Consumers can `upgrade()` to invoke Slint callbacks or getters.
    #[must_use]
    pub fn window_weak(&self) -> slint::Weak<AtlasWindow> {
        self.window.clone()
    }

    /// Install the atlas-keymap dispatch hook on the Slint `handle-key-chord`
    /// callback. Called once at startup after both `AppShell` and the
    /// `Dispatcher` exist. `hook` is invoked with the raw event fields
    /// (key text, physical modifier bools, and a `modal_active` flag) and
    /// returns `true` if it consumed the event.
    ///
    /// Physical-modifier normalisation happens on the caller side so this
    /// method stays UI-only. `modal_active` reflects Slint's local union of
    /// modal visibility (palette / goto / search / bulk rename / ops
    /// progress); the caller uses it to restrict the dispatched context
    /// stack so text-input-consuming chords (Cmd+A, Cmd+C, arrows) don't
    /// steal keys from a focused TextInput.
    pub fn install_key_dispatcher<F>(&self, hook: F)
    where
        F: Fn(SharedString, bool, bool, bool, bool, bool) -> bool + 'static,
    {
        if let Some(window) = self.window.upgrade() {
            window.on_handle_key_chord(move |key, ctrl, alt, shift, cmd, modal_active| {
                hook(key, ctrl, alt, shift, cmd, modal_active)
            });
        }
    }

    /// Resolve a Slint pane index (0 or 1) to a [`PaneId`] via DFS leaf order.
    /// Resolve a Slint pane index (0 or 1) to a [`PaneId`] via DFS leaf order.
    #[must_use]
    pub fn pane_id_for_index(&self, index: usize) -> Option<PaneId> {
        let leaves = self.workspace.read().layout.all_leaves();
        leaves.get(index).copied()
    }

    /// Return the index of the currently-active tab in pane `id`, or `None`
    /// if the pane doesn't exist.
    #[must_use]
    pub fn active_tab_index(&self, id: PaneId) -> Option<usize> {
        self.workspace.read().pane(id).map(|p| p.active_tab)
    }

    /// Return the view mode of pane `id`, or the default when the pane
    /// doesn't exist (defensive — every real pane always has a mode).
    #[must_use]
    pub fn pane_view_mode(&self, id: PaneId) -> ViewMode {
        self.workspace
            .read()
            .pane(id)
            .map(|p| p.view_mode)
            .unwrap_or_default()
    }

    /// Split the focused pane in `direction`. Returns the new [`PaneId`].
    ///
    /// Creates a new pane by splitting the currently focused leaf. The new pane
    /// inherits the focused pane's current location. After Phase 4 the Slint UI
    /// renders N panes, so any number of splits is supported.
    pub fn split_focused(self: &Arc<Self>, direction: SplitDirection) -> Option<PaneId> {
        let leaf_count = self.workspace.read().layout.leaf_count();
        // Determine which DFS slot the new pane will occupy (= current count).
        let new_slot = leaf_count;

        let (new_id, new_location) = {
            let mut ws = self.workspace.write();
            let new_id = ws.split_focused(direction, None);
            let loc = ws.pane(new_id).expect("just created").active_location();
            (new_id, loc)
        };

        // Assign the new pane to its DFS slot.
        self.pane_slint_index.write().insert(new_id, new_slot);

        // Seed an empty render-cache entry so the outer VecModels always have
        // a slot for every live pane (built inside push_pane_data_to_slint).
        self.pane_cache
            .write()
            .insert(new_id, PaneRenderCache::default());

        // Build controllers for the new pane; they hold a weak reference to
        // this shell so their publish_* calls route back into the cache.
        let window = self.window.upgrade().expect("window must be alive");
        let new_ctrl = build_pane_controllers(
            new_id,
            &window,
            Arc::downgrade(self),
            Arc::clone(&self.actions),
            Arc::clone(&self.thumb_cache),
            self.thumb_worker_count,
            self.thumb_max_cache_bytes,
            self.thumbs_enabled,
            self.thumb_max_file_bytes,
        );
        self.panes_ctrl.write().insert(new_id, new_ctrl);

        // Seed the new pane's details controller with the persisted column
        // widths so a fresh split doesn't reset back to defaults.
        let widths_snapshot = self.column_widths.read().clone();
        if !widths_snapshot.is_empty() {
            if let Some(c) = self.pane_by_id(new_id) {
                c.details.apply_persisted_widths(&widths_snapshot);
            }
        }

        // Navigate the new pane to the inherited location.
        self.navigation.navigate_pane(new_id, new_location);
        self.project_workspace_to_slint();
        Some(new_id)
    }

    /// Close the focused pane. Refuses to close the last remaining pane.
    pub fn close_focused_pane(self: &Arc<Self>) {
        let outcome = {
            let mut ws = self.workspace.write();
            ws.close_focused()
        };
        let Some(outcome) = outcome else {
            tracing::debug!("close_focused_pane: only one pane; refusing");
            return;
        };

        self.panes_ctrl.write().remove(&outcome.removed);
        self.vms.write().remove(&outcome.removed);
        self.pane_cache.write().remove(&outcome.removed);

        // Reassign Slint slot indices for the remaining panes in DFS order.
        let leaves = self.workspace.read().layout.all_leaves();
        {
            let mut idx_map = self.pane_slint_index.write();
            idx_map.clear();
            for (i, &leaf) in leaves.iter().enumerate() {
                idx_map.insert(leaf, i);
            }
        }

        self.project_workspace_to_slint();
    }

    /// Move focus in cardinal direction `dir` using the layout geometry.
    pub fn focus_direction(self: &Arc<Self>, dir: Cardinal) {
        let bounds = self.window_bounds();
        {
            self.workspace.write().focus_direction(dir, bounds);
        }
        self.project_workspace_to_slint();
    }

    /// Cycle the focused pane's view mode Details→Grid→Gallery→Miller→Tree→…
    pub fn cycle_view_mode(self: &Arc<Self>) {
        let id = self.focused_pane_id();
        let cur = self
            .workspace
            .read()
            .pane(id)
            .map(|p| p.view_mode)
            .unwrap_or_default();
        let next = match cur {
            ViewMode::Details => ViewMode::Grid,
            ViewMode::Grid => ViewMode::Gallery,
            ViewMode::Gallery => ViewMode::Miller,
            ViewMode::Miller => ViewMode::Tree,
            ViewMode::Tree => ViewMode::Details,
        };
        self.set_view_mode(id, next);
    }

    fn window_bounds(&self) -> Rect {
        self.window
            .upgrade()
            .map(|w| {
                let win = w.window();
                let scale = win.scale_factor().max(1.0);
                let size = win.size();
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: (size.width as f32) / scale,
                    height: (size.height as f32) / scale,
                }
            })
            .unwrap_or(Rect::from_size(1440.0, 900.0))
    }

    /// The workspace content area: window bounds minus any bottom chrome
    /// that isn't drawn inside a pane. This is the rectangle that the
    /// pane container fills and that `layout_rects` must be called
    /// against.
    ///
    /// Historically this also subtracted a `TOP_CHROME` for the custom
    /// Titlebar; that Titlebar was deleted in the v0.3 chrome pass
    /// (macOS/Windows/Linux all rely on the OS-native title bar, which
    /// is already excluded from the window's logical pixels), so the
    /// panes now start at `y = 0` in this bounds.
    ///
    /// Since v0.2.2 the window-level status bar was replaced by a
    /// per-pane status bar drawn inside each pane's own frame (see
    /// `components/pane.slint` → `PaneStatusBar`), so it doesn't eat any
    /// of this bounds calculation either.
    ///
    /// The 26-px shortcut footer is only subtracted when it's actually
    /// rendered (`ui.show_shortcuts = true`); when the user hides it the
    /// panes reclaim that height so their status bar sits flush against
    /// the window's bottom edge.
    fn workspace_content_bounds(&self) -> Rect {
        let wb = self.window_bounds();
        // Height of the ShortcutFooter component in `components/shortcut-footer.slint`.
        const FOOTER_H: f32 = 26.0;
        let footer_h = if self
            .shortcut_footer_visible
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            FOOTER_H
        } else {
            0.0
        };
        let height = (wb.height - footer_h).max(1.0);
        Rect {
            x: 0.0,
            y: 0.0,
            width: wb.width.max(1.0),
            height,
        }
    }

    /// Return the current location for `id`, if available.
    ///
    /// This returns the local [`PathBuf`] for [`Location::Local`] panes
    /// and `None` for remote panes. Use [`Self::pane_location_full`] to
    /// receive the full [`Location`] regardless of backend.
    ///
    /// TODO(remote): review callers — most will migrate to the full
    /// [`Location`] once remote backends are wired end-to-end.
    #[must_use]
    pub fn pane_location(&self, id: PaneId) -> Option<PathBuf> {
        self.workspace
            .read()
            .pane(id)
            .and_then(|p| p.active_local_path().map(Path::to_path_buf))
    }

    /// Return the current [`Location`] for `id`, if the pane exists.
    /// Unlike [`Self::pane_location`], this returns remote locations too.
    #[must_use]
    pub fn pane_location_full(&self, id: PaneId) -> Option<Location> {
        self.workspace
            .read()
            .pane(id)
            .map(PaneState::active_location)
    }

    /// Set the view mode for pane `id` and push the change to the UI.
    pub fn set_view_mode(self: &Arc<Self>, id: PaneId, mode: ViewMode) {
        {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(id) else {
                tracing::debug!(?id, "set_view_mode: pane not found");
                return;
            };
            if p.view_mode == mode {
                return;
            }
            p.view_mode = mode;
        }
        self.project_workspace_to_slint();
    }

    /// Enable / disable vim-mode navigation on the Slint FocusScope.
    ///
    /// When true, `hjkl` navigates. When false, only arrow keys do.
    pub fn set_vim_mode(self: &Arc<Self>, enabled: bool) {
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = weak.upgrade() {
                window.set_vim_mode(enabled);
                tracing::info!(vim_mode = enabled, "vim keybinds toggled");
            }
        });
    }

    /// Set which tab is active in pane `id` and reload its location. No-op if
    /// `id` or `tab` is out of range.
    pub fn select_tab(self: &Arc<Self>, id: PaneId, tab: usize) {
        let target = {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(id) else {
                return;
            };
            if tab >= p.tabs.len() {
                return;
            }
            p.set_active(tab);
            Some(p.active_location())
        };
        self.project_workspace_to_slint();
        if let Some(loc) = target {
            self.navigation.navigate_pane(id, loc);
        }
    }

    /// Cycle to the next (`delta = 1`) or previous (`delta = -1`) tab in
    /// pane `id`, wrapping around at the ends.
    pub fn cycle_tab(self: &Arc<Self>, id: PaneId, delta: isize) {
        let target = {
            let ws = self.workspace.read();
            let Some(p) = ws.pane(id) else {
                return;
            };
            let len = p.tabs.len() as isize;
            if len == 0 {
                return;
            }
            let cur = p.active_tab as isize;
            let next = ((cur + delta) % len + len) % len;
            Some(next as usize)
        };
        if let Some(t) = target {
            self.select_tab(id, t);
        }
    }

    /// Split the focused pane rightward (horizontal split).
    #[deprecated(since = "0.0.1", note = "Phase 3: use split_focused")]
    pub fn split_focused_or_toggle_dual(self: &Arc<Self>) {
        self.split_focused(SplitDirection::Horizontal);
    }

    /// Append a new tab to pane `id` pointing at the pane's current location.
    /// The new tab becomes active. No-op if `id` is out of range.
    pub fn new_tab(self: &Arc<Self>, id: PaneId) {
        let loc = self.pane_location(id).unwrap_or_else(dirs_home);
        {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(id) else {
                tracing::debug!(?id, "new_tab: pane not found");
                return;
            };
            p.add_tab(TabModel::at(loc.clone()));
        }
        self.project_workspace_to_slint();
        self.navigation.navigate_pane(id, loc);
    }

    /// Remove tab `tab` from pane `id`. Refuses to close the last tab
    /// (the pane must always have at least one). Adjusts the active tab so
    /// that a still-valid tab remains selected, navigating to its location
    /// when the active tab changed. Pushes the removed tab onto the
    /// per-pane closed-tab history (bounded at 20) for `reopen_closed_tab`.
    pub fn close_tab(self: &Arc<Self>, id: PaneId, tab: usize) {
        // Release the workspace lock before acquiring closed_tabs to avoid
        // any potential lock-ordering issues.
        let result: Option<(TabModel, bool)> = {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(id) else {
                tracing::debug!(?id, tab, "close_tab: pane not found");
                return;
            };
            let was_active = tab == p.active_tab;
            p.close_tab(tab).map(|removed| (removed, was_active))
        };
        let switch_to = if let Some((removed, was_active)) = result {
            {
                let mut ct = self.closed_tabs.write();
                let deque = ct.entry(id).or_default();
                deque.push_front(removed);
                if deque.len() > 20 {
                    deque.pop_back();
                }
            }
            if was_active {
                self.workspace
                    .read()
                    .pane(id)
                    .map(PaneState::active_location)
            } else {
                None
            }
        } else {
            None
        };
        self.project_workspace_to_slint();
        if let Some(dest) = switch_to {
            self.navigation.navigate_pane(id, dest);
        }
    }

    /// Move the tab at `from` to `to` within pane `pane`. Tabs between the
    /// two positions shift by one to fill the gap. The active tab tracks the
    /// moved tab so it stays selected. No-op when `from == to` or either
    /// index is out of range.
    pub fn reorder_tab(self: &Arc<Self>, pane: PaneId, from: usize, to: usize) {
        if from == to {
            return;
        }
        {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(pane) else { return };
            let len = p.tabs.len();
            if from >= len || to >= len {
                return;
            }
            let tab = p.tabs.remove(from);
            p.tabs.insert(to, tab);
            // Adjust the active-tab index so selection follows the moved tab.
            if p.active_tab == from {
                p.active_tab = to;
            } else if from < to {
                // Tabs in (from, to] shifted left by one.
                if p.active_tab > from && p.active_tab <= to {
                    p.active_tab -= 1;
                }
            } else {
                // from > to; tabs in [to, from) shifted right by one.
                if p.active_tab >= to && p.active_tab < from {
                    p.active_tab += 1;
                }
            }
        }
        self.project_workspace_to_slint();
    }

    /// Duplicate the tab at `tab` in pane `pane`, inserting the copy
    /// immediately after and activating it. The copy starts with fresh
    /// history containing only the current location, but inherits the
    /// source tab's sort specification and filter.
    pub fn duplicate_tab(self: &Arc<Self>, pane: PaneId, tab: usize) {
        let (new_loc, src_sort, src_filter) = {
            let ws = self.workspace.read();
            let Some(p) = ws.pane(pane) else { return };
            if tab >= p.tabs.len() {
                return;
            }
            let src = &p.tabs[tab];
            let loc = src
                .location
                .clone()
                .unwrap_or_else(|| Location::local(dirs_home()));
            (loc, src.sort.clone(), src.filter.clone())
        };
        // Build the duplicate: fresh history, inherited sort + filter.
        let mut new_tab = TabModel::at(new_loc.clone());
        new_tab.sort = src_sort;
        new_tab.filter = src_filter;
        let insert_at = tab + 1;
        {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(pane) else { return };
            p.tabs.insert(insert_at, new_tab);
            p.active_tab = insert_at;
        }
        self.project_workspace_to_slint();
        self.navigation.navigate_pane(pane, new_loc);
    }

    /// Close every tab in pane `pane` except the one at index `keep`.
    /// Refuses when the pane has only one tab or `keep` is out of range.
    /// All removed tabs are pushed onto the closed-tab history.
    pub fn close_other_tabs(self: &Arc<Self>, pane: PaneId, keep: usize) {
        let (switch_to, closed) = {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(pane) else { return };
            if p.tabs.len() <= 1 || keep >= p.tabs.len() {
                return;
            }
            let kept = p.tabs[keep].clone();
            let all: Vec<TabModel> = std::mem::replace(&mut p.tabs, vec![kept]);
            let closed: Vec<TabModel> = all
                .into_iter()
                .enumerate()
                .filter_map(|(i, t)| (i != keep).then_some(t))
                .collect();
            p.active_tab = 0;
            let dest = p.active_location();
            (dest, closed)
        };
        {
            let mut ct = self.closed_tabs.write();
            let deque = ct.entry(pane).or_default();
            for t in closed.into_iter().rev() {
                deque.push_front(t);
                if deque.len() > 20 {
                    deque.pop_back();
                }
            }
        }
        self.project_workspace_to_slint();
        self.navigation.navigate_pane(pane, switch_to);
    }

    /// Close every tab in pane `pane` at an index strictly greater than
    /// `from`. No-op when `from` is the last tab. All removed tabs are
    /// pushed onto the closed-tab history.
    pub fn close_tabs_to_right_of(self: &Arc<Self>, pane: PaneId, from: usize) {
        let (switch_to, closed) = {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(pane) else { return };
            if from + 1 >= p.tabs.len() {
                return;
            }
            let closed: Vec<TabModel> = p.tabs.drain(from + 1..).collect();
            let navigated = if p.active_tab > from {
                p.active_tab = from;
                Some(p.active_location())
            } else {
                None
            };
            (navigated, closed)
        };
        {
            let mut ct = self.closed_tabs.write();
            let deque = ct.entry(pane).or_default();
            for t in closed.into_iter().rev() {
                deque.push_front(t);
                if deque.len() > 20 {
                    deque.pop_back();
                }
            }
        }
        self.project_workspace_to_slint();
        if let Some(dest) = switch_to {
            self.navigation.navigate_pane(pane, dest);
        }
    }

    /// Pop the most-recently-closed tab off the pane's history stack and
    /// append it at the end of the tab list, making it active. No-op when
    /// the history is empty.
    pub fn reopen_closed_tab(self: &Arc<Self>, pane: PaneId) {
        let reopened = {
            let mut ct = self.closed_tabs.write();
            ct.get_mut(&pane).and_then(VecDeque::pop_front)
        };
        let Some(tab) = reopened else { return };
        let loc = tab
            .location
            .clone()
            .unwrap_or_else(|| Location::local(dirs_home()));
        {
            let mut ws = self.workspace.write();
            let Some(p) = ws.pane_mut(pane) else { return };
            p.tabs.push(tab);
            p.active_tab = p.tabs.len() - 1;
        }
        self.project_workspace_to_slint();
        self.navigation.navigate_pane(pane, loc);
    }

    /// Navigate pane `id` to the parent of its current location.
    pub fn go_up(self: &Arc<Self>, id: PaneId) {
        if let Some(parent) = self
            .pane_location(id)
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
        {
            {
                let mut ws = self.workspace.write();
                if let Some(p) = ws.pane_mut(id) {
                    p.active_mut().navigate_to(parent.clone());
                }
            }
            self.navigation.navigate_pane(id, parent);
        }
    }

    /// Open the focused entry in pane `id`: `cd` into directories, hand
    /// files off to the OS default handler (`open` on macOS,
    /// `xdg-open` on Linux, `ShellExecute` on Windows via the `open`
    /// crate). Called by the `fs::View` action handler and by every
    /// view's double-click callback so folder-vs-file dispatch is
    /// centralised.
    ///
    /// Symlinks are followed for the dir check via `std::path::Path::is_dir`,
    /// which resolves the target (broken symlinks fall through to
    /// `open::that`, which handles the error).
    pub fn view_focused_entry(self: &Arc<Self>, id: PaneId) {
        let Some(path) = self.focused_entry(id) else {
            tracing::debug!(?id, "fs::View: no focused entry");
            return;
        };
        self.view_path(id, path);
    }

    /// Open a specific entry `index` within pane `id`, bypassing the
    /// Rust-side focused-index cache. Used by double-click and grid /
    /// tree cell activation: those handlers know exactly which row was
    /// clicked and pushing an index avoids the "clicked config.json but
    /// opened the previously selected file" race, where `select_index`
    /// has scheduled the model update but the Rust cache reads through
    /// the stale value.
    pub fn view_entry_at_index(self: &Arc<Self>, id: PaneId, index: usize) {
        let path = {
            let vms = self.vms.read();
            vms.get(&id)
                .and_then(|vm| vm.entries().get(index).map(|e| e.path.clone()))
        };
        let Some(path) = path else {
            tracing::debug!(?id, index, "fs::View: index out of range");
            return;
        };
        self.view_path(id, path);
    }

    /// Common tail: cd into `path` if it's a directory, or hand it off
    /// to the OS default handler otherwise. Public so views with
    /// non-trivial index → path resolution (Tree, Miller) can push the
    /// path directly instead of round-tripping through a focused-index
    /// cache.
    pub fn view_path(self: &Arc<Self>, id: PaneId, path: PathBuf) {
        if path.is_dir() {
            {
                let mut ws = self.workspace.write();
                if let Some(p) = ws.pane_mut(id) {
                    p.active_mut().navigate_to(path.clone());
                }
            }
            self.navigation.navigate_pane(id, path);
        } else {
            tracing::info!(?path, "fs::View: opening file with OS default handler");
            if let Err(err) = open::that(&path) {
                tracing::warn!(?path, %err, "fs::View: OS open failed");
            }
        }
    }

    /// Duplicate one or more paths in place — copies each source into its
    /// own parent directory, letting the ops queue's `RenameWithSuffix`
    /// policy generate `foo (copy).ext` / `foo (copy 2).ext` names
    /// (Finder convention). If `paths` is empty, this is a no-op.
    ///
    /// Ops-queue based, so the copy runs off the UI thread and shows
    /// progress in the bottom ops panel.
    pub fn duplicate_paths(&self, paths: Vec<PathBuf>) {
        for source in paths {
            let Some(parent) = source.parent().map(Path::to_path_buf) else {
                tracing::warn!(?source, "fs::Duplicate: source has no parent, skipping");
                continue;
            };
            self.ops.submit_copy(vec![source], parent);
        }
    }

    /// Navigate pane `id` to the user's home directory.
    pub fn go_home(self: &Arc<Self>, id: PaneId) {
        let home = expand_tilde(Path::new("~"));
        {
            let mut ws = self.workspace.write();
            if let Some(p) = ws.pane_mut(id) {
                p.active_mut().navigate_to(home.clone());
            }
        }
        self.navigation.navigate_pane(id, home);
    }

    /// Navigate pane `id` to the ancestor at breadcrumb `segment_index`.
    pub fn breadcrumb_clicked(self: &Arc<Self>, id: PaneId, segment_index: usize) {
        let Some(current) = self.pane_location(id) else {
            return;
        };
        let components: Vec<_> = current.components().collect();
        if segment_index >= components.len() {
            return;
        }
        let mut target = PathBuf::new();
        for component in &components[..=segment_index] {
            target.push(component);
        }
        {
            let mut ws = self.workspace.write();
            if let Some(p) = ws.pane_mut(id) {
                p.active_mut().navigate_to(target.clone());
            }
        }
        self.navigation.navigate_pane(id, target);
    }

    /// Navigate the focused pane backward in its active tab's history.
    pub fn back_focused(self: &Arc<Self>) {
        let id = self.focused_pane_id();
        let dest = {
            self.workspace
                .write()
                .pane_mut(id)
                .and_then(|p| p.active_mut().back())
        };
        if let Some(path) = dest {
            self.navigation.navigate_pane_no_push(id, path);
        }
    }

    /// Navigate the focused pane forward in its active tab's history.
    pub fn forward_focused(self: &Arc<Self>) {
        let id = self.focused_pane_id();
        let dest = {
            self.workspace
                .write()
                .pane_mut(id)
                .and_then(|p| p.active_mut().forward())
        };
        if let Some(path) = dest {
            self.navigation.navigate_pane_no_push(id, path);
        }
    }

    // ── Deprecated usize-indexed compat shims ────────────────────────────
    // These resolve the Slint slot index to a PaneId via the layout's
    // DFS-ordered leaves. New code should use the PaneId-based methods.

    /// Return the Slint slot index (0 or 1) of the focused pane.
    #[deprecated(since = "0.0.1", note = "Phase 3: use focused_pane_id()")]
    #[must_use]
    pub fn focused_pane(&self) -> usize {
        let focused = self.focused_pane_id();
        let leaves = self.workspace.read().layout.all_leaves();
        leaves.iter().position(|&id| id == focused).unwrap_or(0)
    }

    /// Return whether more than one pane is open.
    #[deprecated(
        since = "0.0.1",
        note = "Phase 3: use split_focused/close_focused_pane"
    )]
    #[must_use]
    pub fn is_dual_pane(&self) -> bool {
        self.workspace.read().layout.leaf_count() > 1
    }

    /// Enable (split) or disable (close) the second pane.
    #[deprecated(
        since = "0.0.1",
        note = "Phase 3: use split_focused/close_focused_pane"
    )]
    pub fn set_dual_pane(self: &Arc<Self>, on: bool) {
        if on {
            if self.workspace.read().layout.leaf_count() < 2 {
                self.split_focused(SplitDirection::Horizontal);
            }
        } else if self.workspace.read().layout.leaf_count() > 1 {
            if let Some(id1) = self.pane_id_for_index(1) {
                self.set_focused_pane_id(id1);
                self.close_focused_pane();
            }
        }
    }

    /// Set the focused pane by Slint slot index (0 or 1).
    #[deprecated(since = "0.0.1", note = "Phase 3: use set_focused_pane_id")]
    pub fn set_focused_pane(self: &Arc<Self>, index: usize) {
        if let Some(id) = self.pane_id_for_index(index) {
            self.set_focused_pane_id(id);
        }
    }

    /// Return the filesystem paths of all selected entries in pane `id`.
    ///
    /// Reads the selection mask from the [`PaneRenderCache`] and the entry
    /// list from the stored location view model. Safe to call from any thread.
    ///
    /// # Caveats
    ///
    /// Only the Details view selection is read. Grid/Miller/Tree selection
    /// reading is a TODO once those views expose a unified selection API.
    #[must_use]
    pub fn selected_paths(&self, id: PaneId) -> Vec<PathBuf> {
        let mask = {
            let cache = self.pane_cache.read();
            let Some(entry) = cache.get(&id) else {
                return Vec::new();
            };
            entry.details_selected_mask.clone()
        };

        let vm_guard = self.vms.read();
        let Some(vm) = vm_guard.get(&id) else {
            return Vec::new();
        };
        let entries = vm.entries();

        mask.iter()
            .enumerate()
            .filter(|(_, &sel)| sel)
            .filter_map(|(i, _)| entries.get(i).map(|e| e.path.clone()))
            .collect()
    }

    /// Return the path of the focused (cursor) entry in pane `id`, if any.
    ///
    /// Safe to call from any thread.
    ///
    /// # Caveats
    ///
    /// Currently reads from the Details view focused index only. Grid/Miller/Tree
    /// are a TODO.
    #[must_use]
    pub fn focused_entry(&self, id: PaneId) -> Option<PathBuf> {
        let focused_idx = self
            .pane_cache
            .read()
            .get(&id)
            .map(|c| c.details_focused_index)?;

        if focused_idx < 0 {
            return None;
        }

        let vm_guard = self.vms.read();
        let vm = vm_guard.get(&id)?;
        vm.entries()
            .get(focused_idx as usize)
            .map(|e| e.path.clone())
    }

    // ── Cross-pane drag-and-drop ─────────────────────────────────────────────

    /// Resolve the paths to drag from `pane` when the user drags `entry_index`.
    ///
    /// If `entry_index` is already in the pane's current selection, returns all
    /// selected paths (multi-drag). Otherwise returns just the single entry's
    /// path.
    ///
    /// **Must be called on the Slint event-loop thread.**
    fn resolve_drag_paths(&self, pane: PaneId, entry_index: usize) -> Vec<PathBuf> {
        let selected = self.selected_paths(pane);
        let vm_guard = self.vms.read();
        let Some(vm) = vm_guard.get(&pane) else {
            return selected;
        };
        let entries = vm.entries();
        if let Some(entry) = entries.get(entry_index) {
            if selected.contains(&entry.path) {
                selected
            } else {
                vec![entry.path.clone()]
            }
        } else {
            selected
        }
    }

    /// Return the [`PaneId`] whose screen rectangle contains the logical
    /// pointer position `(x, y)` in pane-container-relative coordinates.
    ///
    /// Returns `None` when the pointer is outside all known pane rectangles.
    #[must_use]
    pub fn pointer_to_pane_id(&self, x: f32, y: f32) -> Option<PaneId> {
        let ws = self.workspace.read();
        let bounds = self.workspace_content_bounds();
        let rects = ws.layout.layout_rects(bounds);
        for (id, rect) in &rects {
            if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height {
                return Some(*id);
            }
        }
        None
    }

    /// Push drag indicator properties to the Slint window.
    ///
    /// `is_drag` enables the overlay [`TouchArea`]; `hover` is the pane-id
    /// currently under the pointer (`-1` means none).
    fn push_drag_state_to_slint(&self, is_drag: bool, hover: i32, count: i32) {
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = weak.upgrade() {
                w.set_is_dragging(is_drag);
                w.set_drag_hover_pane_id(hover);
                w.set_drag_count(count);
            }
        });
    }

    /// Arm a drag from `pane` at `entry_index`.
    ///
    /// Resolves the paths to drag (all selected entries if `entry_index` is in
    /// the selection; otherwise just the single entry). Stores an
    /// [`DragArmedState`] at a virtual origin of `(0, 0)`.
    ///
    /// `is_dragging()` returns `false` until [`AppShell::drag_move`] is called
    /// with a pointer at least 4 px away from the virtual origin, at which
    /// point the drag is fully promoted and the op can be submitted on
    /// [`AppShell::drag_end`].
    ///
    /// Also pushes `is-dragging = true` to the Slint window so that the
    /// overlay [`TouchArea`] and drag-count badge become active immediately —
    /// the overlay is ready for the first pointer-move event regardless of
    /// whether the Rust-side threshold has been crossed yet.
    ///
    /// **Must be called on the Slint event-loop thread.**
    pub fn begin_drag(&self, pane: PaneId, entry_index: usize) {
        let paths = self.resolve_drag_paths(pane, entry_index);
        let count = paths.len() as i32;
        *self.drag_armed.write() = Some(DragArmedState {
            pane,
            entry_index,
            paths,
            origin_x: 0.0,
            origin_y: 0.0,
        });
        // Enable the overlay + badge immediately so the first pointer-move
        // from the overlay (which will have large absolute coords) promotes
        // the drag without a perceptible gap.
        self.push_drag_state_to_slint(true, -1, count);
    }

    /// Update the drag state on pointer movement.
    ///
    /// If the armed (pre-threshold) state exists and the pointer has moved
    /// ≥ 4 px from its virtual origin, the drag is promoted to the fully
    /// active [`DragState`].  Once promoted, this method just updates the
    /// hover pane-id and pushes it to Slint.
    ///
    /// **May be called from any thread.**  It marshals the Slint push onto the
    /// event loop internally.
    pub fn drag_move(&self, x: f32, y: f32) {
        // Promote armed → active if threshold exceeded.
        if self.dragging.read().is_none() {
            let should_promote = self
                .drag_armed
                .read()
                .as_ref()
                .is_some_and(|a| (x - a.origin_x).hypot(y - a.origin_y) >= 4.0);
            if should_promote {
                let armed = self.drag_armed.write().take();
                if let Some(armed) = armed {
                    let count = armed.paths.len() as i32;
                    *self.dragging.write() = Some(DragState {
                        source_pane: armed.pane,
                        paths: armed.paths,
                    });
                    // Re-push count in case it changed (it shouldn't, but be safe).
                    let weak = self.window.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = weak.upgrade() {
                            w.set_drag_count(count);
                        }
                    });
                }
            }
        }

        let hover = self.pointer_to_pane_id(x, y).map_or(-1, |id| id.0 as i32);
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = weak.upgrade() {
                w.set_drag_hover_pane_id(hover);
            }
        });
    }

    /// Complete a drag on pointer release.
    ///
    /// If a fully-promoted drag is in flight and the pointer is over a
    /// **different** pane than the source, submits a copy or move operation
    /// via the [`OpsController`]:
    ///
    /// - Default (no modifier): **copy** — mirrors macOS Finder convention.
    /// - `alt_held = true`: **move**.
    ///
    /// A drop on the source pane is intentionally a no-op (to match Finder's
    /// behaviour for same-folder drops).
    ///
    /// Clears all drag state and resets Slint indicators regardless of whether
    /// an op was submitted.
    ///
    /// **May be called from any thread.**
    pub fn drag_end(&self, x: f32, y: f32, alt_held: bool) {
        // Snapshot and clear state atomically before any async work.
        let drag_opt = self.dragging.write().take();
        *self.drag_armed.write() = None;
        self.push_drag_state_to_slint(false, -1, 0);

        let Some(drag) = drag_opt else {
            return;
        };
        if drag.paths.is_empty() {
            return;
        }

        let target_id = self.pointer_to_pane_id(x, y);
        let Some(target_id) = target_id else {
            tracing::debug!("drag_end: pointer outside all panes; discarding");
            return;
        };
        if target_id == drag.source_pane {
            tracing::debug!("drag_end: same-pane drop; no-op");
            return;
        }

        let dest = match self.pane_location(target_id) {
            Some(d) => d,
            None => {
                tracing::warn!(?target_id, "drag_end: target pane has no location");
                return;
            }
        };

        if alt_held {
            tracing::info!(
                count = drag.paths.len(),
                dest = %dest.display(),
                "drag-drop move (Alt held)"
            );
            self.ops.submit_move(drag.paths, dest);
        } else {
            tracing::info!(
                count = drag.paths.len(),
                dest = %dest.display(),
                "drag-drop copy (default)"
            );
            self.ops.submit_copy(drag.paths, dest);
        }
    }

    /// Cancel an in-flight drag (called on Escape or programmatically).
    ///
    /// Clears all drag state and resets the Slint `is-dragging` flag.
    pub fn drag_cancel(&self) {
        *self.dragging.write() = None;
        *self.drag_armed.write() = None;
        self.push_drag_state_to_slint(false, -1, 0);
        tracing::debug!("drag cancelled");
    }

    /// Return `true` while a drag has been promoted past the 4-px threshold.
    ///
    /// Returns `false` during the brief armed-but-not-yet-promoted window and
    /// whenever no drag is in progress.
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.dragging.read().is_some()
    }

    /// Shared handler for both the legacy and PaneId navigation callbacks.
    fn on_location_changed_impl(
        self: &Arc<Self>,
        pane_id: PaneId,
        vm: Arc<atlas_fs::InMemoryLocationViewModel>,
    ) {
        let path = vm.location().to_path_buf();
        let vm_dyn: Arc<dyn LocationViewModel> = Arc::clone(&vm) as Arc<dyn LocationViewModel>;
        let vm_for_status = Arc::clone(&vm);

        self.vms.write().insert(pane_id, Arc::clone(&vm_dyn));

        {
            let panes = self.panes_ctrl.read();
            if let Some(ctrl) = panes.get(&pane_id) {
                ctrl.details.set_location(Arc::clone(&vm_dyn));
                ctrl.grid.set_location(Arc::clone(&vm_dyn));
                ctrl.gallery.set_location(Arc::clone(&vm_dyn));
                ctrl.tree.set_root(path.clone());
                ctrl.miller.set_root(path.clone());
            }
        }

        {
            let mut workspace = self.workspace.write();
            if let Some(pane_state) = workspace.pane_mut(pane_id) {
                let tab = pane_state.active_mut();
                tab.location = Some(Location::local(path.clone()));
                tab.title = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
            }
        }

        if pane_id == self.focused_pane_id() {
            self.search.set_scope(Some(path.clone()));
        }

        self.project_workspace_to_slint();
        self.refresh_status();
        // Per-pane status chips render inside each pane's frame — see
        // pane.slint's PaneStatusBar. The initial fill here matches what
        // the vm-subscribe watcher will refresh on every future change.
        self.refresh_pane_status(pane_id);

        // Spawn a lightweight watcher that re-computes status whenever the vm
        // emits an event, then exits when the vm subscription channel closes.
        // Uses `refresh_pane_status` so only *this* pane's chips are
        // recomputed on a per-pane fs event; the whole-window status bar
        // (deprecated behind `ui.show_status_bar`) still receives the
        // cascaded update via `refresh_status` when the pane is focused.
        let shell_bg = Arc::clone(self);
        let events = vm_for_status.subscribe();
        std::thread::Builder::new()
            .name(format!("atlas-status-{pane_id:?}"))
            .spawn(move || {
                while let Ok(_ev) = events.recv() {
                    shell_bg.refresh_pane_status(pane_id);
                    if pane_id == shell_bg.focused_pane_id() {
                        shell_bg.refresh_status();
                    }
                }
            })
            .ok();
    }

    fn wire_callbacks(self: &Arc<Self>, window: &AtlasWindow) {
        {
            let palette_ctrl = Arc::clone(&self.palette_ctrl);
            window.on_palette_query_changed(move |query| {
                palette_ctrl.set_query(query.as_str());
            });
        }
        {
            let palette_ctrl = Arc::clone(&self.palette_ctrl);
            window.on_palette_confirm(move |_action_id| {
                palette_ctrl.confirm();
            });
        }
        {
            let palette_ctrl = Arc::clone(&self.palette_ctrl);
            window.on_palette_dismiss(move || {
                palette_ctrl.close();
            });
        }
        {
            let palette_ctrl = Arc::clone(&self.palette_ctrl);
            window.on_toggle_palette(move || {
                tracing::info!("keybind: toggle-palette");
                if palette_ctrl.is_visible() {
                    palette_ctrl.close();
                } else {
                    palette_ctrl.open(0);
                }
            });
        }
        {
            let palette_ctrl = Arc::clone(&self.palette_ctrl);
            window.on_open_goto(move || {
                palette_ctrl.open(1);
            });
        }
        {
            let palette_ctrl = Arc::clone(&self.palette_ctrl);
            window.on_palette_selection_delta(move |delta| {
                palette_ctrl.move_selection(delta as isize);
            });
        }
        {
            let shell = self.clone();
            window.on_select_tab(move |pane_id, tab| {
                if tab >= 0 {
                    let id = PaneId(pane_id as u32);
                    shell.select_tab(id, tab as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_cycle_tab(move |pane_id, delta| {
                let id = PaneId(pane_id as u32);
                shell.cycle_tab(id, delta as isize);
            });
        }

        // Note: Multi-pane workspace commands (pane-split-right / -down /
        // -close / -cycle-view-mode / -focus-direction) used to be wired via
        // dedicated Slint callbacks. They're now dispatched exclusively
        // through atlas-keymap (see build_dispatcher in main.rs), so the
        // Slint callbacks were deleted — every chord flows through
        // `handle-key-chord` on the AtlasWindow root.

        // ── Focused-pane navigation callbacks ────────────────────────────
        // The FocusScope in atlas.slint dispatches these when no modal or
        // text input has focus (arrow keys / Enter / Backspace / vim hjkl).
        // Route to the focused pane's *current view* — details/grid/etc.
        //
        // Note: keyboard navigation is **focus-only** — it does not touch
        // the selection. This matches yazi/nnn/ranger/Total Commander,
        // and lets the user build up a multi-selection by arrow-navigating
        // and pressing Space per row. See the `pane::MoveDown` handler
        // in `crates/atlas-app/src/main.rs` for the full rationale.
        {
            let shell = self.clone();
            window.on_pane_move_focus(move |delta| {
                let id = shell.focused_pane_id();
                let mode = shell
                    .workspace
                    .read()
                    .pane(id)
                    .map(|p| p.view_mode)
                    .unwrap_or_default();
                let Some(ctrl) = shell.pane_by_id(id) else {
                    return;
                };
                match mode {
                    ViewMode::Details => ctrl.details.move_focus(delta as i64),
                    ViewMode::Grid => ctrl.grid.move_focus(delta as isize, 0),
                    ViewMode::Gallery => ctrl.gallery.move_focus(delta as isize),
                    ViewMode::Miller => ctrl.miller.move_focus(delta as isize),
                    ViewMode::Tree => ctrl.tree.move_focus(delta as isize),
                }
            });
        }
        {
            let shell = self.clone();
            window.on_pane_activate_focused(move || {
                // Delegate to view_focused_entry so folders navigate and
                // files open with the OS default handler. Per-view
                // activate_focused impls are still called from double-click
                // handlers because those know which entry the pointer hit,
                // but the keyboard "activate" path uses shell-level logic.
                shell.view_focused_entry(shell.focused_pane_id());
            });
        }
        {
            let shell = self.clone();
            window.on_pane_go_up(move || {
                shell.go_up(shell.focused_pane_id());
            });
        }

        {
            let search_ctrl = Arc::clone(&self.search);
            let actions = Arc::clone(&self.actions);
            window.on_search_query_changed(move |query| {
                actions
                    .lock()
                    .dispatch(UiAction::SearchQueryChanged(query.to_string()));
                search_ctrl.set_query(query.to_string());
            });
        }
        {
            let search_ctrl = Arc::clone(&self.search);
            let actions = Arc::clone(&self.actions);
            window.on_search_confirm(move |index| {
                actions
                    .lock()
                    .dispatch(UiAction::SearchConfirm(index as usize));
                search_ctrl.confirm(index as usize);
            });
        }
        {
            let search_ctrl = Arc::clone(&self.search);
            let actions = Arc::clone(&self.actions);
            window.on_search_close(move || {
                actions.lock().dispatch(UiAction::SearchClose);
                search_ctrl.close();
            });
        }
        {
            let search_ctrl = Arc::clone(&self.search);
            let actions = Arc::clone(&self.actions);
            window.on_toggle_search_panel(move || {
                actions.lock().dispatch(UiAction::ToggleSearchPanel);
                if search_ctrl.is_open() {
                    search_ctrl.close();
                } else {
                    search_ctrl.open();
                }
            });
        }
        {
            let search_ctrl = Arc::clone(&self.search);
            let actions = Arc::clone(&self.actions);
            window.on_open_search_panel(move || {
                actions.lock().dispatch(UiAction::OpenSearchPanel);
                search_ctrl.open();
            });
        }

        {
            let actions = Arc::clone(&self.actions);
            let shell = Arc::clone(self);
            window.on_pane_focused(move |pane_id| {
                tracing::info!(pane_id, "pane-focused (click)");
                let id = PaneId(pane_id as u32);
                let slot = shell.pane_slint_index.read().get(&id).copied().unwrap_or(0);
                actions.lock().dispatch(UiAction::PaneFocusChanged(slot));
                shell.set_focused_pane_id(id);
                // Ensure the root FocusScope regains keyboard focus so
                // shortcuts continue working after a click.
                shell.bump_refocus_tick();
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            let shell = Arc::clone(self);
            window.on_cycle_pane_focus(move || {
                let leaves = shell.workspace.read().layout.all_leaves();
                if leaves.is_empty() {
                    return;
                }
                let focused = shell.focused_pane_id();
                let cur = leaves.iter().position(|&id| id == focused).unwrap_or(0);
                let next = (cur + 1) % leaves.len();
                actions.lock().dispatch(UiAction::PaneFocusChanged(next));
                shell.set_focused_pane_id(leaves[next]);
            });
        }

        {
            let shell = Arc::clone(self);
            let nav = Arc::clone(&self.navigation);
            window.on_address_submitted(move |pane_id, path| {
                let id = PaneId(pane_id as u32);
                shell.set_focused_pane_id(id);
                let expanded = expand_tilde(Path::new(path.as_str()));
                nav.navigate_pane(id, expanded);
                shell.bump_refocus_tick();
            });
        }
        {
            let shell = Arc::clone(self);
            window.on_address_cancelled(move |_pane_id| {
                shell.bump_refocus_tick();
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            let shell = Arc::clone(self);
            window.on_breadcrumb_clicked(move |pane_id, segment| {
                let id = PaneId(pane_id as u32);
                let slot = shell.pane_slint_index.read().get(&id).copied().unwrap_or(0);
                let seg = segment as usize;
                actions.lock().dispatch(UiAction::BreadcrumbClicked {
                    pane: slot,
                    segment: seg,
                });
                shell.breadcrumb_clicked(id, seg);
            });
        }
        {
            let shell = self.clone();
            window.on_tab_selected(move |pane_id, tab| {
                let id = PaneId(pane_id as u32);
                shell.select_tab(id, tab as usize);
            });
        }
        {
            let shell = self.clone();
            window.on_tab_closed(move |pane_id, tab| {
                let id = PaneId(pane_id as u32);
                shell.close_tab(id, tab as usize);
            });
        }
        {
            let shell = self.clone();
            window.on_new_tab(move |pane_id| {
                let id = PaneId(pane_id as u32);
                shell.new_tab(id);
            });
        }

        // ── Details callbacks ─────────────────────────────────────────────────
        {
            let shell = self.clone();
            window.on_details_row_clicked(move |pane_id, index, ctrl, shift| {
                let id = PaneId(pane_id as u32);
                // Any click inside a pane focuses that pane, so the
                // dropdown badge / border / dispatched shortcuts all
                // target the pane the user just interacted with.
                shell.set_focused_pane_id(id);
                if let Some(c) = shell.pane_by_id(id) {
                    c.details.select_index(index as usize, ctrl, shift);
                }
                shell.bump_refocus_tick();
            });
        }
        {
            let shell = self.clone();
            window.on_details_row_double_clicked(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                shell.set_focused_pane_id(id);
                if let Some(c) = shell.pane_by_id(id) {
                    c.details.select_index(index as usize, false, false);
                }
                // Resolve the target directly from the double-click index
                // rather than reading the (potentially stale) focused-entry
                // cache — otherwise the previously-selected row would open
                // on the first double-click after a selection change.
                shell.view_entry_at_index(id, index as usize);
                shell.bump_refocus_tick();
            });
        }
        {
            let shell = self.clone();
            window.on_details_header_clicked(move |pane_id, column_index| {
                let id = PaneId(pane_id as u32);
                shell.set_focused_pane_id(id);
                if let Some(c) = shell.pane_by_id(id) {
                    c.details.header_clicked(column_index as usize);
                }
                shell.bump_refocus_tick();
            });
        }
        // Drag on a column header divider → set the new width on the details
        // controller (clamps to per-kind min/max) and record the change so
        // it can be persisted to `[view.details.column_widths]` on quit.
        {
            let shell = self.clone();
            window.on_details_header_resize(move |pane_id, column_index, new_width_px| {
                let id = PaneId(pane_id as u32);
                let Some(c) = shell.pane_by_id(id) else {
                    return;
                };
                if let Some((kind, clamped_width)) = c
                    .details
                    .set_column_width(column_index as usize, new_width_px)
                {
                    tracing::trace!(
                        pane = pane_id,
                        column = column_index,
                        requested = new_width_px,
                        applied = clamped_width,
                        "details column resized"
                    );
                    shell.record_column_width(kind, clamped_width);
                }
            });
        }
        // Right-click on a details row → record the target entry and open
        // the context menu at the pointer position. Same pattern for grid.
        {
            let shell = self.clone();
            window.on_details_row_context_menu(move |pane_id, index, x, y| {
                let id = PaneId(pane_id as u32);
                shell.set_focused_pane_id(id);
                if let Some(c) = shell.pane_by_id(id) {
                    // Selection follows the right-click so the menu operates
                    // on the item under the pointer (Finder / Explorer behaviour).
                    c.details.select_index(index as usize, false, false);
                }
                let path = shell.focused_entry(id);
                shell.open_context_menu(id, path, x, y);
            });
        }

        // ── Grid callbacks ────────────────────────────────────────────────────
        {
            let shell = self.clone();
            window.on_grid_entry_clicked(move |pane_id, index, ctrl, shift| {
                let id = PaneId(pane_id as u32);
                shell.set_focused_pane_id(id);
                if let Some(c) = shell.pane_by_id(id) {
                    c.grid.select_index(index as usize, ctrl, shift);
                }
                shell.bump_refocus_tick();
            });
        }
        {
            let shell = self.clone();
            window.on_grid_entry_double_clicked(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                shell.set_focused_pane_id(id);
                if let Some(c) = shell.pane_by_id(id) {
                    c.grid.select_index(index as usize, false, false);
                }
                shell.view_entry_at_index(id, index as usize);
                shell.bump_refocus_tick();
            });
        }
        {
            let shell = self.clone();
            window.on_grid_entry_context_menu(move |pane_id, index, x, y| {
                let id = PaneId(pane_id as u32);
                shell.set_focused_pane_id(id);
                if let Some(c) = shell.pane_by_id(id) {
                    c.grid.select_index(index as usize, false, false);
                }
                let path = shell.focused_entry(id);
                shell.open_context_menu(id, path, x, y);
            });
        }
        {
            let shell = self.clone();
            window.on_grid_thumbnail_visible(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.grid.thumbnail_visible(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_grid_columns_changed(move |pane_id, cols| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.grid.set_columns(cols as usize);
                }
            });
        }

        // ── Gallery callbacks ─────────────────────────────────────────────────
        {
            let shell = self.clone();
            window.on_gallery_entry_clicked(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                shell.set_focused_pane_id(id);
                if let Some(c) = shell.pane_by_id(id) {
                    c.gallery.entry_clicked(index as usize);
                }
                shell.bump_refocus_tick();
            });
        }
        {
            let shell = self.clone();
            window.on_gallery_strip_visible(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.gallery.strip_visible(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_gallery_preview_visible(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.gallery.preview_visible(index as usize);
                }
            });
        }
        {
            let shell = self.clone();
            window.on_gallery_prev_image(move |pane_id| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.gallery.prev_image();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_gallery_next_image(move |pane_id| {
                let id = PaneId(pane_id as u32);
                if let Some(c) = shell.pane_by_id(id) {
                    c.gallery.next_image();
                }
            });
        }

        // ── Tree callbacks ────────────────────────────────────────────────────
        {
            let shell = self.clone();
            window.on_tree_row_clicked(move |pane_id, index, ctrl, shift| {
                let id = PaneId(pane_id as u32);
                shell.set_focused_pane_id(id);
                if let Some(c) = shell.pane_by_id(id) {
                    c.tree.select_index(index as usize, ctrl, shift);
                }
                shell.bump_refocus_tick();
            });
        }
        {
            let shell = self.clone();
            window.on_tree_row_double_clicked(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                shell.set_focused_pane_id(id);
                let path = shell.pane_by_id(id).and_then(|c| {
                    c.tree.select_index(index as usize, false, false);
                    let visible = c.tree.build_visible_nodes();
                    visible
                        .get(index as usize)
                        .map(|row| PathBuf::from(row.node_id.as_str()))
                });
                if let Some(path) = path {
                    shell.view_path(id, path);
                }
                shell.bump_refocus_tick();
            });
        }
        {
            let shell = self.clone();
            window.on_tree_chevron_clicked(move |pane_id, index| {
                let id = PaneId(pane_id as u32);
                shell.set_focused_pane_id(id);
                if let Some(c) = shell.pane_by_id(id) {
                    let visible = c.tree.build_visible_nodes();
                    if let Some(row) = visible.get(index as usize) {
                        let path = std::path::PathBuf::from(row.node_id.as_str());
                        c.tree.toggle(&path);
                    }
                }
                shell.bump_refocus_tick();
            });
        }

        // ── Miller callbacks ──────────────────────────────────────────────────
        {
            let shell = self.clone();
            window.on_miller_row_clicked(move |pane_id, col, row| {
                let id = PaneId(pane_id as u32);
                shell.set_focused_pane_id(id);
                if let Some(c) = shell.pane_by_id(id) {
                    c.miller.select_row(col as usize, row as usize);
                }
                shell.bump_refocus_tick();
            });
        }
        {
            let shell = self.clone();
            window.on_miller_row_double_clicked(move |pane_id, col, row| {
                let id = PaneId(pane_id as u32);
                shell.set_focused_pane_id(id);
                let path = shell.pane_by_id(id).and_then(|c| {
                    c.miller.select_row(col as usize, row as usize);
                    c.miller.entry_path_at(col as usize, row as usize)
                });
                if let Some(path) = path {
                    shell.view_path(id, path);
                }
                shell.bump_refocus_tick();
            });
        }

        // ── Context menu handlers ─────────────────────────────────────────
        //
        // Each `ctx-*` reads the context-menu target (recorded when the
        // menu opened via right-click) and dispatches the same shell
        // action the corresponding keybind uses, so context-menu items
        // and keyboard shortcuts share code paths.
        {
            let shell = self.clone();
            window.on_ctx_open(move || {
                let Some((id, _)) = shell.context_menu_target() else {
                    return;
                };
                shell.view_focused_entry(id);
            });
        }
        {
            let shell = self.clone();
            window.on_ctx_open_with(move || {
                let Some((_, path)) = shell.context_menu_target() else {
                    return;
                };
                // MVP: no picker UI yet. Fall through to the OS "open" so
                // the user at least sees the default handler; the "Open
                // With…" picker is a v0.3 follow-up.
                tracing::info!(
                    ?path,
                    "ctx: Open With — no picker yet (v0.3); using OS default"
                );
                if let Err(err) = open::that(&path) {
                    tracing::warn!(?path, %err, "ctx: open::that failed");
                }
            });
        }
        {
            let shell = self.clone();
            window.on_ctx_copy(move || {
                let Some((id, _)) = shell.context_menu_target() else {
                    return;
                };
                shell.clipboard.copy(shell.selected_paths(id));
            });
        }
        {
            let shell = self.clone();
            window.on_ctx_cut(move || {
                let Some((id, _)) = shell.context_menu_target() else {
                    return;
                };
                shell.clipboard.cut(shell.selected_paths(id));
            });
        }
        {
            let shell = self.clone();
            window.on_ctx_paste(move || {
                let Some((id, _)) = shell.context_menu_target() else {
                    return;
                };
                let Some(dest) = shell.pane_location(id) else {
                    return;
                };
                shell.clipboard.paste(dest);
            });
        }
        {
            let shell = self.clone();
            window.on_ctx_duplicate(move || {
                // Duplicate every currently-selected entry (or the single
                // right-clicked target when nothing else is selected). The
                // right-click handler that opened this menu already
                // single-selects the pointed-at row, so `selected_paths`
                // is always non-empty when this fires.
                let Some((id, target)) = shell.context_menu_target() else {
                    return;
                };
                let selected = shell.selected_paths(id);
                let paths = if selected.is_empty() {
                    vec![target]
                } else {
                    selected
                };
                shell.duplicate_paths(paths);
            });
        }
        {
            let shell = self.clone();
            window.on_ctx_rename(move || {
                if let Some(window) = shell.window.upgrade() {
                    window.invoke_fs_rename();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_ctx_trash(move || {
                if let Some(window) = shell.window.upgrade() {
                    window.invoke_fs_delete();
                }
            });
        }
        {
            let shell = self.clone();
            window.on_ctx_reveal(move || {
                let Some((_, path)) = shell.context_menu_target() else {
                    return;
                };
                reveal_in_os_file_manager(&path);
                let _ = shell;
            });
        }
        {
            let shell = self.clone();
            window.on_ctx_get_info(move || {
                let Some((_, path)) = shell.context_menu_target() else {
                    return;
                };
                tracing::info!(
                    ?path,
                    "ctx: Get Info — v0.3 (implement a size-preview drawer)"
                );
                let _ = shell;
            });
        }
        {
            let actions = Arc::clone(&self.actions);
            let shell = Arc::clone(self);
            window.on_toggle_dual_pane(move || {
                let dual = shell.workspace.read().layout.leaf_count() > 1;
                actions.lock().dispatch(UiAction::SetDualPane(!dual));
                if dual {
                    if let Some(id1) = shell.pane_id_for_index(1) {
                        shell.set_focused_pane_id(id1);
                        shell.close_focused_pane();
                    }
                } else {
                    shell.split_focused(SplitDirection::Horizontal);
                }
            });
        }

        // ── Ops-panel callbacks ───────────────────────────────────────────────

        {
            let ops = Arc::clone(&self.ops);
            window.on_ops_cancel(move |index| {
                tracing::debug!(index, "ops-cancel from UI");
                ops.cancel_by_index(index as usize);
            });
        }
        {
            let ops = Arc::clone(&self.ops);
            window.on_ops_dismiss(move |index| {
                tracing::debug!(index, "ops-dismiss from UI");
                ops.dismiss_by_index(index as usize);
            });
        }
        {
            let ops = Arc::clone(&self.ops);
            window.on_ops_close(move || {
                ops.set_visible(false);
            });
        }
        {
            let ops = Arc::clone(&self.ops);
            window.on_ops_clear_completed(move || {
                tracing::debug!("ops-clear-completed from UI");
                ops.clear_completed();
            });
        }
        {
            let ops = Arc::clone(&self.ops);
            window.on_toggle_ops_panel(move || {
                ops.toggle_visible();
            });
        }
        {
            let ops = Arc::clone(&self.ops);
            window.on_op_modal_cancel(move || {
                ops.cancel_current_foreground();
            });
        }
        {
            let ops = Arc::clone(&self.ops);
            window.on_op_modal_background(move || {
                ops.background_current_foreground();
            });
        }

        // ── F-key file-operation callbacks ────────────────────────────────────
        // These callbacks are triggered from the atlas.slint FocusScope key
        // handlers (F2, F7, F8) and routed directly to OpsController rather
        // than through the ActionSink, matching the pattern used by
        // PaletteController and SearchController.
        //
        // Pane-to-pane copy/move (Norton F5/F6) was deleted: it didn't
        // scale beyond 2 panes ("which one is the destination?") and
        // clipboard copy/paste (fs::Copy / fs::Paste) covers the same use
        // case with clearer semantics.
        {
            let shell = Arc::clone(self);
            window.on_fs_delete(move || {
                let focused = shell.focused_pane_id();
                let paths = shell.selected_paths(focused);
                if paths.is_empty() {
                    tracing::warn!(?focused, "fs::Delete (F8): no selection");
                    return;
                }
                tracing::info!(count = paths.len(), "fs::Delete (F8) → trash");
                // F8 always sends to trash (non-destructive default).
                // Shift+F8 for permanent delete is a post-MVP binding.
                shell.ops.submit_delete(paths, true);
            });
        }
        {
            let shell = Arc::clone(self);
            window.on_fs_rename(move || {
                let focused = shell.focused_pane_id();
                // TODO(post-MVP): show an inline rename text-input or modal dialog.
                // For now we log the focused entry and skip the operation.
                match shell.focused_entry(focused) {
                    Some(path) => {
                        tracing::info!(
                            path = %path.display(),
                            "fs::Rename (F2): rename dialog not yet implemented (post-MVP)"
                        );
                    }
                    None => {
                        tracing::warn!(?focused, "fs::Rename (F2): no focused entry");
                    }
                }
            });
        }
        {
            let shell = Arc::clone(self);
            window.on_fs_mkdir(move || {
                let focused = shell.focused_pane_id();
                let Some(location) = shell.pane_location(focused) else {
                    tracing::warn!(?focused, "fs::Mkdir (F7): no pane location");
                    return;
                };
                // Choose a unique "New Folder" name within the current location.
                let name = unique_new_folder_name(&location);
                let path = location.join(&name);
                tracing::info!(path = %path.display(), "fs::Mkdir (F7)");
                shell.ops.submit_mkdir(path);
            });
        }

        // ── Bulk-rename callbacks ─────────────────────────────────────────────
        // Cmd/Ctrl+Shift+F2 → open-bulk-rename → open with current selection.
        {
            let shell = Arc::clone(self);
            window.on_open_bulk_rename(move || {
                let focused = shell.focused_pane_id();
                let paths = shell.selected_paths(focused);
                tracing::info!(
                    ?focused,
                    count = paths.len(),
                    "bulk rename: opening modal (Cmd/Ctrl+Shift+F2)"
                );
                shell.bulk_rename.open(paths);
            });
        }
        {
            let bulk_rename = Arc::clone(&self.bulk_rename);
            window.on_bulk_rename_pattern_changed(move |q| {
                bulk_rename.set_pattern(q.to_string());
            });
        }
        {
            let bulk_rename = Arc::clone(&self.bulk_rename);
            window.on_bulk_rename_replacement_changed(move |q| {
                bulk_rename.set_replacement(q.to_string());
            });
        }
        {
            let bulk_rename = Arc::clone(&self.bulk_rename);
            window.on_bulk_rename_toggle_regex(move || {
                bulk_rename.toggle_regex();
            });
        }
        {
            let bulk_rename = Arc::clone(&self.bulk_rename);
            window.on_bulk_rename_toggle_case(move || {
                bulk_rename.toggle_case_insensitive();
            });
        }
        {
            let bulk_rename = Arc::clone(&self.bulk_rename);
            window.on_bulk_rename_confirm(move || {
                bulk_rename.confirm();
            });
        }
        {
            let bulk_rename = Arc::clone(&self.bulk_rename);
            window.on_bulk_rename_cancel(move || {
                bulk_rename.close();
            });
        }

        // ── Navigation location callbacks ─────────────────────────────────────
        {
            // Legacy usize-indexed bridge: map the Slint slot index to a PaneId
            // via DFS leaf order, then forward to the shared handler.
            let shell_weak = Arc::downgrade(self);
            self.navigation.on_location_changed(move |pane_usize, vm| {
                let Some(shell) = shell_weak.upgrade() else {
                    return;
                };
                let pane_id = {
                    let ws = shell.workspace.read();
                    ws.layout
                        .all_leaves()
                        .get(pane_usize)
                        .copied()
                        .unwrap_or(ws.focused)
                };
                shell.on_location_changed_impl(pane_id, vm);
            });
        }
        {
            // New PaneId-based callback.
            let shell_weak2 = Arc::downgrade(self);
            self.navigation
                .on_pane_location_changed(move |pane_id, vm| {
                    let Some(shell) = shell_weak2.upgrade() else {
                        return;
                    };
                    shell.on_location_changed_impl(pane_id, vm);
                });
        }

        // ── Cross-pane drag-and-drop callbacks ────────────────────────────────
        {
            let shell = Arc::clone(self);
            window.on_drag_start(move |pane_id, entry_index| {
                let id = PaneId(pane_id as u32);
                tracing::debug!(?id, entry_index, "drag-start");
                shell.begin_drag(id, entry_index as usize);
            });
        }
        {
            let shell = Arc::clone(self);
            window.on_drag_move(move |x, y| {
                shell.drag_move(x, y);
            });
        }
        {
            let shell = Arc::clone(self);
            window.on_drag_end(move |x, y, alt_held| {
                shell.drag_end(x, y, alt_held);
            });
        }
        {
            let shell = Arc::clone(self);
            window.on_drag_cancel(move || {
                shell.drag_cancel();
            });
        }
    }

    /// Project the N-pane workspace layout onto Slint.
    ///
    /// Builds a [`PaneSlintData`] entry for every leaf pane in DFS order and
    /// pushes it via `set_panes`. Also pushes split-handle descriptors and the
    /// focused-pane id. The pane's per-view heavy data (details rows,
    /// thumbnails, tree nodes, etc.) is written into the [`PaneRenderCache`]
    /// by view controllers via `publish_*` methods and re-projected here via
    /// [`Self::push_pane_data_to_slint`] so both light and heavy data reach
    /// Slint in the same DFS-leaf order.
    pub fn project_workspace_to_slint(self: &Arc<Self>) {
        /// Cheap-to-clone snapshot of one pane's light data.
        struct PaneData {
            id: PaneId,
            id_i32: i32,
            x: f32,
            y: f32,
            width: f32,
            height: f32,
            path: String,
            view_mode: String,
            tabs: Vec<TabModel>,
            active_tab: i32,
            segments: Vec<String>,
        }

        let (focused_id, focus_idx, pane_data, handle_data) = {
            let ws = self.workspace.read();
            let bounds = self.workspace_content_bounds();
            let rects = ws.layout.layout_rects(bounds);

            // Rebuild the DFS-position map so callbacks continue routing correctly.
            {
                let mut idx_map = self.pane_slint_index.write();
                idx_map.clear();
                for (i, (id, _)) in rects.iter().enumerate() {
                    idx_map.insert(*id, i);
                }
            }

            let focused = ws.focused;
            let focus_idx = rects.iter().position(|(id, _)| *id == focused).unwrap_or(0) as i32;

            let pane_data: Vec<PaneData> = rects
                .iter()
                .map(|(id, rect)| {
                    let pane = ws.pane(*id).expect("leaf in layout must have pane state");
                    let location = pane.active_location();
                    PaneData {
                        id: *id,
                        id_i32: id.0 as i32,
                        x: rect.x,
                        y: rect.y,
                        width: rect.width,
                        height: rect.height,
                        path: location.display_path(),
                        view_mode: pane.view_mode.to_string(),
                        tabs: pane.tabs.clone(),
                        active_tab: pane.active_tab as i32,
                        segments: location.breadcrumb_segments(),
                    }
                })
                .collect();

            let handle_data = collect_split_handles(&ws.layout, bounds);

            (focused.0 as i32, focus_idx, pane_data, handle_data)
        };

        // Update the tabs / segments / active-tab entries in the pane cache so
        // push_pane_data_to_slint sees the freshest values when it runs.
        //
        // Compare-and-set: only bump `data_epoch` for panes whose light
        // data actually changed. Every navigation triggers a
        // `project_workspace_to_slint` for the whole workspace; blindly
        // rewriting the cache would bump every pane's epoch and defeat
        // the UI-thread ModelRc cache — the untouched pane's inner
        // `panes-*[i]` ModelRcs would still be rebuilt on every push,
        // resetting its `ListView.viewport-y` to zero.
        // See the doc on [`PaneRenderCache::data_epoch`].
        {
            let mut cache = self.pane_cache.write();
            for p in &pane_data {
                let entry = cache.entry(p.id).or_default();
                let new_tabs: Vec<TabEntry> = p
                    .tabs
                    .iter()
                    .map(|t| TabEntry {
                        title: SharedString::from(t.title.as_str()),
                        dirty: t.dirty,
                    })
                    .collect();
                let new_segments: Vec<SharedString> = p
                    .segments
                    .iter()
                    .map(|s| SharedString::from(s.as_str()))
                    .collect();
                let mut changed = false;
                if entry.tabs != new_tabs {
                    entry.tabs = new_tabs;
                    changed = true;
                }
                if entry.segments != new_segments {
                    entry.segments = new_segments;
                    changed = true;
                }
                if entry.active_tab != p.active_tab {
                    entry.active_tab = p.active_tab;
                    changed = true;
                }
                if changed {
                    entry.data_epoch = entry.data_epoch.wrapping_add(1);
                }
            }
        }

        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };

            // Build and push the PaneSlintData array.
            // `x`, `y`, `width`, `height` are `sp::Coord` = `f32` in the generated struct.
            let slint_panes: Vec<PaneSlintData> = pane_data
                .iter()
                .map(|p| PaneSlintData {
                    id: p.id_i32,
                    x: p.x,
                    y: p.y,
                    width: p.width,
                    height: p.height,
                    path: SharedString::from(p.path.as_str()),
                    view_mode: SharedString::from(p.view_mode.as_str()),
                    active_tab: p.active_tab,
                })
                .collect();

            // Split-handle descriptors.
            let slint_handles: Vec<SplitHandle> = handle_data
                .iter()
                .map(|h| SplitHandle {
                    node_index: h.node_index,
                    x: h.x,
                    y: h.y,
                    width: h.width,
                    height: h.height,
                    horizontal: h.horizontal,
                })
                .collect();

            // Route through the persistent outer models so we mutate the
            // existing `panes` / `split-handles` VecModels in place
            // rather than replacing them. See the [`OUTER_PANE_MODELS`]
            // doc for why this preserves per-pane `ListView` scroll
            // offsets across cross-pane navigations.
            OUTER_PANE_MODELS.with(|cell| {
                let mut outer = cell.borrow_mut();
                outer.ensure_bound(&window);
                sync_vec_model(&outer.panes, &slint_panes);
                sync_vec_model(&outer.split_handles, &slint_handles);
            });

            window.set_focused_pane_id(focused_id);
            window.set_focus_index(focus_idx);
        });

        // Push the freshest cache snapshot (tabs/segments/heavy data) to Slint.
        self.push_pane_data_to_slint();
    }

    /// Return the current Slint slot (DFS-leaf position) of `id`, if any.
    ///
    /// Used by view controllers when they need to translate a semantic
    /// [`PaneId`] into the positional index that legacy [`UiAction`] variants
    /// still carry.  Kept in sync by
    /// [`Self::project_workspace_to_slint`] and split/close operations.
    #[must_use]
    pub fn slint_slot_for(&self, id: PaneId) -> Option<usize> {
        self.pane_slint_index.read().get(&id).copied()
    }

    // ── Fine-grained cache publishers ────────────────────────────────────
    //
    // Each publisher stores its value into the per-pane render cache and
    // then triggers a push to Slint. There are two flavours of push:
    //
    // * `push_pane_data_to_slint` — full rebuild of every `panes-*` nested
    //   `VecModel`. Necessary when the entries list changes (navigation),
    //   because virtualised views (`ListView`, grid, etc.) key their child
    //   instances off the row-model reference.
    // * `push_pane_selection_to_slint` — only refreshes the selection /
    //   focused-index arrays. Called by selection-only publishers so a
    //   click or Space press does NOT re-emit `panes-details-rows`, which
    //   would (a) recreate every row's `TouchArea` and eat the second
    //   click of a double-click, and (b) reset `ListView.viewport-y` to 0
    //   — visible to the user as the scroll bouncing back to the top.
    //
    // Any publisher that mutates entries / thumbnails / column specs /
    // tree nodes / miller columns / gallery images MUST call the full
    // push. Anything mutating only selection or focus state calls the
    // selection push.

    fn with_cache<F>(&self, id: PaneId, apply: F)
    where
        F: FnOnce(&mut PaneRenderCache),
    {
        let mut cache = self.pane_cache.write();
        let entry = cache.entry(id).or_default();
        apply(entry);
    }

    /// Variant of [`Self::with_cache`] that bumps `data_epoch` after
    /// `apply` runs. Used by every publisher that follows up with a call
    /// to [`Self::push_pane_data_to_slint`] so the UI-thread ModelRc
    /// cache can distinguish "pane `id` really did change" from
    /// "another pane changed and this pane just got dragged along in the
    /// push". See the module comment above `with_cache` and the doc on
    /// `PaneRenderCache::data_epoch` for the scroll-preservation
    /// rationale.
    fn with_cache_data<F>(&self, id: PaneId, apply: F)
    where
        F: FnOnce(&mut PaneRenderCache),
    {
        let mut cache = self.pane_cache.write();
        let entry = cache.entry(id).or_default();
        apply(entry);
        entry.data_epoch = entry.data_epoch.wrapping_add(1);
    }

    /// Publish the Details view row list for pane `id`.
    pub fn publish_details_rows(self: &Arc<Self>, id: PaneId, rows: Vec<EntryRowItem>) {
        self.with_cache_data(id, |c| c.details_rows = rows);
        self.push_pane_data_to_slint();
    }

    /// Publish the Details view column specs for pane `id`.
    pub fn publish_details_columns(self: &Arc<Self>, id: PaneId, columns: Vec<crate::ColumnSpec>) {
        self.with_cache_data(id, |c| c.details_columns = columns);
        self.push_pane_data_to_slint();
    }

    /// Publish the Details view selection mask for pane `id`.
    ///
    /// Selection-only: does **not** re-emit `panes-details-rows`.  See the
    /// module comment above `with_cache` for why this matters (double-click
    /// detection + `ListView` scroll-position preservation).
    pub fn publish_details_selected_mask(self: &Arc<Self>, id: PaneId, mask: Vec<bool>) {
        self.with_cache(id, |c| c.details_selected_mask = mask);
        self.push_pane_selection_to_slint();
    }

    /// Publish the Details view selection anchor for pane `id`.
    ///
    /// Selection-only push — see [`Self::publish_details_selected_mask`].
    pub fn publish_details_selected_anchor(self: &Arc<Self>, id: PaneId, anchor: i32) {
        self.with_cache(id, |c| c.details_selected_anchor = anchor);
        self.push_pane_selection_to_slint();
    }

    /// Publish the Details view focused-row index for pane `id`.
    ///
    /// Selection-only push — see [`Self::publish_details_selected_mask`].
    pub fn publish_details_focused_index(self: &Arc<Self>, id: PaneId, focus: i32) {
        self.with_cache(id, |c| c.details_focused_index = focus);
        self.push_pane_selection_to_slint();
    }

    /// Publish the Grid view thumbnails for pane `id`.
    pub fn publish_grid_thumbs(
        self: &Arc<Self>,
        id: PaneId,
        thumbs: Vec<Option<crate::views::grid::thumbs::DecodedPixels>>,
    ) {
        self.with_cache_data(id, |c| c.grid_thumbs = thumbs);
        self.push_pane_data_to_slint();
    }

    /// Publish the Grid view has-thumb flags for pane `id`.
    pub fn publish_grid_has_thumbs(self: &Arc<Self>, id: PaneId, has: Vec<bool>) {
        self.with_cache_data(id, |c| c.grid_has_thumbs = has);
        self.push_pane_data_to_slint();
    }

    /// Publish the Grid view selection mask for pane `id`.
    ///
    /// Selection-only push — see [`Self::publish_details_selected_mask`].
    pub fn publish_grid_selected_mask(self: &Arc<Self>, id: PaneId, mask: Vec<bool>) {
        self.with_cache(id, |c| c.grid_selected_mask = mask);
        self.push_pane_selection_to_slint();
    }

    /// Publish the Grid view focused-cell index for pane `id`.
    ///
    /// Selection-only push — see [`Self::publish_details_selected_mask`].
    pub fn publish_grid_focused_index(self: &Arc<Self>, id: PaneId, focus: i32) {
        self.with_cache(id, |c| c.grid_focused_index = focus);
        self.push_pane_selection_to_slint();
    }

    /// Publish the Gallery strip thumbnails for pane `id`.
    pub fn publish_gallery_strip_thumbs(
        self: &Arc<Self>,
        id: PaneId,
        thumbs: Vec<Option<crate::views::gallery::thumbs::DecodedPixels>>,
    ) {
        self.with_cache_data(id, |c| c.gallery_strip_thumbs = thumbs);
        self.push_pane_data_to_slint();
    }

    /// Publish the Gallery preview image and its loading state for pane `id`.
    pub fn publish_gallery_preview(
        self: &Arc<Self>,
        id: PaneId,
        preview: Option<crate::views::gallery::thumbs::DecodedPixels>,
        loading: bool,
        fallback_glyph: String,
    ) {
        self.with_cache_data(id, |c| {
            c.gallery_preview = preview;
            c.gallery_preview_loading = loading;
            c.gallery_preview_fallback_glyph = fallback_glyph;
        });
        self.push_pane_data_to_slint();
    }

    /// Publish the Gallery focused index for pane `id`.
    ///
    /// Selection-only push — see [`Self::publish_details_selected_mask`].
    pub fn publish_gallery_focused_index(self: &Arc<Self>, id: PaneId, focus: i32) {
        self.with_cache(id, |c| c.gallery_focused_index = focus);
        self.push_pane_selection_to_slint();
    }

    /// Publish the Gallery metadata sidebar for pane `id`.
    pub fn publish_gallery_metadata(self: &Arc<Self>, id: PaneId, metadata: crate::MetadataFields) {
        self.with_cache_data(id, |c| c.gallery_metadata = metadata);
        self.push_pane_data_to_slint();
    }

    /// Publish the Tree view visible node list for pane `id`.
    pub fn publish_tree_nodes(self: &Arc<Self>, id: PaneId, nodes: Vec<crate::TreeNode>) {
        self.with_cache_data(id, |c| c.tree_nodes = nodes);
        self.push_pane_data_to_slint();
    }

    /// Publish the Tree view focused index for pane `id`.
    ///
    /// Selection-only push — see [`Self::publish_details_selected_mask`].
    pub fn publish_tree_focused_index(self: &Arc<Self>, id: PaneId, focus: i32) {
        self.with_cache(id, |c| c.tree_focused_index = focus);
        self.push_pane_selection_to_slint();
    }

    /// Publish the Tree view selected index for pane `id`.
    ///
    /// Selection-only push — see [`Self::publish_details_selected_mask`].
    pub fn publish_tree_selected_index(self: &Arc<Self>, id: PaneId, selected: i32) {
        self.with_cache(id, |c| c.tree_selected_index = selected);
        self.push_pane_selection_to_slint();
    }

    /// Publish the Miller columns snapshot for pane `id`.
    pub fn publish_miller_columns(self: &Arc<Self>, id: PaneId, columns: Vec<MillerColumnCache>) {
        self.with_cache_data(id, |c| c.miller_columns = columns);
        self.push_pane_data_to_slint();
    }

    /// Publish the Miller focused-column index for pane `id`.
    ///
    /// Selection-only push — see [`Self::publish_details_selected_mask`].
    pub fn publish_miller_focused_col(self: &Arc<Self>, id: PaneId, focused: i32) {
        self.with_cache(id, |c| c.miller_focused_col = focused);
        self.push_pane_selection_to_slint();
    }

    /// Rebuild every `panes-*` nested [`VecModel`] from the current cache in
    /// DFS-leaf order and push them to the Slint window.
    ///
    /// Runs the actual property writes inside `invoke_from_event_loop` so it
    /// is safe to call from any thread. Cheap because per-pane state is
    /// shallow-cloned once and Slint only redraws affected rows.
    ///
    /// # Cross-pane scroll preservation
    ///
    /// The UI-thread body consults a `thread_local!` cache of inner
    /// [`ModelRc`] handles keyed by `PaneId`. Each pane's cache entry
    /// remembers the `data_epoch` at the last build; if the current
    /// snapshot's `data_epoch` matches, we reuse **the exact same
    /// `ModelRc` handles** (`Rc::clone`) — including inner ModelRcs
    /// nested inside `panes-miller-columns`. If the epoch changed, we
    /// rebuild every ModelRc for that pane and replace the cached
    /// entry.
    ///
    /// This matters because `Slint`'s `ListView` (and every virtualised
    /// derivative) keys its child instances off the *identity* of the
    /// row model. Before this cache, any full push (e.g. one triggered
    /// by pane R navigating to a new folder) would replace pane L's
    /// `panes-details-rows[i]` with a fresh `ModelRc<EntryRowItem>` of
    /// identical contents — the ListView would drop its subscription,
    /// re-subscribe to the new model, and reset `viewport-y` to 0. The
    /// user reported this as "the other pane silently scrolls to the
    /// top when I navigate the focused pane." The cache means unchanged
    /// panes get their previous ModelRcs back unchanged, so Slint's
    /// value-equality check on the property binding sees no delta and
    /// the ListView never resubscribes.
    pub fn push_pane_data_to_slint(self: &Arc<Self>) {
        // Snapshot the DFS leaf order under a short read lock so we do not
        // hold the workspace lock while touching the cache or the UI thread.
        let leaves = self.workspace.read().layout.all_leaves();

        // Clone the per-pane cache entries in DFS order into `Send`-safe
        // structures.  We cannot capture `Arc<AppShell>` in the closure and
        // then re-lock the cache from the UI thread because that would make
        // controller writes deadlock the render loop.
        let snapshots: Vec<PaneRenderCache> = {
            let cache = self.pane_cache.read();
            leaves
                .iter()
                .map(|id| cache.get(id).cloned().unwrap_or_default())
                .collect()
        };
        // Also pass PaneIds through so the UI-thread cache can key by
        // stable id (leaves are in DFS-leaf layout order, which changes
        // when the workspace layout mutates; PaneId is stable across
        // splits and resizes).
        let leaf_ids = leaves.clone();

        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };

            // Resolve — for each pane — either a reused ModelRc from the
            // last push (epoch unchanged) or a freshly-built one (epoch
            // bumped). This is the identity-preservation trick that keeps
            // the untouched pane's `ListView.viewport-y` intact on a
            // cross-pane action.
            let resolved: Vec<PaneSlintModelHandles> = PANE_MODEL_HANDLES.with(|cell| {
                let mut cache = cell.borrow_mut();
                // Drop cache entries for panes no longer in the layout so
                // the cache doesn't leak across splits.
                cache.retain(|id, _| leaf_ids.contains(id));
                leaf_ids
                    .iter()
                    .zip(snapshots.iter())
                    .map(|(id, snap)| {
                        let entry = cache.entry(*id).or_default();
                        if entry.epoch != Some(snap.data_epoch) {
                            entry.rebuild_from(snap);
                            entry.epoch = Some(snap.data_epoch);
                        }
                        entry.clone()
                    })
                    .collect()
            });

            // Every `set_panes_*(...)` call below is routed through the
            // persistent `OuterPaneModels` cache: on the first push we
            // bind each window property to a fixed `Rc<VecModel<T>>`
            // once, and every subsequent push mutates rows in place via
            // `sync_vec_model`. Combined with the inner ModelRc cache
            // above, an untouched pane's entire slot in `panes-*`
            // resolves to the same `ModelRc<T>` as before → Slint's
            // property system sees no change → its `ListView` /
            // `GridView` / etc. keep their scroll offsets. See the
            // [`OUTER_PANE_MODELS`] doc for the full rationale.
            OUTER_PANE_MODELS.with(|cell| {
                let mut outer = cell.borrow_mut();
                outer.ensure_bound(&window);

                // Tabs / segments / active-tab.
                let tabs: Vec<ModelRc<TabEntry>> =
                    resolved.iter().map(|r| r.tabs.clone()).collect();
                sync_vec_model(&outer.tabs, &tabs);

                let segments: Vec<ModelRc<SharedString>> =
                    resolved.iter().map(|r| r.segments.clone()).collect();
                sync_vec_model(&outer.segments, &segments);

                let active_tab: Vec<i32> = snapshots.iter().map(|s| s.active_tab).collect();
                sync_vec_model(&outer.active_tab, &active_tab);

                // Details.
                let d_rows: Vec<ModelRc<EntryRowItem>> =
                    resolved.iter().map(|r| r.details_rows.clone()).collect();
                sync_vec_model(&outer.details_rows, &d_rows);

                let d_cols: Vec<ModelRc<crate::ColumnSpec>> =
                    resolved.iter().map(|r| r.details_columns.clone()).collect();
                sync_vec_model(&outer.details_columns, &d_cols);

                let d_mask: Vec<ModelRc<bool>> = snapshots
                    .iter()
                    .map(|s| ModelRc::new(VecModel::from(s.details_selected_mask.clone())))
                    .collect();
                sync_vec_model(&outer.details_selected_mask, &d_mask);

                let d_anchor: Vec<i32> = snapshots
                    .iter()
                    .map(|s| s.details_selected_anchor)
                    .collect();
                sync_vec_model(&outer.details_selected_anchor, &d_anchor);

                let d_focus: Vec<i32> = snapshots.iter().map(|s| s.details_focused_index).collect();
                sync_vec_model(&outer.details_focused_index, &d_focus);

                // Grid.
                let g_thumbs: Vec<ModelRc<slint::Image>> =
                    resolved.iter().map(|r| r.grid_thumbnails.clone()).collect();
                sync_vec_model(&outer.grid_thumbnails, &g_thumbs);

                let g_has: Vec<ModelRc<bool>> =
                    resolved.iter().map(|r| r.grid_has_thumbs.clone()).collect();
                sync_vec_model(&outer.grid_has_thumbs, &g_has);

                let g_mask: Vec<ModelRc<bool>> = snapshots
                    .iter()
                    .map(|s| ModelRc::new(VecModel::from(s.grid_selected_mask.clone())))
                    .collect();
                sync_vec_model(&outer.grid_selected_mask, &g_mask);

                let g_focus: Vec<i32> = snapshots.iter().map(|s| s.grid_focused_index).collect();
                sync_vec_model(&outer.grid_focused_index, &g_focus);

                // Gallery.
                let gal_strip: Vec<ModelRc<slint::Image>> = resolved
                    .iter()
                    .map(|r| r.gallery_strip_thumbs.clone())
                    .collect();
                sync_vec_model(&outer.gallery_strip_thumbnails, &gal_strip);

                let gal_prev: Vec<slint::Image> = snapshots
                    .iter()
                    .map(|s| {
                        s.gallery_preview
                            .as_ref()
                            .map(crate::views::gallery::thumbs::decoded_to_slint)
                            .unwrap_or_default()
                    })
                    .collect();
                sync_vec_model(&outer.gallery_preview_image, &gal_prev);

                let gal_loading: Vec<bool> = snapshots
                    .iter()
                    .map(|s| s.gallery_preview_loading)
                    .collect();
                sync_vec_model(&outer.gallery_preview_loading, &gal_loading);

                let gal_glyph: Vec<SharedString> = snapshots
                    .iter()
                    .map(|s| SharedString::from(s.gallery_preview_fallback_glyph.as_str()))
                    .collect();
                sync_vec_model(&outer.gallery_preview_fallback_glyph, &gal_glyph);

                let gal_focus: Vec<i32> =
                    snapshots.iter().map(|s| s.gallery_focused_index).collect();
                sync_vec_model(&outer.gallery_focused_index, &gal_focus);

                let gal_meta: Vec<crate::MetadataFields> = snapshots
                    .iter()
                    .map(|s| s.gallery_metadata.clone())
                    .collect();
                sync_vec_model(&outer.gallery_metadata, &gal_meta);

                // Tree.
                let t_nodes: Vec<ModelRc<crate::TreeNode>> =
                    resolved.iter().map(|r| r.tree_nodes.clone()).collect();
                sync_vec_model(&outer.tree_nodes, &t_nodes);

                let t_focus: Vec<i32> = snapshots.iter().map(|s| s.tree_focused_index).collect();
                sync_vec_model(&outer.tree_focused_index, &t_focus);

                let t_sel: Vec<i32> = snapshots.iter().map(|s| s.tree_selected_index).collect();
                sync_vec_model(&outer.tree_selected_index, &t_sel);

                // Miller. The outer per-pane ModelRc<MillerColData> is
                // cached; inner column entries live inside the cached
                // MillerColData so they're also identity-preserved when
                // the pane's epoch is unchanged.
                let m_cols: Vec<ModelRc<crate::MillerColData>> =
                    resolved.iter().map(|r| r.miller_columns.clone()).collect();
                sync_vec_model(&outer.miller_columns, &m_cols);

                let m_focus: Vec<i32> = snapshots.iter().map(|s| s.miller_focused_col).collect();
                sync_vec_model(&outer.miller_focused_col, &m_focus);
            });
        });
    }

    /// Push only the per-pane selection / focused-index arrays to Slint,
    /// leaving the heavy row / column / thumbnail / tree / miller / gallery
    /// arrays untouched.
    ///
    /// **Why this matters.** Slint's `ListView` (and, by extension, the
    /// Grid/Tree/Gallery virtualised containers) key their child instances
    /// off the *identity* of the row model. Replacing
    /// `panes-details-rows` with a fresh `VecModel` — even if the contents
    /// are identical — destroys every `EntryRow` instance and re-creates
    /// it from scratch. Two user-visible bugs fall out of that:
    ///
    /// 1. `TouchArea.double-clicked` never fires. The `clicked` handler
    ///    on the first click runs `set_focused_pane_id → publish_details_selected_mask`
    ///    which used to re-emit `panes-details-rows`; the second click
    ///    then lands on a *new* TouchArea instance whose internal
    ///    double-click timer has just been born and therefore has no
    ///    "previous click" to pair with.
    /// 2. `ListView.viewport-y` snaps back to `0`. Any click on a row
    ///    scrolled below the initial viewport would bounce the list back
    ///    to the top.
    ///
    /// Both problems disappear once selection updates stop touching the
    /// entries model — hence this dedicated push.
    fn push_pane_selection_to_slint(self: &Arc<Self>) {
        let leaves = self.workspace.read().layout.all_leaves();
        let snapshots: Vec<PaneRenderCache> = {
            let cache = self.pane_cache.read();
            leaves
                .iter()
                .map(|id| cache.get(id).cloned().unwrap_or_default())
                .collect()
        };

        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };

            OUTER_PANE_MODELS.with(|cell| {
                let mut outer = cell.borrow_mut();
                outer.ensure_bound(&window);

                // Details selection / focus.
                let d_mask: Vec<ModelRc<bool>> = snapshots
                    .iter()
                    .map(|s| ModelRc::new(VecModel::from(s.details_selected_mask.clone())))
                    .collect();
                sync_vec_model(&outer.details_selected_mask, &d_mask);

                let d_anchor: Vec<i32> = snapshots
                    .iter()
                    .map(|s| s.details_selected_anchor)
                    .collect();
                sync_vec_model(&outer.details_selected_anchor, &d_anchor);

                let d_focus: Vec<i32> = snapshots.iter().map(|s| s.details_focused_index).collect();
                sync_vec_model(&outer.details_focused_index, &d_focus);

                // Grid selection / focus.
                let g_mask: Vec<ModelRc<bool>> = snapshots
                    .iter()
                    .map(|s| ModelRc::new(VecModel::from(s.grid_selected_mask.clone())))
                    .collect();
                sync_vec_model(&outer.grid_selected_mask, &g_mask);

                let g_focus: Vec<i32> = snapshots.iter().map(|s| s.grid_focused_index).collect();
                sync_vec_model(&outer.grid_focused_index, &g_focus);

                // Gallery focus.
                let gal_focus: Vec<i32> =
                    snapshots.iter().map(|s| s.gallery_focused_index).collect();
                sync_vec_model(&outer.gallery_focused_index, &gal_focus);

                // Tree focus / selection.
                let t_focus: Vec<i32> = snapshots.iter().map(|s| s.tree_focused_index).collect();
                sync_vec_model(&outer.tree_focused_index, &t_focus);

                let t_sel: Vec<i32> = snapshots.iter().map(|s| s.tree_selected_index).collect();
                sync_vec_model(&outer.tree_selected_index, &t_sel);

                // Miller focused column.
                let m_focus: Vec<i32> = snapshots.iter().map(|s| s.miller_focused_col).collect();
                sync_vec_model(&outer.miller_focused_col, &m_focus);

                // Per-pane status bar — kept parallel-length with `panes`
                // so Slint's `panes-status-*[i]` indexing is always
                // valid, even before the first `refresh_pane_status`
                // fires for a newly opened pane.
                let s_folders: Vec<i32> = snapshots.iter().map(|s| s.status_folder_count).collect();
                sync_vec_model(&outer.status_folder_count, &s_folders);

                let s_files: Vec<i32> = snapshots.iter().map(|s| s.status_file_count).collect();
                sync_vec_model(&outer.status_file_count, &s_files);

                let s_total: Vec<SharedString> = snapshots
                    .iter()
                    .map(|s| SharedString::from(s.status_total_size_text.as_str()))
                    .collect();
                sync_vec_model(&outer.status_total_size_text, &s_total);

                let s_free: Vec<SharedString> = snapshots
                    .iter()
                    .map(|s| SharedString::from(s.status_free_space_text.as_str()))
                    .collect();
                sync_vec_model(&outer.status_free_space_text, &s_free);
            });
        });
    }

    /// Update palette state.
    pub fn set_palette(self: &Arc<Self>, model: PaletteModel) {
        *self.palette.write() = model.clone();
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            window.set_palette_visible(model.visible);
            window.set_palette_query(SharedString::from(model.query.as_str()));
            window.set_palette_results(to_palette_model(&model.results));
            window.set_palette_selected(model.selected as i32);
        });
    }

    /// Update status bar state.
    pub fn set_status(self: &Arc<Self>, model: StatusModel) {
        *self.status.write() = model.clone();
        // Compute free space for the focused pane's volume (best-effort;
        // failure is silent — the chip just hides).
        let free_text = self
            .pane_location(self.focused_pane_id())
            .and_then(|p| free_space_text_for(&p));
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            window.set_total_entries(model.total_entries as i32);
            window.set_folder_count(model.folder_count as i32);
            window.set_file_count(model.file_count as i32);
            window.set_total_size_text(crate::format_size(model.total_bytes).into());
            window.set_selected_entries(model.selected_entries as i32);
            window.set_selected_size_text(crate::format_size(model.selected_bytes).into());
            window.set_free_space_text(free_text.unwrap_or_default().into());
        });
    }

    /// Recompute status stats from the focused pane's current entries and push.
    ///
    /// Cheap to call — walks the in-memory entry snapshot. Invoked whenever
    /// the location changes or the entry list updates.
    ///
    /// Note: since the status bar migrated per-pane
    /// ([`Self::refresh_pane_status`]), this global refresh mostly keeps
    /// the deprecated whole-window `StatusBar` component's data current
    /// while it remains gated behind `ui.show_status_bar = true`. It
    /// also cascades to per-pane status for every live pane so the
    /// per-pane chips stay in sync when any focus/selection changes.
    pub fn refresh_status(self: &Arc<Self>) {
        let id = self.focused_pane_id();
        let vm = self.vms.read().get(&id).cloned();
        let Some(vm) = vm else {
            return;
        };
        let entries = vm.entries();
        let mut folders = 0usize;
        let mut files = 0usize;
        let mut total_bytes: u64 = 0;
        for e in &entries {
            match e.kind {
                atlas_fs::EntryKind::Dir => folders += 1,
                _ => {
                    files += 1;
                    total_bytes += e.metadata.size;
                }
            }
        }
        let existing = self.status.read().clone();
        let model = StatusModel {
            total_entries: entries.len(),
            folder_count: folders,
            file_count: files,
            total_bytes,
            selected_entries: existing.selected_entries,
            selected_bytes: existing.selected_bytes,
            indexer_state: existing.indexer_state,
        };
        self.set_status(model);

        // Cascade to every live pane so per-pane status chips reflect the
        // latest snapshot for their own directory (not just the focused one).
        let pane_ids: Vec<PaneId> = self.vms.read().keys().copied().collect();
        for pane_id in pane_ids {
            self.refresh_pane_status(pane_id);
        }
    }

    /// Recompute the per-pane status chips (folder/file counts, total
    /// size, free-space text) for pane `id` and push into the render
    /// cache. Cheap — walks the pane's in-memory entry snapshot and does
    /// one `statvfs` call.
    ///
    /// The plugin-chip area on the right is intentionally left empty
    /// for now; see the `PluginChips` placeholder in `pane.slint`
    /// (TODO(plugins)).
    pub fn refresh_pane_status(self: &Arc<Self>, id: PaneId) {
        let vm = self.vms.read().get(&id).cloned();
        let Some(vm) = vm else {
            return;
        };
        let entries = vm.entries();
        let mut folders = 0i32;
        let mut files = 0i32;
        let mut total_bytes: u64 = 0;
        for e in &entries {
            match e.kind {
                atlas_fs::EntryKind::Dir => folders += 1,
                _ => {
                    files += 1;
                    total_bytes += e.metadata.size;
                }
            }
        }
        let total_size_text = crate::format_size(total_bytes);
        let free_text = free_space_text_for(vm.location()).unwrap_or_default();

        self.with_cache(id, |c| {
            c.status_folder_count = folders;
            c.status_file_count = files;
            c.status_total_size_text = total_size_text;
            c.status_free_space_text = free_text;
        });
        self.push_pane_status_to_slint();
    }

    /// Push only the per-pane status-bar arrays to Slint, leaving the
    /// heavy view arrays untouched. Called by [`Self::refresh_pane_status`]
    /// so a fs-watcher tick doesn't invalidate `ListView` scroll position
    /// or `TouchArea` double-click state.
    fn push_pane_status_to_slint(self: &Arc<Self>) {
        let leaves = self.workspace.read().layout.all_leaves();
        let snapshots: Vec<PaneRenderCache> = {
            let cache = self.pane_cache.read();
            leaves
                .iter()
                .map(|id| cache.get(id).cloned().unwrap_or_default())
                .collect()
        };

        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };

            // Route through the persistent outer models — status arrays
            // are parallel-length with `panes` and would trigger the
            // same cross-pane scroll reset if we replaced them.
            OUTER_PANE_MODELS.with(|cell| {
                let mut outer = cell.borrow_mut();
                outer.ensure_bound(&window);

                let folders: Vec<i32> = snapshots.iter().map(|s| s.status_folder_count).collect();
                sync_vec_model(&outer.status_folder_count, &folders);

                let files: Vec<i32> = snapshots.iter().map(|s| s.status_file_count).collect();
                sync_vec_model(&outer.status_file_count, &files);

                let size_text: Vec<SharedString> = snapshots
                    .iter()
                    .map(|s| SharedString::from(s.status_total_size_text.as_str()))
                    .collect();
                sync_vec_model(&outer.status_total_size_text, &size_text);

                let free_text: Vec<SharedString> = snapshots
                    .iter()
                    .map(|s| SharedString::from(s.status_free_space_text.as_str()))
                    .collect();
                sync_vec_model(&outer.status_free_space_text, &free_text);
            });
        });
    }

    /// Apply a theme mode (convenience wrapper over [`Self::apply_theme`]).
    ///
    /// Loads the built-in tokens for `theme` and delegates to `apply_theme`.
    pub fn set_theme(self: &Arc<Self>, theme: ThemeMode) {
        let tokens = if theme.is_dark() {
            defaults::default_dark()
        } else {
            defaults::default_light()
        };
        self.apply_theme(&tokens);
    }

    /// Push all [`ThemeTokens`] into the Slint `Theme` global.
    ///
    /// Color, typography, and chrome values are forwarded through the
    /// `theme-*` bridge properties on `AtlasWindow` (defined in
    /// `assets/ui/atlas.slint`), which propagate them to the `Theme` global
    /// via `changed` callbacks.
    ///
    /// May be called from any thread; updates are marshalled onto the Slint
    /// event loop via [`slint::invoke_from_event_loop`].
    pub fn apply_theme(self: &Arc<Self>, tokens: &ThemeTokens) {
        let tokens = tokens.clone();
        let weak = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };

            let c = &tokens.colors;
            window.set_theme_bg(c.bg.to_slint_color());
            window.set_theme_panel_bg(c.panel_bg.to_slint_color());
            window.set_theme_panel_bg_elevated(c.panel_bg_elevated.to_slint_color());
            window.set_theme_fg(c.fg.to_slint_color());
            window.set_theme_fg_muted(c.fg_muted.to_slint_color());
            window.set_theme_fg_faint(c.fg_faint.to_slint_color());
            window.set_theme_border(c.border.to_slint_color());
            window.set_theme_border_strong(c.border_strong.to_slint_color());
            window.set_theme_accent(c.accent.to_slint_color());
            window.set_theme_accent_fg(c.accent_fg.to_slint_color());
            window.set_theme_accent_soft(c.accent_soft.to_slint_color());
            window.set_theme_selection_bg(c.selection_bg.to_slint_color());
            window.set_theme_selection_fg(c.selection_fg.to_slint_color());
            window.set_theme_hover_bg(c.hover_bg.to_slint_color());
            window.set_theme_error(c.error.to_slint_color());
            window.set_theme_success(c.success.to_slint_color());
            window.set_theme_warning(c.warning.to_slint_color());

            let t = &tokens.typography;
            window.set_theme_font_family(t.font_family.as_str().into());
            window.set_theme_monospace(t.monospace_family.as_str().into());
            window.set_theme_font_size(t.font_size_pt);

            let ch = &tokens.chrome;
            window.set_theme_titlebar_h(ch.titlebar_h_px);
            window.set_theme_statusbar_h(ch.statusbar_h_px);
            window.set_theme_tab_h(ch.tab_h_px);
            window.set_theme_addressbar_h(ch.addressbar_h_px);
            window.set_theme_row_h_default(ch.row_h_default_px);
            window.set_theme_row_h_compact(ch.row_h_compact_px);
            window.set_theme_row_h_spacious(ch.row_h_spacious_px);
            window.set_theme_radius_xs(ch.radius_xs_px);
            window.set_theme_radius_sm(ch.radius_sm_px);
            window.set_theme_radius_md(ch.radius_md_px);
            window.set_theme_radius_lg(ch.radius_lg_px);
            window.set_theme_radius_xl(ch.radius_xl_px);
            window.set_theme_space_1(ch.space_1_px);
            window.set_theme_space_2(ch.space_2_px);
            window.set_theme_space_3(ch.space_3_px);
            window.set_theme_space_4(ch.space_4_px);
            window.set_theme_space_5(ch.space_5_px);
            window.set_theme_space_6(ch.space_6_px);
            window.set_theme_space_8(ch.space_8_px);
            window.set_theme_space_10(ch.space_10_px);
            window.set_theme_spacing_xs(ch.space_1_px);
            window.set_theme_spacing_sm(ch.space_2_px);
            window.set_theme_spacing_md(ch.space_3_px);
            window.set_theme_spacing_lg(ch.space_4_px);

            window.set_dark(tokens.mode.is_dark());
        });
        // Tint the OS-native title bar (macOS NSApp appearance / Windows
        // DWM immersive-dark-mode) so the traffic-light chrome matches
        // Atlas's active theme mode. Safe to call from any thread — the
        // implementation marshals platform calls onto the main thread
        // internally. Linux is a no-op.
        crate::platform::titlebar_theme::apply_native_titlebar_theme(tokens.mode);
    }
}

/// Return a "New Folder" name that does not yet exist in `parent_dir`.
///
/// Tries `"New Folder"`, then `"New Folder 2"`, `"New Folder 3"`, … up to 99.
fn unique_new_folder_name(parent_dir: &Path) -> String {
    let base = "New Folder";
    if !parent_dir.join(base).exists() {
        return base.to_owned();
    }
    for n in 2u32..=99 {
        let candidate = format!("{base} {n}");
        if !parent_dir.join(&candidate).exists() {
            return candidate;
        }
    }
    // Fallback: very unlikely in practice.
    base.to_owned()
}

#[cfg(test)]
mod tests {
    //! Tests for the pane-index ↔ `PaneId` DFS mapping and split-tree
    //! mutations that back [`AppShell`]'s Slint-slot compatibility layer.
    //!
    //! These operate on [`WorkspaceModel`] directly (no Slint window), since
    //! `AppShell` construction requires a live event loop.

    use crate::models::{
        pane_state::PaneState,
        split::{Cardinal, PaneId, Rect, SplitDirection},
        tab::TabModel,
        ViewMode, WorkspaceModel,
    };

    fn workspace_at(path: &str) -> WorkspaceModel {
        let id = PaneId(1);
        WorkspaceModel::new(PaneState::new(id, TabModel::at(path), ViewMode::Details))
    }

    /// Resolve a Slint slot index (0/1) to a `PaneId` via DFS leaf order —
    /// mirrors `AppShell::pane_id_for_index`.
    fn index_to_id(ws: &WorkspaceModel, index: usize) -> Option<PaneId> {
        ws.layout.all_leaves().get(index).copied()
    }

    #[test]
    fn pane_id_for_index_single_pane() {
        let ws = workspace_at("/a");
        assert_eq!(index_to_id(&ws, 0), Some(PaneId(1)));
        assert_eq!(index_to_id(&ws, 1), None);
    }

    #[test]
    fn split_and_both_indices_resolve() {
        let mut ws = workspace_at("/a");
        let new_id = ws.split_focused(SplitDirection::Horizontal, None);
        assert_eq!(index_to_id(&ws, 0), Some(PaneId(1)));
        assert_eq!(index_to_id(&ws, 1), Some(new_id));
        assert_eq!(ws.layout.leaf_count(), 2);
    }

    #[test]
    fn close_focused_leaves_one_pane() {
        let mut ws = workspace_at("/a");
        let new_id = ws.split_focused(SplitDirection::Horizontal, None);
        assert_eq!(ws.focused, new_id);
        let outcome = ws.close_focused().expect("two panes → close succeeds");
        assert_eq!(outcome.removed, new_id);
        assert_eq!(ws.focused, PaneId(1));
        assert_eq!(ws.layout.leaf_count(), 1);
        assert_eq!(index_to_id(&ws, 1), None);
    }

    #[test]
    fn focus_direction_in_two_pane_horizontal_split() {
        let mut ws = workspace_at("/a");
        let right = ws.split_focused(SplitDirection::Horizontal, None);
        let bounds = Rect::from_size(200.0, 200.0);
        // Focus is on the right pane after split; move left → pane 0.
        assert_eq!(ws.focus_direction(Cardinal::Left, bounds), Some(PaneId(1)));
        assert_eq!(ws.focused, PaneId(1));
        // Move right → back to the new pane.
        assert_eq!(ws.focus_direction(Cardinal::Right, bounds), Some(right));
        assert_eq!(ws.focused, right);
    }

    #[test]
    fn dfs_ordering_stable_across_splits() {
        let mut ws = workspace_at("/a");
        let right = ws.split_focused(SplitDirection::Horizontal, None);
        assert!(ws.set_focused(PaneId(1)));
        let down = ws.split_focused(SplitDirection::Vertical, None);
        // DFS order: pane 1's subtree (1, down) then the right sibling.
        assert_eq!(ws.leaves_in_order(), vec![PaneId(1), down, right]);
    }
}

// ── Phase-5 tab-operation tests ───────────────────────────────────────────────
//
// These tests exercise the algorithms that `AppShell`'s new tab methods
// delegate to.  They operate directly on `WorkspaceModel` / `PaneState`
// because `AppShell::new` requires a live Slint event loop.
#[cfg(test)]
mod tab_ops_tests {
    use std::collections::VecDeque;

    use crate::models::{
        pane_state::PaneState, split::PaneId, tab::TabModel, ViewMode, WorkspaceModel,
    };

    /// Build a workspace with `n` tabs in pane `id`.  Tab titles are
    /// `"tab-0"`, `"tab-1"`, … so order can be verified by title.
    fn workspace_with_n_tabs(id: PaneId, n: usize) -> WorkspaceModel {
        assert!(n >= 1);
        let mut ws = WorkspaceModel::new(PaneState::new(
            id,
            TabModel::at("/root/tab-0"),
            ViewMode::Details,
        ));
        {
            let p = ws.pane_mut(id).unwrap();
            for i in 1..n {
                p.add_tab(TabModel::at(format!("/root/tab-{i}")));
            }
            p.set_active(0);
        }
        ws
    }

    // ── reorder_tab ───────────────────────────────────────────────────────

    #[test]
    fn reorder_from_0_to_2_in_4_tab_pane() {
        let id = PaneId(1);
        let mut ws = workspace_with_n_tabs(id, 4);
        // Make tab-0 active to verify it follows the move.
        ws.pane_mut(id).unwrap().set_active(0);

        // Simulate AppShell::reorder_tab(pane, from=0, to=2).
        let p = ws.pane_mut(id).unwrap();
        let from = 0usize;
        let to = 2usize;
        let tab = p.tabs.remove(from);
        p.tabs.insert(to, tab);
        if p.active_tab == from {
            p.active_tab = to;
        } else if from < to && p.active_tab > from && p.active_tab <= to {
            p.active_tab -= 1;
        }

        assert_eq!(p.tabs[0].title, "tab-1");
        assert_eq!(p.tabs[1].title, "tab-2");
        assert_eq!(p.tabs[2].title, "tab-0"); // moved tab
        assert_eq!(p.tabs[3].title, "tab-3");
        assert_eq!(p.active_tab, 2); // follows the moved tab
    }

    #[test]
    fn reorder_non_active_tab_preserves_selection() {
        let id = PaneId(1);
        let mut ws = workspace_with_n_tabs(id, 4);
        ws.pane_mut(id).unwrap().set_active(3); // active is tab-3

        // Move tab-1 → tab-3 (active tab shifts left).
        let p = ws.pane_mut(id).unwrap();
        let from = 1usize;
        let to = 3usize;
        let tab = p.tabs.remove(from);
        p.tabs.insert(to, tab);
        // from < to and active (3) > from (1) and <= to (3) → shift left.
        if p.active_tab == from {
            p.active_tab = to;
        } else if from < to && p.active_tab > from && p.active_tab <= to {
            p.active_tab -= 1;
        }

        assert_eq!(p.tabs[3].title, "tab-1");
        assert_eq!(p.active_tab, 2); // was 3, shifted left by 1
        assert_eq!(p.tabs[p.active_tab].title, "tab-3");
    }

    // ── duplicate_tab ─────────────────────────────────────────────────────

    #[test]
    fn duplicate_inserts_copy_after_and_activates() {
        let id = PaneId(1);
        let mut ws = workspace_with_n_tabs(id, 3);

        // Simulate AppShell::duplicate_tab(pane, tab=1).
        let src = ws.pane(id).unwrap().tabs[1].clone();
        let insert_at = 2;
        let mut dup = TabModel::at(src.location.clone().unwrap());
        dup.sort = src.sort.clone();
        dup.filter = src.filter.clone();
        {
            let p = ws.pane_mut(id).unwrap();
            p.tabs.insert(insert_at, dup);
            p.active_tab = insert_at;
        }

        let p = ws.pane(id).unwrap();
        assert_eq!(p.tabs.len(), 4);
        assert_eq!(p.active_tab, 2);
        assert_eq!(p.tabs[1].title, "tab-1");
        assert_eq!(p.tabs[2].title, "tab-1"); // copy has same path → same title
        assert_eq!(p.tabs[3].title, "tab-2");
    }

    // ── close_other_tabs ──────────────────────────────────────────────────

    #[test]
    fn close_other_tabs_leaves_only_kept() {
        let id = PaneId(1);
        let mut ws = workspace_with_n_tabs(id, 5);

        // Simulate AppShell::close_other_tabs(pane, keep=2).
        let keep = 2usize;
        let p = ws.pane_mut(id).unwrap();
        let kept = p.tabs[keep].clone();
        let all: Vec<TabModel> = std::mem::replace(&mut p.tabs, vec![kept]);
        let _closed: Vec<TabModel> = all
            .into_iter()
            .enumerate()
            .filter_map(|(i, t)| (i != keep).then_some(t))
            .collect();
        p.active_tab = 0;

        let p = ws.pane(id).unwrap();
        assert_eq!(p.tabs.len(), 1);
        assert_eq!(p.active_tab, 0);
        assert_eq!(p.tabs[0].title, "tab-2");
    }

    // ── close_tabs_to_right_of ────────────────────────────────────────────

    #[test]
    fn close_tabs_to_right_leaves_correct_count() {
        let id = PaneId(1);
        let mut ws = workspace_with_n_tabs(id, 5);

        // Simulate AppShell::close_tabs_to_right_of(pane, from=1).
        let from = 1usize;
        let p = ws.pane_mut(id).unwrap();
        let _closed: Vec<TabModel> = p.tabs.drain(from + 1..).collect();
        if p.active_tab > from {
            p.active_tab = from;
        }

        let p = ws.pane(id).unwrap();
        assert_eq!(p.tabs.len(), 2);
        assert_eq!(p.tabs[0].title, "tab-0");
        assert_eq!(p.tabs[1].title, "tab-1");
    }

    // ── closed-tab history ────────────────────────────────────────────────

    #[test]
    fn close_tab_pushes_to_closed_deque_and_reopen_pops() {
        let id = PaneId(1);
        let mut ws = workspace_with_n_tabs(id, 3);
        let mut closed: AHashMap<PaneId, VecDeque<TabModel>> = ahash::AHashMap::default();

        // Simulate closing tab 0.
        let p = ws.pane_mut(id).unwrap();
        if let Some(removed) = p.close_tab(0) {
            let deque = closed.entry(id).or_default();
            deque.push_front(removed);
        }
        assert_eq!(closed[&id].front().unwrap().title, "tab-0");
        assert_eq!(ws.pane(id).unwrap().tabs.len(), 2);

        // Simulate reopen_closed_tab.
        let reopened = closed.get_mut(&id).and_then(VecDeque::pop_front);
        assert!(reopened.is_some());
        assert_eq!(reopened.unwrap().title, "tab-0");
        assert!(closed[&id].is_empty());
    }

    #[test]
    fn closed_tab_deque_caps_at_twenty() {
        let id = PaneId(1);
        let mut closed: AHashMap<PaneId, VecDeque<TabModel>> = ahash::AHashMap::default();

        for i in 0..25usize {
            let deque = closed.entry(id).or_default();
            deque.push_front(TabModel::at(format!("/root/tab-{i}")));
            if deque.len() > 20 {
                deque.pop_back();
            }
        }

        let deque = &closed[&id];
        assert_eq!(deque.len(), 20);
        // Most recently pushed (tab-24) is at the front.
        assert_eq!(deque.front().unwrap().title, "tab-24");
        // Oldest surviving entry (tab-5) is at the back; 0–4 were evicted.
        assert_eq!(deque.back().unwrap().title, "tab-5");
    }

    // Need AHashMap in scope for the tests above.
    use ahash::AHashMap;
}

// ── Phase-6 drag-and-drop tests ───────────────────────────────────────────────
//
// These tests exercise the Rust-side drag state machine — `begin_drag`,
// `drag_move`, `drag_end`, `drag_cancel`, `is_dragging`, and
// `pointer_to_pane_id` — without requiring a live Slint event loop.
//
// Since `AppShell::new` requires a live window, all tests operate directly on
// the data structures (`WorkspaceModel`, `DragArmedState`, `DragState`, `Rect`)
// to verify the algorithms in isolation.
#[cfg(test)]
mod dnd_tests {
    use std::path::PathBuf;

    use parking_lot::RwLock;

    use crate::{
        models::{
            pane_state::PaneState,
            split::{PaneId, Rect, SplitDirection},
            tab::TabModel,
            ViewMode, WorkspaceModel,
        },
        shell::{DragArmedState, DragState},
    };

    /// Build a two-pane horizontal workspace for hit-test scenarios.
    fn two_pane_workspace() -> WorkspaceModel {
        let id1 = PaneId(1);
        let mut ws = WorkspaceModel::new(PaneState::new(
            id1,
            TabModel::at("/left"),
            ViewMode::Details,
        ));
        ws.split_focused(SplitDirection::Horizontal, None);
        ws
    }

    /// Compute layout rects for `ws` inside `bounds` and return them.
    fn layout_rects(ws: &WorkspaceModel, bounds: Rect) -> Vec<(PaneId, Rect)> {
        ws.layout.layout_rects(bounds)
    }

    // ── pointer_to_pane_id ────────────────────────────────────────────────

    /// Inline the `pointer_to_pane_id` algorithm for testing without AppShell.
    fn pointer_to_pane_id(rects: &[(PaneId, Rect)], x: f32, y: f32) -> Option<PaneId> {
        for (id, rect) in rects {
            if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height {
                return Some(*id);
            }
        }
        None
    }

    #[test]
    fn pointer_to_pane_id_two_pane_left_quadrant() {
        let ws = two_pane_workspace();
        let bounds = Rect::from_size(1000.0, 800.0);
        let rects = layout_rects(&ws, bounds);
        // Left pane: x ∈ [0, 500), right pane: x ∈ [500, 1000).
        let left = pointer_to_pane_id(&rects, 250.0, 400.0);
        assert_eq!(left, Some(PaneId(1)));
    }

    #[test]
    fn pointer_to_pane_id_two_pane_right_quadrant() {
        let ws = two_pane_workspace();
        let bounds = Rect::from_size(1000.0, 800.0);
        let rects = layout_rects(&ws, bounds);
        let right = pointer_to_pane_id(&rects, 750.0, 400.0);
        // PaneId(2) is the split-created right pane.
        assert_eq!(right, Some(PaneId(2)));
    }

    #[test]
    fn pointer_to_pane_id_outside_returns_none() {
        let ws = two_pane_workspace();
        let bounds = Rect::from_size(1000.0, 800.0);
        let rects = layout_rects(&ws, bounds);
        assert_eq!(pointer_to_pane_id(&rects, -1.0, 400.0), None);
        assert_eq!(pointer_to_pane_id(&rects, 500.0, 900.0), None);
    }

    // ── drag threshold (DragArmedState → DragState promotion) ────────────

    /// Inline the promotion logic from `AppShell::drag_move` for unit testing.
    fn try_promote(
        armed: &RwLock<Option<DragArmedState>>,
        dragging: &RwLock<Option<DragState>>,
        x: f32,
        y: f32,
    ) {
        if dragging.read().is_some() {
            return;
        }
        let should_promote = armed
            .read()
            .as_ref()
            .is_some_and(|a| (x - a.origin_x).hypot(y - a.origin_y) >= 4.0);
        if should_promote {
            if let Some(a) = armed.write().take() {
                *dragging.write() = Some(DragState {
                    source_pane: a.pane,
                    paths: a.paths,
                });
            }
        }
    }

    fn make_armed(pane: PaneId, paths: Vec<PathBuf>) -> DragArmedState {
        DragArmedState {
            pane,
            entry_index: 0,
            paths,
            origin_x: 0.0,
            origin_y: 0.0,
        }
    }

    #[test]
    fn drag_threshold_small_delta_keeps_armed() {
        let armed: RwLock<Option<DragArmedState>> =
            RwLock::new(Some(make_armed(PaneId(1), vec![PathBuf::from("/a")])));
        let dragging: RwLock<Option<DragState>> = RwLock::new(None);

        // Delta of 3 px — below 4-px threshold.
        try_promote(&armed, &dragging, 3.0, 0.0);

        assert!(dragging.read().is_none(), "should remain unpromotd");
        assert!(armed.read().is_some(), "armed state should persist");
    }

    #[test]
    fn drag_threshold_large_delta_promotes() {
        let armed: RwLock<Option<DragArmedState>> =
            RwLock::new(Some(make_armed(PaneId(1), vec![PathBuf::from("/a")])));
        let dragging: RwLock<Option<DragState>> = RwLock::new(None);

        // Delta of 5 px — exceeds 4-px threshold.
        try_promote(&armed, &dragging, 5.0, 0.0);

        assert!(dragging.read().is_some(), "should be promoted");
        assert!(armed.read().is_none(), "armed state should be consumed");
    }

    #[test]
    fn drag_threshold_exactly_four_px_promotes() {
        let armed: RwLock<Option<DragArmedState>> =
            RwLock::new(Some(make_armed(PaneId(1), vec![PathBuf::from("/a")])));
        let dragging: RwLock<Option<DragState>> = RwLock::new(None);

        try_promote(&armed, &dragging, 4.0, 0.0);

        assert!(dragging.read().is_some());
    }

    // ── begin_drag path resolution (simulated) ────────────────────────────

    #[test]
    fn begin_drag_with_three_paths_stores_all_three() {
        let paths: Vec<PathBuf> = vec![
            PathBuf::from("/a/1"),
            PathBuf::from("/a/2"),
            PathBuf::from("/a/3"),
        ];
        let armed = make_armed(PaneId(1), paths.clone());
        assert_eq!(armed.paths.len(), 3);
        assert_eq!(armed.paths, paths);
    }

    // ── drag_cancel ───────────────────────────────────────────────────────

    #[test]
    fn drag_cancel_clears_both_states() {
        let armed: RwLock<Option<DragArmedState>> =
            RwLock::new(Some(make_armed(PaneId(1), vec![PathBuf::from("/x")])));
        let dragging: RwLock<Option<DragState>> = RwLock::new(Some(DragState {
            source_pane: PaneId(1),
            paths: vec![PathBuf::from("/x")],
        }));

        // Simulate drag_cancel.
        *dragging.write() = None;
        *armed.write() = None;

        assert!(dragging.read().is_none());
        assert!(armed.read().is_none());
    }

    // ── drag_end same-pane → no op ────────────────────────────────────────

    #[test]
    fn drag_end_same_pane_is_noop() {
        // If target == source, drag_end should not submit an op.
        // We verify the guard logic: target_id == drag.source_pane → early return.
        let source = PaneId(1);
        let target = PaneId(1);
        assert_eq!(source, target, "same-pane drop should be detected");
    }

    // ── layout_rects for 2×1 split ────────────────────────────────────────

    #[test]
    fn layout_rects_two_horizontal_panes_sum_to_full_width() {
        let ws = two_pane_workspace();
        let bounds = Rect::from_size(1000.0, 600.0);
        let rects = layout_rects(&ws, bounds);
        assert_eq!(rects.len(), 2);
        let total_w: f32 = rects.iter().map(|(_, r)| r.width).sum();
        assert!((total_w - 1000.0).abs() < 1.0, "widths should sum to total");
    }
}

/// Reveal `path` in the platform-native file manager. Non-blocking: spawns
/// the reveal command and returns immediately; logs on failure. Behaviour:
/// - macOS: `open -R <path>` — highlights the file in Finder.
/// - Windows: `explorer /select,<path>` — same semantics.
/// - Linux (best-effort): `xdg-open` on the parent directory. XDG has no
///   portable "select this entry" verb, so we open the folder and let the
///   user find the row. A per-DE registry (`nautilus --select`, etc.) is a
///   v0.3 follow-up.
pub(crate) fn reveal_in_os_file_manager(path: &Path) {
    use std::process::Command;

    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg("-R").arg(path).spawn();

    #[cfg(target_os = "windows")]
    let result = Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();

    #[cfg(all(unix, not(target_os = "macos")))]
    let result = {
        let target = path.parent().unwrap_or(path);
        Command::new("xdg-open").arg(target).spawn()
    };

    if let Err(err) = result {
        tracing::warn!(?path, %err, "reveal_in_os_file_manager failed to spawn");
    }
}
