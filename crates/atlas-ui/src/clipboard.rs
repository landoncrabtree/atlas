//! OS clipboard integration for file-copy / cut / paste.
//!
//! Bridges the Atlas file-operations pipeline with the platform clipboard:
//!
//! - **Copy** puts a newline-separated list of URIs on the OS clipboard.
//!   For local locations we emit `file:///…` first (so pasting into
//!   Finder still works), followed by the atlas-scheme
//!   [`Location::to_string`] form. Remote locations emit only the
//!   atlas-scheme URI (`sftp://user@host/path`, `s3://bucket/key`, …).
//!   An internal `mode` flag remembers whether the last copy/cut was a
//!   cut (so paste-back becomes a move).
//! - **Cut** is the same but sets the internal flag to `Move`. Note that
//!   macOS Finder doesn't have a true cut concept; the URIs we place on the
//!   clipboard still copy when pasted into Finder. Cut is only "smart"
//!   when the paste target is another Atlas window.
//! - **Paste** reads the clipboard, parses each non-empty line — either
//!   an atlas-scheme URI (`sftp://…`, `s3://…`, `local:///…`) or a
//!   legacy `file://…` (mapped to `Location::Local`) — deduplicates,
//!   and submits the resulting sources to [`crate::ops::OpsController`]
//!   as a copy or move whose destination is `dest`'s [`Location`].
//!
//! Cross-platform note: the "gold standard" for file-clipboard operations
//! is the platform-native pasteboard format (`NSFilenamesPboardType` on
//! macOS, `CFSTR_FILEDESCRIPTOR` on Windows, `text/uri-list` on Linux).
//! arboard 3.x only exposes plain text, so we ship the URI-list format as
//! plain text — pasting into apps that only accept the native format
//! (Finder for images copied from Preview, e.g.) is a v0.3 follow-up.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use atlas_core::Location;
use parking_lot::Mutex;

use crate::ops::OpsController;

/// Whether the last Copy/Cut was a copy (paste-back copies) or a cut
/// (paste-back moves).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipboardMode {
    /// Last clipboard write was a copy. Paste creates duplicates.
    #[default]
    Copy,
    /// Last clipboard write was a cut. Paste moves and clears the source.
    Cut,
}

/// Clipboard bridge shared across the app. Wraps a lazily-initialised
/// `arboard::Clipboard` behind a mutex (arboard is `!Send` on some
/// platforms, and the underlying pasteboard APIs are single-threaded
/// anyway).
pub struct ClipboardController {
    ops: Arc<OpsController>,
    /// Backing clipboard handle, opened lazily on first use so a headless
    /// test environment (no display) doesn't crash at startup.
    inner: Mutex<Option<arboard::Clipboard>>,
    /// Sticky flag remembering whether the last write was a cut. Reset
    /// after a successful cut-paste so a subsequent paste behaves like a
    /// copy.
    mode: Mutex<ClipboardMode>,
}

impl ClipboardController {
    /// Build a new controller. Cheap — no clipboard handle is opened until
    /// the first copy/cut/paste call.
    #[must_use]
    pub fn new(ops: Arc<OpsController>) -> Arc<Self> {
        Arc::new(Self {
            ops,
            inner: Mutex::new(None),
            mode: Mutex::new(ClipboardMode::Copy),
        })
    }

    /// Copy `locations` to the OS clipboard.
    /// A subsequent [`Self::paste`] will treat this as a copy.
    pub fn copy(&self, locations: Vec<Location>) {
        if locations.is_empty() {
            tracing::debug!("clipboard: copy called with empty selection");
            return;
        }
        self.write_uris(&locations);
        *self.mode.lock() = ClipboardMode::Copy;
        tracing::info!(count = locations.len(), "clipboard: copied locations");
    }

    /// Cut `locations` to the OS clipboard. A subsequent [`Self::paste`] into
    /// this Atlas window will move rather than copy.
    pub fn cut(&self, locations: Vec<Location>) {
        if locations.is_empty() {
            tracing::debug!("clipboard: cut called with empty selection");
            return;
        }
        self.write_uris(&locations);
        *self.mode.lock() = ClipboardMode::Cut;
        tracing::info!(count = locations.len(), "clipboard: cut locations");
    }

    /// Paste whatever's on the clipboard into `dest`. Text on the
    /// clipboard is parsed as one URI per line — atlas-scheme URIs
    /// (`sftp://`, `s3://`, `local://`, `webdav://`, `ftp://`) map
    /// straight through [`Location::from_str`]; `file://` URIs are
    /// percent-decoded into [`Location::Local`]; bare absolute paths
    /// are treated as local for backwards compatibility.
    pub fn paste(&self, dest: Location) {
        let text = match self.read_text() {
            Some(t) => t,
            None => {
                tracing::debug!("clipboard: paste — clipboard empty or unreadable");
                return;
            }
        };
        let sources = decode_clipboard(&text);
        if sources.is_empty() {
            tracing::debug!("clipboard: paste — no URIs or paths found in clipboard text");
            return;
        }
        let mode = *self.mode.lock();
        tracing::info!(
            count = sources.len(),
            ?mode,
            dest = %dest.display_path(),
            "clipboard: pasting"
        );
        match mode {
            ClipboardMode::Copy => self.ops.submit_copy(sources, dest),
            ClipboardMode::Cut => {
                self.ops.submit_move(sources, dest);
                // Cut-paste is one-shot: reset to copy so a second paste
                // duplicates rather than moving the (now-relocated) items.
                *self.mode.lock() = ClipboardMode::Copy;
            }
        }
    }

    // ── Internal helpers ─────────────────────────────────────────────────

    fn write_uris(&self, locations: &[Location]) {
        let body = encode_clipboard(locations);
        let mut guard = self.inner.lock();
        let clipboard = guard.get_or_insert_with(|| {
            arboard::Clipboard::new().unwrap_or_else(|err| {
                tracing::warn!(%err, "clipboard: could not open clipboard handle");
                // Placeholder — subsequent set_text calls will fail loudly.
                // Wrapping in Option would be cleaner but complicates the
                // control flow; the tracing::warn is the diagnostic path.
                panic!("failed to open OS clipboard: {err}");
            })
        });
        if let Err(err) = clipboard.set_text(body) {
            tracing::warn!(%err, "clipboard: set_text failed");
        }
    }

    fn read_text(&self) -> Option<String> {
        let mut guard = self.inner.lock();
        if guard.is_none() {
            match arboard::Clipboard::new() {
                Ok(c) => *guard = Some(c),
                Err(err) => {
                    tracing::warn!(%err, "clipboard: could not open clipboard handle");
                    return None;
                }
            }
        }
        let clipboard = guard.as_mut().expect("just inserted");
        match clipboard.get_text() {
            Ok(t) => Some(t),
            Err(err) => {
                tracing::debug!(%err, "clipboard: get_text failed");
                None
            }
        }
    }
}

/// Serialise `locations` as newline-separated clipboard URIs.
///
/// Local locations emit their `file://` form first (so pasting into
/// Finder / Explorer / native GUIs still works), followed by the
/// atlas-scheme `local://` form. Remote locations emit only the
/// atlas-scheme URI.
pub fn encode_clipboard(locations: &[Location]) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(locations.len() * 2);
    // First pass: file:// URIs for locals only — Finder-friendly.
    for loc in locations {
        if let Location::Local(path) = loc {
            lines.push(path_to_file_uri(path.as_path()));
        }
    }
    // Second pass: atlas-scheme URIs for every location.
    for loc in locations {
        lines.push(loc.to_string());
    }
    lines.join("\n")
}

/// Parse clipboard text back into a de-duplicated list of
/// [`Location`]s.
pub fn decode_clipboard(text: &str) -> Vec<Location> {
    let mut out: Vec<Location> = Vec::new();
    let mut seen: Vec<Location> = Vec::new();
    for line in text.lines() {
        let Some(loc) = parse_clipboard_line(line) else {
            continue;
        };
        if seen.iter().any(|existing| locations_equal(existing, &loc)) {
            continue;
        }
        seen.push(loc.clone());
        out.push(loc);
    }
    out
}

/// One clipboard line → [`Location`], if it looks parseable.
fn parse_clipboard_line(line: &str) -> Option<Location> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("file://") {
        return percent_decode(rest).map(|p| Location::Local(PathBuf::from(p)));
    }
    if trimmed.contains("://") {
        return Location::from_str(trimmed).ok();
    }
    if trimmed.starts_with('/') || trimmed.starts_with('~') {
        return Some(Location::Local(PathBuf::from(trimmed)));
    }
    None
}

fn locations_equal(a: &Location, b: &Location) -> bool {
    match (a, b) {
        (Location::Local(x), Location::Local(y)) => x == y,
        (Location::Remote(a_uri, a_kind), Location::Remote(b_uri, b_kind)) => {
            a_kind == b_kind
                && a_uri.scheme == b_uri.scheme
                && a_uri.host == b_uri.host
                && a_uri.port == b_uri.port
                && a_uri.username == b_uri.username
                && a_uri.path == b_uri.path
        }
        _ => false,
    }
}

/// Render `path` as an RFC 8089 `file://` URI, percent-encoding characters
/// that would otherwise break the URI grammar.
fn path_to_file_uri(path: &std::path::Path) -> String {
    // Minimal percent-encoding — good enough for local paths.
    let mut out = String::from("file://");
    for byte in path.to_string_lossy().as_bytes() {
        match byte {
            b'/' | b'.' | b'-' | b'_' | b'~' => out.push(*byte as char),
            b if b.is_ascii_alphanumeric() => out.push(*b as char),
            other => out.push_str(&format!("%{:02X}", other)),
        }
    }
    out
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_encodes_spaces_and_specials() {
        let uri = path_to_file_uri(std::path::Path::new("/tmp/hello world!"));
        assert_eq!(uri, "file:///tmp/hello%20world%21");
    }

    #[test]
    fn parse_clipboard_line_handles_file_uri() {
        let loc = parse_clipboard_line("file:///tmp/hello%20world.txt").unwrap();
        assert_eq!(loc, Location::Local(PathBuf::from("/tmp/hello world.txt")));
    }

    #[test]
    fn parse_clipboard_line_handles_bare_path() {
        let loc = parse_clipboard_line("/tmp/plain.txt").unwrap();
        assert_eq!(loc, Location::Local(PathBuf::from("/tmp/plain.txt")));
    }

    #[test]
    fn parse_clipboard_line_handles_atlas_scheme_sftp() {
        let loc = parse_clipboard_line("sftp://alice@host/var/log").unwrap();
        assert!(matches!(loc, Location::Remote(..)));
        assert_eq!(loc.display_path(), "sftp://alice@host/var/log");
    }

    #[test]
    fn parse_clipboard_line_skips_garbage() {
        assert!(parse_clipboard_line("").is_none());
        assert!(parse_clipboard_line("hello world").is_none());
        assert!(parse_clipboard_line("http://example.com").is_none());
    }

    #[test]
    fn encode_decode_local_roundtrip() {
        let locs = vec![
            Location::local("/tmp/a.txt"),
            Location::local("/Users/alice/report.pdf"),
        ];
        let text = encode_clipboard(&locs);
        let decoded = decode_clipboard(&text);
        assert_eq!(decoded, locs);
    }

    #[test]
    fn encode_decode_sftp_roundtrip() {
        let locs = vec![Location::from_str("sftp://alice@host:2222/var/log").unwrap()];
        let text = encode_clipboard(&locs);
        let decoded = decode_clipboard(&text);
        assert_eq!(decoded, locs);
    }

    #[test]
    fn encode_decode_s3_roundtrip() {
        let locs = vec![Location::from_str("s3://bucket/prefix/key").unwrap()];
        let text = encode_clipboard(&locs);
        let decoded = decode_clipboard(&text);
        assert_eq!(decoded, locs);
    }

    #[test]
    fn encode_decode_webdav_roundtrip() {
        let locs = vec![Location::from_str("webdav://user@host/dav/files").unwrap()];
        let text = encode_clipboard(&locs);
        let decoded = decode_clipboard(&text);
        assert_eq!(decoded, locs);
    }

    #[test]
    fn encode_decode_ftp_roundtrip() {
        let locs = vec![Location::from_str("ftp://ftp.example.com/pub").unwrap()];
        let text = encode_clipboard(&locs);
        let decoded = decode_clipboard(&text);
        assert_eq!(decoded, locs);
    }

    #[test]
    fn encode_local_emits_file_uri_first_then_atlas_scheme() {
        let locs = vec![Location::local("/tmp/hello world.txt")];
        let text = encode_clipboard(&locs);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "one file:// line + one atlas line");
        assert!(lines[0].starts_with("file:///tmp/hello%20world"));
        assert!(lines[1].starts_with("local:///tmp/hello"));
    }

    #[test]
    fn decode_mixed_file_and_atlas_uris_deduplicates() {
        // Simulate an Atlas → Atlas clipboard payload: local file
        // shows up twice (file:// + local://), remote shows up once
        // as sftp://.
        let text = "file:///tmp/a.txt\nlocal:///tmp/a.txt\nsftp://user@host/tmp/b.txt";
        let decoded = decode_clipboard(text);
        assert_eq!(decoded.len(), 2, "duplicate local should be deduped");
        assert_eq!(decoded[0], Location::local("/tmp/a.txt"));
        assert!(matches!(decoded[1], Location::Remote(..)));
    }
}
