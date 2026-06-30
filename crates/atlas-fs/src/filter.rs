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
/// matches everything.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// Case-insensitive substring matched against the entry name.
    pub query: Option<String>,
    /// Glob patterns; if non-empty, the name must match at least one.
    pub include_globs: Vec<String>,
    /// Glob patterns; if the name matches any, the entry is excluded.
    pub exclude_globs: Vec<String>,
    /// Regular expression matched against the entry name.
    pub regex: Option<String>,
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
        })
    }
}

impl CompiledFilter {
    /// Returns `true` when `entry` satisfies every configured criterion.
    #[must_use]
    pub fn matches(&self, entry: &Entry) -> bool {
        let name = &entry.name;

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
