//! Unified search facade.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use atlas_index::{Query, SearchOptions as IndexSearchOptions};
use crossbeam_channel::{Receiver, RecvTimeoutError};

use crate::{
    content::{
        self, CaseSensitivity, FileFilter, PatternSpec, SearchEvent, SearchOptions, SearchRequest,
    },
    fuzzy_rank, IndexClient,
};

/// Search source used by [`UnifiedRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifiedSource {
    /// Path results from atlas-indexd.
    PathIndex,
    /// Literal content results from the ripgrep-backed searcher.
    Content,
    /// In-process fuzzy scoring over a provided candidate list.
    FuzzyLocal,
}

/// Request payload for [`run`].
#[derive(Debug, Clone)]
pub struct UnifiedRequest {
    /// Raw query string.
    pub query: String,
    /// Sources to consult.
    pub sources: Vec<UnifiedSource>,
    /// Root scope for indexed-path and content searches.
    pub scope: Option<PathBuf>,
    /// Maximum results emitted per source.
    pub max_results_per_source: usize,
    /// Local candidate paths used by [`UnifiedSource::FuzzyLocal`].
    pub candidates: Vec<PathBuf>,
    /// Glob patterns skipped by the content-search walker. Wired from
    /// `config.search.default_globs_exclude`.
    pub exclude_globs: Vec<String>,
    /// Worker-thread count for the content-search walker. `None` = auto.
    /// Wired from `config.search.content_search_threads`.
    pub content_search_threads: Option<usize>,
}

impl Default for UnifiedRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            sources: vec![UnifiedSource::Content],
            scope: None,
            max_results_per_source: 50,
            candidates: Vec::new(),
            exclude_globs: Vec::new(),
            content_search_threads: None,
        }
    }
}

/// Result emitted by the unified search stream.
#[derive(Debug, Clone)]
pub enum UnifiedResult {
    /// A path-only result.
    Path {
        /// Source that produced this path.
        source: UnifiedSource,
        /// Matching path.
        path: PathBuf,
        /// Source-defined score.
        score: u32,
    },
    /// A content match result.
    Content {
        /// Matching file path.
        path: PathBuf,
        /// 1-based line number.
        line: u64,
        /// Matched line snippet.
        snippet: String,
        /// Byte spans within `snippet`.
        spans: Vec<(u32, u32)>,
        /// Match score.
        score: u32,
    },
}

/// Receivers and cancellation handle for a unified search.
pub struct UnifiedHandle {
    /// Streaming results from all configured sources.
    pub results: Receiver<UnifiedResult>,
    /// Per-source completion summaries.
    pub summary: Receiver<UnifiedSummary>,
    cancel: Arc<AtomicBool>,
}

impl UnifiedHandle {
    /// Signal all running sources to stop as soon as possible.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Destructure into parts for the consumer to own the receivers.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        Receiver<UnifiedResult>,
        Receiver<UnifiedSummary>,
        Arc<AtomicBool>,
    ) {
        (self.results, self.summary, self.cancel)
    }
}

/// Completion metadata for one unified source.
#[derive(Debug, Clone)]
pub struct UnifiedSummary {
    /// Source this summary belongs to.
    pub source: UnifiedSource,
    /// Number of matches emitted by the source.
    pub matches: u64,
    /// Whether the source encountered an error.
    pub errored: bool,
    /// Optional human-readable message.
    pub message: Option<String>,
}

/// Launch a unified search.
///
/// This must be called from a Tokio runtime because [`UnifiedSource::PathIndex`]
/// uses async IPC under the hood.
pub async fn run(req: UnifiedRequest, index: Option<Arc<IndexClient>>) -> UnifiedHandle {
    let (results_tx, results_rx) = crossbeam_channel::bounded(1024);
    let (summary_tx, summary_rx) = crossbeam_channel::bounded(1024);
    let cancel = Arc::new(AtomicBool::new(false));

    for source in req.sources.clone() {
        match source {
            UnifiedSource::PathIndex => {
                let Some(index) = index.clone() else {
                    let _ = summary_tx.send(UnifiedSummary {
                        source: UnifiedSource::PathIndex,
                        matches: 0,
                        errored: true,
                        message: Some("index unavailable".to_owned()),
                    });
                    continue;
                };

                let results_tx = results_tx.clone();
                let summary_tx = summary_tx.clone();
                let cancel = Arc::clone(&cancel);
                let query = req.query.clone();
                let scope = req.scope.clone();
                let max_results = req.max_results_per_source.max(1);

                tokio::spawn(async move {
                    let query = if query.len() >= 2 {
                        Query::NameSubstring(query)
                    } else {
                        Query::NamePrefix(query)
                    };
                    let query = match scope {
                        Some(scope) => Query::All(vec![query, Query::InSubtree(scope)]),
                        None => query,
                    };
                    let options = IndexSearchOptions {
                        limit: max_results,
                        ..IndexSearchOptions::default()
                    };

                    match index.search_paths(&query, &options).await {
                        Ok(hits) => {
                            let mut count = 0_u64;
                            for hit in hits.into_iter().take(max_results) {
                                if cancel.load(Ordering::Relaxed) {
                                    break;
                                }
                                count += 1;
                                if results_tx
                                    .send(UnifiedResult::Path {
                                        source: UnifiedSource::PathIndex,
                                        path: hit.path,
                                        score: hit.score.max(0.0).round() as u32,
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            let _ = summary_tx.send(UnifiedSummary {
                                source: UnifiedSource::PathIndex,
                                matches: count,
                                errored: false,
                                message: None,
                            });
                        }
                        Err(error) => {
                            let _ = summary_tx.send(UnifiedSummary {
                                source: UnifiedSource::PathIndex,
                                matches: 0,
                                errored: true,
                                message: Some(error.to_string()),
                            });
                        }
                    }
                });
            }
            UnifiedSource::Content => {
                let results_tx = results_tx.clone();
                let summary_tx = summary_tx.clone();
                let cancel = Arc::clone(&cancel);
                let query = req.query.clone();
                let scope = req.scope.clone();
                let exclude_globs = req.exclude_globs.clone();
                let content_threads = req.content_search_threads;

                std::thread::spawn(move || {
                    let Some(scope) = scope else {
                        let _ = summary_tx.send(UnifiedSummary {
                            source: UnifiedSource::Content,
                            matches: 0,
                            errored: true,
                            message: Some("search scope is not set".to_owned()),
                        });
                        return;
                    };

                    let filter = FileFilter {
                        exclude_globs,
                        ..FileFilter::default()
                    };
                    let options = SearchOptions {
                        threads: content_threads,
                        ..SearchOptions::default()
                    };
                    let handle = content::run(SearchRequest {
                        roots: vec![scope],
                        pattern: PatternSpec::Literal {
                            text: query,
                            case: CaseSensitivity::Insensitive,
                            word_boundary: false,
                        },
                        filter,
                        options,
                    });

                    let mut summary = UnifiedSummary {
                        source: UnifiedSource::Content,
                        matches: 0,
                        errored: false,
                        message: None,
                    };

                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            handle.cancel();
                        }

                        match handle.receiver.recv_timeout(Duration::from_millis(25)) {
                            Ok(SearchEvent::Match(found)) => {
                                summary.matches += 1;
                                let spans = found
                                    .spans
                                    .iter()
                                    .map(|span| (span.start, span.end))
                                    .collect();
                                if results_tx
                                    .send(UnifiedResult::Content {
                                        path: found.path,
                                        line: found.line_number,
                                        snippet: found.line,
                                        spans,
                                        score: 0,
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Ok(SearchEvent::Error { error, .. }) => {
                                summary.errored = true;
                                if summary.message.is_none() {
                                    summary.message = Some(error);
                                }
                            }
                            Ok(SearchEvent::Summary(search_summary)) => {
                                summary.matches = search_summary.matches;
                            }
                            Ok(SearchEvent::Done) => break,
                            Ok(SearchEvent::FileSearched { .. }) => {}
                            Err(RecvTimeoutError::Timeout) => {
                                if cancel.load(Ordering::Relaxed) {
                                    continue;
                                }
                            }
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    }

                    let _ = summary_tx.send(summary);
                    handle.join();
                });
            }
            UnifiedSource::FuzzyLocal => {
                let results_tx = results_tx.clone();
                let summary_tx = summary_tx.clone();
                let cancel = Arc::clone(&cancel);
                let query = req.query.clone();
                let candidates = req.candidates.clone();
                let max_results = req.max_results_per_source.max(1);

                std::thread::spawn(move || {
                    let ranked = fuzzy_rank(candidates, &query, |path| path.to_str().unwrap_or(""));
                    let mut count = 0_u64;
                    for (path, score) in ranked.into_iter().take(max_results) {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        count += 1;
                        if results_tx
                            .send(UnifiedResult::Path {
                                source: UnifiedSource::FuzzyLocal,
                                path,
                                score,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    let _ = summary_tx.send(UnifiedSummary {
                        source: UnifiedSource::FuzzyLocal,
                        matches: count,
                        errored: false,
                        message: None,
                    });
                });
            }
        }
    }

    drop(results_tx);
    drop(summary_tx);

    UnifiedHandle {
        results: results_rx,
        summary: summary_rx,
        cancel,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::Ordering,
        time::{Duration, Instant},
    };

    use super::{run, UnifiedRequest, UnifiedResult, UnifiedSource};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should build")
    }

    #[test]
    fn content_results_stream_in() {
        let dir = tempfile::tempdir().expect("tempdir should build");
        let file = dir.path().join("sample.txt");
        fs::write(
            &file,
            "atlas
hello atlas
",
        )
        .expect("fixture should write");

        let handle = runtime().block_on(run(
            UnifiedRequest {
                query: "atlas".into(),
                sources: vec![UnifiedSource::Content],
                scope: Some(dir.path().to_path_buf()),
                max_results_per_source: 50,
                candidates: Vec::new(),
                exclude_globs: Vec::new(),
                content_search_threads: None,
            },
            None,
        ));
        let (results_rx, summary_rx, _) = handle.into_parts();

        let results: Vec<_> = results_rx.iter().collect();
        let summaries: Vec<_> = summary_rx.iter().collect();

        assert!(results
            .iter()
            .any(|result| matches!(result, UnifiedResult::Content { .. })));
        assert!(summaries
            .iter()
            .any(|summary| { summary.source == UnifiedSource::Content && summary.matches >= 2 }));
    }

    #[test]
    fn content_cancellation_stops_search() {
        let dir = tempfile::tempdir().expect("tempdir should build");
        for index in 0..32 {
            let file = dir.path().join(format!("file-{index}.txt"));
            let body = std::iter::repeat_n("atlas line\n", 256).collect::<String>();
            fs::write(file, body).expect("fixture should write");
        }

        let handle = runtime().block_on(run(
            UnifiedRequest {
                query: "atlas".into(),
                sources: vec![UnifiedSource::Content],
                scope: Some(dir.path().to_path_buf()),
                max_results_per_source: 50,
                candidates: Vec::new(),
                exclude_globs: Vec::new(),
                content_search_threads: None,
            },
            None,
        ));
        let (results_rx, summary_rx, cancel) = handle.into_parts();

        let first = results_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first result should arrive before cancellation");
        assert!(matches!(first, UnifiedResult::Content { .. }));
        cancel.store(true, Ordering::Relaxed);

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut results_closed = false;
        let mut summary_closed = false;
        while Instant::now() < deadline && !(results_closed && summary_closed) {
            crossbeam_channel::select! {
                recv(results_rx) -> message => {
                    if message.is_err() {
                        results_closed = true;
                    }
                },
                recv(summary_rx) -> message => {
                    if message.is_err() {
                        summary_closed = true;
                    }
                },
                default(Duration::from_millis(50)) => {}
            }
        }

        assert!(results_closed);
        assert!(summary_closed);
    }
}
