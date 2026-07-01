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

    /// Open the palette with the source at `source_index`.
    pub fn open(&self, source_index: usize) {
        let source = {
            let sources = self.sources.read();
            sources.get(source_index).cloned()
        };
        let Some(source) = source else {
            return;
        };

        self.active_source.store(source_index, Ordering::Relaxed);
        self.visible.store(true, Ordering::Relaxed);

        let mut items = Vec::new();
        source.populate(&mut VecSink(&mut items));
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
                self.actions.lock().dispatch(UiAction::Navigate {
                    pane: 0,
                    path: PathBuf::from(item.id),
                });
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
}
