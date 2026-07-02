//! Pure resolver helpers for remote-aware navigation.
//!
//! [`AppShell`] wires Slint callbacks to workspace state, which makes
//! its methods awkward to unit-test in isolation. The transformations
//! that drive `fs::View`, the address bar, breadcrumb clicks, and the
//! Go-Up action are pure functions: given the pane's current
//! [`Location`] and an operand (entry, raw input string, breadcrumb
//! index), they produce the destination [`Location`].
//!
//! Extracting them here lets the shell just wire the plumbing and gives
//! us a battery of cheap unit tests for the [`Location::Local`]-vs-
//! [`Location::Remote`] fork that caused the original `open::that("pub")`
//! bug.
//!
//! [`AppShell`]: crate::shell::AppShell

use std::path::{Path, PathBuf};

use atlas_core::Location;
use atlas_fs::Entry;

/// Given the pane's current [`Location`] and a listed [`Entry`],
/// produce the destination [`Location`] the entry activates to.
///
/// - Local panes → the entry's absolute `path` unchanged.
/// - Remote panes → the pane's URI joined with `entry.name`, so
///   `sftp://demo@host/` + `pub` becomes `sftp://demo@host/pub`.
///   Falling back to `entry.name` avoids the bug where the SFTP
///   backend's basename-only `entry.path` (e.g. `"pub"`) gets shoved
///   into `open::that`.
#[must_use]
pub fn resolve_entry_location(pane_loc: &Location, entry: &Entry) -> Location {
    match pane_loc {
        Location::Local(_) => Location::Local(entry.path.clone()),
        Location::Remote(_, _) => pane_loc.join(&entry.name),
    }
}

/// Compute the destination for a breadcrumb click on pane whose
/// current [`Location`] is `current`. `segment_index` is 0-based; the
/// 0-th segment on a remote pane is the URI authority (e.g.
/// `sftp://demo@host`), and subsequent segments are `/`-delimited path
/// components. On local panes segments correspond directly to
/// [`std::path::Component`]s.
///
/// Returns `None` if the requested segment is out of range (i.e. the
/// index points past the current path's depth).
#[must_use]
pub fn breadcrumb_target(current: &Location, segment_index: usize) -> Option<Location> {
    match current {
        Location::Local(path) => {
            let components: Vec<_> = path.components().collect();
            if segment_index >= components.len() {
                return None;
            }
            let mut target = PathBuf::new();
            for component in &components[..=segment_index] {
                target.push(component);
            }
            Some(Location::Local(target))
        }
        Location::Remote(uri, kind) => {
            let segments: Vec<&str> = uri.path.split('/').filter(|s| !s.is_empty()).collect();
            if segment_index == 0 {
                let mut new_uri = uri.clone();
                new_uri.path = "/".into();
                Some(Location::Remote(new_uri, *kind))
            } else if segment_index <= segments.len() {
                let mut path = String::from("/");
                for (i, seg) in segments.iter().enumerate().take(segment_index) {
                    if i > 0 {
                        path.push('/');
                    }
                    path.push_str(seg);
                }
                let mut new_uri = uri.clone();
                new_uri.path = path;
                Some(Location::Remote(new_uri, *kind))
            } else {
                None
            }
        }
    }
}

/// Parse an address-bar submission.
///
/// - Inputs with an explicit scheme (`://`) parse as [`Location`]
///   verbatim.
/// - Otherwise, if the focused pane is a [`Location::Remote`], the
///   raw input is treated as a same-host navigation on that URI —
///   absolute (`/pub`) replaces the URI path outright, relative
///   (`downloads/foo`) joins onto the current URI path.
/// - Otherwise the input is expanded (via `expand_tilde`) into a
///   [`Location::Local`].
///
/// Returns `None` when a scheme-carrying input fails to parse or when
/// the input is empty on a remote pane.
#[must_use]
pub fn parse_address_input(
    input: &str,
    pane_loc: Option<&Location>,
    expand_tilde: impl FnOnce(&Path) -> PathBuf,
) -> Option<Location> {
    if input.contains("://") {
        return input.parse::<Location>().ok();
    }
    if let Some(Location::Remote(uri, kind)) = pane_loc {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return None;
        }
        let mut new_uri = uri.clone();
        if trimmed.starts_with('/') {
            new_uri.path = trimmed.to_owned();
        } else {
            let base = new_uri.path.trim_end_matches('/');
            new_uri.path = if base.is_empty() {
                format!("/{trimmed}")
            } else {
                format!("{base}/{trimmed}")
            };
        }
        return Some(Location::Remote(new_uri, *kind));
    }
    Some(Location::Local(expand_tilde(Path::new(input))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::{BackendKind, RemoteUri};
    use atlas_fs::{Entry, EntryKind, Metadata};
    use std::path::PathBuf;

    fn remote_root() -> Location {
        Location::Remote(
            RemoteUri {
                scheme: "sftp".into(),
                host: Some("demo.test".into()),
                port: Some(22),
                username: Some("demo".into()),
                path: "/".into(),
                credential_ref: None,
            },
            BackendKind::Sftp,
        )
    }

    fn remote_at(path: &str) -> Location {
        Location::Remote(
            RemoteUri {
                scheme: "sftp".into(),
                host: Some("demo.test".into()),
                port: Some(22),
                username: Some("demo".into()),
                path: path.into(),
                credential_ref: None,
            },
            BackendKind::Sftp,
        )
    }

    fn stub_entry(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: name.to_owned(),
            path: PathBuf::from(name),
            kind,
            metadata: Metadata::default(),
        }
    }

    #[test]
    fn resolver_joins_remote_pane_with_entry_name() {
        let pane = remote_root();
        let entry = stub_entry("pub", EntryKind::Dir);

        let dest = resolve_entry_location(&pane, &entry);

        match dest {
            Location::Remote(uri, kind) => {
                assert_eq!(uri.path, "/pub");
                assert_eq!(kind, BackendKind::Sftp);
                assert_eq!(uri.host.as_deref(), Some("demo.test"));
            }
            Location::Local(_) => panic!("expected remote"),
        }
    }

    #[test]
    fn resolver_returns_absolute_local_path_for_local_pane() {
        let pane = Location::Local(PathBuf::from("/usr"));
        let entry = Entry {
            name: "bin".into(),
            path: PathBuf::from("/usr/bin"),
            kind: EntryKind::Dir,
            metadata: Metadata::default(),
        };

        assert_eq!(
            resolve_entry_location(&pane, &entry),
            Location::Local(PathBuf::from("/usr/bin"))
        );
    }

    #[test]
    fn resolver_ignores_basename_only_path_on_remote() {
        // Regression guard for the reported bug: SFTP `list("")` emits
        // Entry { name: "readme.txt", path: PathBuf::from("readme.txt") },
        // which the old shell fed to `open::that("readme.txt")`.
        let pane = remote_at("/pub");
        let entry = stub_entry("readme.txt", EntryKind::File);

        let dest = resolve_entry_location(&pane, &entry);

        match dest {
            Location::Remote(uri, _) => assert_eq!(uri.path, "/pub/readme.txt"),
            Location::Local(_) => panic!("must not fall back to local"),
        }
    }

    #[test]
    fn breadcrumb_root_click_returns_uri_root() {
        let current = remote_at("/pub/example/nested");
        let dest = breadcrumb_target(&current, 0).expect("root segment is always valid");
        match dest {
            Location::Remote(uri, _) => assert_eq!(uri.path, "/"),
            Location::Local(_) => panic!("expected remote"),
        }
    }

    #[test]
    fn breadcrumb_intermediate_segment_trims_uri_path() {
        let current = remote_at("/pub/example/nested");
        let dest = breadcrumb_target(&current, 1).expect("first path segment is valid");
        match dest {
            Location::Remote(uri, _) => assert_eq!(uri.path, "/pub"),
            Location::Local(_) => panic!("expected remote"),
        }
    }

    #[test]
    fn breadcrumb_out_of_range_returns_none() {
        let current = remote_at("/pub");
        assert!(breadcrumb_target(&current, 99).is_none());
    }

    #[test]
    fn breadcrumb_local_pane_uses_path_components() {
        let current = Location::Local(PathBuf::from("/usr/local/bin"));
        let dest = breadcrumb_target(&current, 2).expect("valid segment");
        assert_eq!(dest, Location::Local(PathBuf::from("/usr/local")));
    }

    #[test]
    fn address_input_scheme_parses_verbatim() {
        let dest = parse_address_input(
            "sftp://user@host:22/pub",
            Some(&Location::Local(PathBuf::from("/tmp"))),
            |p| p.to_path_buf(),
        );
        match dest {
            Some(Location::Remote(uri, kind)) => {
                assert_eq!(kind, BackendKind::Sftp);
                assert_eq!(uri.host.as_deref(), Some("host"));
                assert_eq!(uri.path, "/pub");
            }
            other => panic!("expected sftp Remote, got {other:?}"),
        }
    }

    #[test]
    fn address_input_relative_on_remote_pane_stays_remote() {
        let pane = remote_at("/pub");
        let dest = parse_address_input("example", Some(&pane), |p| p.to_path_buf());
        match dest {
            Some(Location::Remote(uri, _)) => assert_eq!(uri.path, "/pub/example"),
            other => panic!("expected remote-relative join, got {other:?}"),
        }
    }

    #[test]
    fn address_input_absolute_on_remote_pane_replaces_path() {
        let pane = remote_at("/pub");
        let dest = parse_address_input("/etc", Some(&pane), |p| p.to_path_buf());
        match dest {
            Some(Location::Remote(uri, _)) => assert_eq!(uri.path, "/etc"),
            other => panic!("expected remote absolute path, got {other:?}"),
        }
    }

    #[test]
    fn address_input_bare_path_on_local_pane_is_local() {
        let pane = Location::Local(PathBuf::from("/tmp"));
        let dest = parse_address_input("~/Downloads", Some(&pane), |p| {
            PathBuf::from("/home/demo").join(p.strip_prefix("~/").unwrap_or(p))
        });
        match dest {
            Some(Location::Local(p)) => assert_eq!(p, PathBuf::from("/home/demo/Downloads")),
            other => panic!("expected local, got {other:?}"),
        }
    }

    #[test]
    fn address_input_empty_on_remote_returns_none() {
        let pane = remote_at("/pub");
        assert!(parse_address_input("   ", Some(&pane), |p| p.to_path_buf()).is_none());
    }

    /// `go_up` on a remote pane is `Location::parent()` funneled
    /// through the shell's [`navigate_pane_to_location`]; unit-check
    /// that parenting a remote URI stays remote (regression guard
    /// for the class of bugs where `AppShell::go_up` reached for
    /// [`Path::parent`] instead).
    #[test]
    fn go_up_on_remote_pane_stays_remote() {
        let deep = remote_at("/pub/example/nested");
        let up = deep.parent().expect("remote paths have parents until root");
        match up {
            Location::Remote(uri, kind) => {
                assert_eq!(kind, BackendKind::Sftp);
                assert_eq!(uri.path, "/pub/example");
                assert_eq!(uri.host.as_deref(), Some("demo.test"));
            }
            Location::Local(p) => panic!("go_up leaked to local path {p:?}"),
        }
    }
}
