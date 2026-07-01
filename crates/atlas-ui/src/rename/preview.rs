//! Preview computation for bulk rename: applies substitution, detects conflicts.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use regex::RegexBuilder;

/// User inputs driving the preview.
#[derive(Clone, Default)]
pub struct Inputs {
    /// The find pattern — a regex or literal string, depending on [`use_regex`].
    pub pattern: String,
    /// The replacement string.  Capture-group references (`$1`, `${name}`)
    /// are expanded in regex mode; they are treated literally in literal mode.
    pub replacement: String,
    /// When `true`, interpret `pattern` as a regular expression.
    /// When `false`, perform a plain string replacement.
    pub use_regex: bool,
    /// Case-insensitive matching.  Applied to both literal and regex modes.
    pub case_insensitive: bool,
}

/// One row in the live preview table.
#[derive(Debug, Clone)]
pub struct PreviewRow {
    /// Full original path.
    pub original: PathBuf,
    /// Proposed new file name (last segment only, no parent directory).
    pub proposed_name: String,
    /// `true` when the proposed name collides with another proposed name or
    /// with an existing sibling that is not part of the rename set.
    pub is_conflict: bool,
    /// `true` when the substitution produces no change.
    pub is_unchanged: bool,
}

/// Compute the preview rows for `original_paths` given `inputs`.
///
/// Returns `(rows, error)`:
/// - On regex compile failure, returns `([], Some(error_message))`.
/// - Otherwise returns `(rows, None)` where each row carries the proposed name
///   plus conflict / unchanged flags.
///
/// This function performs filesystem `exists()` checks for sibling conflicts
/// and must therefore be called from a background thread, never the UI thread.
pub fn compute_preview(
    original_paths: &[PathBuf],
    inputs: &Inputs,
) -> (Vec<PreviewRow>, Option<String>) {
    if original_paths.is_empty() {
        return (Vec::new(), None);
    }

    // Build a single-pass substitution closure.
    //
    // Literal mode: escape the pattern and optionally add `(?i)`.
    // Regex mode:   use the pattern as-is, with `RegexBuilder::case_insensitive`.
    let pattern_str: String;
    let compiled = if inputs.use_regex {
        let result = RegexBuilder::new(&inputs.pattern)
            .case_insensitive(inputs.case_insensitive)
            .build();
        match result {
            Ok(re) => re,
            Err(e) => return (Vec::new(), Some(e.to_string())),
        }
    } else {
        if inputs.pattern.is_empty() {
            // Nothing to replace — all rows will be unchanged.
            return (
                original_paths
                    .iter()
                    .map(|p| {
                        let name = file_name_str(p);
                        PreviewRow {
                            original: p.clone(),
                            proposed_name: name.clone(),
                            is_conflict: false,
                            is_unchanged: true,
                        }
                    })
                    .collect(),
                None,
            );
        }
        // Escape for literal matching.
        pattern_str = regex::escape(&inputs.pattern);
        let result = RegexBuilder::new(&pattern_str)
            .case_insensitive(inputs.case_insensitive)
            .build();
        match result {
            Ok(re) => re,
            Err(e) => return (Vec::new(), Some(e.to_string())),
        }
    };

    // In literal mode the replacement must not expand capture groups.
    // In regex mode the replacement IS allowed to reference groups.
    let replacement = if inputs.use_regex {
        inputs.replacement.as_str()
    } else {
        // `NoExpand` is obtained by passing the replacement directly to
        // `replace_all`; we'll use `regex::NoExpand` wrapper below.
        inputs.replacement.as_str()
    };

    // Apply the substitution to every file name.
    let original_names: Vec<String> = original_paths.iter().map(|p| file_name_str(p)).collect();
    let proposed_names: Vec<String> = original_names
        .iter()
        .map(|name| {
            if inputs.use_regex {
                compiled.replace_all(name, replacement).into_owned()
            } else {
                // Literal: use NoExpand so `$1` in the replacement is literal.
                compiled
                    .replace_all(name, regex::NoExpand(replacement))
                    .into_owned()
            }
        })
        .collect();

    // Detect in-set conflicts: multiple rows mapping to the same proposed name.
    // For each proposed name, collect all row indices that produce it.
    let mut name_to_indices: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, proposed) in proposed_names.iter().enumerate() {
        name_to_indices.entry(proposed.clone()).or_default().push(i);
    }

    let mut is_conflict: Vec<bool> = vec![false; original_paths.len()];
    for indices in name_to_indices.values() {
        if indices.len() > 1 {
            for &i in indices {
                is_conflict[i] = true;
            }
        }
    }

    // Build a set of original absolute paths for fast lookup.
    let original_set: std::collections::HashSet<&Path> =
        original_paths.iter().map(PathBuf::as_path).collect();

    // Detect sibling conflicts: the proposed path already exists on disk and
    // is NOT one of the paths being renamed.
    for (i, orig) in original_paths.iter().enumerate() {
        if original_names[i] == proposed_names[i] {
            // Unchanged — skip the filesystem check.
            continue;
        }
        let proposed_path = orig.with_file_name(&proposed_names[i]);
        if proposed_path.exists() && !original_set.contains(proposed_path.as_path()) {
            is_conflict[i] = true;
        }
    }

    // Assemble the final rows.
    let rows = original_paths
        .iter()
        .enumerate()
        .map(|(i, orig)| PreviewRow {
            original: orig.clone(),
            proposed_name: proposed_names[i].clone(),
            is_conflict: is_conflict[i],
            is_unchanged: original_names[i] == proposed_names[i],
        })
        .collect();

    (rows, None)
}

/// Return the file name as a `String`, falling back to an empty string.
fn file_name_str(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str], dir: &Path) -> Vec<PathBuf> {
        names.iter().map(|n| dir.join(n)).collect()
    }

    #[test]
    fn literal_substitution() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = paths(&["IMG_001.jpg", "IMG_002.jpg"], dir.path());

        let inputs = Inputs {
            pattern: "IMG_".to_owned(),
            replacement: "photo_".to_owned(),
            use_regex: false,
            case_insensitive: false,
        };
        let (rows, err) = compute_preview(&p, &inputs);
        assert!(err.is_none());
        assert_eq!(rows[0].proposed_name, "photo_001.jpg");
        assert_eq!(rows[1].proposed_name, "photo_002.jpg");
        assert!(!rows[0].is_unchanged);
        assert!(!rows[0].is_conflict);
    }

    #[test]
    fn regex_with_capture_reorders_segments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = paths(&["001_foo.txt", "002_bar.txt"], dir.path());

        let inputs = Inputs {
            pattern: r"^(\d+)_(.+)$".to_owned(),
            replacement: "${2}_${1}".to_owned(),
            use_regex: true,
            case_insensitive: false,
        };
        let (rows, err) = compute_preview(&p, &inputs);
        assert!(err.is_none());
        assert_eq!(rows[0].proposed_name, "foo.txt_001");
        assert_eq!(rows[1].proposed_name, "bar.txt_002");
    }

    #[test]
    fn case_insensitive_literal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = paths(&["report.txt", "REPORT_final.txt"], dir.path());

        let inputs = Inputs {
            pattern: "report".to_owned(),
            replacement: "summary".to_owned(),
            use_regex: false,
            case_insensitive: true,
        };
        let (rows, err) = compute_preview(&p, &inputs);
        assert!(err.is_none());
        assert_eq!(rows[0].proposed_name, "summary.txt");
        assert_eq!(rows[1].proposed_name, "summary_final.txt");
    }

    #[test]
    fn conflict_within_rename_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Both files produce the same proposed name.
        let p = paths(&["foo_a.txt", "foo_b.txt"], dir.path());

        let inputs = Inputs {
            pattern: r"_[ab]".to_owned(),
            replacement: "".to_owned(),
            use_regex: true,
            case_insensitive: false,
        };
        let (rows, err) = compute_preview(&p, &inputs);
        assert!(err.is_none());
        assert!(rows[0].is_conflict);
        assert!(rows[1].is_conflict);
    }

    #[test]
    fn conflict_against_existing_sibling() {
        let dir = tempfile::tempdir().expect("tempdir");
        // "existing.txt" is NOT in the rename set but will be the proposed name.
        std::fs::write(dir.path().join("existing.txt"), b"").expect("write");
        let p = paths(&["source.txt"], dir.path());

        let inputs = Inputs {
            pattern: "source".to_owned(),
            replacement: "existing".to_owned(),
            use_regex: false,
            case_insensitive: false,
        };
        let (rows, err) = compute_preview(&p, &inputs);
        assert!(err.is_none());
        assert!(rows[0].is_conflict, "should conflict with existing sibling");
    }

    #[test]
    fn regex_compile_error_surfaces() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = paths(&["file.txt"], dir.path());

        let inputs = Inputs {
            pattern: "[invalid".to_owned(), // unclosed bracket
            replacement: "x".to_owned(),
            use_regex: true,
            case_insensitive: false,
        };
        let (rows, err) = compute_preview(&p, &inputs);
        assert!(rows.is_empty());
        assert!(err.is_some(), "expected regex compile error");
    }

    #[test]
    fn unchanged_rows_flagged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = paths(&["nomatch.txt"], dir.path());

        let inputs = Inputs {
            pattern: "xyz".to_owned(),
            replacement: "abc".to_owned(),
            use_regex: false,
            case_insensitive: false,
        };
        let (rows, err) = compute_preview(&p, &inputs);
        assert!(err.is_none());
        assert!(rows[0].is_unchanged);
        assert!(!rows[0].is_conflict);
    }
}
