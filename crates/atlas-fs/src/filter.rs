//! Filtering primitives: a declarative [`Filter`] that compiles to a
//! [`CompiledFilter`] for repeated application.
//!
//! Filtering supports a case-insensitive substring query, glob include/exclude
//! lists (via [`globset`]), and an optional regular expression — all matched
//! against the entry name.

use atlas_core::{AtlasError, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;

use crate::entry::Entry;

/// A declarative filter describing which entries should be retained.
///
/// All present criteria must match (logical AND). An empty/default filter
/// matches everything visible.
#[derive(Clone, Debug)]
pub struct Filter {
    /// Case-insensitive substring matched against the entry name.
    pub query: Option<String>,
    /// Glob patterns; if non-empty, the name must match at least one.
    pub include_globs: Vec<String>,
    /// Glob patterns; if the name matches any, the entry is excluded.
    pub exclude_globs: Vec<String>,
    /// Regular expression matched against the entry name.
    pub regex: Option<String>,
    /// When `false`, entries whose [`Entry::metadata`] carries
    /// `is_hidden = true` are filtered out post-list. Defaults to `true`
    /// (show everything), matching the historical behaviour when the
    /// raw list already excluded hidden entries at the walker layer.
    ///
    /// Setting this to `false` is the runtime-toggleable knob for the
    /// per-pane hidden-file visibility (Cmd+.): the shell always opens
    /// the underlying view model with hidden entries listed, then
    /// applies a filter reflecting the pane's own `show_hidden` state.
    pub include_hidden: bool,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            query: None,
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            regex: None,
            include_hidden: true,
        }
    }
}

/// A compiled, ready-to-apply form of [`Filter`].
///
/// Compile once with [`Filter::compile`], then call [`CompiledFilter::matches`]
/// many times.
#[derive(Clone, Debug)]
pub struct CompiledFilter {
    query: Option<String>,
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
    regex: Option<Regex>,
    include_hidden: bool,
}

impl Filter {
    /// Compile this filter, validating globs and the regex up front.
    ///
    /// # Errors
    ///
    /// Returns an error if any glob or the regex fails to parse.
    pub fn compile(&self) -> Result<CompiledFilter> {
        Ok(CompiledFilter {
            query: self.query.as_ref().map(|q| q.to_ascii_lowercase()),
            include: build_globset(&self.include_globs)?,
            exclude: build_globset(&self.exclude_globs)?,
            regex: match &self.regex {
                Some(pat) => {
                    Some(Regex::new(pat).map_err(|e| {
                        AtlasError::InvalidPath(format!("invalid regex `{pat}`: {e}"))
                    })?)
                }
                None => None,
            },
            include_hidden: self.include_hidden,
        })
    }
}

impl CompiledFilter {
    /// Returns `true` when `entry` satisfies every configured criterion.
    #[must_use]
    pub fn matches(&self, entry: &Entry) -> bool {
        let name = &entry.name;

        // Hidden-file gate — cheap boolean check first.
        if !self.include_hidden && entry.metadata.is_hidden {
            return false;
        }

        if let Some(q) = &self.query {
            if !name.to_ascii_lowercase().contains(q) {
                return false;
            }
        }

        if let Some(inc) = &self.include {
            if !inc.is_match(name) {
                return false;
            }
        }

        if let Some(exc) = &self.exclude {
            if exc.is_match(name) {
                return false;
            }
        }

        if let Some(re) = &self.regex {
            if !re.is_match(name) {
                return false;
            }
        }

        true
    }
}

fn build_globset(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        let glob = Glob::new(pat)
            .map_err(|e| AtlasError::InvalidPath(format!("invalid glob `{pat}`: {e}")))?;
        builder.add(glob);
    }
    let set = builder
        .build()
        .map_err(|e| AtlasError::InvalidPath(format!("failed to build glob set: {e}")))?;
    Ok(Some(set))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::entry::{EntryKind, Metadata};

    use super::*;

    fn entry(name: &str, hidden: bool) -> Entry {
        Entry {
            path: PathBuf::from(name),
            name: name.to_owned(),
            kind: EntryKind::File,
            metadata: Metadata {
                size: 0,
                modified: None,
                created: None,
                accessed: None,
                permissions_mode: None,
                is_hidden: hidden,
            },
        }
    }

    #[test]
    fn default_filter_includes_hidden() {
        let f = Filter::default();
        assert!(
            f.include_hidden,
            "default filter must include hidden entries so the pane-level toggle drives visibility"
        );
        let cf = f.compile().expect("default filter must compile");
        assert!(cf.matches(&entry(".rc", true)));
        assert!(cf.matches(&entry("visible", false)));
    }

    #[test]
    fn filter_hides_hidden_entries_when_include_hidden_false() {
        let f = Filter {
            include_hidden: false,
            ..Filter::default()
        };
        let cf = f.compile().expect("filter must compile");
        assert!(
            !cf.matches(&entry(".rc", true)),
            "hidden entry must be rejected when include_hidden = false"
        );
        assert!(
            cf.matches(&entry("visible", false)),
            "non-hidden entry must still match"
        );
    }

    #[test]
    fn include_hidden_short_circuits_before_other_criteria() {
        // Regression: the hidden gate must run first so we don't allocate
        // a lowercased query string for entries we'll drop anyway.
        let f = Filter {
            query: Some("cache".into()),
            include_hidden: false,
            ..Filter::default()
        };
        let cf = f.compile().expect("filter must compile");
        // Hidden + matches query — still rejected.
        assert!(!cf.matches(&entry(".cache", true)));
    }
}
