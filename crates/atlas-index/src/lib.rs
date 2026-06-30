//! Tantivy-backed file-path/filename index library.
//!
//! `atlas-index` provides a persistent, on-disk search index backed by
//! [tantivy](https://docs.rs/tantivy). It is shared between the `atlas-indexd`
//! daemon (large, persistent indexes) and the embedded fallback inside the
//! application (ad-hoc, smaller indexes).
//!
//! # Overview
//!
//! ```text
//!  ┌─────────────┐   IndexDoc   ┌──────────────┐
//!  │  Caller     │ ──────────▶  │  IndexWriter │  (writer.rs)
//!  │  (daemon /  │              └──────────────┘
//!  │   app)      │   Query      ┌──────────────┐
//!  │             │ ──────────▶  │  IndexReader │  (reader.rs)
//!  └─────────────┘              └──────────────┘
//! ```
//!
//! # Quick start
//!
//! ```no_run
//! use atlas_index::{IndexDoc, IndexWriter, IndexReader, Query, SearchOptions, SortBy, DocKind};
//! use std::path::PathBuf;
//!
//! # fn main() -> atlas_core::Result<()> {
//! let dir = PathBuf::from("/var/cache/atlas/index");
//!
//! // --- write ---
//! let mut writer = IndexWriter::open(&dir, 64)?;
//! writer.upsert(&IndexDoc {
//!     path: PathBuf::from("/home/user/main.rs"),
//!     name: "main.rs".into(),
//!     parent: PathBuf::from("/home/user"),
//!     extension: Some("rs".into()),
//!     kind: DocKind::File,
//!     size: 1024,
//!     mtime: Some(1_700_000_000),
//!     is_hidden: false,
//! })?;
//! writer.commit()?;
//!
//! // --- read ---
//! let reader = IndexReader::open(&dir)?;
//! let hits = reader.search(
//!     &Query::NamePrefix("ma".into()),
//!     &SearchOptions { limit: 10, include_hidden: true, sort: SortBy::Score },
//! )?;
//! assert!(!hits.is_empty());
//! # Ok(())
//! # }
//! ```

pub mod doc;
pub mod query;
pub mod reader;
pub mod schema;
pub mod stats;
pub mod writer;

pub use doc::{DocKind, IndexDoc};
pub use query::{Hit, Query, SearchOptions, SortBy};
pub use reader::IndexReader;
pub use schema::AtlasSchema;
pub use stats::IndexStats;
pub use writer::IndexWriter;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn tmpdir() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[allow(clippy::too_many_arguments)]
    fn make_doc(
        path: &str,
        name: &str,
        parent: &str,
        ext: Option<&str>,
        kind: DocKind,
        size: u64,
        mtime: Option<i64>,
        hidden: bool,
    ) -> IndexDoc {
        IndexDoc {
            path: PathBuf::from(path),
            name: name.to_string(),
            parent: PathBuf::from(parent),
            extension: ext.map(str::to_string),
            kind,
            size,
            mtime,
            is_hidden: hidden,
        }
    }

    /// Write `docs` to a new index at `dir`, commit, then open a reader.
    fn write_and_open(dir: &TempDir, docs: &[IndexDoc]) -> (IndexWriter, IndexReader) {
        let mut w = IndexWriter::open(dir.path(), 15).expect("writer");
        for doc in docs {
            w.upsert(doc).expect("upsert");
        }
        w.commit().expect("commit");
        let r = IndexReader::open(dir.path()).expect("reader");
        r.reload().expect("reload");
        (w, r)
    }

    fn opts(limit: usize, hidden: bool) -> SearchOptions {
        SearchOptions {
            limit,
            include_hidden: hidden,
            sort: SortBy::Score,
        }
    }

    // -----------------------------------------------------------------------
    // 1. Schema round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn round_trip_single_doc() {
        let dir = tmpdir();
        let doc = make_doc(
            "/home/user/main.rs",
            "main.rs",
            "/home/user",
            Some("rs"),
            DocKind::File,
            1024,
            Some(1_700_000_000),
            false,
        );
        let (_w, r) = write_and_open(&dir, &[doc]);

        let hits = r
            .search(
                &Query::ExactPath(PathBuf::from("/home/user/main.rs")),
                &opts(1, true),
            )
            .expect("search");
        assert_eq!(hits.len(), 1);
        let h = &hits[0];
        assert_eq!(h.name, "main.rs");
        assert_eq!(h.extension.as_deref(), Some("rs"));
        assert_eq!(h.size, 1024);
        assert_eq!(h.mtime, Some(1_700_000_000));
        assert!(matches!(h.kind, DocKind::File));
    }

    // -----------------------------------------------------------------------
    // 2. NamePrefix
    // -----------------------------------------------------------------------

    #[test]
    fn name_prefix_matches() {
        let dir = tmpdir();
        let docs = vec![
            make_doc(
                "/p/main.rs",
                "main.rs",
                "/p",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/p/map.go",
                "map.go",
                "/p",
                Some("go"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/p/other.py",
                "other.py",
                "/p",
                Some("py"),
                DocKind::File,
                1,
                None,
                false,
            ),
        ];
        let (_w, r) = write_and_open(&dir, &docs);

        let hits = r
            .search(&Query::NamePrefix("ma".into()), &opts(10, true))
            .expect("search");
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"main.rs"), "expected main.rs in {names:?}");
        assert!(names.contains(&"map.go"), "expected map.go in {names:?}");
        assert!(
            !names.contains(&"other.py"),
            "other.py should not match ma*"
        );
    }

    // -----------------------------------------------------------------------
    // 3. NameSubstring
    // -----------------------------------------------------------------------

    #[test]
    fn name_substring_matches() {
        let dir = tmpdir();
        let docs = vec![
            make_doc(
                "/p/marker.rs",
                "marker.rs",
                "/p",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/p/parser.go",
                "parser.go",
                "/p",
                Some("go"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/p/library.py",
                "library.py",
                "/p",
                Some("py"),
                DocKind::File,
                1,
                None,
                false,
            ),
            // "ar" appears in marker, parser, library
            make_doc(
                "/p/main.rs",
                "main.rs",
                "/p",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
        ];
        let (_w, r) = write_and_open(&dir, &docs);

        let hits = r
            .search(&Query::NameSubstring("ar".into()), &opts(20, true))
            .expect("search");
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert!(
            names.contains(&"marker.rs"),
            "expected marker.rs in {names:?}"
        );
        assert!(
            names.contains(&"parser.go"),
            "expected parser.go in {names:?}"
        );
        assert!(
            names.contains(&"library.py"),
            "expected library.py in {names:?}"
        );
        assert!(!names.contains(&"main.rs"), "main.rs should not match 'ar'");
    }

    // -----------------------------------------------------------------------
    // 4. NameFuzzy
    // -----------------------------------------------------------------------

    #[test]
    fn name_fuzzy_distance_1() {
        let dir = tmpdir();
        let docs = vec![
            make_doc(
                "/p/text.rs",
                "text.rs",
                "/p",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/p/toast.rs",
                "toast.rs",
                "/p",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
        ];
        let (_w, r) = write_and_open(&dir, &docs);

        // "textt" is 1 edit away from "text"
        let q = Query::NameFuzzy {
            term: "text.rs".into(),
            distance: 1,
        };
        let hits = r.search(&q, &opts(10, true)).expect("search");
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"text.rs"), "expected text.rs in {names:?}");
    }

    // -----------------------------------------------------------------------
    // 5. Extension filter
    // -----------------------------------------------------------------------

    #[test]
    fn extension_filter() {
        let dir = tmpdir();
        let docs = vec![
            make_doc(
                "/p/main.rs",
                "main.rs",
                "/p",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/p/lib.rs",
                "lib.rs",
                "/p",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/p/build.go",
                "build.go",
                "/p",
                Some("go"),
                DocKind::File,
                1,
                None,
                false,
            ),
        ];
        let (_w, r) = write_and_open(&dir, &docs);

        let hits = r
            .search(&Query::Extension("rs".into()), &opts(10, true))
            .expect("search");
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.extension.as_deref() == Some("rs")));
    }

    // -----------------------------------------------------------------------
    // 6. InSubtree
    // -----------------------------------------------------------------------

    #[test]
    fn in_subtree_restricts() {
        let dir = tmpdir();
        let docs = vec![
            make_doc(
                "/root/a/foo.rs",
                "foo.rs",
                "/root/a",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/root/a/bar.rs",
                "bar.rs",
                "/root/a",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/root/b/baz.rs",
                "baz.rs",
                "/root/b",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/other/qux.rs",
                "qux.rs",
                "/other",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
        ];
        let (_w, r) = write_and_open(&dir, &docs);

        let hits = r
            .search(&Query::InSubtree(PathBuf::from("/root/a")), &opts(20, true))
            .expect("search");
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"foo.rs"));
        assert!(names.contains(&"bar.rs"));
        assert!(
            !names.contains(&"baz.rs"),
            "baz.rs is under /root/b, not /root/a"
        );
        assert!(!names.contains(&"qux.rs"));
    }

    // -----------------------------------------------------------------------
    // 7. KindAnyOf
    // -----------------------------------------------------------------------

    #[test]
    fn kind_any_of_restricts() {
        let dir = tmpdir();
        let docs = vec![
            make_doc("/p/src", "src", "/p", None, DocKind::Dir, 0, None, false),
            make_doc(
                "/p/main.rs",
                "main.rs",
                "/p",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/p/link",
                "link",
                "/p",
                None,
                DocKind::Symlink,
                0,
                None,
                false,
            ),
        ];
        let (_w, r) = write_and_open(&dir, &docs);

        let hits = r
            .search(
                &Query::KindAnyOf(vec![DocKind::Dir, DocKind::Symlink]),
                &opts(10, true),
            )
            .expect("search");
        assert!(hits
            .iter()
            .all(|h| matches!(h.kind, DocKind::Dir | DocKind::Symlink)));
        assert!(!hits.iter().any(|h| matches!(h.kind, DocKind::File)));
    }

    // -----------------------------------------------------------------------
    // 8. All / Any combinators
    // -----------------------------------------------------------------------

    #[test]
    fn combined_all_and_any() {
        let dir = tmpdir();
        let docs = vec![
            make_doc(
                "/p/main.rs",
                "main.rs",
                "/p",
                Some("rs"),
                DocKind::File,
                10,
                None,
                false,
            ),
            make_doc(
                "/p/lib.rs",
                "lib.rs",
                "/p",
                Some("rs"),
                DocKind::File,
                20,
                None,
                false,
            ),
            make_doc(
                "/p/build.go",
                "build.go",
                "/p",
                Some("go"),
                DocKind::File,
                5,
                None,
                false,
            ),
        ];
        let (_w, r) = write_and_open(&dir, &docs);

        // Any(rs OR go) AND InSubtree(/p)
        let q = Query::All(vec![
            Query::Any(vec![
                Query::Extension("rs".into()),
                Query::Extension("go".into()),
            ]),
            Query::InSubtree(PathBuf::from("/p")),
        ]);
        let hits = r.search(&q, &opts(10, true)).expect("search");
        assert_eq!(hits.len(), 3, "all three files should match");
    }

    #[test]
    fn combined_any_returns_union() {
        let dir = tmpdir();
        let docs = vec![
            make_doc(
                "/p/main.rs",
                "main.rs",
                "/p",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/p/build.go",
                "build.go",
                "/p",
                Some("go"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/p/config.toml",
                "config.toml",
                "/p",
                Some("toml"),
                DocKind::File,
                1,
                None,
                false,
            ),
        ];
        let (_w, r) = write_and_open(&dir, &docs);

        let q = Query::Any(vec![
            Query::Extension("rs".into()),
            Query::Extension("go".into()),
        ]);
        let hits = r.search(&q, &opts(10, true)).expect("search");
        assert_eq!(hits.len(), 2);
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"main.rs"));
        assert!(names.contains(&"build.go"));
    }

    // -----------------------------------------------------------------------
    // 9. remove_subtree
    // -----------------------------------------------------------------------

    #[test]
    fn remove_subtree_deletes_descendants() {
        let dir = tmpdir();
        let docs = vec![
            make_doc(
                "/root/sub/a.rs",
                "a.rs",
                "/root/sub",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/root/sub/b.rs",
                "b.rs",
                "/root/sub",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
            make_doc(
                "/root/keep.rs",
                "keep.rs",
                "/root",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
        ];
        let (mut w, _) = write_and_open(&dir, &docs);

        w.remove_subtree(PathBuf::from("/root/sub").as_path())
            .expect("remove_subtree");
        w.commit().expect("commit after remove");

        let r = IndexReader::open(dir.path()).expect("reader 2");
        r.reload().expect("reload");

        let hits = r
            .search(&Query::Extension("rs".into()), &opts(20, true))
            .expect("search");
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert!(!names.contains(&"a.rs"), "a.rs should be removed");
        assert!(!names.contains(&"b.rs"), "b.rs should be removed");
        assert!(names.contains(&"keep.rs"), "keep.rs should survive");
    }

    // -----------------------------------------------------------------------
    // 10. stats
    // -----------------------------------------------------------------------

    #[test]
    fn stats_doc_count_increases() {
        let dir = tmpdir();
        let doc1 = make_doc(
            "/p/a.rs",
            "a.rs",
            "/p",
            Some("rs"),
            DocKind::File,
            1,
            None,
            false,
        );
        let doc2 = make_doc(
            "/p/b.rs",
            "b.rs",
            "/p",
            Some("rs"),
            DocKind::File,
            2,
            None,
            false,
        );

        let mut w = IndexWriter::open(dir.path(), 15).expect("writer");
        w.upsert(&doc1).expect("upsert 1");
        w.commit().expect("commit 1");

        let r = IndexReader::open(dir.path()).expect("reader");
        r.reload().expect("reload 1");
        let stats1 = r.stats().expect("stats 1");
        assert_eq!(stats1.num_docs, 1);
        assert!(stats1.on_disk_bytes > 0, "on_disk_bytes should be > 0");

        w.upsert(&doc2).expect("upsert 2");
        w.commit().expect("commit 2");
        r.reload().expect("reload 2");

        // Force cache invalidation by waiting a moment.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let stats2 = r.stats().expect("stats 2");
        assert_eq!(stats2.num_docs, 2);
    }

    // -----------------------------------------------------------------------
    // 11. Hidden filter
    // -----------------------------------------------------------------------

    #[test]
    fn hidden_filter_excludes_hidden_files() {
        let dir = tmpdir();
        let docs = vec![
            make_doc(
                "/p/.hidden.rs",
                ".hidden.rs",
                "/p",
                Some("rs"),
                DocKind::File,
                1,
                None,
                true,
            ),
            make_doc(
                "/p/visible.rs",
                "visible.rs",
                "/p",
                Some("rs"),
                DocKind::File,
                1,
                None,
                false,
            ),
        ];
        let (_w, r) = write_and_open(&dir, &docs);

        let without_hidden = r
            .search(&Query::Extension("rs".into()), &opts(10, false))
            .expect("without hidden");
        assert_eq!(without_hidden.len(), 1);
        assert_eq!(without_hidden[0].name, "visible.rs");

        let with_hidden = r
            .search(&Query::Extension("rs".into()), &opts(10, true))
            .expect("with hidden");
        assert_eq!(with_hidden.len(), 2);
    }
}
