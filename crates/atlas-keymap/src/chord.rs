//! Key chord types and VS Code / Zed-style string parsing.
//!
//! A [`Chord`] is a single keypress with optional modifier keys.
//! A [`ChordSequence`] is a sequence of one or more chords (e.g. `"g g"`).

use std::fmt;
use std::str::FromStr;

use smallvec::SmallVec;
use thiserror::Error;

/// Modifier keys that can accompany a key press.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    /// Control key.
    pub ctrl: bool,
    /// Alt key (Option on macOS).
    pub alt: bool,
    /// Shift key.
    pub shift: bool,
    /// Command / Meta / Super / Win key.
    pub cmd: bool,
}

impl Modifiers {
    /// Returns `true` if no modifier is active.
    pub fn is_empty(self) -> bool {
        !self.ctrl && !self.alt && !self.shift && !self.cmd
    }
}

/// Non-character keys that have explicit names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NamedKey {
    /// Escape key.
    Escape,
    /// Tab key.
    Tab,
    /// Enter / Return key.
    Enter,
    /// Backspace key.
    Backspace,
    /// Delete (forward-delete) key.
    Delete,
    /// Space bar.
    Space,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Home key.
    Home,
    /// End key.
    End,
    /// Page Up key.
    PageUp,
    /// Page Down key.
    PageDown,
    /// Insert key.
    Insert,
}

impl NamedKey {
    /// Canonical lowercase string representation used in chord strings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Escape => "escape",
            Self::Tab => "tab",
            Self::Enter => "enter",
            Self::Backspace => "backspace",
            Self::Delete => "delete",
            Self::Space => "space",
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
            Self::Home => "home",
            Self::End => "end",
            Self::PageUp => "pageup",
            Self::PageDown => "pagedown",
            Self::Insert => "insert",
        }
    }

    fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "escape" | "esc" => Some(Self::Escape),
            "tab" => Some(Self::Tab),
            "enter" | "return" => Some(Self::Enter),
            "backspace" | "back" => Some(Self::Backspace),
            "delete" | "del" => Some(Self::Delete),
            "space" => Some(Self::Space),
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "home" => Some(Self::Home),
            "end" => Some(Self::End),
            "pageup" | "pgup" => Some(Self::PageUp),
            "pagedown" | "pgdn" | "pagedn" => Some(Self::PageDown),
            "insert" | "ins" => Some(Self::Insert),
            _ => None,
        }
    }
}

impl fmt::Display for NamedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The primary (non-modifier) part of a [`Chord`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// A printable character (always stored lowercased; shift is in [`Modifiers`]).
    Char(char),
    /// A function key F1–F24.
    Function(u8),
    /// A named special key.
    Named(NamedKey),
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Char(c) => write!(f, "{c}"),
            Self::Function(n) => write!(f, "f{n}"),
            Self::Named(n) => write!(f, "{n}"),
        }
    }
}

/// A single key press, optionally combined with modifier keys.
///
/// # String format
///
/// Modifiers and the key are joined with `-`. Modifiers must come before the key.
/// Examples: `"cmd-shift-p"`, `"ctrl-alt-shift-enter"`, `"f5"`, `"/"`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    /// Modifier keys held during this key press.
    pub modifiers: Modifiers,
    /// The primary key.
    pub key: Key,
}

/// Error type for chord and sequence parsing.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseChordError {
    /// A chord string was empty.
    #[error("empty chord string")]
    Empty,
    /// Only modifiers were specified — no key.
    #[error("chord has modifiers but no key: {0:?}")]
    NoKey(String),
    /// An unrecognised token appeared in the chord.
    #[error("unknown token {token:?} in chord {chord:?}")]
    UnknownToken { token: String, chord: String },
    /// A sequence string was empty.
    #[error("empty sequence string")]
    EmptySequence,
}

impl Chord {
    /// Parse a single chord from a string like `"cmd-shift-p"` or `"escape"`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, ParseChordError> {
        if s.is_empty() {
            return Err(ParseChordError::Empty);
        }

        if s == "-" {
            return Ok(Self {
                modifiers: Modifiers::default(),
                key: Key::Char('-'),
            });
        }

        let lower = s.to_ascii_lowercase();
        let parts: Vec<&str> = lower.split('-').collect();

        let mut modifiers = Modifiers::default();
        let mut key = None;
        let mut i = 0;
        while i < parts.len() {
            match parts[i] {
                "ctrl" | "control" => modifiers.ctrl = true,
                "alt" | "option" | "opt" => modifiers.alt = true,
                "shift" => modifiers.shift = true,
                "cmd" | "meta" | "super" | "win" => modifiers.cmd = true,
                _ => {
                    let remaining = parts[i..].join("-");
                    key = Some(parse_key(&remaining, s)?);
                    break;
                }
            }
            i += 1;
        }

        key.map(|key| Self { modifiers, key })
            .ok_or_else(|| ParseChordError::NoKey(s.to_owned()))
    }

    /// Canonical display string for this chord (round-trips through [`Chord::from_str`]).
    pub fn display(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.modifiers.ctrl {
            parts.push("ctrl");
        }
        if self.modifiers.alt {
            parts.push("alt");
        }
        if self.modifiers.shift {
            parts.push("shift");
        }
        if self.modifiers.cmd {
            parts.push("cmd");
        }
        let key_str = self.key.to_string();
        parts.push(&key_str);
        parts.join("-")
    }
}

impl FromStr for Chord {
    type Err = ParseChordError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_str(s)
    }
}

fn parse_key(s: &str, original: &str) -> Result<Key, ParseChordError> {
    if let Some(rest) = s.strip_prefix('f') {
        if let Ok(n) = rest.parse::<u8>() {
            if (1..=24).contains(&n) {
                return Ok(Key::Function(n));
            }
        }
    }

    if let Some(named) = NamedKey::from_str_opt(s) {
        return Ok(Key::Named(named));
    }

    let mut chars = s.chars();
    if let Some(ch) = chars.next() {
        if chars.next().is_none() {
            return Ok(Key::Char(ch.to_ascii_lowercase()));
        }
    }

    Err(ParseChordError::UnknownToken {
        token: s.to_owned(),
        chord: original.to_owned(),
    })
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

/// A sequence of one or more chords, separated by whitespace in string form.
///
/// A two-element sequence like `"g g"` is sometimes called a "leader sequence".
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct ChordSequence(pub SmallVec<[Chord; 2]>);

impl ChordSequence {
    /// Parse a chord sequence from a whitespace-separated string of chord tokens.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, ParseChordError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(ParseChordError::EmptySequence);
        }
        let chords = s
            .split_whitespace()
            .map(Chord::from_str)
            .collect::<Result<SmallVec<[Chord; 2]>, _>>()?;
        Ok(Self(chords))
    }

    /// Canonical display string (space-separated chord display strings).
    pub fn display(&self) -> String {
        self.0
            .iter()
            .map(Chord::display)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Returns `true` if this sequence has no chords.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of chords in the sequence.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Display for ChordSequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

impl FromStr for ChordSequence {
    type Err = ParseChordError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_char() {
        let chord = Chord::from_str("a").unwrap();
        assert_eq!(chord.key, Key::Char('a'));
        assert!(chord.modifiers.is_empty());
    }

    #[test]
    fn test_parse_cmd_p() {
        let chord = Chord::from_str("cmd-p").unwrap();
        assert_eq!(chord.key, Key::Char('p'));
        assert!(chord.modifiers.cmd);
        assert!(!chord.modifiers.shift);
    }

    #[test]
    fn test_parse_cmd_shift_p() {
        let chord = Chord::from_str("cmd-shift-p").unwrap();
        assert!(chord.modifiers.cmd && chord.modifiers.shift);
        assert_eq!(chord.key, Key::Char('p'));
    }

    #[test]
    fn test_parse_ctrl_alt_shift_enter() {
        let chord = Chord::from_str("ctrl-alt-shift-enter").unwrap();
        assert!(chord.modifiers.ctrl && chord.modifiers.alt && chord.modifiers.shift);
        assert_eq!(chord.key, Key::Named(NamedKey::Enter));
    }

    #[test]
    fn test_parse_function_key() {
        let chord = Chord::from_str("f5").unwrap();
        assert_eq!(chord.key, Key::Function(5));
        assert!(chord.modifiers.is_empty());
    }

    #[test]
    fn test_parse_escape() {
        let chord = Chord::from_str("escape").unwrap();
        assert_eq!(chord.key, Key::Named(NamedKey::Escape));
    }

    #[test]
    fn test_parse_space() {
        let chord = Chord::from_str("space").unwrap();
        assert_eq!(chord.key, Key::Named(NamedKey::Space));
    }

    #[test]
    fn test_parse_slash() {
        let chord = Chord::from_str("/").unwrap();
        assert_eq!(chord.key, Key::Char('/'));
    }

    #[test]
    fn test_parse_hyphen() {
        let chord = Chord::from_str("-").unwrap();
        assert_eq!(chord.key, Key::Char('-'));
    }

    #[test]
    fn test_parse_modifier_aliases() {
        let chord = Chord::from_str("control-a").unwrap();
        assert!(chord.modifiers.ctrl);

        let chord = Chord::from_str("option-a").unwrap();
        assert!(chord.modifiers.alt);
        let chord = Chord::from_str("opt-a").unwrap();
        assert!(chord.modifiers.alt);

        let chord = Chord::from_str("meta-a").unwrap();
        assert!(chord.modifiers.cmd);
        let chord = Chord::from_str("super-a").unwrap();
        assert!(chord.modifiers.cmd);
        let chord = Chord::from_str("win-a").unwrap();
        assert!(chord.modifiers.cmd);
    }

    #[test]
    fn test_round_trip() {
        let cases = [
            "cmd-shift-p",
            "ctrl-alt-shift-enter",
            "f5",
            "escape",
            "space",
            "/",
            "g",
        ];
        for case in cases {
            let chord = Chord::from_str(case).unwrap();
            let displayed = chord.display();
            let reparsed = Chord::from_str(&displayed).unwrap();
            assert_eq!(chord, reparsed, "round-trip failed for {case:?}");
        }
    }

    #[test]
    fn test_multi_chord_sequence() {
        let sequence = ChordSequence::from_str("cmd-k cmd-s").unwrap();
        assert_eq!(sequence.len(), 2);
        assert_eq!(sequence.0[0].key, Key::Char('k'));
        assert!(sequence.0[0].modifiers.cmd);
        assert_eq!(sequence.0[1].key, Key::Char('s'));
        assert!(sequence.0[1].modifiers.cmd);
    }

    #[test]
    fn test_g_g_sequence() {
        let sequence = ChordSequence::from_str("g g").unwrap();
        assert_eq!(sequence.len(), 2);
        assert_eq!(sequence.0[0].key, Key::Char('g'));
        assert_eq!(sequence.0[1].key, Key::Char('g'));
    }

    #[test]
    fn test_sequence_display_round_trip() {
        let source = "cmd-k cmd-s";
        let sequence = ChordSequence::from_str(source).unwrap();
        assert_eq!(sequence.display(), source);
    }

    #[test]
    fn test_reject_empty() {
        assert_eq!(Chord::from_str(""), Err(ParseChordError::Empty));
        assert_eq!(
            ChordSequence::from_str(""),
            Err(ParseChordError::EmptySequence)
        );
        assert_eq!(
            ChordSequence::from_str("   "),
            Err(ParseChordError::EmptySequence)
        );
    }

    #[test]
    fn test_reject_modifier_only() {
        let error = Chord::from_str("shift");
        assert!(matches!(error, Err(ParseChordError::NoKey(_))));
    }

    #[test]
    fn test_reject_trailing_dash() {
        let error = Chord::from_str("cmd-");
        assert!(matches!(error, Err(ParseChordError::UnknownToken { .. })));
    }

    #[test]
    fn test_reject_unknown_token() {
        let error = Chord::from_str("cmd-blorp");
        assert!(matches!(error, Err(ParseChordError::UnknownToken { .. })));
    }

    #[test]
    fn test_case_insensitive_modifiers() {
        let chord = Chord::from_str("CMD-SHIFT-P").unwrap();
        assert!(chord.modifiers.cmd && chord.modifiers.shift);
        assert_eq!(chord.key, Key::Char('p'));
    }

    // ── Modifier alias round-trip tests ──────────────────────────────────────

    /// `cmd`, `meta`, `super`, and `win` all map to the same [`Modifiers::cmd`]
    /// field.  The canonical serialised form is always `"cmd"`.
    ///
    /// Cross-platform note: `cmd` routes to the Command key on macOS and to
    /// Ctrl on Linux/Windows at dispatch time; the keymap stores it uniformly as
    /// `cmd` so that a single default binding table works on every platform.
    #[test]
    fn test_cmd_alias_round_trip() {
        for alias in ["cmd-p", "meta-p", "super-p", "win-p"] {
            let chord =
                Chord::from_str(alias).unwrap_or_else(|e| panic!("failed to parse {alias:?}: {e}"));
            assert!(
                chord.modifiers.cmd,
                "expected cmd modifier for alias {alias:?}"
            );
            assert!(!chord.modifiers.ctrl);
            assert!(!chord.modifiers.alt);
            assert_eq!(chord.key, Key::Char('p'));
            assert_eq!(
                chord.display(),
                "cmd-p",
                "canonical form must be 'cmd-p' for alias {alias:?}"
            );
        }
    }

    /// All four `cmd`-family aliases produce identical [`Chord`] values.
    #[test]
    fn test_cmd_aliases_are_equivalent() {
        let canonical = Chord::from_str("cmd-p").unwrap();
        assert_eq!(canonical, Chord::from_str("meta-p").unwrap());
        assert_eq!(canonical, Chord::from_str("super-p").unwrap());
        assert_eq!(canonical, Chord::from_str("win-p").unwrap());
    }

    /// `alt`, `option`, and `opt` all map to [`Modifiers::alt`]; canonical
    /// serialised form is `"alt"`.
    #[test]
    fn test_alt_alias_round_trip() {
        for alias in ["alt-a", "option-a", "opt-a"] {
            let chord =
                Chord::from_str(alias).unwrap_or_else(|e| panic!("failed to parse {alias:?}: {e}"));
            assert!(
                chord.modifiers.alt,
                "expected alt modifier for alias {alias:?}"
            );
            assert!(!chord.modifiers.cmd);
            assert_eq!(chord.key, Key::Char('a'));
            assert_eq!(
                chord.display(),
                "alt-a",
                "canonical form must be 'alt-a' for alias {alias:?}"
            );
        }
    }

    /// All three `alt`-family aliases produce identical [`Chord`] values.
    #[test]
    fn test_alt_aliases_are_equivalent() {
        let canonical = Chord::from_str("alt-a").unwrap();
        assert_eq!(canonical, Chord::from_str("option-a").unwrap());
        assert_eq!(canonical, Chord::from_str("opt-a").unwrap());
    }

    /// `ctrl` and `control` both map to [`Modifiers::ctrl`]; canonical
    /// serialised form is `"ctrl"`.
    #[test]
    fn test_ctrl_alias_round_trip() {
        for alias in ["ctrl-x", "control-x"] {
            let chord =
                Chord::from_str(alias).unwrap_or_else(|e| panic!("failed to parse {alias:?}: {e}"));
            assert!(
                chord.modifiers.ctrl,
                "expected ctrl modifier for alias {alias:?}"
            );
            assert!(!chord.modifiers.cmd);
            assert_eq!(chord.key, Key::Char('x'));
            assert_eq!(
                chord.display(),
                "ctrl-x",
                "canonical form must be 'ctrl-x' for alias {alias:?}"
            );
        }
    }

    /// `ctrl` and `control` produce identical [`Chord`] values.
    #[test]
    fn test_ctrl_aliases_are_equivalent() {
        assert_eq!(
            Chord::from_str("ctrl-x").unwrap(),
            Chord::from_str("control-x").unwrap()
        );
    }
}
