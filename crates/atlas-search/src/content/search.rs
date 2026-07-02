//! Core search engine: request types, the streaming handle, and the parallel walker.

use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use anyhow::Context as _;
use crossbeam_channel::{Receiver, Sender};
use grep::matcher::Matcher;
use grep_regex::RegexMatcher;
use grep_searcher::{
    BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkFinish,
    SinkMatch,
};
use ignore::{overrides::OverrideBuilder, WalkBuilder, WalkState};

use atlas_core::Result;

use crate::content::{
    filter::{BinaryHandling, FileFilter},
    pattern::{compile, PatternSpec},
    result::{ContentMatch, MatchSpan, SearchEvent, SearchSummary},
};

/// Options that tune the search behaviour.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Stop after this many matches in a single file. `None` means unlimited.
    pub max_matches_per_file: Option<u64>,
    /// Stop after this many total matches across all files. `None` means unlimited.
    pub max_total_matches: Option<u64>,
    /// Number of context lines to collect **before** each match (like `grep -B`).
    pub context_before: u32,
    /// Number of context lines to collect **after** each match (like `grep -A`).
    pub context_after: u32,
    /// Worker-thread count for the parallel walk. `None` uses `available_parallelism`.
    pub threads: Option<usize>,
    /// Follow symbolic links during traversal.
    pub follow_symlinks: bool,
}

/// A complete description of a content-search operation.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    /// Directories (or files) to search. Must be non-empty.
    pub roots: Vec<PathBuf>,
    /// What to look for.
    pub pattern: PatternSpec,
    /// Which files to walk.
    pub filter: FileFilter,
    /// Tuning knobs.
    pub options: SearchOptions,
}

/// A live search operation returned by [`run`].
pub struct SearchHandle {
    /// Streaming events. Drained until [`SearchEvent::Done`] is received.
    pub receiver: Receiver<SearchEvent>,
    cancel: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl SearchHandle {
    /// Signal the search to stop as soon as possible.
    ///
    /// Does not block; the search may still emit a few more events before honouring
    /// the cancellation. A [`SearchEvent::Summary`] with `cancelled = true` will
    /// be sent before the channel closes.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Block until the coordinator thread exits.
    pub fn join(mut self) {
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// Launch a content search and return a streaming [`SearchHandle`].
///
/// The coordinator and walker threads are started immediately; events begin
/// flowing through `handle.receiver` as soon as matches are found.
pub fn run(req: SearchRequest) -> SearchHandle {
    let (tx, rx) = crossbeam_channel::unbounded::<SearchEvent>();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_clone = Arc::clone(&cancel);

    let join = std::thread::spawn(move || {
        coordinator(req, tx, cancel_clone);
    });

    SearchHandle {
        receiver: rx,
        cancel,
        join: Some(join),
    }
}

/// Blocking convenience wrapper around [`run`]; collects all events and returns
/// the final [`SearchSummary`].
///
/// Matches are discarded — this is intended for tests and CLI use.
pub fn run_blocking(req: SearchRequest) -> Result<SearchSummary> {
    let handle = run(req);
    let mut summary = SearchSummary::default();

    for event in handle.receiver.iter() {
        if let SearchEvent::Summary(next) = event {
            summary = next;
        }
    }

    handle.join();
    Ok(summary)
}

struct Shared {
    tx: Sender<SearchEvent>,
    cancel: Arc<AtomicBool>,
    stop: AtomicBool,
    total_matches: AtomicU64,
    files_searched: AtomicU64,
    files_with_matches: AtomicU64,
    bytes_searched: AtomicU64,
    stopped_due_to_limit: AtomicBool,
    options: SearchOptions,
}

impl Shared {
    fn should_stop(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.stop.load(Ordering::Relaxed)
    }

    fn reserve_total_match(&self) -> bool {
        loop {
            if self.should_stop() {
                return false;
            }

            let current = self.total_matches.load(Ordering::Relaxed);
            if let Some(max) = self.options.max_total_matches {
                if current >= max {
                    self.stopped_due_to_limit.store(true, Ordering::Relaxed);
                    self.stop.store(true, Ordering::Relaxed);
                    return false;
                }
            }

            if self
                .total_matches
                .compare_exchange(current, current + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }
}

fn coordinator(req: SearchRequest, tx: Sender<SearchEvent>, cancel: Arc<AtomicBool>) {
    let start = Instant::now();

    if req.roots.is_empty() {
        send_terminal_events(
            &tx,
            SearchSummary {
                elapsed_ms: start.elapsed().as_millis() as u64,
                ..SearchSummary::default()
            },
            Some(SearchEvent::Error {
                path: None,
                error: "search request must include at least one root".to_owned(),
            }),
        );
        return;
    }

    let matcher = match compile(&req.pattern).context("failed to compile search pattern") {
        Ok(matcher) => Arc::new(matcher),
        Err(error) => {
            send_terminal_events(
                &tx,
                SearchSummary {
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    ..SearchSummary::default()
                },
                Some(SearchEvent::Error {
                    path: None,
                    error: error.to_string(),
                }),
            );
            return;
        }
    };
    let multi_line = matches!(
        &req.pattern,
        PatternSpec::Regex {
            multiline: true,
            ..
        }
    );

    let threads = req.options.threads.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|parallelism| parallelism.get())
            .unwrap_or(4)
    });

    let mut roots_iter = req.roots.iter();
    let first_root = match roots_iter.next() {
        Some(root) => root,
        None => {
            send_terminal_events(
                &tx,
                SearchSummary {
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    ..SearchSummary::default()
                },
                None,
            );
            return;
        }
    };

    let overrides = match build_overrides(overrides_root(first_root), &req.filter) {
        Ok(overrides) => overrides,
        Err(error) => {
            send_terminal_events(
                &tx,
                SearchSummary {
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    ..SearchSummary::default()
                },
                Some(SearchEvent::Error {
                    path: Some(first_root.to_path_buf()),
                    error: error.to_string(),
                }),
            );
            return;
        }
    };

    let mut builder = WalkBuilder::new(first_root);
    for root in roots_iter {
        builder.add(root);
    }

    let gitignore = req.filter.respect_gitignore;
    builder
        .threads(threads)
        .overrides(overrides)
        .git_ignore(gitignore)
        .git_global(gitignore)
        .git_exclude(gitignore)
        .ignore(gitignore)
        .parents(gitignore)
        .hidden(!req.filter.include_hidden)
        .follow_links(req.options.follow_symlinks)
        .max_filesize(req.filter.max_filesize_bytes);

    let shared = Arc::new(Shared {
        tx: tx.clone(),
        cancel: Arc::clone(&cancel),
        stop: AtomicBool::new(false),
        total_matches: AtomicU64::new(0),
        files_searched: AtomicU64::new(0),
        files_with_matches: AtomicU64::new(0),
        bytes_searched: AtomicU64::new(0),
        stopped_due_to_limit: AtomicBool::new(false),
        options: req.options.clone(),
    });

    let binary = req.filter.binary;
    let context_before = req.options.context_before;
    let context_after = req.options.context_after;

    builder.build_parallel().run(|| {
        let matcher = Arc::clone(&matcher);
        let shared = Arc::clone(&shared);

        Box::new(move |entry_result| {
            if shared.should_stop() {
                return WalkState::Quit;
            }

            let entry = match entry_result {
                Ok(entry) => entry,
                Err(error) => {
                    let _ = shared.tx.send(SearchEvent::Error {
                        path: None,
                        error: error.to_string(),
                    });
                    return WalkState::Continue;
                }
            };

            if entry
                .file_type()
                .map(|file_type| !file_type.is_file())
                .unwrap_or(true)
            {
                return WalkState::Continue;
            }

            search_file(
                entry.path().to_path_buf(),
                Arc::clone(&matcher),
                &shared,
                binary,
                context_before,
                context_after,
                multi_line,
            );

            if shared.should_stop() {
                WalkState::Quit
            } else {
                WalkState::Continue
            }
        })
    });

    let summary = SearchSummary {
        files_searched: shared.files_searched.load(Ordering::Relaxed),
        files_with_matches: shared.files_with_matches.load(Ordering::Relaxed),
        matches: shared.total_matches.load(Ordering::Relaxed),
        bytes_searched: shared.bytes_searched.load(Ordering::Relaxed),
        elapsed_ms: start.elapsed().as_millis() as u64,
        stopped_due_to_limit: shared.stopped_due_to_limit.load(Ordering::Relaxed),
        cancelled: cancel.load(Ordering::Relaxed),
    };

    send_terminal_events(&tx, summary, None);
}

fn send_terminal_events(
    tx: &Sender<SearchEvent>,
    summary: SearchSummary,
    error: Option<SearchEvent>,
) {
    if let Some(error) = error {
        let _ = tx.send(error);
    }
    let _ = tx.send(SearchEvent::Summary(summary));
    let _ = tx.send(SearchEvent::Done);
}

fn overrides_root(root: &Path) -> &Path {
    match std::fs::metadata(root) {
        Ok(metadata) if metadata.is_file() => root.parent().unwrap_or(root),
        _ => root,
    }
}

fn build_overrides(
    root: &Path,
    filter: &FileFilter,
) -> anyhow::Result<ignore::overrides::Override> {
    let mut builder = OverrideBuilder::new(root);

    for glob in &filter.include_globs {
        builder
            .add(glob)
            .with_context(|| format!("invalid include glob `{glob}`"))?;
    }
    for glob in &filter.exclude_globs {
        builder
            .add(&format!("!{glob}"))
            .with_context(|| format!("invalid exclude glob `{glob}`"))?;
    }

    builder.build().context("failed to finalize file overrides")
}

fn search_file(
    path: PathBuf,
    matcher: Arc<RegexMatcher>,
    shared: &Arc<Shared>,
    binary: BinaryHandling,
    context_before: u32,
    context_after: u32,
    multi_line: bool,
) {
    let file_size = std::fs::metadata(&path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let is_binary =
        matches!(binary, BinaryHandling::BinaryAsBinary) && probe_binary(&path).unwrap_or(false);

    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .before_context(context_before as usize)
        .after_context(context_after as usize)
        .multi_line(multi_line)
        .binary_detection(match binary {
            BinaryHandling::Skip => BinaryDetection::quit(b' '),
            BinaryHandling::AsText | BinaryHandling::BinaryAsBinary => BinaryDetection::none(),
        })
        .build();

    let mut sink = AtlasSink {
        path: path.clone(),
        matcher: Arc::clone(&matcher),
        shared: Arc::clone(shared),
        file_matches: 0,
        binary_mode: is_binary,
        before_buf: Vec::new(),
        pending_match: None,
    };

    let result = searcher.search_path(matcher.as_ref(), &path, &mut sink);
    sink.flush_pending();

    if let Err(error) = result {
        let _ = shared.tx.send(SearchEvent::Error {
            path: Some(path.clone()),
            error: error.to_string(),
        });
    }

    shared.files_searched.fetch_add(1, Ordering::Relaxed);
    shared
        .bytes_searched
        .fetch_add(file_size, Ordering::Relaxed);

    if sink.file_matches > 0 {
        shared.files_with_matches.fetch_add(1, Ordering::Relaxed);
    }

    let _ = shared.tx.send(SearchEvent::FileSearched {
        path,
        matches: sink.file_matches,
    });
}

struct AtlasSink {
    path: PathBuf,
    matcher: Arc<RegexMatcher>,
    shared: Arc<Shared>,
    file_matches: u64,
    binary_mode: bool,
    before_buf: Vec<String>,
    pending_match: Option<ContentMatch>,
}

impl AtlasSink {
    fn flush_pending(&mut self) {
        if let Some(content_match) = self.pending_match.take() {
            let _ = self.shared.tx.send(SearchEvent::Match(content_match));
        }
    }
}

impl Sink for AtlasSink {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        mat: &SinkMatch<'_>,
    ) -> std::result::Result<bool, Self::Error> {
        self.flush_pending();

        if self.shared.should_stop() {
            return Ok(false);
        }

        if let Some(max_matches) = self.shared.options.max_matches_per_file {
            if self.file_matches >= max_matches {
                return Ok(false);
            }
        }

        if !self.shared.reserve_total_match() {
            return Ok(false);
        }

        self.file_matches += 1;
        self.pending_match = Some(ContentMatch {
            path: self.path.clone(),
            line_number: mat.line_number().unwrap_or(0),
            byte_offset: mat.absolute_byte_offset(),
            line: normalize_bytes(mat.bytes()),
            spans: build_spans(self.matcher.as_ref(), mat.bytes()),
            before: std::mem::take(&mut self.before_buf),
            after: Vec::new(),
            is_binary: self.binary_mode,
        });

        Ok(true)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        ctx: &SinkContext<'_>,
    ) -> std::result::Result<bool, Self::Error> {
        match ctx.kind() {
            SinkContextKind::Before => {
                if self.pending_match.is_some() {
                    self.flush_pending();
                }
                self.before_buf.push(normalize_bytes(ctx.bytes()));
            }
            SinkContextKind::After => {
                if let Some(pending_match) = &mut self.pending_match {
                    pending_match.after.push(normalize_bytes(ctx.bytes()));
                }
            }
            SinkContextKind::Other => self.flush_pending(),
        }

        Ok(!self.shared.should_stop())
    }

    fn finish(
        &mut self,
        _searcher: &Searcher,
        _finish: &SinkFinish,
    ) -> std::result::Result<(), Self::Error> {
        self.flush_pending();
        Ok(())
    }
}

fn build_spans(matcher: &RegexMatcher, bytes: &[u8]) -> Vec<MatchSpan> {
    let mut spans = Vec::new();
    let _ = matcher.find_iter(bytes, |mat| {
        spans.push(MatchSpan {
            start: mat.start() as u32,
            end: mat.end() as u32,
        });
        true
    });
    spans
}

fn normalize_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches(['\n', '\r'])
        .to_owned()
}

fn probe_binary(path: &Path) -> std::io::Result<bool> {
    let mut file = File::open(path)?;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(false);
        }
        if buffer[..read].contains(&0) {
            return Ok(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use tempfile::TempDir;

    use super::{run, run_blocking, SearchEvent, SearchOptions, SearchRequest, SearchSummary};
    use crate::content::{BinaryHandling, CaseSensitivity, FileFilter, PatternSpec};

    fn temp_dir() -> TempDir {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp");
        fs::create_dir_all(&base).unwrap();
        TempDir::new_in(base).unwrap()
    }

    fn request(root: PathBuf, pattern: PatternSpec) -> SearchRequest {
        SearchRequest {
            roots: vec![root],
            pattern,
            filter: FileFilter::default(),
            options: SearchOptions {
                threads: Some(1),
                ..SearchOptions::default()
            },
        }
    }

    #[test]
    fn search_reports_context_and_summary() {
        let temp = temp_dir();
        let file = temp.path().join("sample.txt");
        fs::write(
            &file,
            "zero
alpha beta
omega
",
        )
        .unwrap();

        let mut req = request(
            temp.path().to_path_buf(),
            PatternSpec::Literal {
                text: "alpha".to_owned(),
                case: CaseSensitivity::Sensitive,
                word_boundary: false,
            },
        );
        req.options.context_before = 1;
        req.options.context_after = 1;

        let handle = run(req);
        let mut events = Vec::new();
        for event in handle.receiver.iter() {
            events.push(event);
        }
        handle.join();

        let matches = events
            .iter()
            .filter_map(|event| match event {
                SearchEvent::Match(content_match) => Some(content_match),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, file);
        assert_eq!(matches[0].line_number, 2);
        assert_eq!(matches[0].line, "alpha beta");
        assert_eq!(matches[0].before, vec!["zero"]);
        assert_eq!(matches[0].after, vec!["omega"]);
        assert_eq!(matches[0].spans.len(), 1);
        assert_eq!((matches[0].spans[0].start, matches[0].spans[0].end), (0, 5));

        let summary = events
            .iter()
            .find_map(|event| match event {
                SearchEvent::Summary(summary) => Some(summary.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(summary.files_searched, 1);
        assert_eq!(summary.files_with_matches, 1);
        assert_eq!(summary.matches, 1);
        assert!(!summary.stopped_due_to_limit);
        assert!(!summary.cancelled);
    }

    #[test]
    fn include_globs_limit_walked_files() {
        let temp = temp_dir();
        fs::write(
            temp.path().join("keep.rs"),
            "needle
",
        )
        .unwrap();
        fs::write(
            temp.path().join("skip.txt"),
            "needle
",
        )
        .unwrap();

        let summary = run_blocking(SearchRequest {
            roots: vec![temp.path().to_path_buf()],
            pattern: PatternSpec::Literal {
                text: "needle".to_owned(),
                case: CaseSensitivity::Sensitive,
                word_boundary: false,
            },
            filter: FileFilter {
                include_globs: vec!["*.rs".to_owned()],
                ..FileFilter::default()
            },
            options: SearchOptions {
                threads: Some(1),
                ..SearchOptions::default()
            },
        })
        .unwrap();

        assert_eq!(summary.files_searched, 1);
        assert_eq!(summary.files_with_matches, 1);
        assert_eq!(summary.matches, 1);
    }

    #[test]
    fn max_total_matches_stops_without_marking_cancelled() {
        let temp = temp_dir();
        fs::write(
            temp.path().join("sample.txt"),
            "alpha
alpha
",
        )
        .unwrap();

        let summary = run_blocking(SearchRequest {
            roots: vec![temp.path().to_path_buf()],
            pattern: PatternSpec::Literal {
                text: "alpha".to_owned(),
                case: CaseSensitivity::Sensitive,
                word_boundary: false,
            },
            filter: FileFilter::default(),
            options: SearchOptions {
                max_total_matches: Some(1),
                threads: Some(1),
                ..SearchOptions::default()
            },
        })
        .unwrap();

        assert_eq!(summary.matches, 1);
        assert!(summary.stopped_due_to_limit);
        assert!(!summary.cancelled);
    }

    #[test]
    fn binary_mode_marks_binary_matches() {
        let temp = temp_dir();
        fs::write(
            temp.path().join("binary.bin"),
            b"alpha omega
",
        )
        .unwrap();

        let handle = run(SearchRequest {
            roots: vec![temp.path().to_path_buf()],
            pattern: PatternSpec::Literal {
                text: "alpha".to_owned(),
                case: CaseSensitivity::Sensitive,
                word_boundary: false,
            },
            filter: FileFilter {
                binary: BinaryHandling::BinaryAsBinary,
                ..FileFilter::default()
            },
            options: SearchOptions {
                threads: Some(1),
                ..SearchOptions::default()
            },
        });

        let mut saw_binary = false;
        for event in handle.receiver.iter() {
            if let SearchEvent::Match(content_match) = event {
                saw_binary = content_match.is_binary;
            }
        }
        handle.join();

        assert!(saw_binary);
    }

    // ── regex ──────────────────────────────────────────────────────────────

    #[test]
    fn regex_matches_function_definitions() {
        let temp = temp_dir();
        fs::write(
            temp.path().join("code.rs"),
            "// comment\nfn main() {\n    println!(\"hello\");\n}\nfn helper(x: u32) -> u32 { x }\nlet x = 1;\n",
        )
        .unwrap();

        let summary = run_blocking(SearchRequest {
            roots: vec![temp.path().to_path_buf()],
            pattern: PatternSpec::Regex {
                pattern: r"^fn\s+\w+".to_owned(),
                case: CaseSensitivity::Sensitive,
                multiline: false,
            },
            filter: FileFilter::default(),
            options: SearchOptions {
                threads: Some(1),
                ..SearchOptions::default()
            },
        })
        .unwrap();

        // Two function definitions: `fn main` and `fn helper`.
        assert_eq!(summary.matches, 2);
    }

    // ── case sensitivity ───────────────────────────────────────────────────

    #[test]
    fn case_insensitive_matches_uppercase_pattern_against_lowercase_text() {
        let temp = temp_dir();
        fs::write(temp.path().join("a.txt"), "atlas is great\n").unwrap();

        let summary = run_blocking(SearchRequest {
            roots: vec![temp.path().to_path_buf()],
            pattern: PatternSpec::Literal {
                text: "ATLAS".to_owned(),
                case: CaseSensitivity::Insensitive,
                word_boundary: false,
            },
            filter: FileFilter::default(),
            options: SearchOptions {
                threads: Some(1),
                ..SearchOptions::default()
            },
        })
        .unwrap();

        assert_eq!(summary.matches, 1);
    }

    #[test]
    fn smart_case_uppercase_pattern_is_sensitive() {
        let temp = temp_dir();
        // Only the mixed-case line should match "Atlas" when pattern has uppercase.
        fs::write(
            temp.path().join("a.txt"),
            "Atlas is great\natlas is great\n",
        )
        .unwrap();

        let summary = run_blocking(SearchRequest {
            roots: vec![temp.path().to_path_buf()],
            pattern: PatternSpec::Literal {
                text: "Atlas".to_owned(),
                case: CaseSensitivity::Smart,
                word_boundary: false,
            },
            filter: FileFilter::default(),
            options: SearchOptions {
                threads: Some(1),
                ..SearchOptions::default()
            },
        })
        .unwrap();

        // Smart+uppercase → sensitive: only "Atlas" line matches, not "atlas".
        assert_eq!(summary.matches, 1);
    }

    #[test]
    fn smart_case_lowercase_pattern_is_insensitive() {
        let temp = temp_dir();
        fs::write(
            temp.path().join("a.txt"),
            "Atlas is great\natlas is great\n",
        )
        .unwrap();

        let summary = run_blocking(SearchRequest {
            roots: vec![temp.path().to_path_buf()],
            pattern: PatternSpec::Literal {
                text: "atlas".to_owned(),
                case: CaseSensitivity::Smart,
                word_boundary: false,
            },
            filter: FileFilter::default(),
            options: SearchOptions {
                threads: Some(1),
                ..SearchOptions::default()
            },
        })
        .unwrap();

        // Smart+lowercase → insensitive: both "Atlas" and "atlas" lines match.
        assert_eq!(summary.matches, 2);
    }

    // ── word boundary ──────────────────────────────────────────────────────

    #[test]
    fn word_boundary_skips_embedded_occurrences() {
        let temp = temp_dir();
        fs::write(temp.path().join("w.txt"), "foo bar\nfoobar\n").unwrap();

        let summary = run_blocking(SearchRequest {
            roots: vec![temp.path().to_path_buf()],
            pattern: PatternSpec::Literal {
                text: "foo".to_owned(),
                case: CaseSensitivity::Sensitive,
                word_boundary: true,
            },
            filter: FileFilter::default(),
            options: SearchOptions {
                threads: Some(1),
                ..SearchOptions::default()
            },
        })
        .unwrap();

        // "foo bar" matches but "foobar" does not.
        assert_eq!(summary.matches, 1);
    }

    // ── per-file limit ─────────────────────────────────────────────────────

    #[test]
    fn max_matches_per_file_limits_to_one_per_file() {
        let temp = temp_dir();
        // Two files, each with three occurrences of the pattern.
        fs::write(temp.path().join("a.txt"), "hit\nhit\nhit\n").unwrap();
        fs::write(temp.path().join("b.txt"), "hit\nhit\nhit\n").unwrap();

        let summary = run_blocking(SearchRequest {
            roots: vec![temp.path().to_path_buf()],
            pattern: PatternSpec::Literal {
                text: "hit".to_owned(),
                case: CaseSensitivity::Sensitive,
                word_boundary: false,
            },
            filter: FileFilter::default(),
            options: SearchOptions {
                max_matches_per_file: Some(1),
                threads: Some(1),
                ..SearchOptions::default()
            },
        })
        .unwrap();

        // One match per file × two files = two matches.
        assert_eq!(summary.matches, 2);
    }

    // ── exclude globs ──────────────────────────────────────────────────────

    #[test]
    fn exclude_globs_skip_matching_files() {
        let temp = temp_dir();
        fs::write(temp.path().join("main.rs"), "needle\n").unwrap();
        fs::write(temp.path().join("main_test.rs"), "needle\n").unwrap();

        let summary = run_blocking(SearchRequest {
            roots: vec![temp.path().to_path_buf()],
            pattern: PatternSpec::Literal {
                text: "needle".to_owned(),
                case: CaseSensitivity::Sensitive,
                word_boundary: false,
            },
            filter: FileFilter {
                exclude_globs: vec!["*test*".to_owned()],
                ..FileFilter::default()
            },
            options: SearchOptions {
                threads: Some(1),
                ..SearchOptions::default()
            },
        })
        .unwrap();

        // Only main.rs is searched; main_test.rs is excluded.
        assert_eq!(summary.files_with_matches, 1);
        assert_eq!(summary.matches, 1);
    }

    // ── gitignore ──────────────────────────────────────────────────────────

    #[test]
    fn respect_gitignore_skips_ignored_directory() {
        let temp = temp_dir();
        // Create .gitignore that ignores the `ignored/` directory.
        fs::write(temp.path().join(".gitignore"), "ignored/\n").unwrap();
        fs::create_dir(temp.path().join("ignored")).unwrap();
        fs::write(temp.path().join("ignored").join("secret.rs"), "needle\n").unwrap();
        // A file outside the ignored dir that should be found.
        fs::write(temp.path().join("visible.rs"), "needle\n").unwrap();

        let summary = run_blocking(SearchRequest {
            roots: vec![temp.path().to_path_buf()],
            pattern: PatternSpec::Literal {
                text: "needle".to_owned(),
                case: CaseSensitivity::Sensitive,
                word_boundary: false,
            },
            filter: FileFilter {
                respect_gitignore: true,
                ..FileFilter::default()
            },
            options: SearchOptions {
                threads: Some(1),
                ..SearchOptions::default()
            },
        })
        .unwrap();

        // Only visible.rs is found; ignored/secret.rs is skipped.
        assert_eq!(summary.files_with_matches, 1);
        assert_eq!(summary.matches, 1);
    }

    // ── hidden files ───────────────────────────────────────────────────────

    #[test]
    fn hidden_files_skipped_when_include_hidden_false() {
        let temp = temp_dir();
        fs::write(temp.path().join(".hidden.rs"), "needle\n").unwrap();
        fs::write(temp.path().join("visible.rs"), "needle\n").unwrap();

        let summary = run_blocking(SearchRequest {
            roots: vec![temp.path().to_path_buf()],
            pattern: PatternSpec::Literal {
                text: "needle".to_owned(),
                case: CaseSensitivity::Sensitive,
                word_boundary: false,
            },
            filter: FileFilter {
                include_hidden: false,
                respect_gitignore: false, // avoid .gitignore side-effects
                ..FileFilter::default()
            },
            options: SearchOptions {
                threads: Some(1),
                ..SearchOptions::default()
            },
        })
        .unwrap();

        // .hidden.rs is skipped; only visible.rs is searched.
        assert_eq!(summary.files_with_matches, 1);
        assert_eq!(summary.matches, 1);
    }

    // ── binary skip ────────────────────────────────────────────────────────

    #[test]
    fn binary_skip_skips_files_with_nul_bytes() {
        let temp = temp_dir();
        // File that contains a NUL byte — detected as binary.
        let mut content = b"needle on line one\n".to_vec();
        content.push(0x00); // NUL byte that triggers binary detection
        content.extend_from_slice(b"\nneedle on line three\n");
        fs::write(temp.path().join("binary.bin"), &content).unwrap();

        let summary = run_blocking(SearchRequest {
            roots: vec![temp.path().to_path_buf()],
            pattern: PatternSpec::Literal {
                text: "needle".to_owned(),
                case: CaseSensitivity::Sensitive,
                word_boundary: false,
            },
            filter: FileFilter {
                binary: BinaryHandling::Skip,
                ..FileFilter::default()
            },
            options: SearchOptions {
                threads: Some(1),
                ..SearchOptions::default()
            },
        })
        .unwrap();

        // Binary file is skipped entirely.
        assert_eq!(summary.matches, 0);
    }

    // ── cancellation ───────────────────────────────────────────────────────

    #[test]
    fn cancellation_stops_search_and_sets_cancelled_flag() {
        let temp = temp_dir();
        // Create enough files that the search cannot finish before we cancel.
        // Each file contains the pattern so the first event is a Match.
        for i in 0..500_u32 {
            let mut body = String::new();
            for j in 0..20_u32 {
                body.push_str(&format!("needle line {i}-{j}\n"));
            }
            fs::write(temp.path().join(format!("file_{i:04}.txt")), &body).unwrap();
        }

        let handle = run(SearchRequest {
            roots: vec![temp.path().to_path_buf()],
            pattern: PatternSpec::Literal {
                text: "needle".to_owned(),
                case: CaseSensitivity::Sensitive,
                word_boundary: false,
            },
            filter: FileFilter {
                respect_gitignore: false,
                ..FileFilter::default()
            },
            options: SearchOptions {
                // Use more threads so there's definitely work in flight.
                threads: Some(4),
                ..SearchOptions::default()
            },
        });

        // Drain until the first Match, then cancel immediately.
        let mut got_match = false;
        let mut summary = SearchSummary::default();
        for event in handle.receiver.iter() {
            match event {
                SearchEvent::Match(_) if !got_match => {
                    got_match = true;
                    handle.cancel();
                }
                SearchEvent::Summary(s) => summary = s,
                SearchEvent::Done => break,
                _ => {}
            }
        }
        handle.join();

        assert!(got_match, "expected at least one match before cancellation");
        assert!(summary.cancelled, "expected cancelled=true in summary");
        // Note: we deliberately do not assert `summary.matches < N` here.
        // Cancellation latency depends on how many worker threads are
        // already mid-file when the flag flips — on fast CI runners the
        // whole 500-file / 10 000-match set can complete before cancel
        // reaches every worker. The correctness invariant is that
        // `summary.cancelled` is set and the pipeline returns cleanly;
        // the latency of cancellation is measured via benches, not this
        // functional test.
    }
}
