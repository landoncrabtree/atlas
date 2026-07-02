//! Recursive listing helpers for remote backends.
//!
//! [`enumerate_recursive`] walks a subtree by iterative
//! depth-first traversal, calling
//! [`BackendClient::list`](crate::vm::BackendClient::list) once per
//! directory. Entries are returned with paths relative to the
//! requested root so `atlas-ops`'s cross-backend copy loop can
//! reconstruct the destination path without extra string arithmetic.
//!
//! # Latency budget
//!
//! Every directory in the tree costs a network round-trip. For the
//! mock servers used in tests this is negligible (< 5 ms per call).
//! Real SFTP / WebDAV servers may take 50–200 ms per listing; a
//! future phase will add per-server concurrency limits and caching.

use std::sync::Arc;

use crate::error::{RemoteError, RemoteMode, RemoteResult};
use crate::vm::BackendClient;

/// One entry from a recursive walk, relative to the requested root.
#[derive(Debug, Clone)]
pub struct WalkEntry {
    /// Path of this entry relative to the walk root. Empty when the
    /// root itself is reported (only for directories).
    pub relative_path: String,
    /// Coarse kind — file / dir / other.
    pub kind: RemoteMode,
    /// Content length in bytes for files; `0` for directories.
    pub size: u64,
}

/// Recursively enumerate entries under `root`.
///
/// The `root` path is interpreted as a directory. A depth-first
/// traversal expands every discovered subdirectory in turn. The
/// returned list is ordered so parents appear before their children,
/// suitable for a copy loop that mirrors the tree top-down.
///
/// # Errors
///
/// Propagates any [`RemoteError`] surfaced by the backend. The
/// enumeration stops on the first error.
pub async fn enumerate_recursive(
    client: &Arc<dyn BackendClient>,
    root: &str,
) -> RemoteResult<Vec<WalkEntry>> {
    let mut out = Vec::new();
    walk_dir(client, root, "", &mut out).await?;
    Ok(out)
}

fn normalize_dir(path: &str) -> String {
    let trimmed = path.trim_start_matches('/').trim_end_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}

fn join_relative(rel_prefix: &str, name: &str) -> String {
    if rel_prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{rel_prefix}/{name}")
    }
}

fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(idx) => &trimmed[idx + 1..],
        None => trimmed,
    }
}

fn walk_dir<'a>(
    client: &'a Arc<dyn BackendClient>,
    dir_path: &'a str,
    rel_prefix: &'a str,
    out: &'a mut Vec<WalkEntry>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = RemoteResult<()>> + Send + 'a>> {
    Box::pin(async move {
        let list_path = normalize_dir(dir_path);
        let entries = client.list(&list_path).await?;
        for entry in entries {
            let name = basename(&entry.path);
            if name.is_empty() {
                continue;
            }
            // Skip the root marker most backends surface.
            if entry.path.trim_end_matches('/') == list_path.trim_end_matches('/') {
                continue;
            }
            let rel = join_relative(rel_prefix, name);
            match entry.mode {
                RemoteMode::Dir => {
                    out.push(WalkEntry {
                        relative_path: rel.clone(),
                        kind: RemoteMode::Dir,
                        size: 0,
                    });
                    let child_dir = if list_path.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{}{}", list_path, name)
                    };
                    walk_dir(client, &child_dir, &rel, out).await?;
                }
                RemoteMode::File => {
                    out.push(WalkEntry {
                        relative_path: rel,
                        kind: RemoteMode::File,
                        size: entry.size,
                    });
                }
                RemoteMode::Other => {
                    // Skip: e.g. symlinks, character devices — future
                    // phases will handle these explicitly.
                }
            }
        }
        Ok(())
    })
}

#[allow(dead_code)]
fn _unused_error_ref(_: &RemoteError) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_dir_strips_and_appends_slash() {
        assert_eq!(normalize_dir(""), "");
        assert_eq!(normalize_dir("/"), "");
        assert_eq!(normalize_dir("/foo"), "foo/");
        assert_eq!(normalize_dir("foo/bar"), "foo/bar/");
    }

    #[test]
    fn join_relative_produces_relative_paths() {
        assert_eq!(join_relative("", "a.txt"), "a.txt");
        assert_eq!(join_relative("dir", "a.txt"), "dir/a.txt");
        assert_eq!(join_relative("dir/sub", "a.txt"), "dir/sub/a.txt");
    }

    #[test]
    fn basename_returns_last_component() {
        assert_eq!(basename(""), "");
        assert_eq!(basename("foo"), "foo");
        assert_eq!(basename("foo/"), "foo");
        assert_eq!(basename("foo/bar"), "bar");
        assert_eq!(basename("foo/bar/"), "bar");
    }
}
