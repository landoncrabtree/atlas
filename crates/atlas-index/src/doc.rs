//! Input document type for the Atlas index.
//!
//! [`IndexDoc`] is the caller-facing input struct. It is intentionally
//! decoupled from `atlas-fs` types so that both the daemon and the embedded
//! app can construct it however is most convenient (from `atlas_fs::Entry`, a
//! raw `std::fs::Metadata`, or constructed synthetically in tests).
//!
//! # Example
//!
//! ```
//! use atlas_index::{IndexDoc, DocKind};
//! use std::path::PathBuf;
//!
//! let doc = IndexDoc {
//!     path:      PathBuf::from("/home/user/main.rs"),
//!     name:      "main.rs".into(),
//!     parent:    PathBuf::from("/home/user"),
//!     extension: Some("rs".into()),
//!     kind:      DocKind::File,
//!     size:      4096,
//!     mtime:     Some(1_700_000_000),
//!     is_hidden: false,
//! };
//! ```

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use tantivy::schema::Field;
use tantivy::TantivyDocument;

use crate::schema::AtlasSchema;

/// The kind of filesystem entry being indexed.
///
/// Mirrors `atlas_fs::EntryKind` but collapses the symlink target detail —
/// the index only needs to know it *is* a symlink, not where it points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link.
    Symlink,
    /// Any other node (socket, FIFO, block/char device, …).
    Other,
}

impl DocKind {
    /// The discriminant stored in the `kind` fast field.
    #[must_use]
    pub(crate) fn as_u64(self) -> u64 {
        match self {
            Self::File => 0,
            Self::Dir => 1,
            Self::Symlink => 2,
            Self::Other => 3,
        }
    }

    /// Reconstruct from the stored discriminant; falls back to [`DocKind::Other`]
    /// for unknown values.
    #[must_use]
    pub(crate) fn from_u64(v: u64) -> Self {
        match v {
            0 => Self::File,
            1 => Self::Dir,
            2 => Self::Symlink,
            _ => Self::Other,
        }
    }
}

/// A single document to be inserted into (or removed from) the Atlas index.
///
/// Build via the provided constructor or fill fields directly.
#[derive(Debug, Clone)]
pub struct IndexDoc {
    /// Full, absolute path of the entry.
    pub path: PathBuf,
    /// Last path segment (file / directory name).
    pub name: String,
    /// Parent directory path.
    pub parent: PathBuf,
    /// Lowercased extension without a leading dot, e.g. `"rs"` for `main.rs`.
    /// `None` when the entry has no extension.
    pub extension: Option<String>,
    /// Entry kind.
    pub kind: DocKind,
    /// Size in bytes (`0` for directories).
    pub size: u64,
    /// Last-modified time as Unix epoch seconds. `None` becomes `0` in the index.
    pub mtime: Option<i64>,
    /// Whether the entry is hidden (dot-prefix on Unix; hidden attribute on Windows).
    pub is_hidden: bool,
}

impl IndexDoc {
    /// Build an [`IndexDoc`] from a path and its [`std::fs::Metadata`].
    ///
    /// The metadata is read with `lstat` semantics (symlinks are *not*
    /// followed automatically here — if you need the target metadata you must
    /// stat the target yourself before calling this).
    ///
    /// # Notes
    ///
    /// * Hidden detection is `name.starts_with('.')` on non-Windows, and uses
    ///   `FILE_ATTRIBUTE_HIDDEN` on Windows.
    /// * Directory size is reported as `0`; the caller may substitute an actual
    ///   recursive size if desired.
    #[must_use]
    pub fn from_path_and_metadata(path: PathBuf, meta: &std::fs::Metadata) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());

        let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();

        let extension = Path::new(&name)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());

        let kind = if meta.is_symlink() {
            DocKind::Symlink
        } else if meta.is_dir() {
            DocKind::Dir
        } else if meta.is_file() {
            DocKind::File
        } else {
            DocKind::Other
        };

        let size = if meta.is_dir() { 0 } else { meta.len() };

        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);

        Self {
            path,
            name: name.clone(),
            parent,
            extension,
            kind,
            size,
            mtime,
            is_hidden: is_hidden_name(&name, meta),
        }
    }

    /// Convert to a [`TantivyDocument`] using the provided schema field handles.
    pub(crate) fn to_tantivy_doc(&self, s: &AtlasSchema) -> TantivyDocument {
        fn add_text(doc: &mut TantivyDocument, field: Field, value: &str) {
            doc.add_text(field, value);
        }

        let mut doc = TantivyDocument::new();
        add_text(&mut doc, s.path, &self.path.to_string_lossy());
        add_text(&mut doc, s.name, &self.name);
        add_text(&mut doc, s.name_lc, &self.name.to_lowercase());
        add_text(&mut doc, s.parent, &self.parent.to_string_lossy());
        add_text(
            &mut doc,
            s.extension,
            self.extension.as_deref().unwrap_or(""),
        );
        doc.add_u64(s.kind, self.kind.as_u64());
        doc.add_u64(s.size, self.size);
        doc.add_i64(s.mtime, self.mtime.unwrap_or(0));
        doc.add_u64(s.is_hidden, u64::from(self.is_hidden));
        doc
    }
}

// ---------------------------------------------------------------------------
// Platform-specific hidden detection
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn is_hidden_name(name: &str, _meta: &std::fs::Metadata) -> bool {
    name.starts_with('.')
}

#[cfg(windows)]
fn is_hidden_name(_name: &str, meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(any(unix, windows)))]
fn is_hidden_name(name: &str, _meta: &std::fs::Metadata) -> bool {
    name.starts_with('.')
}
