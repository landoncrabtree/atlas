//! [`RenameSession`] + [`stem_range`] — pure state, no UI, no I/O.
//!
//! The session captures the target [`atlas_core::Location`], the
//! original entry name (kept immutable so we can compute the "did the
//! user actually change anything?" gate) and the current buffer. The
//! [`stem_range`] helper computes the UTF-8 byte range that a fresh
//! rename should pre-select on open — Finder-parity behaviour.

use atlas_core::Location;

use crate::PaneId;

/// A single in-flight rename attempt.
///
/// One-at-a-time per shell: the controller in
/// `crate::rename_inline::controller` guarantees no overlapping
/// sessions by dropping any previous session on `open()`. The struct
/// is small and cheap to clone (a `Location` + a couple of `String`s);
/// the controller keeps it under a `parking_lot::RwLock` so read-heavy
/// Slint pushers can borrow it without contention.
#[derive(Debug, Clone)]
pub struct RenameSession {
    /// The entry we're renaming. Owned so the pane can navigate away
    /// without invalidating the session; the commit path resubmits
    /// this exact location to `OpsController::submit_rename`.
    pub target: Location,
    /// Immutable original name — never mutated. Used to gate the
    /// commit path (`buffer == original_name` ⇒ silent no-op) and to
    /// compute the dirty flag for validation gating.
    pub original_name: String,
    /// Whether the entry is a directory. Directories are stem-selected
    /// across their whole name (there's no extension convention) —
    /// see [`stem_range`].
    pub is_dir: bool,
    /// Working buffer — bound `<=>` to the `InlineRenameCell` in the
    /// view; the controller reads it back on submit.
    pub buffer: String,
    /// Pane the session belongs to. The view swap (`InlineRenameCell`
    /// in place of the filename label) is keyed on `(pane_id,
    /// entry_index)`; if the pane's list model changes underneath us
    /// we consider that a cancellation because the target index no
    /// longer means the same entry.
    pub pane_id: PaneId,
    /// Row index within the pane's current listing. Used only for
    /// the view-side swap — the *authoritative* target is the
    /// [`Self::target`] Location; the index is a rendering hint.
    pub entry_index: i32,
}

impl RenameSession {
    /// Construct a fresh session with the buffer pre-filled from
    /// `original_name`.
    #[must_use]
    pub fn new(
        target: Location,
        original_name: String,
        is_dir: bool,
        pane_id: PaneId,
        entry_index: i32,
    ) -> Self {
        let buffer = original_name.clone();
        Self {
            target,
            original_name,
            is_dir,
            buffer,
            pane_id,
            entry_index,
        }
    }

    /// Has the user changed the buffer? Used to gate commits; also
    /// short-circuits the commit path to a silent no-op when the user
    /// hits Return without typing anything.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.buffer != self.original_name
    }
}

/// Compute the UTF-8 byte-range to pre-select when the rename UI opens.
///
/// Rules (Finder-parity):
///
/// * Directories → whole name selected. Directories don't carry
///   extension semantics; users almost always want to replace the
///   entire label.
/// * Files with a *final* dot at position > 0 → range is `0..i` where
///   `i` is the byte offset of that dot. That's the file's "stem".
///   Compound extensions like `archive.tar.gz` still only strip the
///   final `.gz` — matches Finder's behaviour precisely and is what
///   most users mean when they type over a filename.
/// * Files with no dot, or a dot only at position `0` (dotfiles like
///   `.env`, `.gitignore`) → whole name selected. Renaming a dotfile
///   almost always means renaming the "extension part" that the user
///   sees as the whole name.
///
/// The returned range is always a valid UTF-8 boundary: `str::rfind`
/// only reports byte positions between code points. Both endpoints
/// are inclusive-lower / exclusive-upper (Rust-style).
///
/// # Examples
///
/// ```
/// use atlas_ui::rename_inline::stem_range;
/// // Ordinary file: stem is everything before the final dot.
/// assert_eq!(stem_range("report.pdf", false), (0, 6));
/// // Compound extension: only the *last* dot counts.
/// assert_eq!(stem_range("archive.tar.gz", false), (0, 11));
/// // Dotfile: whole name is the "stem".
/// assert_eq!(stem_range(".env", false), (0, 4));
/// // No extension at all: whole name.
/// assert_eq!(stem_range("Makefile", false), (0, 8));
/// // Directory: always whole name.
/// assert_eq!(stem_range("src", true), (0, 3));
/// assert_eq!(stem_range("build.d", true), (0, 7));
/// ```
#[must_use]
pub fn stem_range(name: &str, is_dir: bool) -> (usize, usize) {
    if is_dir {
        return (0, name.len());
    }
    match name.rfind('.').filter(|&i| i > 0) {
        Some(i) => (0, i),
        None => (0, name.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    fn any_pane() -> PaneId {
        PaneId(0)
    }

    #[test]
    fn stem_range_ordinary_file() {
        assert_eq!(stem_range("Untitled 3.txt", false), (0, 10));
        assert_eq!(stem_range("report.pdf", false), (0, 6));
    }

    #[test]
    fn stem_range_compound_extension_strips_only_final_dot() {
        // Finder convention: `archive.tar.gz` → select `archive.tar`.
        assert_eq!(stem_range("archive.tar.gz", false), (0, 11));
        assert_eq!(stem_range("bundle.spec.json", false), (0, 11));
    }

    #[test]
    fn stem_range_dotfile_selects_whole_name() {
        // Dotfiles have no user-visible extension — the leading dot
        // is part of the name.
        assert_eq!(stem_range(".env", false), (0, 4));
        assert_eq!(stem_range(".gitignore", false), (0, 10));
    }

    #[test]
    fn stem_range_no_extension_selects_whole_name() {
        assert_eq!(stem_range("Makefile", false), (0, 8));
        assert_eq!(stem_range("README", false), (0, 6));
    }

    #[test]
    fn stem_range_directory_always_whole_name() {
        assert_eq!(stem_range("src", true), (0, 3));
        assert_eq!(stem_range("build.d", true), (0, 7));
        assert_eq!(stem_range(".config", true), (0, 7));
    }

    #[test]
    fn stem_range_empty_string_is_empty_range() {
        assert_eq!(stem_range("", false), (0, 0));
        assert_eq!(stem_range("", true), (0, 0));
    }

    #[test]
    fn stem_range_trailing_dot_is_treated_as_extension_boundary() {
        // A file literally named `foo.` — some backup schemes create
        // these. rfind returns the final dot; range is 0..3.
        assert_eq!(stem_range("foo.", false), (0, 3));
    }

    #[test]
    fn stem_range_unicode_names_ride_utf8_boundaries() {
        // 4-byte code points around a plain-ASCII dot.
        let name = "note-💾.md";
        // Byte layout: `note-` (5) + `💾` (4) + `.md` (3) = 12 bytes.
        // The final dot is at byte 9; range 0..9 covers `note-💾`.
        assert_eq!(name.len(), 12);
        assert_eq!(stem_range(name, false), (0, 9));
        assert!(name.is_char_boundary(9));
    }

    #[test]
    fn session_is_dirty_flips_on_buffer_edit() {
        let target = Location::local(PathBuf::from("/tmp/foo.txt"));
        let mut session = RenameSession::new(target, "foo.txt".to_owned(), false, any_pane(), 0);
        assert!(!session.is_dirty(), "fresh session equals original name");
        session.buffer.push('!');
        assert!(session.is_dirty(), "any deviation should register");
        session.buffer.truncate(7);
        assert!(!session.is_dirty(), "back to original ⇒ clean again");
    }

    #[test]
    fn session_preserves_original_name_across_edits() {
        let target = Location::local(PathBuf::from("/tmp/foo.txt"));
        let mut session = RenameSession::new(target, "foo.txt".to_owned(), false, any_pane(), 0);
        session.buffer = "bar.txt".to_owned();
        // Original name must NOT change — it's the immutable comparison
        // baseline for the dirty check.
        assert_eq!(session.original_name, "foo.txt");
        assert_eq!(session.buffer, "bar.txt");
    }
}
