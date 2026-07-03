//! Shared helpers used by the atlas-fs criterion benches.
//!
//! Included via `#[path = "common/mod.rs"] mod common;` from each bench
//! target. Provides deterministic synthetic tempdir trees and in-memory
//! `Entry` collections so benchmarks measure the code under test, not
//! the setup.

#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;

use atlas_fs::{Entry, EntryKind, Metadata};
use tempfile::TempDir;

/// Build a temp directory with `count` regular files at the top level.
pub(crate) fn flat_dir(count: usize) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    for i in 0..count {
        let name = format!("file-{i:06}.txt");
        fs::write(root.join(name), b"x").expect("write file");
    }
    dir
}

/// Build a temp tree with `dirs` directories, each containing
/// `files_per_dir` regular files. Total entries:
/// `dirs * files_per_dir + dirs`.
pub(crate) fn tree(dirs: usize, files_per_dir: usize) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    for d in 0..dirs {
        let sub = root.join(format!("dir-{d:04}"));
        fs::create_dir(&sub).expect("create dir");
        for f in 0..files_per_dir {
            fs::write(sub.join(format!("f-{f:04}.txt")), b"x").expect("write");
        }
    }
    dir
}

/// Deterministic synthetic entry set (no filesystem IO).
///
/// Names mix upper/lower-case prefixes, digits, and a rotating set of
/// extensions so sort/filter benchmarks exercise the natural ordering
/// path and the case-insensitive branch realistically.
pub(crate) fn synthetic_entries(n: usize) -> Vec<Entry> {
    const EXTS: &[&str] = &["rs", "toml", "md", "txt", "json", "TXT", "png", "gif"];
    const PREFIXES: &[&str] = &[
        "alpha", "Beta", "gamma", "Delta", "epsilon", "Zeta", "eta", "Theta",
    ];
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let ext = EXTS[i % EXTS.len()];
        let prefix = PREFIXES[i % PREFIXES.len()];
        let name = format!("{prefix}-{i:06}.{ext}");
        let kind = if i % 8 == 0 {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let is_hidden = i % 32 == 0;
        out.push(Entry {
            path: PathBuf::from(&name),
            name,
            kind,
            metadata: Metadata {
                size: (i as u64).wrapping_mul(1_337) % 8_000_000,
                is_hidden,
                ..Metadata::default()
            },
        });
    }
    out
}
