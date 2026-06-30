//! Read-side of the Atlas index: search and statistics.
//!
//! [`IndexReader`] wraps a [`tantivy::IndexReader`] and exposes the
//! [`IndexReader::search`] API, which translates [`Query`] trees into tantivy
//! queries and returns [`Hit`] slices.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tantivy::collector::TopDocs;
use tantivy::query::{
    BooleanQuery, FuzzyTermQuery, Occur, Query as TantivyQuery, RegexQuery, TermQuery,
};
use tantivy::schema::{IndexRecordOption, Term, Value as _};
use tantivy::tokenizer::TokenStream as _;
use tantivy::{Index, Order, ReloadPolicy, TantivyDocument};
use walkdir::WalkDir;

use atlas_core::Result;

use crate::doc::DocKind;
use crate::query::{Hit, Query, SearchOptions, SortBy};
use crate::schema::{register_tokenizers, AtlasSchema, NGRAM_TOKENIZER_NAME};
use crate::stats::IndexStats;

// ---------------------------------------------------------------------------
// Cached stats state
// ---------------------------------------------------------------------------

/// One-second TTL cache for on-disk byte count.
struct StatsCache {
    cached_at: Option<Instant>,
    value: Option<IndexStats>,
}

impl StatsCache {
    const fn empty() -> Self {
        Self {
            cached_at: None,
            value: None,
        }
    }
}

// ---------------------------------------------------------------------------
// IndexReader
// ---------------------------------------------------------------------------

/// Read-side wrapper over a [`tantivy::IndexReader`].
///
/// Obtain via [`IndexReader::open`].
pub struct IndexReader {
    inner: tantivy::IndexReader,
    schema: Arc<AtlasSchema>,
    /// Kept alive for access to the tokenizer manager.
    index: Index,
    index_dir: PathBuf,
    stats_cache: parking_lot::Mutex<StatsCache>,
}

impl IndexReader {
    /// Open an existing on-disk index for reading.
    ///
    /// The reader is configured with [`ReloadPolicy::OnCommitWithDelay`] so it
    /// automatically picks up new commits in the background.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be opened or the index
    /// metadata is corrupt.
    pub fn open(dir: &Path) -> Result<Self> {
        let index =
            Index::open_in_dir(dir).map_err(|e| anyhow::anyhow!("open index at {:?}: {e}", dir))?;

        register_tokenizers(&index);

        let inner = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(|e| anyhow::anyhow!("build reader: {e}"))?;

        Ok(Self {
            inner,
            schema: Arc::new(AtlasSchema::build()),
            index,
            index_dir: dir.to_path_buf(),
            stats_cache: parking_lot::Mutex::new(StatsCache::empty()),
        })
    }

    /// Force the reader to see the latest committed segments immediately.
    ///
    /// Usually not needed when the reader is opened with
    /// [`ReloadPolicy::OnCommitWithDelay`], but can be called explicitly after
    /// a [`IndexWriter::commit`] in tests or synchronous workflows.
    ///
    /// [`IndexWriter::commit`]: crate::IndexWriter::commit
    ///
    /// # Errors
    ///
    /// Propagates tantivy reload errors.
    pub fn reload(&self) -> Result<()> {
        self.inner
            .reload()
            .map_err(|e| anyhow::anyhow!("reload: {e}"))?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    /// Execute a search against the current index snapshot.
    ///
    /// The query is translated into a tantivy query tree, optionally wrapped
    /// with a hidden-file filter (when `opts.include_hidden` is `false`), and
    /// then evaluated with the requested sort order.
    ///
    /// Results are returned in the order specified by [`SearchOptions::sort`].
    /// When `sort` is [`SortBy::Name`] the top `opts.limit` candidates by
    /// relevance score are retrieved and then sorted in memory — this keeps
    /// the implementation simple at the cost of precision when there are many
    /// more than `opts.limit` matching documents.
    ///
    /// # Errors
    ///
    /// Returns an error when the query cannot be compiled (e.g. an invalid
    /// regex) or when tantivy search fails.
    pub fn search(&self, query: &Query, opts: &SearchOptions) -> Result<Vec<Hit>> {
        let searcher = self.inner.searcher();
        let tq = build_tantivy_query(query, &self.schema, &self.index)?;
        let tq = apply_hidden_filter(tq, opts, &self.schema);

        let limit = opts.limit.max(1);

        let hits = match opts.sort {
            SortBy::Score => {
                let top = searcher
                    .search(&tq, &TopDocs::with_limit(limit))
                    .map_err(|e| anyhow::anyhow!("search: {e}"))?;
                top.into_iter()
                    .filter_map(|(score, addr)| {
                        let doc: TantivyDocument = searcher.doc(addr).ok()?;
                        doc_to_hit(doc, &self.schema, score)
                    })
                    .collect()
            }

            SortBy::Size => {
                let collector = TopDocs::with_limit(limit).order_by_u64_field("size", Order::Desc);
                let top = searcher
                    .search(&tq, &collector)
                    .map_err(|e| anyhow::anyhow!("search by size: {e}"))?;
                top.into_iter()
                    .filter_map(|(_size, addr)| {
                        let doc: TantivyDocument = searcher.doc(addr).ok()?;
                        doc_to_hit(doc, &self.schema, 1.0)
                    })
                    .collect()
            }

            SortBy::Mtime => {
                let collector =
                    TopDocs::with_limit(limit).order_by_fast_field::<i64>("mtime", Order::Desc);
                let top = searcher
                    .search(&tq, &collector)
                    .map_err(|e| anyhow::anyhow!("search by mtime: {e}"))?;
                top.into_iter()
                    .filter_map(|(_mtime, addr)| {
                        let doc: TantivyDocument = searcher.doc(addr).ok()?;
                        doc_to_hit(doc, &self.schema, 1.0)
                    })
                    .collect()
            }

            SortBy::Name => {
                // Collect more candidates than requested so that the in-memory
                // name sort is reasonably representative. For very large result
                // sets this is an approximation.
                let fetch = (limit * 4).max(limit);
                let top = searcher
                    .search(&tq, &TopDocs::with_limit(fetch))
                    .map_err(|e| anyhow::anyhow!("search by name: {e}"))?;
                let mut hits: Vec<Hit> = top
                    .into_iter()
                    .filter_map(|(score, addr)| {
                        let doc: TantivyDocument = searcher.doc(addr).ok()?;
                        doc_to_hit(doc, &self.schema, score)
                    })
                    .collect();
                hits.sort_unstable_by(|a, b| a.name.cmp(&b.name));
                hits.truncate(limit);
                hits
            }
        };

        Ok(hits)
    }

    // -----------------------------------------------------------------------
    // Stats
    // -----------------------------------------------------------------------

    /// Return a snapshot of index statistics.
    ///
    /// The on-disk byte count is cached for 1 second to avoid repeated
    /// directory walks on hot-path callers.
    ///
    /// # Errors
    ///
    /// Returns an error if the index cannot be read.
    pub fn stats(&self) -> Result<IndexStats> {
        const TTL: Duration = Duration::from_secs(1);

        let mut cache = self.stats_cache.lock();

        // Return cached value if still fresh.
        if let (Some(at), Some(v)) = (cache.cached_at, cache.value) {
            if at.elapsed() < TTL {
                return Ok(v);
            }
        }

        let searcher = self.inner.searcher();
        let num_docs = searcher
            .segment_readers()
            .iter()
            .map(|r| u64::from(r.num_docs()))
            .sum();

        let on_disk_bytes = WalkDir::new(&self.index_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum();

        // `last_commit_unix` is not stored directly by tantivy in its public
        // API, so we return None for now. The daemon can track this itself.
        let last_commit_unix = None;

        let stats = IndexStats {
            num_docs,
            on_disk_bytes,
            last_commit_unix,
        };

        cache.cached_at = Some(Instant::now());
        cache.value = Some(stats);

        Ok(stats)
    }
}

// ---------------------------------------------------------------------------
// Query translation
// ---------------------------------------------------------------------------

/// Translate an [`IndexDoc`]-level [`Query`] into a boxed tantivy query.
fn build_tantivy_query(
    query: &Query,
    schema: &AtlasSchema,
    index: &Index,
) -> Result<Box<dyn TantivyQuery>> {
    let tq: Box<dyn TantivyQuery> = match query {
        // ----------------------------------------------------------------
        // NamePrefix — regex on name_lc field
        // ----------------------------------------------------------------
        Query::NamePrefix(prefix) => {
            let escaped = regex::escape(&prefix.to_lowercase());
            // FST regex is implicitly anchored; `<prefix>.*` = starts-with.
            let pattern = format!("{escaped}.*");
            let q = RegexQuery::from_pattern(&pattern, schema.name_lc)
                .map_err(|e| anyhow::anyhow!("NamePrefix regex: {e}"))?;
            Box::new(q)
        }

        // ----------------------------------------------------------------
        // NameSubstring — tokenize with ngram analyzer and AND terms
        // ----------------------------------------------------------------
        Query::NameSubstring(substr) => {
            let query_lc = substr.to_lowercase();
            let mut analyzer = index
                .tokenizers()
                .get(NGRAM_TOKENIZER_NAME)
                .ok_or_else(|| anyhow::anyhow!("ngram tokenizer not registered"))?;
            let mut stream = analyzer.token_stream(&query_lc);

            let mut terms: Vec<Term> = Vec::new();
            stream.process(&mut |token| {
                terms.push(Term::from_field_text(schema.name, &token.text));
            });

            if terms.is_empty() {
                // Query is shorter than min-gram; match nothing.
                return Ok(Box::new(tantivy::query::EmptyQuery));
            }

            let clauses: Vec<(Occur, Box<dyn TantivyQuery>)> = terms
                .into_iter()
                .map(|t| {
                    let tq: Box<dyn TantivyQuery> =
                        Box::new(TermQuery::new(t, IndexRecordOption::Basic));
                    (Occur::Must, tq)
                })
                .collect();

            Box::new(BooleanQuery::new(clauses))
        }

        // ----------------------------------------------------------------
        // NameFuzzy — FuzzyTermQuery on name_lc
        // ----------------------------------------------------------------
        Query::NameFuzzy { term, distance } => {
            let dist = (*distance).min(2);
            let t = Term::from_field_text(schema.name_lc, &term.to_lowercase());
            // `prefix = false`: the full term must be within edit distance.
            Box::new(FuzzyTermQuery::new(t, dist, false))
        }

        // ----------------------------------------------------------------
        // Extension — exact match on the extension field
        // ----------------------------------------------------------------
        Query::Extension(ext) => {
            let t = Term::from_field_text(schema.extension, &ext.to_lowercase());
            Box::new(TermQuery::new(t, IndexRecordOption::Basic))
        }

        // ----------------------------------------------------------------
        // ExactPath — exact match on the path field
        // ----------------------------------------------------------------
        Query::ExactPath(path) => {
            let t = Term::from_field_text(schema.path, &path.to_string_lossy());
            Box::new(TermQuery::new(t, IndexRecordOption::Basic))
        }

        // ----------------------------------------------------------------
        // All — BooleanQuery with Must clauses
        // ----------------------------------------------------------------
        Query::All(sub) => {
            if sub.is_empty() {
                return Ok(Box::new(tantivy::query::AllQuery));
            }
            let clauses: Vec<(Occur, Box<dyn TantivyQuery>)> = sub
                .iter()
                .map(|q| -> Result<_> { Ok((Occur::Must, build_tantivy_query(q, schema, index)?)) })
                .collect::<Result<_>>()?;
            Box::new(BooleanQuery::new(clauses))
        }

        // ----------------------------------------------------------------
        // Any — BooleanQuery with Should clauses
        // ----------------------------------------------------------------
        Query::Any(sub) => {
            if sub.is_empty() {
                return Ok(Box::new(tantivy::query::EmptyQuery));
            }
            let clauses: Vec<(Occur, Box<dyn TantivyQuery>)> = sub
                .iter()
                .map(|q| -> Result<_> {
                    Ok((Occur::Should, build_tantivy_query(q, schema, index)?))
                })
                .collect::<Result<_>>()?;
            Box::new(BooleanQuery::new(clauses))
        }

        // ----------------------------------------------------------------
        // InSubtree — regex on parent field
        // ----------------------------------------------------------------
        Query::InSubtree(root) => {
            let escaped = regex::escape(&root.to_string_lossy());
            // Matches the parent directory itself or any deeper path.
            let pattern = format!("{escaped}(/.*)?");
            let q = RegexQuery::from_pattern(&pattern, schema.parent)
                .map_err(|e| anyhow::anyhow!("InSubtree regex: {e}"))?;
            Box::new(q)
        }

        // ----------------------------------------------------------------
        // KindAnyOf — union of TermQuery on kind field
        // ----------------------------------------------------------------
        Query::KindAnyOf(kinds) => {
            if kinds.is_empty() {
                return Ok(Box::new(tantivy::query::EmptyQuery));
            }
            if kinds.len() == 1 {
                let t = Term::from_field_u64(schema.kind, kinds[0].as_u64());
                return Ok(Box::new(TermQuery::new(t, IndexRecordOption::Basic)));
            }
            let clauses: Vec<(Occur, Box<dyn TantivyQuery>)> = kinds
                .iter()
                .map(|k| {
                    let t = Term::from_field_u64(schema.kind, k.as_u64());
                    let tq: Box<dyn TantivyQuery> =
                        Box::new(TermQuery::new(t, IndexRecordOption::Basic));
                    (Occur::Should, tq)
                })
                .collect();
            Box::new(BooleanQuery::new(clauses))
        }
    };

    Ok(tq)
}

/// Optionally wrap `query` with a `is_hidden = 0` Must clause.
fn apply_hidden_filter(
    query: Box<dyn TantivyQuery>,
    opts: &SearchOptions,
    schema: &AtlasSchema,
) -> Box<dyn TantivyQuery> {
    if opts.include_hidden {
        return query;
    }
    let visible_term = Term::from_field_u64(schema.is_hidden, 0);
    let visible_filter: Box<dyn TantivyQuery> =
        Box::new(TermQuery::new(visible_term, IndexRecordOption::Basic));
    Box::new(BooleanQuery::new(vec![
        (Occur::Must, query),
        (Occur::Must, visible_filter),
    ]))
}

// ---------------------------------------------------------------------------
// Document → Hit conversion
// ---------------------------------------------------------------------------

fn doc_to_hit(doc: TantivyDocument, schema: &AtlasSchema, score: f32) -> Option<Hit> {
    let path_str = doc.get_first(schema.path)?.as_str()?.to_owned();
    let name = doc.get_first(schema.name)?.as_str()?.to_owned();
    let parent_str = doc.get_first(schema.parent)?.as_str()?.to_owned();
    let ext_str = doc.get_first(schema.extension)?.as_str()?.to_owned();
    let kind_u64 = doc.get_first(schema.kind)?.as_u64()?;
    let size = doc.get_first(schema.size)?.as_u64()?;
    let mtime_raw = doc.get_first(schema.mtime)?.as_i64()?;

    Some(Hit {
        path: PathBuf::from(path_str),
        name,
        parent: PathBuf::from(parent_str),
        extension: if ext_str.is_empty() {
            None
        } else {
            Some(ext_str)
        },
        kind: DocKind::from_u64(kind_u64),
        size,
        mtime: if mtime_raw == 0 {
            None
        } else {
            Some(mtime_raw)
        },
        score,
    })
}
