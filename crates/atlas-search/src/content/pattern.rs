//! Pattern specification and compilation to a grep-regex matcher.

use grep_regex::{RegexMatcher, RegexMatcherBuilder};

/// Case sensitivity mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseSensitivity {
    /// Always case-sensitive.
    Sensitive,
    /// Always case-insensitive.
    Insensitive,
    /// Case-insensitive unless the pattern contains an uppercase ASCII letter.
    Smart,
}

/// Describes the pattern to search for.
#[derive(Debug, Clone)]
pub enum PatternSpec {
    /// Literal (fixed-string) match.
    Literal {
        /// The text to search for.
        text: String,
        /// Case sensitivity.
        case: CaseSensitivity,
        /// Require the match to sit on a word boundary (`\b`).
        word_boundary: bool,
    },
    /// Regular-expression match.
    Regex {
        /// The regex pattern.
        pattern: String,
        /// Case sensitivity.
        case: CaseSensitivity,
        /// Enable multi-line mode (`(?m)`).
        multiline: bool,
    },
}

/// Resolve `Smart` case to an actual sensitivity based on whether the pattern string
/// contains any uppercase ASCII characters.
pub(crate) fn resolve_case(case: CaseSensitivity, pattern: &str) -> bool {
    match case {
        CaseSensitivity::Sensitive => false,
        CaseSensitivity::Insensitive => true,
        CaseSensitivity::Smart => !pattern.chars().any(|c| c.is_ascii_uppercase()),
    }
}

/// Compile a [`PatternSpec`] into a [`RegexMatcher`].
pub(crate) fn compile(spec: &PatternSpec) -> anyhow::Result<RegexMatcher> {
    let mut builder = RegexMatcherBuilder::new();
    match spec {
        PatternSpec::Literal {
            text,
            case,
            word_boundary,
        } => {
            builder
                .case_insensitive(resolve_case(*case, text))
                .fixed_strings(true)
                .word(*word_boundary);
            Ok(builder.build(text)?)
        }
        PatternSpec::Regex {
            pattern,
            case,
            multiline,
        } => {
            builder
                .case_insensitive(resolve_case(*case, pattern))
                .multi_line(*multiline);
            Ok(builder.build(pattern)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use grep::matcher::Matcher;

    use super::{compile, resolve_case, CaseSensitivity, PatternSpec};

    #[test]
    fn smart_case_resolves_from_pattern_text() {
        assert!(resolve_case(CaseSensitivity::Smart, "atlas"));
        assert!(!resolve_case(CaseSensitivity::Smart, "Atlas"));
        assert!(resolve_case(CaseSensitivity::Insensitive, "Atlas"));
        assert!(!resolve_case(CaseSensitivity::Sensitive, "atlas"));
    }

    #[test]
    fn literal_word_boundary_matches_only_whole_words() {
        let matcher = compile(&PatternSpec::Literal {
            text: "cat".to_owned(),
            case: CaseSensitivity::Sensitive,
            word_boundary: true,
        })
        .unwrap();

        let mut matches = Vec::new();
        matcher
            .find_iter(b"concatenate cat catnip", |m| {
                matches.push((m.start(), m.end()));
                true
            })
            .unwrap();

        assert_eq!(matches, vec![(12, 15)]);
    }
}
