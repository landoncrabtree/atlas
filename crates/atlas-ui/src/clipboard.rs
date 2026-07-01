//! OS clipboard integration for file-copy / cut / paste.
//!
//! Bridges the Atlas file-operations pipeline with the platform clipboard:
//!
//! - **Copy** puts a newline-separated list of `file://` URIs on the OS
//!   clipboard. Pastes into Finder, Explorer, TextEdit, VS Code — anywhere
//!   that reads text. An internal `mode` flag remembers whether the last
//!   copy/cut was a cut (so paste-back becomes a move).
//! - **Cut** is the same but sets the internal flag to `Move`. Note that
//!   macOS Finder doesn't have a true cut concept; the URIs we place on the
//!   clipboard still copy when pasted into Finder. Cut is only "smart"
//!   when the paste target is another Atlas window.
//! - **Paste** reads the clipboard, parses any lines that look like
//!   `file://…` URIs (falling back to bare paths on Linux where some apps
//!   drop the scheme), and submits the resulting sources to the
//!   [`crate::ops::OpsController`] as a copy or move into the focused pane's
//!   directory.
//!
//! Cross-platform note: the "gold standard" for file-clipboard operations
//! is the platform-native pasteboard format (`NSFilenamesPboardType` on
//! macOS, `CFSTR_FILEDESCRIPTOR` on Windows, `text/uri-list` on Linux).
//! arboard 3.x only exposes plain text, so we ship the URI-list format as
//! plain text — pasting into apps that only accept the native format
//! (Finder for images copied from Preview, e.g.) is a v0.3 follow-up.

use std::path::PathBuf;
use std::sync::Arc;

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

    /// Copy `paths` to the OS clipboard as newline-separated `file://` URIs.
    /// A subsequent [`Self::paste`] will treat this as a copy.
    pub fn copy(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            tracing::debug!("clipboard: copy called with empty selection");
            return;
        }
        self.write_uris(&paths);
        *self.mode.lock() = ClipboardMode::Copy;
        tracing::info!(count = paths.len(), "clipboard: copied paths");
    }

    /// Cut `paths` to the OS clipboard. A subsequent [`Self::paste`] into
    /// this Atlas window will move rather than copy.
    pub fn cut(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            tracing::debug!("clipboard: cut called with empty selection");
            return;
        }
        self.write_uris(&paths);
        *self.mode.lock() = ClipboardMode::Cut;
        tracing::info!(count = paths.len(), "clipboard: cut paths");
    }

    /// Paste whatever's on the clipboard into `dest_dir`. Text on the
    /// clipboard is parsed as one path per line — `file://…` URIs are
    /// URL-decoded; bare absolute paths are used as-is; everything else
    /// is skipped.
    pub fn paste(&self, dest_dir: PathBuf) {
        let text = match self.read_text() {
            Some(t) => t,
            None => {
                tracing::debug!("clipboard: paste — clipboard empty or unreadable");
                return;
            }
        };
        let sources: Vec<PathBuf> = text.lines().filter_map(parse_clipboard_line).collect();
        if sources.is_empty() {
            tracing::debug!("clipboard: paste — no file:// URIs or paths found in clipboard text");
            return;
        }
        let mode = *self.mode.lock();
        tracing::info!(
            count = sources.len(),
            ?mode,
            dest = %dest_dir.display(),
            "clipboard: pasting"
        );
        match mode {
            ClipboardMode::Copy => self.ops.submit_copy(sources, dest_dir),
            ClipboardMode::Cut => {
                self.ops.submit_move(sources, dest_dir);
                // Cut-paste is one-shot: reset to copy so a second paste
                // duplicates rather than moving the (now-relocated) items.
                *self.mode.lock() = ClipboardMode::Copy;
            }
        }
    }

    // ── Internal helpers ─────────────────────────────────────────────────

    fn write_uris(&self, paths: &[PathBuf]) {
        let body = paths
            .iter()
            .map(|p| path_to_file_uri(p.as_path()))
            .collect::<Vec<_>>()
            .join("\n");
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

/// Parse one line of clipboard text into a filesystem path, if it looks
/// like one. Returns `None` for empty lines, lines that don't start with a
/// path/URI, and any URI that fails to percent-decode.
fn parse_clipboard_line(line: &str) -> Option<PathBuf> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("file://") {
        percent_decode(rest).map(PathBuf::from)
    } else if trimmed.starts_with('/') || trimmed.starts_with('~') {
        Some(PathBuf::from(trimmed))
    } else {
        None
    }
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
        let p = parse_clipboard_line("file:///tmp/hello%20world.txt").unwrap();
        assert_eq!(p, PathBuf::from("/tmp/hello world.txt"));
    }

    #[test]
    fn parse_clipboard_line_handles_bare_path() {
        let p = parse_clipboard_line("/tmp/plain.txt").unwrap();
        assert_eq!(p, PathBuf::from("/tmp/plain.txt"));
    }

    #[test]
    fn parse_clipboard_line_skips_garbage() {
        assert!(parse_clipboard_line("").is_none());
        assert!(parse_clipboard_line("hello world").is_none());
        assert!(parse_clipboard_line("http://example.com").is_none());
    }
}
