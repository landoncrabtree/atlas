//! Name-validation for the inline rename cell.
//!
//! Errors are surfaced as caption-size text under the cell in
//! `Theme.error`. None of these checks touch the filesystem —
//! sibling-collision detection is deferred to commit and routed
//! through the shared `AtlasConflictModal`.

/// Ordered validation outcome. Rust picks the first failure and
/// pushes a corresponding user-facing message into the cell via
/// [`Self::message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameValidation {
    /// Name is acceptable — commit is safe.
    Ok,
    /// Empty string.
    Empty,
    /// Contains a path separator (`/` on any OS; `\` on Windows).
    ContainsSeparator,
    /// Reserved current/parent directory sentinel (`.` / `..`).
    ReservedDot,
    /// UTF-8 byte length exceeds 255.
    TooLong,
    /// Contains a control byte, `NUL`, or (on Windows) one of the
    /// reserved characters `<>:"|?*`.
    IllegalChar(char),
}

impl RenameValidation {
    /// Convert the outcome into the user-facing string shown under
    /// the cell (`Theme.error`, `caption` weight).
    ///
    /// Kept short so the caption line under a compact cell stays
    /// legible on macOS. Returns `""` for [`Self::Ok`].
    #[must_use]
    pub fn message(self) -> String {
        match self {
            Self::Ok => String::new(),
            Self::Empty => "Enter a name.".to_owned(),
            Self::ContainsSeparator => "Names can't contain slashes.".to_owned(),
            Self::ReservedDot => "That name is reserved.".to_owned(),
            Self::TooLong => "Name is too long (255-byte limit).".to_owned(),
            Self::IllegalChar(c) => format!("Names can't contain '{c}'."),
        }
    }

    /// Was the check successful?
    #[must_use]
    pub fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Validate `name` against the platform-appropriate rules.
///
/// Rules applied in order (first failure wins):
///
/// 1. Empty string → [`RenameValidation::Empty`].
/// 2. `.` or `..` → [`RenameValidation::ReservedDot`].
/// 3. UTF-8 length > 255 bytes → [`RenameValidation::TooLong`] (POSIX
///    NAME_MAX / Windows-file-name cap).
/// 4. `/` anywhere → [`RenameValidation::ContainsSeparator`].
/// 5. On Windows only: `\` anywhere → [`RenameValidation::ContainsSeparator`].
/// 6. `NUL` (`\0`) anywhere → [`RenameValidation::IllegalChar('\0')`].
/// 7. Any C0 control byte (`\x01..=\x1f`) → same.
/// 8. On Windows only: any of `<>:"|?*` → [`RenameValidation::IllegalChar(c)`].
///
/// Sibling-collision (same-directory duplicate) is NOT checked here
/// — it happens at commit time and routes through
/// `AtlasConflictModal`.
///
/// The check is `#[inline]` because it runs on every keystroke; the
/// hot path is short circuiting on the first illegal character.
#[inline]
#[must_use]
pub fn validate_name(name: &str) -> RenameValidation {
    if name.is_empty() {
        return RenameValidation::Empty;
    }
    if name == "." || name == ".." {
        return RenameValidation::ReservedDot;
    }
    if name.len() > 255 {
        return RenameValidation::TooLong;
    }
    for ch in name.chars() {
        match ch {
            '/' => return RenameValidation::ContainsSeparator,
            '\0' => return RenameValidation::IllegalChar('\0'),
            #[cfg(windows)]
            '\\' => return RenameValidation::ContainsSeparator,
            #[cfg(windows)]
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => {
                return RenameValidation::IllegalChar(ch);
            }
            c if (c as u32) < 0x20 => return RenameValidation::IllegalChar(c),
            _ => {}
        }
    }
    RenameValidation::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_is_rejected() {
        assert_eq!(validate_name(""), RenameValidation::Empty);
    }

    #[test]
    fn ordinary_names_pass() {
        assert!(validate_name("readme.md").is_ok());
        assert!(validate_name("My File 2024.pdf").is_ok());
        assert!(validate_name(".env").is_ok());
        assert!(validate_name("archive.tar.gz").is_ok());
        // Unicode names — code points above U+007F are fine on
        // every filesystem Atlas supports.
        assert!(validate_name("笔记.md").is_ok());
        assert!(validate_name("café.txt").is_ok());
        assert!(validate_name("📁 memes").is_ok());
    }

    #[test]
    fn slash_anywhere_is_rejected() {
        assert_eq!(
            validate_name("foo/bar"),
            RenameValidation::ContainsSeparator
        );
        assert_eq!(
            validate_name("/leading"),
            RenameValidation::ContainsSeparator
        );
        assert_eq!(
            validate_name("trailing/"),
            RenameValidation::ContainsSeparator
        );
    }

    #[test]
    fn nul_byte_is_rejected() {
        assert_eq!(
            validate_name("foo\0bar"),
            RenameValidation::IllegalChar('\0')
        );
    }

    #[test]
    fn control_bytes_are_rejected() {
        assert_eq!(
            validate_name("foo\x07bar"),
            RenameValidation::IllegalChar('\x07')
        );
        // Newline is a C0 control byte too — some filesystems accept
        // it but it's a common source of foot-guns; refuse.
        assert_eq!(
            validate_name("line\nbreak"),
            RenameValidation::IllegalChar('\n')
        );
    }

    #[test]
    fn dot_and_dot_dot_are_reserved() {
        assert_eq!(validate_name("."), RenameValidation::ReservedDot);
        assert_eq!(validate_name(".."), RenameValidation::ReservedDot);
        // Leading-dot only: still fine.
        assert!(validate_name("...").is_ok());
    }

    #[test]
    fn name_longer_than_255_bytes_is_rejected() {
        let s = "a".repeat(256);
        assert_eq!(validate_name(&s), RenameValidation::TooLong);
        let just_ok = "a".repeat(255);
        assert!(validate_name(&just_ok).is_ok());
    }

    #[test]
    fn utf8_byte_length_not_char_length() {
        // 200 4-byte code points ⇒ 800 bytes; well over the cap even
        // though the char length is 200.
        let long = "💾".repeat(200);
        assert_eq!(long.len(), 800);
        assert_eq!(validate_name(&long), RenameValidation::TooLong);
    }

    #[cfg(windows)]
    #[test]
    fn windows_reserved_chars_are_rejected() {
        for c in ['<', '>', ':', '"', '|', '?', '*'] {
            let name = format!("foo{c}bar");
            assert_eq!(
                validate_name(&name),
                RenameValidation::IllegalChar(c),
                "expected {c:?} to be rejected on Windows"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_backslash_is_rejected() {
        assert_eq!(
            validate_name("foo\\bar"),
            RenameValidation::ContainsSeparator
        );
    }

    #[test]
    fn message_strings_are_present_and_non_empty_except_ok() {
        assert!(RenameValidation::Ok.message().is_empty());
        for v in [
            RenameValidation::Empty,
            RenameValidation::ContainsSeparator,
            RenameValidation::ReservedDot,
            RenameValidation::TooLong,
            RenameValidation::IllegalChar('*'),
        ] {
            assert!(!v.message().is_empty(), "{v:?} must have a message");
        }
    }
}
