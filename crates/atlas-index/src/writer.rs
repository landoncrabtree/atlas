//! Incremental writer for the Atlas index.
//!
//! [`IndexWriter`] wraps a [`tantivy::IndexWriter`] and exposes the minimal
//! surface needed by the daemon and the app: upsert, remove, and commit.
//!
//! # Thread-safety
//!
//! `IndexWriter` is `Send + Sync`. The inner tantivy writer uses a background
//! thread and a channel for `add_document` / `delete_*`, so concurrent upserts
//! from multiple threads are fine. `commit` requires `&mut self` so only one
//! caller commits at a time.

use std::path::Path;
use std::sync::Arc;

use tantivy::directory::MmapDirectory;
use tantivy::query::RegexQuery;
use tantivy::schema::Term;
use tantivy::{Index, TantivyDocument};

use atlas_core::Result;

use crate::doc::IndexDoc;
use crate::schema::{register_tokenizers, AtlasSchema};

/// Wraps a [`tantivy::IndexWriter`] with Atlas-specific helpers.
///
/// Obtain via [`IndexWriter::open`].
pub struct IndexWriter {
    inner: tantivy::IndexWriter<TantivyDocument>,
    schema: Arc<AtlasSchema>,
}

// Safety: tantivy::IndexWriter<D> is Send (it uses a channel internally).
// The &self methods (add_document, delete_term, delete_query) are also Sync.
// We expose commit as &mut self, mirroring tantivy.
unsafe impl Sync for IndexWriter {}

impl IndexWriter {
    /// Open (or create) an on-disk index at `dir`.
    ///
    /// * `dir` is created with `create_dir_all` if it does not exist.
    /// * `memory_budget_mb` is the indexing heap in megabytes. The minimum
    ///   enforced by tantivy is 15 MB; 64–256 MB is typical for a daemon.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created or opened, or if
    /// the schema on disk is incompatible with the current schema definition.
    pub fn open(dir: &Path, memory_budget_mb: usize) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .map_err(|e| atlas_core::AtlasError::io(dir.to_path_buf(), e))?;

        let schema_desc = AtlasSchema::build();
        let mmap = MmapDirectory::open(dir)
            .map_err(|e| anyhow::anyhow!("open index dir {:?}: {e}", dir))?;

        let index = Index::builder()
            .schema(schema_desc.schema.clone())
            .open_or_create(mmap)
            .map_err(|e| anyhow::anyhow!("open_or_create index: {e}"))?;

        register_tokenizers(&index);

        let budget = memory_budget_mb.max(15) * 1_024 * 1_024;
        let inner: tantivy::IndexWriter<TantivyDocument> = index
            .writer(budget)
            .map_err(|e| anyhow::anyhow!("create writer: {e}"))?;

        Ok(Self {
            inner,
            schema: Arc::new(schema_desc),
        })
    }

    /// Insert or replace a document identified by its absolute `path`.
    ///
    /// Any existing document with the same `path` value is deleted before the
    /// new one is added (tantivy's delete + add within the same batch is
    /// atomic from the reader's perspective after the next commit).
    ///
    /// # Errors
    ///
    /// Propagates tantivy errors from the background indexing channel.
    pub fn upsert(&self, doc: &IndexDoc) -> Result<()> {
        let path_term = Term::from_field_text(self.schema.path, &doc.path.to_string_lossy());
        self.inner.delete_term(path_term);
        self.inner
            .add_document(doc.to_tantivy_doc(&self.schema))
            .map_err(|e| anyhow::anyhow!("add_document: {e}"))?;
        Ok(())
    }

    /// Remove the document with the exact given `path`.
    ///
    /// No-op if the path is not in the index.
    pub fn remove_path(&self, path: &Path) -> Result<()> {
        let term = Term::from_field_text(self.schema.path, &path.to_string_lossy());
        self.inner.delete_term(term);
        Ok(())
    }

    /// Remove all documents whose `parent` path equals or is a descendant of
    /// `root`.
    ///
    /// This is used when a directory is deleted or unindexed: a single regex
    /// query on the `parent` field removes all affected entries in one batch.
    ///
    /// # Pattern
    ///
    /// The tantivy FST regex engine implicitly anchors patterns, so the
    /// pattern `<escaped_root>(/.*)?` matches exactly:
    ///
    /// * `root` itself (e.g. `/home/user/docs`)
    /// * any descendant (e.g. `/home/user/docs/sub/dir`)
    ///
    /// # Errors
    ///
    /// Returns an error if the regex cannot be compiled (should not happen
    /// for valid paths) or if tantivy rejects the query.
    pub fn remove_subtree(&self, root: &Path) -> Result<()> {
        let escaped = regex::escape(&root.to_string_lossy());
        // The FST regex is implicitly anchored start-to-end, so this matches
        // the root itself or any path that continues with '/'.
        let pattern = format!("{escaped}(/.*)?");
        let q = RegexQuery::from_pattern(&pattern, self.schema.parent)
            .map_err(|e| anyhow::anyhow!("build subtree regex: {e}"))?;
        self.inner
            .delete_query(Box::new(q))
            .map_err(|e| anyhow::anyhow!("delete_query: {e}"))?;
        Ok(())
    }

    /// Commit all pending changes to disk.
    ///
    /// Safe to call at high frequency — tantivy batches all operations
    /// accumulated since the previous commit into a single segment merge.
    ///
    /// # Errors
    ///
    /// Propagates tantivy flush/merge errors.
    pub fn commit(&mut self) -> Result<()> {
        self.inner
            .commit()
            .map_err(|e| anyhow::anyhow!("commit: {e}"))?;
        Ok(())
    }
}
