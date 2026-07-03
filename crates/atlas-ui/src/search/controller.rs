//! [`SearchController`] — drives the search panel from Rust.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use atlas_config::MAX_VISIBLE_RESULTS_CAP;
use atlas_search::{
    fuzzy_score, run_unified, IndexClient, UnifiedRequest, UnifiedResult, UnifiedSource,
    UnifiedSummary,
};
use parking_lot::{Condvar, Mutex, RwLock};
use slint::{ModelRc, SharedString, VecModel};

use crate::{
    actions::{ActionSink, UiAction},
    search::row::SearchRow as UiRow,
};

type SharedActionSink = Arc<Mutex<Box<dyn ActionSink>>>;

/// Default debounce window between the last keystroke and the dispatched
/// query. Wired from `config.search.debounce_ms`; this is the fallback used
/// when the config isn't loaded yet.
const DEFAULT_DEBOUNCE_MS: u64 = 150;
/// Default minimum query length. Wired from `config.search.min_query_length`.
const DEFAULT_MIN_QUERY_LENGTH: usize = 2;
/// Default max rows pushed to the Slint list. Wired from
/// `config.search.max_visible_results`; hard-capped at
/// [`MAX_VISIBLE_RESULTS_CAP`] to keep rendering responsive.
const DEFAULT_MAX_VISIBLE: usize = 100;

struct SearchState {
    rows: RwLock<Vec<UnifiedResult>>,
    visible: AtomicBool,
    query: RwLock<String>,
    scope: RwLock<Option<PathBuf>>,
    status: RwLock<String>,
    window: RwLock<Option<slint::Weak<crate::AtlasWindow>>>,
    /// Maximum results per source, derived from `config.search.fuzzy_max_results`.
    max_results_per_source: Mutex<usize>,
    /// Maximum rows shown in the Slint list, derived from
    /// `config.search.max_visible_results` and clamped to
    /// [`MAX_VISIBLE_RESULTS_CAP`].
    max_visible_results: Mutex<usize>,
    /// Minimum query length before a search dispatches. Below this the panel
    /// shows a hint and no work is scheduled. Derived from
    /// `config.search.min_query_length`.
    min_query_length: Mutex<usize>,
    /// Debounce delay between the last keystroke and the dispatched query.
    /// Derived from `config.search.debounce_ms`.
    debounce_ms: Mutex<u64>,
    /// Glob patterns skipped by the content-search walker. Wired from
    /// `config.search.default_globs_exclude`.
    exclude_globs: RwLock<Vec<String>>,
    /// Worker-thread count for the content-search walker. `None` = auto.
    /// Wired from `config.search.content_search_threads`.
    content_search_threads: RwLock<Option<usize>>,
}

/// Shared state for the debouncer thread. The controller updates
/// `pending_query` + `generation` on every keystroke; the worker sleeps up
/// to `debounce_ms` before dispatching the *latest* pending query. New
/// keystrokes bump `generation` and re-wake the worker, coalescing bursts.
struct Debouncer {
    pending: Mutex<DebouncerState>,
    cvar: Condvar,
}

struct DebouncerState {
    /// Latest query text queued for dispatch.
    query: String,
    /// Bumped on every keystroke; the worker uses it to detect superseded queries.
    generation: u64,
    /// Signals the worker thread to exit at drop time.
    shutdown: bool,
}

/// Controller for the right-side search panel.
///
/// The controller owns three concurrency primitives:
///
/// 1. A dedicated Tokio runtime for the async index client.
/// 2. A background debouncer thread that coalesces bursts of keystrokes into
///    a single dispatched query (see [`Self::set_query`]).
/// 3. A per-query drainer thread that consumes streaming results from
///    [`atlas_search::run_unified`], applies fuzzy re-ranking, caps the row
///    count, and pushes snapshots back to the Slint window via
///    [`slint::invoke_from_event_loop`].
pub struct SearchController {
    state: Arc<SearchState>,
    index_client: RwLock<Option<Arc<IndexClient>>>,
    active_cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// Latest query generation that was actually dispatched. Used by the
    /// drainer to bail out early when a newer keystroke has arrived.
    dispatched_generation: Arc<AtomicU64>,
    debouncer: Arc<Debouncer>,
    rt: Arc<tokio::runtime::Runtime>,
    actions: Mutex<Option<SharedActionSink>>,
}

impl SearchController {
    /// Create a new controller backed by a dedicated Tokio runtime and a
    /// background debouncer thread.
    #[must_use]
    pub fn new() -> Arc<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("atlas-search-rt")
            .enable_all()
            .build()
            .unwrap_or_else(|error| panic!("failed to build search runtime: {error}"));
        let controller = Arc::new(Self {
            state: Arc::new(SearchState {
                rows: RwLock::new(Vec::new()),
                visible: AtomicBool::new(false),
                query: RwLock::new(String::new()),
                scope: RwLock::new(None),
                status: RwLock::new(String::new()),
                window: RwLock::new(None),
                max_results_per_source: Mutex::new(50),
                max_visible_results: Mutex::new(DEFAULT_MAX_VISIBLE),
                min_query_length: Mutex::new(DEFAULT_MIN_QUERY_LENGTH),
                debounce_ms: Mutex::new(DEFAULT_DEBOUNCE_MS),
                exclude_globs: RwLock::new(Vec::new()),
                content_search_threads: RwLock::new(None),
            }),
            index_client: RwLock::new(None),
            active_cancel: Mutex::new(None),
            dispatched_generation: Arc::new(AtomicU64::new(0)),
            debouncer: Arc::new(Debouncer {
                pending: Mutex::new(DebouncerState {
                    query: String::new(),
                    generation: 0,
                    shutdown: false,
                }),
                cvar: Condvar::new(),
            }),
            rt: Arc::new(rt),
            actions: Mutex::new(None),
        });
        controller.spawn_debouncer();
        controller
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
        let hint = self.empty_query_hint();
        let effective = if query.trim().is_empty() && !hint.is_empty() {
            hint
        } else {
            status
        };
        self.push_status(&effective, true);
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

    /// Set the maximum number of rows pushed to the Slint list. Clamped to
    /// `[1, MAX_VISIBLE_RESULTS_CAP]`. Wired from
    /// `config.search.max_visible_results`.
    pub fn set_max_visible_results(&self, n: usize) {
        *self.state.max_visible_results.lock() = n.clamp(1, MAX_VISIBLE_RESULTS_CAP);
    }

    /// Set the minimum query length before a search dispatches. Below this,
    /// the panel shows a hint and no CPU work is scheduled. Wired from
    /// `config.search.min_query_length`; clamped to `>= 1`.
    pub fn set_min_query_length(&self, n: usize) {
        *self.state.min_query_length.lock() = n.max(1);
    }

    /// Set the debounce delay in milliseconds. Coalesces bursts of typing
    /// into a single dispatched query. Wired from `config.search.debounce_ms`;
    /// clamped to `[0, 1000]`.
    pub fn set_debounce_ms(&self, ms: u32) {
        *self.state.debounce_ms.lock() = u64::from(ms.min(1000));
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

    /// Enqueue a new query. Coalesces multiple keystrokes within the
    /// debounce window into a single dispatched search (see the
    /// [`Debouncer`] worker in [`Self::spawn_debouncer`]).
    ///
    /// Empty queries and queries shorter than
    /// `config.search.min_query_length` short-circuit to a hint and skip
    /// dispatch entirely.
    pub fn set_query(self: &Arc<Self>, query: String) {
        // Clear the in-flight search unconditionally: even if the debouncer
        // hasn't fired yet, any running drainer for the previous query is
        // now stale.
        if let Some(cancel) = self.active_cancel.lock().take() {
            cancel.store(true, Ordering::Relaxed);
        }
        *self.state.query.write() = query.clone();
        self.push_query(&query);

        let min = *self.state.min_query_length.lock();
        let trimmed = query.trim();
        if trimmed.is_empty() {
            self.state.rows.write().clear();
            self.push_rows_to_slint(&[]);
            self.push_status(&self.empty_query_hint(), true);
            return;
        }
        if trimmed.chars().count() < min {
            self.state.rows.write().clear();
            self.push_rows_to_slint(&[]);
            self.push_status(&self.short_query_hint(min), true);
            return;
        }

        // Queue the query for the debouncer worker. The worker sleeps for
        // `debounce_ms`, wakes on subsequent keystrokes, and dispatches
        // whatever query is pending once the window expires quiet.
        {
            let mut pending = self.debouncer.pending.lock();
            pending.query = query;
            pending.generation = pending.generation.wrapping_add(1);
        }
        self.debouncer.cvar.notify_one();
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

    /// Spawn the debouncer worker that coalesces bursts of keystrokes into
    /// a single dispatched search. The worker owns a strong `Arc<Self>` so
    /// it lives as long as the controller; shutdown is signalled via
    /// `pending.shutdown` in [`Drop`].
    fn spawn_debouncer(self: &Arc<Self>) {
        let this = Arc::clone(self);
        let debouncer = Arc::clone(&self.debouncer);
        let spawn_result = thread::Builder::new()
            .name("atlas-search-debouncer".to_owned())
            .spawn(move || {
                let mut last_seen_generation = 0_u64;
                loop {
                    // Wait for a new keystroke or for shutdown.
                    let mut pending = debouncer.pending.lock();
                    while !pending.shutdown && pending.generation == last_seen_generation {
                        debouncer.cvar.wait(&mut pending);
                    }
                    if pending.shutdown {
                        break;
                    }
                    last_seen_generation = pending.generation;
                    drop(pending);

                    // Sleep for the debounce window, waking early if a
                    // newer keystroke arrives; when it does, restart the
                    // window so the user's *last* character wins.
                    loop {
                        let debounce_ms = *this.state.debounce_ms.lock();
                        let deadline = Instant::now() + Duration::from_millis(debounce_ms);
                        let mut pending = debouncer.pending.lock();
                        loop {
                            if pending.shutdown {
                                return;
                            }
                            if pending.generation != last_seen_generation {
                                last_seen_generation = pending.generation;
                                break;
                            }
                            let now = Instant::now();
                            if now >= deadline {
                                break;
                            }
                            let remaining = deadline - now;
                            let result = debouncer.cvar.wait_for(&mut pending, remaining);
                            if result.timed_out() {
                                break;
                            }
                        }
                        // If the loop broke because the deadline passed and
                        // no new keystroke arrived, dispatch now.
                        if pending.generation == last_seen_generation {
                            let query = pending.query.clone();
                            drop(pending);
                            this.dispatched_generation
                                .store(last_seen_generation, Ordering::SeqCst);
                            this.dispatch_query(query);
                            break;
                        }
                        // Otherwise: newer keystroke won, sleep again for
                        // the fresh debounce window.
                    }
                }
            });
        if let Err(error) = spawn_result {
            tracing::error!(%error, "failed to spawn atlas-search-debouncer thread");
        }
    }

    /// Actually dispatch the query to the unified search backend. Called
    /// by the debouncer worker once the input has quiesced.
    fn dispatch_query(self: &Arc<Self>, query: String) {
        if !self.state.visible.load(Ordering::Relaxed) {
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
            query: query.clone(),
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
        let dispatched_generation = Arc::clone(&self.dispatched_generation);
        // The generation this dispatch corresponds to.  If a newer keystroke
        // bumps `dispatched_generation` past this value, the drainer treats
        // the current results as stale and bails without pushing.
        let my_generation = dispatched_generation.load(Ordering::SeqCst);
        let query_for_thread = query;
        let spawn_result = thread::Builder::new()
            .name("atlas-search-drainer".to_owned())
            .spawn(move || {
                let handle = rt.block_on(run_unified(req, index));
                let (results_rx, summary_rx, internal_cancel) = handle.into_parts();
                let mut summaries = Vec::<UnifiedSummary>::new();
                let mut results_closed = false;
                let mut summary_closed = false;
                let mut last_push = Instant::now();

                while !(results_closed && summary_closed) {
                    if cancel_flag.load(Ordering::Relaxed)
                        || dispatched_generation.load(Ordering::SeqCst) != my_generation
                    {
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
                                        push_rows_snapshot(&state, &query_for_thread);
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
                                push_rows_snapshot(&state, &query_for_thread);
                                last_push = Instant::now();
                            }
                        }
                    }
                }

                if !cancel_flag.load(Ordering::Relaxed)
                    && dispatched_generation.load(Ordering::SeqCst) == my_generation
                {
                    push_rows_snapshot(&state, &query_for_thread);
                    let total_hits = total_hits(&summaries, state.rows.read().len());
                    let visible_cap = *state.max_visible_results.lock();
                    let ranked_len = state.rows.read().len().min(visible_cap);
                    let status = summarize(&summaries, total_hits, ranked_len);
                    *state.status.write() = status.clone();
                    push_status_snapshot(&state, status);
                }
            });
        if let Err(error) = spawn_result {
            tracing::error!(%error, "failed to spawn atlas-search-drainer thread");
        }
    }

    fn empty_query_hint(&self) -> String {
        let min = *self.state.min_query_length.lock();
        if min <= 1 {
            "Type to search…".to_owned()
        } else {
            format!("Type ≥ {min} characters")
        }
    }

    fn short_query_hint(&self, min: usize) -> String {
        format!("Type ≥ {min} characters")
    }

    fn push_visible(&self, _visible: bool) {
        // Visibility is coordinated by AppShell's single RightDockSurface so
        // Search and Operations cannot both claim the right-side dock slot.
        // The controller still tracks its own open/closed state for search
        // cancellation and tests, but it no longer pushes an independent
        // Slint boolean.
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
        let query = self.state.query.read().clone();
        let cap = *self.state.max_visible_results.lock();
        let ranked = rank_and_cap(rows, &query, cap);
        let ui_rows = to_search_rows(&ranked);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            window.set_search_panel_rows(to_search_rows_model(&ui_rows));
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

    /// Test-only accessor for the currently displayed status text.
    #[cfg(test)]
    fn status_snapshot(&self) -> String {
        self.state.status.read().clone()
    }

    /// Test-only accessor for the debouncer generation counter. Bumped
    /// once per keystroke; used by the tests to assert coalescing.
    #[cfg(test)]
    fn pending_generation(&self) -> u64 {
        self.debouncer.pending.lock().generation
    }
}

impl Drop for SearchController {
    fn drop(&mut self) {
        // Signal the debouncer worker to exit; without this the thread
        // parks forever on the condvar and leaks past the controller.
        {
            let mut pending = self.debouncer.pending.lock();
            pending.shutdown = true;
        }
        self.debouncer.cvar.notify_all();
        // Also cancel any in-flight drainer so it stops pushing snapshots
        // to a torn-down window.
        if let Some(cancel) = self.active_cancel.lock().take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// Score each raw [`UnifiedResult`] against `query` using
/// [`atlas_search::fuzzy_score`], sort in descending order (contiguous /
/// prefix / word-boundary matches float to the top), then truncate to at
/// most `cap` entries. Content-match rows are always kept at their raw
/// order after path results because their `spans` already encode the exact
/// match position — scoring the whole line would double-count.
fn rank_and_cap(rows: &[UnifiedResult], query: &str, cap: usize) -> Vec<UnifiedResult> {
    if rows.is_empty() {
        return Vec::new();
    }
    let mut paths: Vec<(UnifiedResult, u32)> = Vec::new();
    let mut contents: Vec<UnifiedResult> = Vec::new();
    for row in rows {
        match row {
            UnifiedResult::Path { path, .. } => {
                let haystack = path.to_string_lossy();
                let score = fuzzy_score(query, haystack.as_ref()).unwrap_or(0);
                paths.push((row.clone(), score));
            }
            UnifiedResult::Content { .. } => contents.push(row.clone()),
        }
    }
    paths.sort_by_key(|(_, score)| std::cmp::Reverse(*score));

    let mut out: Vec<UnifiedResult> = Vec::with_capacity(cap.min(rows.len()));
    for (row, _) in paths {
        if out.len() >= cap {
            break;
        }
        out.push(row);
    }
    for row in contents {
        if out.len() >= cap {
            break;
        }
        out.push(row);
    }
    out
}

fn total_hits(summaries: &[UnifiedSummary], fallback_row_count: usize) -> u64 {
    let sum: u64 = summaries.iter().map(|s| s.matches).sum();
    if sum == 0 {
        fallback_row_count as u64
    } else {
        sum
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

fn push_rows_snapshot(state: &SearchState, query: &str) {
    let weak = state.window.read().clone();
    let Some(weak) = weak else {
        return;
    };
    let cap = *state.max_visible_results.lock();
    let ranked = rank_and_cap(&state.rows.read(), query, cap);
    let rows = to_search_rows(&ranked);
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

fn summarize(summaries: &[UnifiedSummary], total_hits: u64, shown: usize) -> String {
    let errors = summaries.iter().filter(|summary| summary.errored).count();
    if total_hits == 0 && shown == 0 {
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

    let core = if (shown as u64) < total_hits {
        format!(
            "Showing {} of {} results",
            shown,
            format_thousands(total_hits)
        )
    } else {
        format!("{} result{}", shown, if shown == 1 { "" } else { "s" })
    };

    if errors > 0 {
        format!("{core} · {errors} source error(s)")
    } else {
        core
    }
}

/// Format `n` with thousands separators (`35293` → `35,293`) using a
/// tiny allocation-free helper — good enough for a status line and
/// dependency-free.
fn format_thousands(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (idx, byte) in bytes.iter().enumerate() {
        if idx > 0 && (bytes.len() - idx).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*byte as char);
    }
    out
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

    #[test]
    fn short_query_shows_hint_and_skips_dispatch() {
        // Below min_query_length: no dispatch happens, status shows a hint.
        let controller = SearchController::new();
        controller.set_min_query_length(3);
        controller.open();
        controller.set_query("ab".to_owned());
        // The status snapshot is written synchronously in `set_query` so
        // we don't need to sleep for the debouncer.
        let status = controller.status_snapshot();
        assert!(
            status.contains("Type"),
            "expected hint status, got {status:?}"
        );
    }

    #[test]
    fn debouncer_coalesces_bursts() {
        // Ten keystrokes within a 100ms window must yield exactly one
        // dispatch: the debouncer generation ticks per keystroke but the
        // dispatched_generation only advances when the debounce window
        // has elapsed quiet.
        let controller = SearchController::new();
        controller.set_min_query_length(1);
        controller.set_debounce_ms(120);
        // Don't attach a window — dispatch_query would try to reach into
        // the index client anyway; we only want to verify coalescing here.
        controller.open();
        for i in 0..10 {
            let s = format!("query{i}");
            controller.set_query(s);
            std::thread::sleep(Duration::from_millis(5));
        }
        let gen_after_burst = controller.pending_generation();
        // Wait past the debounce window plus a small safety margin.
        std::thread::sleep(Duration::from_millis(400));
        let dispatched = controller
            .dispatched_generation
            .load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            dispatched, gen_after_burst,
            "debouncer must dispatch exactly the last queued generation, \
             not every intermediate one"
        );
    }

    #[test]
    fn rank_and_cap_truncates_to_cap() {
        let rows: Vec<UnifiedResult> = (0..500)
            .map(|i| UnifiedResult::Path {
                source: UnifiedSource::PathIndex,
                path: PathBuf::from(format!("/tmp/atlas-{i}")),
                score: 0,
            })
            .collect();
        let out = rank_and_cap(&rows, "atlas", 100);
        assert_eq!(out.len(), 100);
    }

    #[test]
    fn rank_and_cap_puts_best_match_first() {
        let rows = vec![
            UnifiedResult::Path {
                source: UnifiedSource::PathIndex,
                path: PathBuf::from("/x/unrelated-name"),
                score: 0,
            },
            UnifiedResult::Path {
                source: UnifiedSource::PathIndex,
                path: PathBuf::from("/x/atlas-exact"),
                score: 0,
            },
        ];
        let out = rank_and_cap(&rows, "atlas", 10);
        assert_eq!(out.len(), 2);
        match &out[0] {
            UnifiedResult::Path { path, .. } => {
                assert!(
                    path.to_string_lossy().contains("atlas-exact"),
                    "best match should come first, got {}",
                    path.display()
                );
            }
            other => panic!("expected path result, got {other:?}"),
        }
    }

    #[test]
    fn format_thousands_inserts_separators() {
        assert_eq!(format_thousands(0), "0");
        assert_eq!(format_thousands(999), "999");
        assert_eq!(format_thousands(1_000), "1,000");
        assert_eq!(format_thousands(35_293), "35,293");
        assert_eq!(format_thousands(1_000_000), "1,000,000");
    }

    #[test]
    fn summarize_reports_capped_of_total() {
        let summaries = vec![UnifiedSummary {
            source: UnifiedSource::PathIndex,
            matches: 35_293,
            errored: false,
            message: None,
        }];
        let s = summarize(&summaries, 35_293, 100);
        assert!(s.contains("Showing 100"), "got {s:?}");
        assert!(s.contains("35,293"), "got {s:?}");
    }
}
