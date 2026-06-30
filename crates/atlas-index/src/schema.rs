//! Tantivy schema definition and field handle bundle for the Atlas index.
//!
//! [`AtlasSchema::build`] is the single authoritative place that defines every
//! field. All other modules (writer, reader, query builder) receive an
//! `Arc<AtlasSchema>` and use the pre-resolved [`tantivy::schema::Field`]
//! handles instead of looking fields up by name at runtime.

use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, Schema, SchemaBuilder, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{NgramTokenizer, TextAnalyzer};
use tantivy::Index;

/// The name under which the ngram(2,3) [`TextAnalyzer`] is registered on every
/// [`tantivy::Index`] this crate creates or opens.
pub const NGRAM_TOKENIZER_NAME: &str = "ngram_2_3";

/// All field handles for the Atlas tantivy schema, kept together so callers
/// never need to do a by-name field lookup.
///
/// Construct via [`AtlasSchema::build`]; the resulting value is cheap to clone
/// via `Arc`.
#[derive(Debug, Clone)]
pub struct AtlasSchema {
    /// The compiled tantivy schema. Needed when creating a new index.
    pub schema: Schema,
    /// Full absolute path — raw tokenizer, stored.
    pub path: Field,
    /// Last path segment — ngram(2,3) tokenizer, stored.
    pub name: Field,
    /// Lowercased last segment — raw tokenizer, **not** stored (query only).
    pub name_lc: Field,
    /// Parent directory — raw tokenizer, stored.
    pub parent: Field,
    /// Lowercased extension without leading dot — raw tokenizer, stored.
    pub extension: Field,
    /// Entry kind: 0=file, 1=dir, 2=symlink, 3=other — u64 FAST.
    pub kind: Field,
    /// File size in bytes — u64 FAST.
    pub size: Field,
    /// Last-modified time as Unix epoch seconds — i64 FAST.
    pub mtime: Field,
    /// Hidden flag: 0 = visible, 1 = hidden — u64 FAST.
    pub is_hidden: Field,
}

impl AtlasSchema {
    /// Build the schema and resolve all field handles.
    ///
    /// This **must** always add fields in the same order so that field indices
    /// remain stable across successive [`IndexWriter::open`] /
    /// [`IndexReader::open`] calls on the same on-disk index.
    ///
    /// [`IndexWriter::open`]: crate::IndexWriter::open
    /// [`IndexReader::open`]: crate::IndexReader::open
    #[must_use]
    pub fn build() -> Self {
        let mut b = SchemaBuilder::default();

        // --- text fields --------------------------------------------------

        let raw_text = |stored: bool| {
            let opts = TextOptions::default().set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("raw")
                    .set_index_option(IndexRecordOption::Basic),
            );
            if stored {
                opts.set_stored()
            } else {
                opts
            }
        };

        let ngram_text = TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer(NGRAM_TOKENIZER_NAME)
                    .set_index_option(IndexRecordOption::WithFreqs),
            )
            .set_stored();

        let path = b.add_text_field("path", raw_text(true));
        let name = b.add_text_field("name", ngram_text);
        let name_lc = b.add_text_field("name_lc", raw_text(false));
        let parent = b.add_text_field("parent", raw_text(true));
        let extension = b.add_text_field("extension", raw_text(true));

        // --- numeric fields -----------------------------------------------

        let u64_fast = || -> NumericOptions {
            NumericOptions::default()
                .set_fast()
                .set_stored()
                .set_indexed()
        };

        let kind = b.add_u64_field("kind", u64_fast());
        let size = b.add_u64_field("size", u64_fast());
        let mtime = b.add_i64_field("mtime", u64_fast());
        let is_hidden = b.add_u64_field("is_hidden", u64_fast());

        Self {
            schema: b.build(),
            path,
            name,
            name_lc,
            parent,
            extension,
            kind,
            size,
            mtime,
            is_hidden,
        }
    }
}

/// Register the custom ngram tokenizer on `index`.
///
/// Must be called after every `Index::open_or_create` or `Index::open_in_dir`
/// because tantivy does not persist tokenizer configurations on disk — only the
/// schema (field names + types) is stored.
pub(crate) fn register_tokenizers(index: &Index) {
    let ngram =
        TextAnalyzer::builder(NgramTokenizer::new(2, 3, false).expect("valid ngram params"))
            .build();
    index.tokenizers().register(NGRAM_TOKENIZER_NAME, ngram);
}
