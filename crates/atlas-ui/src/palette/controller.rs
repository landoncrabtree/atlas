//! [`PaletteController`] — orchestrates palette open/close/query/confirm.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

use parking_lot::{Mutex, RwLock};

use crate::{
    actions::{ActionSink, UiAction},
    models::{PaletteModel, PaletteResult},
    palette::{
        matcher::PaletteMatcher,
        source::{ItemSink, PaletteItem, PaletteItemKind, PaletteSource},
    },
};

/// Maximum number of results shown in the palette at once.
const MAX_RESULTS: usize = 200;

type DispatchCallback = dyn Fn(&str) + Send + Sync;
type PathConfirmCallback = dyn Fn(PathBuf) + Send + Sync;
type ServerConfirmCallback = dyn Fn(&str) + Send + Sync;

/// Orchestrates the command palette: mode switching, fuzzy query dispatch,
/// result caching, and selection movement.
pub struct PaletteController {
    window: RwLock<Option<slint::Weak<crate::AtlasWindow>>>,
    matcher: Mutex<PaletteMatcher>,
    sources: RwLock<Vec<Arc<dyn PaletteSource>>>,
    active_source: AtomicUsize,
    visible: AtomicBool,
    model: Mutex<PaletteModel>,
    current_items: Mutex<Vec<PaletteItem>>,
    actions: Arc<Mutex<Box<dyn ActionSink>>>,
    on_dispatch: RwLock<Option<Box<DispatchCallback>>>,
    on_path_confirm: RwLock<Option<Box<PathConfirmCallback>>>,
    on_server_confirm: RwLock<Option<Box<ServerConfirmCallback>>>,
}

impl PaletteController {
    /// Create a new controller.
    #[must_use]
    pub fn new(actions: Arc<Mutex<Box<dyn ActionSink>>>) -> Arc<Self> {
        Arc::new(Self {
            window: RwLock::new(None),
            matcher: Mutex::new(PaletteMatcher::new()),
            sources: RwLock::new(Vec::new()),
            active_source: AtomicUsize::new(0),
            visible: AtomicBool::new(false),
            model: Mutex::new(PaletteModel::default()),
            current_items: Mutex::new(Vec::new()),
            actions,
            on_dispatch: RwLock::new(None),
            on_path_confirm: RwLock::new(None),
            on_server_confirm: RwLock::new(None),
        })
    }

    /// Attach the Slint window used for model updates.
    pub fn attach_window(&self, window: slint::Weak<crate::AtlasWindow>) {
        *self.window.write() = Some(window);
    }

    /// Returns whether the palette is currently visible.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
    }

    /// Register a source and return its index.
    pub fn register_source(&self, source: Arc<dyn PaletteSource>) -> usize {
        let mut sources = self.sources.write();
        let index = sources.len();
        sources.push(source);
        index
    }

    /// Register a callback invoked when an action item is confirmed.
    pub fn set_on_dispatch(&self, callback: impl Fn(&str) + Send + Sync + 'static) {
        *self.on_dispatch.write() = Some(Box::new(callback));
    }

    /// Register a callback invoked when a `Path`-kind item is confirmed.
    ///
    /// The callback owns the "open this path" decision — typically wired
    /// to `AppShell::view_path(focused_pane_id, path)` so directories
    /// navigate the focused pane while files hand off to the OS default
    /// application (the same fs::View unification used by Enter and
    /// double-click).  When unset, the palette falls back to dispatching
    /// [`UiAction::Navigate`] on pane 0, which is the pre-goto-view
    /// behaviour and only works for directories.
    pub fn set_on_path_confirm(&self, callback: impl Fn(PathBuf) + Send + Sync + 'static) {
        *self.on_path_confirm.write() = Some(Box::new(callback));
    }

    /// Register a callback invoked when a `Server`-kind item is
    /// confirmed. Wired by `shell::AppShell::wire_callbacks` to
    /// [`crate::remote::connect::ConnectController::run_connect_saved`]
    /// so Enter on a Cmd+P server hit mounts the pane directly (no
    /// modal). Called with the saved-server id string.
    ///
    /// When unset, confirming a server entry logs a warning and does
    /// nothing.
    pub fn set_on_server_confirm(&self, callback: impl Fn(&str) + Send + Sync + 'static) {
        *self.on_server_confirm.write() = Some(Box::new(callback));
    }

    /// Open the palette with the source at `source_index`.
    pub fn open(&self, source_index: usize) {
        self.open_multi(&[source_index]);
    }

    /// Open the palette with a composite item list built from multiple
    /// sources merged in the order they appear. Used to combine goto
    /// paths with saved servers so a single Cmd+P search hits both.
    /// Empty slice is a no-op.
    pub fn open_multi(&self, source_indices: &[usize]) {
        if source_indices.is_empty() {
            return;
        }

        let sources: Vec<Arc<dyn PaletteSource>> = {
            let all = self.sources.read();
            source_indices
                .iter()
                .filter_map(|i| all.get(*i).cloned())
                .collect()
        };
        if sources.is_empty() {
            return;
        }

        self.active_source
            .store(source_indices[0], Ordering::Relaxed);
        self.visible.store(true, Ordering::Relaxed);

        let mut items = Vec::new();
        for source in sources {
            source.populate(&mut VecSink(&mut items));
        }
        self.matcher.lock().set_items(items);
        self.run_query_and_push(String::new());
    }

    /// Close the palette and clear its model.
    pub fn close(&self) {
        self.visible.store(false, Ordering::Relaxed);
        self.current_items.lock().clear();
        let model = PaletteModel::default();
        *self.model.lock() = model.clone();
        self.push_model(model);
    }

    /// Update the current query.
    pub fn set_query(&self, query: &str) {
        if !self.is_visible() {
            return;
        }
        self.run_query_and_push(query.to_owned());
    }

    /// Move the keyboard selection by `delta` rows, clamped to the result list.
    pub fn move_selection(&self, delta: isize) {
        let mut model = self.model.lock();
        let len = model.results.len();
        if len == 0 {
            return;
        }

        let new_index = (model.selected as isize + delta).clamp(0, len as isize - 1) as usize;
        model.selected = new_index;
        let updated = model.clone();
        drop(model);
        self.push_model(updated);
    }

    /// Confirm the currently selected item.
    pub fn confirm(&self) {
        let selected = self.model.lock().selected;
        let item = self.current_items.lock().get(selected).cloned();
        let Some(item) = item else {
            return;
        };

        self.close();

        match item.kind {
            PaletteItemKind::Action => {
                if let Some(callback) = self.on_dispatch.read().as_ref() {
                    callback(&item.id);
                }
                self.actions
                    .lock()
                    .dispatch(UiAction::PaletteConfirm(item.id.clone()));
            }
            PaletteItemKind::Path => {
                let path = PathBuf::from(item.id);
                // Prefer the fs::View-aware callback (folders navigate,
                // files open in the OS default app). Fall back to a plain
                // Navigate dispatch — which only makes sense for
                // directories — when no callback is registered.
                if let Some(callback) = self.on_path_confirm.read().as_ref() {
                    callback(path);
                } else {
                    tracing::debug!(
                        "palette path confirm without on_path_confirm callback; \
                         falling back to UiAction::Navigate (files will not open)"
                    );
                    self.actions
                        .lock()
                        .dispatch(UiAction::Navigate { pane: 0, path });
                }
            }
            PaletteItemKind::Server => {
                if let Some(callback) = self.on_server_confirm.read().as_ref() {
                    callback(&item.id);
                } else {
                    tracing::warn!(
                        server_id = %item.id,
                        "palette server confirm without on_server_confirm callback; entry ignored"
                    );
                }
            }
        }
    }

    fn run_query_and_push(&self, query: String) {
        let matches = self.matcher.lock().query(&query, MAX_RESULTS);
        let current_items: Vec<PaletteItem> =
            matches.iter().map(|(item, _)| item.clone()).collect();
        let results: Vec<PaletteResult> = current_items
            .iter()
            .map(|item| PaletteResult {
                title: item.title.clone(),
                subtitle: item.subtitle.clone(),
                action_id: item.id.clone(),
            })
            .collect();

        *self.current_items.lock() = current_items;

        let previous = self.model.lock().selected;
        let selected = if results.is_empty() {
            0
        } else {
            previous.min(results.len() - 1)
        };
        let model = PaletteModel {
            visible: true,
            query,
            results,
            selected,
        };
        *self.model.lock() = model.clone();
        self.push_model(model);
    }

    fn push_model(&self, model: PaletteModel) {
        let weak = self.window.read().clone();
        let Some(weak) = weak else {
            return;
        };

        let _ = slint::invoke_from_event_loop(move || {
            use slint::{ModelRc, SharedString, VecModel};

            let Some(window) = weak.upgrade() else {
                return;
            };
            let entries: Vec<crate::PaletteEntry> = model
                .results
                .iter()
                .map(|result| crate::PaletteEntry {
                    title: SharedString::from(result.title.as_str()),
                    subtitle: SharedString::from(result.subtitle.as_str()),
                    action_id: SharedString::from(result.action_id.as_str()),
                })
                .collect();
            window.set_palette_visible(model.visible);
            window.set_palette_query(SharedString::from(model.query.as_str()));
            window.set_palette_results(ModelRc::new(VecModel::from(entries)));
            window.set_palette_selected(model.selected as i32);
        });
    }
}

struct VecSink<'a>(&'a mut Vec<PaletteItem>);

impl ItemSink for VecSink<'_> {
    fn push(&mut self, item: PaletteItem) {
        self.0.push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopSink;

    impl ActionSink for NoopSink {
        fn dispatch(&mut self, _action: UiAction) {}
    }

    #[test]
    fn controller_can_be_constructed() {
        let _controller = PaletteController::new(Arc::new(Mutex::new(Box::new(NoopSink))));
    }

    #[test]
    fn controller_close_resets_model() {
        let controller = PaletteController::new(Arc::new(Mutex::new(Box::new(NoopSink))));
        controller.visible.store(true, Ordering::Relaxed);
        *controller.model.lock() = PaletteModel {
            visible: true,
            query: String::from("foo"),
            results: vec![],
            selected: 0,
        };
        controller.close();
        assert!(!controller.visible.load(Ordering::Relaxed));
        let model = controller.model.lock();
        assert!(!model.visible);
        assert!(model.query.is_empty());
    }

    #[test]
    fn controller_move_selection_clamps() {
        let controller = PaletteController::new(Arc::new(Mutex::new(Box::new(NoopSink))));
        *controller.model.lock() = PaletteModel {
            visible: true,
            query: String::new(),
            results: vec![
                PaletteResult {
                    title: String::from("a"),
                    subtitle: String::new(),
                    action_id: String::from("a"),
                },
                PaletteResult {
                    title: String::from("b"),
                    subtitle: String::new(),
                    action_id: String::from("b"),
                },
            ],
            selected: 0,
        };
        controller.move_selection(1);
        assert_eq!(controller.model.lock().selected, 1);
        controller.move_selection(100);
        assert_eq!(controller.model.lock().selected, 1);
        controller.move_selection(-100);
        assert_eq!(controller.model.lock().selected, 0);
    }

    /// When the palette confirms a `Path`-kind item, it MUST route through
    /// the registered `on_path_confirm` callback (which shell wires to
    /// `AppShell::view_path`, so files open in the OS default app). It
    /// must NOT emit `UiAction::Navigate` in that case, because Navigate
    /// on a file path is a no-op that appears to hang.
    #[test]
    fn path_confirm_routes_through_callback_not_navigate() {
        use crate::palette::source::PaletteItemKind;
        use std::sync::atomic::AtomicBool;

        struct RecordingSink {
            saw_navigate: Arc<AtomicBool>,
        }
        impl ActionSink for RecordingSink {
            fn dispatch(&mut self, action: UiAction) {
                if matches!(action, UiAction::Navigate { .. }) {
                    self.saw_navigate.store(true, Ordering::Relaxed);
                }
            }
        }

        let saw_navigate = Arc::new(AtomicBool::new(false));
        let sink = RecordingSink {
            saw_navigate: Arc::clone(&saw_navigate),
        };
        let controller = PaletteController::new(Arc::new(Mutex::new(Box::new(sink))));

        // Register the fs::View-aware path callback. We also record how
        // many times it fires and with what path so we can assert on it.
        let seen_path: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        {
            let seen_path = Arc::clone(&seen_path);
            controller.set_on_path_confirm(move |p| {
                *seen_path.lock() = Some(p);
            });
        }

        controller.visible.store(true, Ordering::Relaxed);
        *controller.current_items.lock() = vec![PaletteItem {
            id: String::from("/some/file.txt"),
            title: String::from("file.txt"),
            subtitle: String::from("/some"),
            kind: PaletteItemKind::Path,
        }];
        *controller.model.lock() = PaletteModel {
            visible: true,
            query: String::new(),
            results: vec![PaletteResult {
                title: String::from("file.txt"),
                subtitle: String::from("/some"),
                action_id: String::from("/some/file.txt"),
            }],
            selected: 0,
        };

        controller.confirm();

        assert_eq!(
            seen_path.lock().as_deref(),
            Some(PathBuf::from("/some/file.txt").as_path()),
            "on_path_confirm should fire with the exact palette path"
        );
        assert!(
            !saw_navigate.load(Ordering::Relaxed),
            "confirm() must NOT dispatch UiAction::Navigate when a path callback is set — that would try to cd into files and hang"
        );
    }

    /// When no `on_path_confirm` callback is registered, the palette
    /// falls back to the old Navigate behaviour so directories still
    /// work in isolation (e.g. in tests that don't wire a full shell).
    #[test]
    fn path_confirm_falls_back_to_navigate_without_callback() {
        use crate::palette::source::PaletteItemKind;
        use std::sync::atomic::AtomicBool;

        struct RecordingSink {
            saw_navigate: Arc<AtomicBool>,
        }
        impl ActionSink for RecordingSink {
            fn dispatch(&mut self, action: UiAction) {
                if matches!(action, UiAction::Navigate { .. }) {
                    self.saw_navigate.store(true, Ordering::Relaxed);
                }
            }
        }

        let saw_navigate = Arc::new(AtomicBool::new(false));
        let sink = RecordingSink {
            saw_navigate: Arc::clone(&saw_navigate),
        };
        let controller = PaletteController::new(Arc::new(Mutex::new(Box::new(sink))));

        controller.visible.store(true, Ordering::Relaxed);
        *controller.current_items.lock() = vec![PaletteItem {
            id: String::from("/some/dir"),
            title: String::from("dir"),
            subtitle: String::from("/some"),
            kind: PaletteItemKind::Path,
        }];
        *controller.model.lock() = PaletteModel {
            visible: true,
            query: String::new(),
            results: vec![PaletteResult {
                title: String::from("dir"),
                subtitle: String::from("/some"),
                action_id: String::from("/some/dir"),
            }],
            selected: 0,
        };

        controller.confirm();

        assert!(
            saw_navigate.load(Ordering::Relaxed),
            "confirm() must fall back to Navigate dispatch when no path callback is registered"
        );
    }

    /// When the palette confirms a `Server`-kind item, it MUST route
    /// through the registered `on_server_confirm` callback with the
    /// server id. It must NOT emit `UiAction::Navigate` because that
    /// dispatch is only meaningful for local paths.
    #[test]
    fn server_confirm_routes_through_server_callback() {
        use crate::palette::source::PaletteItemKind;
        use std::sync::atomic::AtomicBool;

        struct RecordingSink {
            saw_navigate: Arc<AtomicBool>,
        }
        impl ActionSink for RecordingSink {
            fn dispatch(&mut self, action: UiAction) {
                if matches!(action, UiAction::Navigate { .. }) {
                    self.saw_navigate.store(true, Ordering::Relaxed);
                }
            }
        }

        let saw_navigate = Arc::new(AtomicBool::new(false));
        let sink = RecordingSink {
            saw_navigate: Arc::clone(&saw_navigate),
        };
        let controller = PaletteController::new(Arc::new(Mutex::new(Box::new(sink))));

        let seen_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        {
            let seen_id = Arc::clone(&seen_id);
            controller.set_on_server_confirm(move |id| {
                *seen_id.lock() = Some(id.to_owned());
            });
        }

        controller.visible.store(true, Ordering::Relaxed);
        *controller.current_items.lock() = vec![PaletteItem {
            id: String::from("srv::abc::landon@host"),
            title: String::from("🔐 prod"),
            subtitle: String::from("sftp://landon@host:22/"),
            kind: PaletteItemKind::Server,
        }];
        *controller.model.lock() = PaletteModel {
            visible: true,
            query: String::new(),
            results: vec![PaletteResult {
                title: String::from("🔐 prod"),
                subtitle: String::from("sftp://landon@host:22/"),
                action_id: String::from("srv::abc::landon@host"),
            }],
            selected: 0,
        };

        controller.confirm();

        assert_eq!(
            seen_id.lock().as_deref(),
            Some("srv::abc::landon@host"),
            "on_server_confirm should fire with the server id"
        );
        assert!(
            !saw_navigate.load(Ordering::Relaxed),
            "confirm() must NOT dispatch UiAction::Navigate for Server-kind items"
        );
    }

    /// `open_multi` merges items from every listed source into one
    /// palette open. Verify a two-source combine yields the union.
    #[test]
    fn open_multi_merges_sources() {
        use crate::palette::source::PaletteItemKind;

        struct FixedSource(Vec<PaletteItem>);
        impl PaletteSource for FixedSource {
            fn placeholder(&self) -> &'static str {
                "fixed"
            }
            fn populate(&self, sink: &mut dyn ItemSink) {
                for item in &self.0 {
                    sink.push(item.clone());
                }
            }
        }

        let controller = PaletteController::new(Arc::new(Mutex::new(Box::new(NoopSink))));
        let s1 = Arc::new(FixedSource(vec![PaletteItem {
            id: "path1".into(),
            title: "alpha".into(),
            subtitle: "/tmp/alpha".into(),
            kind: PaletteItemKind::Path,
        }]));
        let s2 = Arc::new(FixedSource(vec![PaletteItem {
            id: "srv1".into(),
            title: "🔐 prod".into(),
            subtitle: "sftp://user@host".into(),
            kind: PaletteItemKind::Server,
        }]));
        let i1 = controller.register_source(s1);
        let i2 = controller.register_source(s2);

        controller.open_multi(&[i1, i2]);

        let items = controller.current_items.lock().clone();
        assert_eq!(items.len(), 2, "should merge items from both sources");
        assert!(items.iter().any(|it| it.kind == PaletteItemKind::Path));
        assert!(items.iter().any(|it| it.kind == PaletteItemKind::Server));
    }
}
