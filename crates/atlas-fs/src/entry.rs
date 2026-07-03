//! Filesystem entry types: [`Entry`], [`EntryKind`], and [`Metadata`].
//!
//! These types are the lightweight, cheap-to-clone representation of a single
//! filesystem object that flows through the lister, walker, and view models.

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A single filesystem entry: a file, directory, symlink, or other special node.
///
/// `Entry` is intentionally cheap to clone — it owns a `PathBuf`, a `String`
/// name, and small `Copy`/`Option` metadata, so cloning is a couple of heap
/// allocations at most.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Full path to the entry.
    pub path: PathBuf,
    /// Final path segment (file name), lossily converted from the OS string.
    pub name: String,
    /// What kind of filesystem object this entry represents.
    pub kind: EntryKind,
    /// Cached metadata for the entry.
    pub metadata: Metadata,
}

/// The kind of filesystem object an [`Entry`] represents.
#[derive(Debug, Clone)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link, with its resolved target (if readable) and whether it
    /// is broken (the target does not exist).
    Symlink {
        /// The link target, if it could be read.
        target: Option<PathBuf>,
        /// `true` when the link points at a non-existent path.
        broken: bool,
    },
    /// Any other node: sockets, FIFOs, block/character devices, etc.
    Other,
}

impl EntryKind {
    /// Returns `true` when this entry is a directory.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        matches!(self, EntryKind::Dir)
    }

    /// Returns `true` when this entry is a regular file.
    #[must_use]
    pub fn is_file(&self) -> bool {
        matches!(self, EntryKind::File)
    }

    /// Returns `true` when this entry is a symbolic link.
    #[must_use]
    pub fn is_symlink(&self) -> bool {
        matches!(self, EntryKind::Symlink { .. })
    }

    /// A stable ordering rank used by kind-based sorting (directories first).
    #[must_use]
    pub(crate) fn sort_rank(&self) -> u8 {
        match self {
            EntryKind::Dir => 0,
            EntryKind::File => 1,
            EntryKind::Symlink { .. } => 2,
            EntryKind::Other => 3,
        }
    }
}

/// Cached metadata for an [`Entry`].
///
/// Times and permissions are optional because not all platforms (or
/// filesystems) expose them. `size` is `0` for directories unless explicitly
/// computed — directory size is not computed by default.
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    /// Size in bytes. `0` for directories unless computed.
    pub size: u64,
    /// Last modification time, if available.
    pub modified: Option<SystemTime>,
    /// Creation time, if available.
    pub created: Option<SystemTime>,
    /// Last access time, if available.
    pub accessed: Option<SystemTime>,
    /// Unix permission mode bits, when available.
    pub permissions_mode: Option<u32>,
    /// Whether the entry is hidden (leading `.` on unix, the hidden attribute
    /// on windows).
    pub is_hidden: bool,
}

impl Metadata {
    /// Build [`Metadata`] from `std::fs::Metadata` and the entry name.
    ///
    /// `name` is used for unix-style hidden detection (leading `.`).
    #[must_use]
    pub fn from_std(meta: &fs::Metadata, name: &str) -> Self {
        let size = if meta.is_dir() { 0 } else { meta.len() };
        Self {
            size,
            modified: meta.modified().ok(),
            created: meta.created().ok(),
            accessed: meta.accessed().ok(),
            permissions_mode: permissions_mode(meta),
            is_hidden: is_hidden(name, meta),
        }
    }
}

impl Entry {
    /// Extract the lossy file-name string for a path.
    #[must_use]
    pub fn name_of(path: &Path) -> String {
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    }

    /// The lowercased ASCII extension of the entry's name, if any.
    ///
    /// Returns a [`Cow`] — borrowed when the extension is already all
    /// lower-case ASCII (the overwhelming common case: `.rs`, `.png`,
    /// `.txt`) and owned only when a case fold is actually needed
    /// (e.g. `.PNG`, `.JPEG`). This matters because this function is
    /// called twice per comparison in the `SortKey::Extension`
    /// comparator; the previous `Option<String>` return allocated
    /// unconditionally, and sorting 10k entries would allocate ~260 k
    /// short strings just to compare.
    ///
    /// Non-ASCII extensions fall back to the full-Unicode
    /// `to_ascii_lowercase` path (still allocating), which is the
    /// desired semantic — non-ASCII bytes are left alone.
    #[must_use]
    pub fn extension(&self) -> Option<Cow<'_, str>> {
        let ext = Path::new(&self.name).extension()?;
        // `OsStr::to_str` returns `Some` for the valid-UTF-8 case,
        // which every extension we sort by in practice is.
        let s = ext.to_str()?;
        if s.bytes().all(|b| !b.is_ascii_uppercase()) {
            Some(Cow::Borrowed(s))
        } else {
            Some(Cow::Owned(s.to_ascii_lowercase()))
        }
    }
}

#[cfg(unix)]
fn permissions_mode(meta: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.mode())
}

#[cfg(not(unix))]
fn permissions_mode(_meta: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn is_hidden(name: &str, _meta: &fs::Metadata) -> bool {
    name.starts_with('.')
}

#[cfg(windows)]
fn is_hidden(_name: &str, meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(any(unix, windows)))]
fn is_hidden(name: &str, _meta: &fs::Metadata) -> bool {
    name.starts_with('.')
}

/// Build an [`Entry`] for `path`, classifying its kind and gathering metadata.
///
/// `path` is `lstat`-ed first so symlinks are detected as such. When
/// `follow_symlinks` is `true`, metadata for symlinks is taken from the link
/// target (falling back to the link itself when the target is unreadable);
/// otherwise the link's own metadata is used.
///
/// # Errors
///
/// Returns an error if the entry cannot be `lstat`-ed.
pub(crate) fn build_entry(path: PathBuf, follow_symlinks: bool) -> atlas_core::Result<Entry> {
    let name = Entry::name_of(&path);
    let lmeta = fs::symlink_metadata(&path)
        .map_err(|e| atlas_core::AtlasError::io(Some(path.clone()), e))?;

    if lmeta.file_type().is_symlink() {
        let target = fs::read_link(&path).ok();
        // `fs::metadata` follows the link; if it errors, the link is broken.
        let resolved = fs::metadata(&path);
        let broken = resolved.is_err();
        let meta_src = match (follow_symlinks, &resolved) {
            (true, Ok(m)) => m,
            _ => &lmeta,
        };
        let metadata = Metadata::from_std(meta_src, &name);
        return Ok(Entry {
            kind: EntryKind::Symlink { target, broken },
            name,
            path,
            metadata,
        });
    }

    let ft = lmeta.file_type();
    let kind = if ft.is_dir() {
        EntryKind::Dir
    } else if ft.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    let metadata = Metadata::from_std(&lmeta, &name);
    Ok(Entry {
        kind,
        name,
        path,
        metadata,
    })
}
