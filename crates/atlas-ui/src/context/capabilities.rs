//! [`ContextTarget`] and [`ContextCapabilities`] — the two data
//! structures behind the capability-aware context menu.
//!
//! See [`super`] for the design overview and extension recipe.

use atlas_core::{BackendKind, Location};
use atlas_fs::EntryKind;

/// Description of the entry the user right-clicked on.
///
/// Feeds [`crate::shell::AppShell::context_capabilities_for`] which
/// converts it into a [`ContextCapabilities`] bitset. The individual
/// `ctx_*` handlers then read the target back off the shell to
/// dispatch the action against the specific entry.
#[derive(Debug, Clone)]
pub struct ContextTarget {
    /// The entry's location (local `PathBuf` or remote URI). This is
    /// what the action operates on — not the pane's current cwd.
    pub location: Location,
    /// What kind of filesystem object was clicked.
    pub entry_kind: EntryKind,
    /// Whether the mount / session hosting this entry is known to
    /// accept writes. Read-only remote mounts (e.g. `test.rebex.net`)
    /// or filesystems mounted `ro` set this to `false`, which
    /// disables destructive actions like Rename / Trash / Paste
    /// through the capability resolver.
    ///
    /// Current model: local entries are always writable; remote entries are
    /// writable unless the caller can positively identify a
    /// read-only host. The upload-on-write flow surfaces per-op
    /// failures separately (see [`crate::remote::preview_watch`]).
    pub is_writable: bool,
    /// Backend that owns the entry. `BackendKind::Local` for local
    /// entries; SFTP / FTP / WebDAV / S3 for remotes. Used by the
    /// resolver to decide `can_show_in_native_manager` and
    /// `can_copy_remote_uri`.
    pub backend_kind: BackendKind,
}

impl ContextTarget {
    /// Convenience: is this a remote target?
    #[must_use]
    pub fn is_remote(&self) -> bool {
        matches!(self.location, Location::Remote(_, _))
    }

    /// Convenience: is this a directory?
    #[must_use]
    pub fn is_dir(&self) -> bool {
        matches!(self.entry_kind, EntryKind::Dir)
    }
}

/// Which actions apply to a given [`ContextTarget`].
///
/// One flag per menu item. Slint bindings on the static menu items
/// key off these flags via `visible:` bindings. The order of fields
/// mirrors the order the items appear in the menu itself.
///
/// New capabilities: see the extension recipe in [`super`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextCapabilities {
    /// "Open" — activate the entry (file → OS default handler /
    /// preview; directory → navigate).
    pub can_open: bool,
    /// "Open With…" — spawns the platform-native application picker
    /// (macOS *Choose Application*, Windows *Open With* shell dialog,
    /// Linux `mimeopen -a` / `xdg-open`). Local entries route the
    /// picker directly to the on-disk path; remote entries first
    /// materialise the file through the preview cache
    /// ([`crate::remote::PreviewCache::open_remote_file_with`]) and
    /// then hand the cached local path to the picker.
    pub can_open_with: bool,
    /// "Copy" — copy the selection into the internal clipboard.
    pub can_copy: bool,
    /// "Cut" — cut requires a writable mount (can't cut what we
    /// can't delete on paste).
    pub can_cut: bool,
    /// "Paste" — paste into the target's *parent* location; requires
    /// a writable mount.
    pub can_paste: bool,
    /// "Rename" — writable mounts only.
    pub can_rename: bool,
    /// "Move to Trash" (local) / "Delete permanently" (remote):
    /// destructive, writable mounts only.
    pub can_trash: bool,
    /// "Duplicate" — same-directory copy-with-suffix. Writable
    /// mounts only.
    pub can_duplicate: bool,
    /// "Show in Finder" / "Show in Explorer" / "Show in Files" —
    /// local targets only (or remotes with a local mount, which is
    /// a v0.6+ concern).
    pub can_show_in_native_manager: bool,
    /// "Copy Remote URI" — remote targets only. Copies
    /// `sftp://user@host/path` to the OS clipboard.
    pub can_copy_remote_uri: bool,
    /// "Copy Path" — copies an absolute local path (Local) or an
    /// `sftp://…` URI (Remote) to the OS clipboard.
    pub can_copy_shell_path: bool,
    /// "Reveal in New Pane" — opens a new split pane pointing at
    /// the entry's location. Available for both local and remote.
    pub can_reveal_in_new_pane: bool,
    /// "Get Info" — always available (informational).
    pub can_get_info: bool,
}

impl ContextCapabilities {
    /// Compute the capability set for a given [`ContextTarget`].
    ///
    /// TODO(plugins): expose this as a public trait
    /// (`ContextCapabilityProvider`) so plugins can add per-context
    /// items in v0.6+. The static flag surface + the extension
    /// recipe in [`super`] stay stable — plugins just get a hook to
    /// contribute additional `can_*` decisions.
    #[must_use]
    pub fn resolve(target: &ContextTarget) -> Self {
        let is_local = matches!(target.location, Location::Local(_));
        let is_remote = matches!(target.location, Location::Remote(_, _));
        let is_writable = target.is_writable;
        // Broken symlinks: nothing to open / copy / move / rename;
        // only "Get Info" makes sense.
        let is_broken_symlink =
            matches!(target.entry_kind, EntryKind::Symlink { broken: true, .. });
        if is_broken_symlink {
            return Self {
                can_get_info: true,
                ..Self::default()
            };
        }
        Self {
            can_open: true,
            can_open_with: true,
            can_copy: true,
            can_cut: is_writable,
            can_paste: is_writable,
            can_rename: is_writable,
            can_trash: is_writable,
            can_duplicate: is_writable,
            can_show_in_native_manager: is_local,
            can_copy_remote_uri: is_remote,
            can_copy_shell_path: true,
            can_reveal_in_new_pane: true,
            can_get_info: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn target_local_file() -> ContextTarget {
        ContextTarget {
            location: Location::Local(PathBuf::from("/tmp/foo.txt")),
            entry_kind: EntryKind::File,
            is_writable: true,
            backend_kind: BackendKind::Local,
        }
    }

    fn target_local_dir() -> ContextTarget {
        ContextTarget {
            location: Location::Local(PathBuf::from("/tmp/dir")),
            entry_kind: EntryKind::Dir,
            is_writable: true,
            backend_kind: BackendKind::Local,
        }
    }

    fn target_remote_file(writable: bool) -> ContextTarget {
        let uri = atlas_core::RemoteUri {
            scheme: "sftp".into(),
            host: Some("host".into()),
            port: None,
            username: Some("u".into()),
            path: "/f".into(),
            credential_ref: None,
        };
        ContextTarget {
            location: Location::Remote(uri, BackendKind::Sftp),
            entry_kind: EntryKind::File,
            is_writable: writable,
            backend_kind: BackendKind::Sftp,
        }
    }

    fn target_remote_dir() -> ContextTarget {
        let uri = atlas_core::RemoteUri {
            scheme: "sftp".into(),
            host: Some("host".into()),
            port: None,
            username: Some("u".into()),
            path: "/d".into(),
            credential_ref: None,
        };
        ContextTarget {
            location: Location::Remote(uri, BackendKind::Sftp),
            entry_kind: EntryKind::Dir,
            is_writable: true,
            backend_kind: BackendKind::Sftp,
        }
    }

    fn target_broken_symlink() -> ContextTarget {
        let uri = atlas_core::RemoteUri {
            scheme: "sftp".into(),
            host: Some("host".into()),
            port: None,
            username: Some("u".into()),
            path: "/dangling".into(),
            credential_ref: None,
        };
        ContextTarget {
            location: Location::Remote(uri, BackendKind::Sftp),
            entry_kind: EntryKind::Symlink {
                target: Some(PathBuf::from("/nowhere")),
                broken: true,
            },
            is_writable: true,
            backend_kind: BackendKind::Sftp,
        }
    }

    #[test]
    fn local_file_has_full_writable_menu_but_no_remote_uri() {
        let caps = ContextCapabilities::resolve(&target_local_file());
        assert!(caps.can_open);
        assert!(caps.can_open_with);
        assert!(caps.can_copy);
        assert!(caps.can_cut);
        assert!(caps.can_paste);
        assert!(caps.can_rename);
        assert!(caps.can_trash);
        assert!(caps.can_duplicate);
        assert!(caps.can_show_in_native_manager);
        assert!(!caps.can_copy_remote_uri);
        assert!(caps.can_copy_shell_path);
        assert!(caps.can_reveal_in_new_pane);
        assert!(caps.can_get_info);
    }

    #[test]
    fn local_dir_matches_local_file_capability_set() {
        // The resolver doesn't currently branch on File-vs-Dir at
        // the local layer; both get the same menu.
        assert_eq!(
            ContextCapabilities::resolve(&target_local_file()),
            ContextCapabilities::resolve(&target_local_dir()),
        );
    }

    #[test]
    fn remote_file_writable_gets_copy_remote_uri_no_native_reveal() {
        let caps = ContextCapabilities::resolve(&target_remote_file(true));
        assert!(caps.can_copy_remote_uri);
        assert!(!caps.can_show_in_native_manager);
        assert!(
            caps.can_open_with,
            "remote Open With materialises via the preview cache"
        );
        assert!(caps.can_trash);
        assert!(caps.can_rename);
    }

    #[test]
    fn remote_file_readonly_disables_destructive_actions() {
        let caps = ContextCapabilities::resolve(&target_remote_file(false));
        assert!(caps.can_open, "open still available on read-only");
        assert!(caps.can_copy, "copy always available");
        assert!(!caps.can_cut);
        assert!(!caps.can_paste);
        assert!(!caps.can_rename);
        assert!(!caps.can_trash);
        assert!(!caps.can_duplicate);
        assert!(caps.can_copy_remote_uri);
        assert!(caps.can_copy_shell_path);
        assert!(
            caps.can_open_with,
            "Open With works on read-only remotes too — user picks an app to read the cached copy"
        );
    }

    #[test]
    fn remote_dir_writable_gets_full_menu_minus_native_reveal() {
        let caps = ContextCapabilities::resolve(&target_remote_dir());
        assert!(caps.can_open);
        assert!(caps.can_copy);
        assert!(caps.can_paste);
        assert!(caps.can_rename);
        assert!(caps.can_reveal_in_new_pane);
        assert!(!caps.can_show_in_native_manager);
        // Directories can't be piped through Open With — the picker
        // is file-oriented — but the capability resolver doesn't
        // branch on kind today; the shell dispatch layer refuses
        // remote directories before spawning the materialisation.
    }

    #[test]
    fn broken_symlink_only_offers_get_info() {
        let caps = ContextCapabilities::resolve(&target_broken_symlink());
        assert!(caps.can_get_info, "get info is the only survivor");
        assert!(!caps.can_open);
        assert!(!caps.can_copy);
        assert!(!caps.can_rename);
        assert!(!caps.can_trash);
        assert!(!caps.can_copy_remote_uri);
    }
}
