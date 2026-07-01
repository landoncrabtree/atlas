//! [`SearchController`] — drives the search panel from Rust.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use atlas_search::{
    run_unified, IndexClient, UnifiedRequest, UnifiedResult, UnifiedSource, UnifiedSummary,
};
use parking_lot::{Mutex, RwLock};
use slint::{ModelRc, SharedString, VecModel};

use crate::{
    actions::{ActionSink, UiAction},
    search::row::SearchRow as UiRow,
};

type SharedActionSink = Arc<Mutex<Box<dyn ActionSink>>>;

struct SearchState {
    rows: RwLock<Vec<UnifiedResult>>,
    visible: AtomicBool,
    query: RwLock<String>,
    scope: RwLock<Option<PathBuf>>,
    status: RwLock<String>,
    window: RwLock<Option<slint::Weak<crate::AtlasWindow>>>,
    /// Maximum results per source, derived from `config.search.fuzzy_max_results`.
    max_results_per_source: parking_lot::Mutex<usize>,
    /// Glob patterns skipped by the content-search walker. Wired from
    /// `config.search.default_globs_exclude`.
    exclude_globs: RwLock<Vec<String>>,
    /// Worker-thread count for the content-search walker. `None` = auto.
    /// Wired from `config.search.content_search_threads`.
    content_search_threads: RwLock<Option<usize>>,
}

/// Controller for the right-side search panel.
pub struct SearchController {
    state: Arc<SearchState>,
    index_client: RwLock<Option<Arc<IndexClient>>>,
    active_cancel: Mutex<Option<Arc<AtomicBool>>>,
    rt: Arc<tokio::runtime::Runtime>,
    actions: Mutex<Option<SharedActionSink>>,
}

impl SearchController {
    /// Create a new controller backed by a dedicated Tokio runtime.
    #[must_use]
    pub fn new() -> Arc<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("atlas-search-rt")
            .enable_all()
            .build()
            .unwrap_or_else(|error| panic!("failed to build search runtime: {error}"));
        Arc::new(Self {
            state: Arc::new(SearchState {
                rows: RwLock::new(Vec::new()),
                visible: AtomicBool::new(false),
                query: RwLock::new(String::new()),
                scope: RwLock::new(None),
                status: RwLock::new(String::new()),
                window: RwLock::new(None),
                max_results_per_source: parking_lot::Mutex::new(50),
                exclude_globs: RwLock::new(Vec::new()),
                content_search_threads: RwLock::new(None),
            }),
            index_client: RwLock::new(None),
            active_cancel: Mutex::new(None),
            rt: Arc::new(rt),
            actions: Mutex::new(None),
        })
    }

    /// Return a handle to the controller's Tokio runtime.
    #[must_use]
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.rt.handle().clone()
    }

    /// Attach the Slint window used for property updates.
    pub fn attach_window(&self, window: slint::Weak<crate::AtlasWindow>) {
        *self.state.window.write() = Some(window);
    }

    /// Provide the action sink used for navigation on result confirmation.
    pub fn set_action_sink(&self, actions: SharedActionSink) {
        *self.actions.lock() = Some(actions);
    }

    /// Install an optional index client for path-index searches.
    pub fn set_index_client(&self, client: Option<Arc<IndexClient>>) {
        *self.index_client.write() = client;
    }

    /// Open the search panel.
    pub fn open(self: &Arc<Self>) {
        self.state.visible.store(true, Ordering::Relaxed);
        self.push_visible(true);
        let query = self.state.query.read().clone();
        self.push_query(&query);
        let rows = self.state.rows.read().clone();
        self.push_rows_to_slint(&rows);
        let status = self.state.status.read().clone();
        self.push_status(&status, true);
        if !query.is_empty() {
            self.set_query(query);
        }
    }

    /// Close the search panel and cancel any running search.
    pub fn close(&self) {
        if let Some(cancel) = self.active_cancel.lock().take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.state.visible.store(false, Ordering::Relaxed);
        self.push_visible(false);
    }

    /// Returns whether the panel is currently open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.state.visible.load(Ordering::Relaxed)
    }

    /// Update the current search scope.
    pub fn set_scope(&self, scope: Option<PathBuf>) {
        *self.state.scope.write() = scope;
    }

    /// Set the maximum number of results per source (config: search.fuzzy_max_results).
    pub fn set_max_results(&self, n: usize) {
        *self.state.max_results_per_source.lock() = n.max(1);
    }

    /// Configure the file-filter exclude globs used for content search.
    ///
    /// Wired from `config.search.default_globs_exclude`.
    pub fn set_exclude_globs(&self, globs: Vec<String>) {
        *self.state.exclude_globs.write() = globs;
    }

    /// Configure the worker-thread count for content search.
    ///
    /// Wired from `config.search.content_search_threads`. `None` uses
    /// `available_parallelism`.
    pub fn set_content_search_threads(&self, threads: Option<usize>) {
        *self.state.content_search_threads.write() = threads;
    }

    /// Update the current query and start a fresh unified search.
    pub fn set_query(self: &Arc<Self>, query: String) {
        if let Some(cancel) = self.active_cancel.lock().take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.state.rows.write().clear();
        *self.state.query.write() = query.clone();
        self.push_query(&query);
        self.push_rows_to_slint(&[]);

        if query.trim().is_empty() {
            self.push_status("", true);
            return;
        }

        let scope = self.state.scope.read().clone();
        let index = self.index_client.read().clone();
        let mut sources = Vec::new();
        if index.is_some() {
            sources.push(UnifiedSource::PathIndex);
        }
        if scope.is_some() {
            sources.push(UnifiedSource::Content);
        }

        if sources.is_empty() {
            self.push_status("Search unavailable", true);
            return;
        }

        self.push_status("Searching…", false);

        let req = UnifiedRequest {
            query,
            sources,
            scope,
            // config: reads config.search.fuzzy_max_results
            max_results_per_source: *self.state.max_results_per_source.lock(),
            candidates: Vec::new(),
            exclude_globs: self.state.exclude_globs.read().clone(),
            content_search_threads: *self.state.content_search_threads.read(),
        };
        let cancel_flag = Arc::new(AtomicBool::new(false));
        *self.active_cancel.lock() = Some(Arc::clone(&cancel_flag));

        let state = Arc::clone(&self.state);
        let rt = Arc::clone(&self.rt);
        std::thread::Builder::new()
            .name("atlas-search-drainer".to_owned())
            .spawn(move || {
                let handle = rt.block_on(run_unified(req, index));
                let (results_rx, summary_rx, internal_cancel) = handle.into_parts();
                let mut summaries = Vec::<UnifiedSummary>::new();
                let mut results_closed = false;
                let mut summary_closed = false;
                let mut last_push = Instant::now();

                while !(results_closed && summary_closed) {
                    if cancel_flag.load(Ordering::Relaxed) {
                        internal_cancel.store(true, Ordering::Relaxed);
                        break;
                    }

                    crossbeam_channel::select! {
                        recv(results_rx) -> message => {
                            match message {
                                Ok(result) => {
                                    if cancel_flag.load(Ordering::Relaxed) {
                                        internal_cancel.store(true, Ordering::Relaxed);
                                        break;
                                    }
                                    state.rows.write().push(result);
                                    if last_push.elapsed() >= Duration::from_millis(50) {
                                        push_rows_snapshot(&state);
                                        last_push = Instant::now();
                                    }
                                }
                                Err(_) => {
                                    results_closed = true;
                                }
                            }
                        },
                        recv(summary_rx) -> message => {
                            match message {
                                Ok(summary) => summaries.push(summary),
                                Err(_) => summary_closed = true,
                            }
                        },
                        default(Duration::from_millis(25)) => {
                            if !state.rows.read().is_empty() && last_push.elapsed() >= Duration::from_millis(50) {
                                push_rows_snapshot(&state);
                                last_push = Instant::now();
                            }
                        }
                    }
                }

                if !cancel_flag.load(Ordering::Relaxed) {
                    push_rows_snapshot(&state);
                    let status = summarize(&summaries, state.rows.read().len());
                    *state.status.write() = status.clone();
                    push_status_snapshot(&state, status);
                }
            })
            .unwrap_or_else(|error| panic!("failed to spawn atlas-search-drainer: {error}"));
    }

    /// Confirm a result row and navigate to its path.
    pub fn confirm(&self, row_index: usize) {
        let rows = self.state.rows.read();
        if let Some(result) = rows.get(row_index) {
            let path = match result {
                UnifiedResult::Path { path, .. } | UnifiedResult::Content { path, .. } => {
                    path.clone()
                }
            };
            if let Some(actions) = self.actions.lock().as_ref() {
                actions
                    .lock()
                    .dispatch(UiAction::Navigate { pane: 0, path });
            }
            self.close();
        }
    }

    fn push_visible(&self, visible: bool) {
        let weak = self.state.window.read().clone();
        let Some(weak) = weak else {
            return;
        };
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            window.set_search_panel_visible(visible);
        });
    }

    fn push_query(&self, query: &str) {
        let weak = self.state.window.read().clone();
        let Some(weak) = weak else {
            return;
        };
        let query = query.to_owned();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            window.set_search_panel_query(SharedString::from(query.as_str()));
        });
    }

    fn push_rows_to_slint(&self, rows: &[UnifiedResult]) {
        let weak = self.state.window.read().clone();
        let Some(weak) = weak else {
            return;
        };
        let rows = to_search_rows(rows);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            window.set_search_panel_rows(to_search_rows_model(&rows));
            window.set_search_panel_selected(0);
        });
    }

    fn push_status(&self, status: &str, is_final: bool) {
        let display = if !is_final && status.is_empty() {
            "Searching…".to_owned()
        } else {
            status.to_owned()
        };
        *self.state.status.write() = display.clone();
        push_status_snapshot(&self.state, display);
    }
}

fn to_search_rows(rows: &[UnifiedResult]) -> Vec<UiRow> {
    rows.iter().map(UiRow::from_result).collect()
}

fn to_search_rows_model(rows: &[UiRow]) -> ModelRc<crate::SearchRow> {
    let entries: Vec<crate::SearchRow> = rows
        .iter()
        .map(|row| crate::SearchRow {
            path: SharedString::from(row.path.to_string_lossy().as_ref()),
            label: SharedString::from(row.label.as_str()),
            snippet: SharedString::from(row.snippet.as_str()),
            kind: SharedString::from(row.kind.as_str()),
        })
        .collect();
    ModelRc::new(VecModel::from(entries))
}

fn push_rows_snapshot(state: &SearchState) {
    let weak = state.window.read().clone();
    let Some(weak) = weak else {
        return;
    };
    let rows = to_search_rows(&state.rows.read());
    let _ = slint::invoke_from_event_loop(move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        window.set_search_panel_rows(to_search_rows_model(&rows));
        window.set_search_panel_selected(0);
    });
}

fn push_status_snapshot(state: &SearchState, status: String) {
    let weak = state.window.read().clone();
    let Some(weak) = weak else {
        return;
    };
    let _ = slint::invoke_from_event_loop(move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        window.set_search_panel_status(SharedString::from(status.as_str()));
    });
}

fn summarize(summaries: &[UnifiedSummary], row_count: usize) -> String {
    let total_matches: u64 = summaries.iter().map(|summary| summary.matches).sum();
    let errors = summaries.iter().filter(|summary| summary.errored).count();
    if row_count == 0 && total_matches == 0 {
        return if errors > 0 {
            let messages: Vec<_> = summaries
                .iter()
                .filter_map(|summary| summary.message.as_deref())
                .collect();
            if messages.is_empty() {
                "No results".to_owned()
            } else {
                format!("No results · {}", messages.join(" · "))
            }
        } else {
            "No results".to_owned()
        };
    }

    if errors > 0 {
        format!("{total_matches} results · {errors} source error(s)")
    } else {
        format!("{total_matches} results")
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
    fn new_controller_starts_closed() {
        let controller = SearchController::new();
        controller.set_action_sink(Arc::new(Mutex::new(Box::new(NoopSink))));
        assert!(!controller.is_open());
    }

    #[test]
    fn open_marks_controller_visible() {
        let controller = SearchController::new();
        controller.open();
        assert!(controller.is_open());
    }

    #[test]
    fn close_marks_controller_hidden() {
        let controller = SearchController::new();
        controller.open();
        controller.close();
        assert!(!controller.is_open());
    }

    #[test]
    fn empty_query_does_not_panic() {
        let controller = SearchController::new();
        controller.set_query(String::new());
        assert!(controller.state.rows.read().is_empty());
    }
}
