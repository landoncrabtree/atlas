//! Path utilities used across crates.

use std::path::{Path, PathBuf};

/// Expand a leading `~` to the user's home directory if present.
pub fn expand_tilde(input: impl AsRef<Path>) -> PathBuf {
    let p = input.as_ref();
    let Some(stripped) = p.strip_prefix("~").ok() else {
        return p.to_path_buf();
    };
    match directories_home() {
        Some(home) => home.join(stripped),
        None => p.to_path_buf(),
    }
}

fn directories_home() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
}
