//! Sorting primitives: [`SortKey`], [`SortOrder`], [`SortSpec`], and
//! comparators that order [`Entry`] values.
//!
//! Includes a dependency-free natural ordering implementation so that names
//! like `file2` sort before `file10`.

use std::cmp::Ordering;

use crate::entry::Entry;

/// The field used to order entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    /// Order by file name.
    Name,
    /// Order by size in bytes.
    Size,
    /// Order by last-modified time.
    Modified,
    /// Order by entry kind (dir/file/symlink/other).
    Kind,
    /// Order by file extension.
    Extension,
}

/// Ascending or descending order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortOrder {
    /// Smallest/earliest/`a..z` first.
    Asc,
    /// Largest/latest/`z..a` first.
    Desc,
}

/// A full sort specification.
#[derive(Clone, Debug)]
pub struct SortSpec {
    /// The primary field to sort by.
    pub key: SortKey,
    /// Ascending or descending.
    pub order: SortOrder,
    /// When `true`, directories are grouped before non-directories regardless
    /// of `order`.
    pub dirs_first: bool,
    /// When `true`, use natural numeric ordering for [`SortKey::Name`].
    pub natural: bool,
    /// When `true`, name/extension comparison ignores ASCII case.
    pub case_insensitive: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            key: SortKey::Name,
            order: SortOrder::Asc,
            dirs_first: true,
            natural: true,
            case_insensitive: true,
        }
    }
}

/// Compare two entries according to `spec`.
///
/// When `spec.dirs_first` is set, directories always sort before
/// non-directories; the `order` flips the remaining comparison only.
#[must_use]
pub fn compare(a: &Entry, b: &Entry, spec: &SortSpec) -> Ordering {
    if spec.dirs_first {
        let (ad, bd) = (a.kind.is_dir(), b.kind.is_dir());
        if ad != bd {
            return if ad {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
    }

    let base = match spec.key {
        SortKey::Name => compare_names(&a.name, &b.name, spec),
        SortKey::Size => a.metadata.size.cmp(&b.metadata.size),
        SortKey::Modified => a.metadata.modified.cmp(&b.metadata.modified),
        SortKey::Kind => a.kind.sort_rank().cmp(&b.kind.sort_rank()),
        SortKey::Extension => {
            // `Entry::extension()` returns a borrow when the name's
            // extension is already lowercase ASCII — no per-comparison
            // allocation in the common case.
            let ae = a.extension();
            let be = b.extension();
            compare_opt_str(ae.as_deref(), be.as_deref())
                .then_with(|| compare_names(&a.name, &b.name, spec))
        }
    };

    // Stable tie-break by name so equal keys produce a deterministic order.
    let base = base.then_with(|| compare_names(&a.name, &b.name, spec));

    match spec.order {
        SortOrder::Asc => base,
        SortOrder::Desc => base.reverse(),
    }
}

/// Sort `entries` in place using `spec`.
pub fn sort_in_place(entries: &mut [Entry], spec: &SortSpec) {
    entries.sort_by(|a, b| compare(a, b, spec));
}

fn compare_opt_str(a: Option<&str>, b: Option<&str>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => a.cmp(b),
    }
}

fn compare_names(a: &str, b: &str, spec: &SortSpec) -> Ordering {
    if spec.natural {
        natural_cmp(a, b, spec.case_insensitive)
    } else if spec.case_insensitive {
        // Fold on the fly instead of `to_ascii_lowercase().cmp(&…)`,
        // which allocated two Strings per comparison. Non-ASCII bytes
        // are left as-is, matching the previous behaviour.
        ascii_case_insensitive_cmp(a, b)
    } else {
        a.cmp(b)
    }
}

fn ascii_case_insensitive_cmp(a: &str, b: &str) -> Ordering {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let n = ab.len().min(bb.len());
    for i in 0..n {
        let ca = ab[i].to_ascii_lowercase();
        let cb = bb[i].to_ascii_lowercase();
        match ca.cmp(&cb) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    ab.len().cmp(&bb.len())
}

/// Natural ordering: compares strings segment by segment, treating runs of
/// ASCII digits as numbers and other runs as (optionally case-insensitive)
/// bytes. This makes `file2` sort before `file10`.
fn natural_cmp(a: &str, b: &str, case_insensitive: bool) -> Ordering {
    let mut ai = a.bytes().peekable();
    let mut bi = b.bytes().peekable();

    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let na = take_number(&mut ai);
                    let nb = take_number(&mut bi);
                    match na.cmp(&nb) {
                        Ordering::Equal => continue,
                        other => return other,
                    }
                } else {
                    let ca = normalize(ca, case_insensitive);
                    let cb = normalize(cb, case_insensitive);
                    match ca.cmp(&cb) {
                        Ordering::Equal => {
                            ai.next();
                            bi.next();
                        }
                        other => return other,
                    }
                }
            }
        }
    }
}

fn normalize(c: u8, case_insensitive: bool) -> u8 {
    if case_insensitive {
        c.to_ascii_lowercase()
    } else {
        c
    }
}

fn take_number(it: &mut std::iter::Peekable<std::str::Bytes<'_>>) -> u128 {
    let mut value: u128 = 0;
    while let Some(&c) = it.peek() {
        if c.is_ascii_digit() {
            value = value
                .saturating_mul(10)
                .saturating_add(u128::from(c - b'0'));
            it.next();
        } else {
            break;
        }
    }
    value
}
