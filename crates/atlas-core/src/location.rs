//! [`Location`] — a unified identifier for a filesystem-like address that
//! Atlas can navigate to, whether it lives on the local disk or on a remote
//! backend (SFTP, S3, WebDAV, FTP, …).
//!
//! # Design
//!
//! Locations are addressed by either a native [`PathBuf`] (the fast, local
//! path used by 99% of interactive navigation today) or a parsed
//! [`RemoteUri`] tagged with a [`BackendKind`]. The two variants live behind
//! the same type so cross-cutting APIs (`AppShell`, `NavigationController`,
//! `TabModel`, breadcrumb rendering, …) can accept any location without
//! caring which backend it belongs to. Callers that are demonstrably
//! local-only (thumbnails, native trash, os-clipboard copy) reach for
//! [`Location::as_local`] to recover the [`Path`].
//!
//! # Canonical URI grammar
//!
//! - `local:///Users/x/Downloads` — an explicit local URI.
//! - `sftp://user@host:22/var/log` — SFTP with user, host, port, path.
//! - `s3://bucket/prefix/key` — S3 with the bucket in the URI host slot.
//! - `webdav://user@cloud.example.com/dav/root` — WebDAV.
//! - `ftp://anon@ftp.example.com/pub` — FTP.
//!
//! Bare local paths (`/foo`, `~/foo`, `./foo`, `foo/bar`, `C:\foo`) parse as
//! [`Location::Local`] — Atlas keeps the historical shell-style input mode
//! for the address bar. Serialising a [`Location::Local`] round-trips
//! through the canonical `local://` form so session state files are
//! unambiguous even for weird paths.
//!
//! # Backwards compatibility
//!
//! Session state used to persist locations as bare `PathBuf` strings (for
//! example `/Users/alice/Downloads`). The [`serde::Deserialize`]
//! implementation is untagged: any string, whether it is a URI or a raw
//! path, deserialises to the corresponding [`Location`]. Existing
//! `~/.config/atlas/state.toml` files therefore continue to load with no
//! migration.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Identifies the concrete backend that owns a [`Location`].
///
/// The variants mirror `atlas-remote::BackendKind`; this copy in
/// `atlas-core` exists so leaf crates that only need to reason about
/// locations do not have to depend on `atlas-remote`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// Native local filesystem.
    Local,
    /// SSH filesystem via SFTP.
    Sftp,
    /// FTP / FTPS.
    Ftp,
    /// WebDAV over HTTP(S).
    WebDav,
    /// Amazon S3 or an S3-compatible endpoint.
    S3,
}

impl BackendKind {
    /// The scheme string this backend advertises in a canonical URI.
    #[must_use]
    pub fn scheme(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Sftp => "sftp",
            Self::Ftp => "ftp",
            Self::WebDav => "webdav",
            Self::S3 => "s3",
        }
    }

    /// Parse a scheme string into a [`BackendKind`], case-insensitively.
    #[must_use]
    pub fn from_scheme(scheme: &str) -> Option<Self> {
        match scheme.to_ascii_lowercase().as_str() {
            "local" | "file" => Some(Self::Local),
            "sftp" | "ssh" => Some(Self::Sftp),
            "ftp" | "ftps" => Some(Self::Ftp),
            "webdav" | "dav" | "webdavs" => Some(Self::WebDav),
            "s3" => Some(Self::S3),
            _ => None,
        }
    }
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.scheme())
    }
}

/// A parsed remote URI: scheme + optional userinfo + optional host/port +
/// absolute path + optional keychain reference.
///
/// Constructed by [`Location::from_str`] or by callers assembling a URI
/// from Connect-modal form fields. The `credential_ref` field is opaque to
/// this crate — it is populated by `atlas-remote::secrets::store` and
/// consumed by `atlas-remote::secrets::retrieve`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RemoteUri {
    /// Scheme (`sftp`, `s3`, `webdav`, `ftp`). Always lowercase.
    pub scheme: String,
    /// Host (or S3 bucket). `None` for endpoints that resolve out of
    /// band (for example an S3 endpoint carried entirely in
    /// `credential_ref`).
    pub host: Option<String>,
    /// TCP port, if the scheme has a numeric port and one was specified.
    pub port: Option<u16>,
    /// Username portion of `userinfo`. Never contains the password.
    pub username: Option<String>,
    /// Absolute path within the remote namespace. Always begins with `/`
    /// for the root, may be empty for schemes where the host itself is the
    /// entire address (some S3 configurations).
    pub path: String,
    /// Opaque keychain lookup key resolving to the secret material
    /// (password, IAM secret, etc.). Serialised so that saved workspaces
    /// remember which credential a pane was using.
    pub credential_ref: Option<String>,
}

impl RemoteUri {
    /// Construct a minimal remote URI for `scheme://path` with no host or
    /// credentials — useful for tests and for schemes like `s3://bucket`
    /// where the bucket lives in the URI path.
    #[must_use]
    pub fn new(scheme: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            scheme: scheme.into(),
            host: None,
            port: None,
            username: None,
            path: path.into(),
            credential_ref: None,
        }
    }

    /// Render the canonical URI (without any password material). This is
    /// the form used in address bars, breadcrumbs, and serde round-trips.
    #[must_use]
    pub fn to_uri(&self) -> String {
        let mut out = String::with_capacity(self.scheme.len() + self.path.len() + 16);
        out.push_str(&self.scheme);
        out.push_str("://");
        if let Some(user) = &self.username {
            out.push_str(user);
            out.push('@');
        }
        if let Some(host) = &self.host {
            out.push_str(host);
            if let Some(port) = self.port {
                out.push(':');
                out.push_str(&port.to_string());
            }
        }
        if !self.path.is_empty() && !self.path.starts_with('/') {
            out.push('/');
        }
        out.push_str(&self.path);
        out
    }
}

impl fmt::Display for RemoteUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_uri())
    }
}

/// A location Atlas can navigate to, either on the local disk or on a
/// remote backend.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Location {
    /// Native local filesystem path.
    Local(PathBuf),
    /// Remote address (SFTP, S3, WebDAV, FTP …) tagged with the backend
    /// that services it. The [`BackendKind`] is intentionally denormalised
    /// (it duplicates the scheme in [`RemoteUri::scheme`]) so consumers
    /// can dispatch on it without reparsing the URI.
    Remote(RemoteUri, BackendKind),
}

impl Location {
    /// Construct a [`Location::Local`] from any path-like value.
    #[must_use]
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self::Local(path.into())
    }

    /// Borrow the inner [`Path`] when this is a [`Location::Local`].
    ///
    /// Callers on the hot local-only path reach for this instead of
    /// pattern-matching. Returns `None` for remote locations.
    #[must_use]
    pub fn as_local(&self) -> Option<&Path> {
        match self {
            Self::Local(path) => Some(path.as_path()),
            Self::Remote(_, _) => None,
        }
    }

    /// Consume `self` and return the inner [`PathBuf`] when this is a
    /// [`Location::Local`], falling back to `None` on remote.
    #[must_use]
    pub fn into_local(self) -> Option<PathBuf> {
        match self {
            Self::Local(path) => Some(path),
            Self::Remote(_, _) => None,
        }
    }

    /// The backend that services this location.
    #[must_use]
    pub fn backend(&self) -> BackendKind {
        match self {
            Self::Local(_) => BackendKind::Local,
            Self::Remote(_, kind) => *kind,
        }
    }

    /// Returns `true` when this is a [`Location::Local`].
    #[must_use]
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local(_))
    }

    /// Returns `true` when this is a [`Location::Remote`].
    #[must_use]
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote(_, _))
    }

    /// A user-facing string suitable for the address bar and breadcrumb
    /// bar.
    ///
    /// For local locations this is the native path (`/Users/x/Downloads`
    /// on Unix, `C:\Users\x` on Windows). For remote locations it is the
    /// canonical URI (`sftp://user@host/var/log`). Never includes
    /// password material.
    #[must_use]
    pub fn display_path(&self) -> String {
        match self {
            Self::Local(path) => path.to_string_lossy().into_owned(),
            Self::Remote(uri, _) => uri.to_uri(),
        }
    }

    /// Split the location into breadcrumb segments. For local paths this
    /// is the component list (matching the historical `PaneModel::path_segments`
    /// behaviour). For remote paths the first segment is the URI root
    /// (`sftp://user@host`) and the remaining segments are the path
    /// components — this keeps the breadcrumb bar meaningful without
    /// hiding the scheme.
    #[must_use]
    pub fn breadcrumb_segments(&self) -> Vec<String> {
        match self {
            Self::Local(path) => {
                let mut segments: Vec<String> = path
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                if segments.is_empty() {
                    segments.push("/".to_owned());
                }
                segments
            }
            Self::Remote(uri, _) => {
                let mut segments = Vec::new();
                let mut root = String::new();
                root.push_str(&uri.scheme);
                root.push_str("://");
                if let Some(user) = &uri.username {
                    root.push_str(user);
                    root.push('@');
                }
                if let Some(host) = &uri.host {
                    root.push_str(host);
                    if let Some(port) = uri.port {
                        root.push(':');
                        root.push_str(&port.to_string());
                    }
                }
                segments.push(root);
                for segment in uri.path.split('/').filter(|s| !s.is_empty()) {
                    segments.push(segment.to_owned());
                }
                segments
            }
        }
    }
}

impl From<PathBuf> for Location {
    fn from(path: PathBuf) -> Self {
        Self::Local(path)
    }
}

impl From<&Path> for Location {
    fn from(path: &Path) -> Self {
        Self::Local(path.to_path_buf())
    }
}

impl From<&str> for Location {
    fn from(input: &str) -> Self {
        Self::from_str(input).unwrap_or_else(|_| Self::Local(PathBuf::from(input)))
    }
}

impl From<String> for Location {
    fn from(input: String) -> Self {
        Self::from_str(&input).unwrap_or_else(|_| Self::Local(PathBuf::from(input)))
    }
}

impl fmt::Display for Location {
    /// Render the canonical URI form (`local:///…` for local,
    /// `sftp://user@host/…` for remote). Use [`Location::display_path`]
    /// for the friendlier user-facing form that shows native paths for
    /// local locations.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(path) => {
                f.write_str("local://")?;
                let s = path.to_string_lossy();
                if !s.starts_with('/') {
                    f.write_str("/")?;
                }
                f.write_str(&s)
            }
            Self::Remote(uri, _) => f.write_str(&uri.to_uri()),
        }
    }
}

/// Errors returned by [`Location::from_str`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LocationParseError {
    /// The input started with a scheme separator (`scheme://`) but the
    /// scheme is not recognised.
    #[error("unknown location scheme: {0}")]
    UnknownScheme(String),
    /// The input carries a scheme separator (`scheme://`) but the
    /// remainder is empty.
    #[error("empty authority in location URI")]
    EmptyAuthority,
    /// A port token was present but did not parse as a `u16`.
    #[error("invalid port {0:?} in location URI")]
    InvalidPort(String),
}

impl FromStr for Location {
    type Err = LocationParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Ok(Self::Local(PathBuf::new()));
        }

        let Some(sep_idx) = trimmed.find("://") else {
            return Ok(Self::Local(PathBuf::from(trimmed)));
        };

        let scheme = &trimmed[..sep_idx];
        let rest = &trimmed[sep_idx + 3..];

        let kind = BackendKind::from_scheme(scheme)
            .ok_or_else(|| LocationParseError::UnknownScheme(scheme.to_string()))?;

        if kind == BackendKind::Local {
            let path = if let Some(stripped) = rest.strip_prefix('/') {
                if stripped.starts_with('/') {
                    // `local:////foo` — collapse the extra slash.
                    format!("/{}", stripped.trim_start_matches('/'))
                } else {
                    format!("/{stripped}")
                }
            } else {
                rest.to_string()
            };
            return Ok(Self::Local(PathBuf::from(path)));
        }

        if rest.is_empty() {
            return Err(LocationParseError::EmptyAuthority);
        }

        let (authority, path) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, ""),
        };

        let (username, hostport) = match authority.find('@') {
            Some(idx) => (Some(&authority[..idx]), &authority[idx + 1..]),
            None => (None, authority),
        };

        let (host_opt, port_opt) = if hostport.is_empty() {
            (None, None)
        } else if let Some(idx) = hostport.rfind(':') {
            let (h, p) = hostport.split_at(idx);
            let p = &p[1..];
            let port = p
                .parse::<u16>()
                .map_err(|_| LocationParseError::InvalidPort(p.to_string()))?;
            (Some(h.to_string()), Some(port))
        } else {
            (Some(hostport.to_string()), None)
        };

        let uri = RemoteUri {
            scheme: kind.scheme().to_string(),
            host: host_opt,
            port: port_opt,
            username: username.map(str::to_owned),
            path: path.to_string(),
            credential_ref: None,
        };

        Ok(Self::Remote(uri, kind))
    }
}

impl Serialize for Location {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Location {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_from_bare_path_roundtrips() {
        let loc = Location::from_str("/Users/alice/Downloads").unwrap();
        assert!(loc.is_local());
        assert_eq!(loc.as_local(), Some(Path::new("/Users/alice/Downloads")));
        // Canonical form.
        assert_eq!(loc.to_string(), "local:///Users/alice/Downloads");
        // Display path stays native.
        assert_eq!(loc.display_path(), "/Users/alice/Downloads");
    }

    #[test]
    fn local_from_relative_path_stays_relative() {
        let loc = Location::from_str("foo/bar").unwrap();
        assert_eq!(loc.as_local(), Some(Path::new("foo/bar")));
    }

    #[test]
    fn local_from_tilde_stays_literal() {
        // Tilde expansion is the caller's job (atlas_core::path::expand_tilde).
        let loc = Location::from_str("~/Downloads").unwrap();
        assert_eq!(loc.as_local(), Some(Path::new("~/Downloads")));
    }

    #[test]
    fn local_explicit_uri_parses() {
        let loc = Location::from_str("local:///Users/x").unwrap();
        assert_eq!(loc.as_local(), Some(Path::new("/Users/x")));
    }

    #[test]
    fn local_file_uri_parses() {
        let loc = Location::from_str("file:///var/log").unwrap();
        assert_eq!(loc.as_local(), Some(Path::new("/var/log")));
    }

    #[test]
    fn sftp_full_uri_roundtrips() {
        let loc = Location::from_str("sftp://alice@example.com:2222/var/log").unwrap();
        let Location::Remote(uri, kind) = &loc else {
            panic!("expected Remote");
        };
        assert_eq!(*kind, BackendKind::Sftp);
        assert_eq!(uri.scheme, "sftp");
        assert_eq!(uri.username.as_deref(), Some("alice"));
        assert_eq!(uri.host.as_deref(), Some("example.com"));
        assert_eq!(uri.port, Some(2222));
        assert_eq!(uri.path, "/var/log");
        assert!(uri.credential_ref.is_none());
        assert_eq!(loc.to_string(), "sftp://alice@example.com:2222/var/log");
        assert_eq!(loc.display_path(), "sftp://alice@example.com:2222/var/log");
    }

    #[test]
    fn sftp_minimal_uri_parses() {
        let loc = Location::from_str("sftp://host/").unwrap();
        let Location::Remote(uri, _) = loc else {
            panic!()
        };
        assert!(uri.username.is_none());
        assert_eq!(uri.host.as_deref(), Some("host"));
        assert!(uri.port.is_none());
        assert_eq!(uri.path, "/");
    }

    #[test]
    fn s3_bucket_uri_parses() {
        let loc = Location::from_str("s3://my-bucket/prefix/key").unwrap();
        let Location::Remote(uri, kind) = &loc else {
            panic!()
        };
        assert_eq!(*kind, BackendKind::S3);
        assert_eq!(uri.host.as_deref(), Some("my-bucket"));
        assert_eq!(uri.path, "/prefix/key");
        assert_eq!(loc.to_string(), "s3://my-bucket/prefix/key");
    }

    #[test]
    fn webdav_uri_parses() {
        let loc = Location::from_str("webdav://user@cloud.example.com/dav/root").unwrap();
        let Location::Remote(uri, kind) = loc else {
            panic!()
        };
        assert_eq!(kind, BackendKind::WebDav);
        assert_eq!(uri.username.as_deref(), Some("user"));
        assert_eq!(uri.host.as_deref(), Some("cloud.example.com"));
        assert_eq!(uri.path, "/dav/root");
    }

    #[test]
    fn ftp_uri_parses() {
        let loc = Location::from_str("ftp://anon@ftp.example.com/pub").unwrap();
        let Location::Remote(uri, kind) = loc else {
            panic!()
        };
        assert_eq!(kind, BackendKind::Ftp);
        assert_eq!(uri.username.as_deref(), Some("anon"));
        assert_eq!(uri.host.as_deref(), Some("ftp.example.com"));
        assert_eq!(uri.port, None);
        assert_eq!(uri.path, "/pub");
    }

    #[test]
    fn unknown_scheme_errors() {
        let err = Location::from_str("gopher://host/path").unwrap_err();
        assert_eq!(err, LocationParseError::UnknownScheme("gopher".into()));
    }

    #[test]
    fn invalid_port_errors() {
        let err = Location::from_str("sftp://host:abc/path").unwrap_err();
        assert_eq!(err, LocationParseError::InvalidPort("abc".into()));
    }

    #[test]
    fn empty_authority_errors() {
        let err = Location::from_str("sftp://").unwrap_err();
        assert_eq!(err, LocationParseError::EmptyAuthority);
    }

    #[test]
    fn empty_string_parses_as_empty_local() {
        let loc = Location::from_str("").unwrap();
        assert_eq!(loc.as_local(), Some(Path::new("")));
    }

    #[test]
    fn unicode_paths_survive_roundtrip() {
        let raw = "sftp://user@host/naïve/résumé/ünicode";
        let loc = Location::from_str(raw).unwrap();
        assert_eq!(loc.to_string(), raw);
    }

    #[test]
    fn local_backend_reports_correctly() {
        let loc = Location::local("/tmp/x");
        assert_eq!(loc.backend(), BackendKind::Local);
        assert!(loc.is_local());
        assert!(!loc.is_remote());
    }

    #[test]
    fn remote_backend_reports_correctly() {
        let loc = Location::from_str("s3://bucket/key").unwrap();
        assert_eq!(loc.backend(), BackendKind::S3);
        assert!(!loc.is_local());
        assert!(loc.is_remote());
    }

    #[test]
    fn credential_ref_survives_roundtrip_via_direct_construct() {
        let uri = RemoteUri {
            scheme: "sftp".into(),
            host: Some("h".into()),
            port: None,
            username: Some("u".into()),
            path: "/p".into(),
            credential_ref: Some("keychain-key-42".into()),
        };
        let loc = Location::Remote(uri.clone(), BackendKind::Sftp);
        // to_uri never exposes credential_ref (that's a keychain lookup, not a URI thing).
        assert_eq!(loc.to_string(), "sftp://u@h/p");
        // But the credential_ref survives on the value.
        let Location::Remote(uri2, _) = loc else {
            panic!()
        };
        assert_eq!(uri2.credential_ref.as_deref(), Some("keychain-key-42"));
    }

    #[test]
    fn serde_local_roundtrip() {
        let loc = Location::local("/Users/alice/Downloads");
        let json = serde_json::to_string(&loc).unwrap();
        assert_eq!(json, "\"local:///Users/alice/Downloads\"");
        let back: Location = serde_json::from_str(&json).unwrap();
        assert_eq!(back, loc);
    }

    #[test]
    fn serde_remote_roundtrip() {
        let loc = Location::from_str("sftp://alice@example.com:22/var").unwrap();
        let json = serde_json::to_string(&loc).unwrap();
        assert_eq!(json, "\"sftp://alice@example.com:22/var\"");
        let back: Location = serde_json::from_str(&json).unwrap();
        assert_eq!(back, loc);
    }

    #[test]
    fn serde_accepts_legacy_bare_path_string() {
        // Old session state persisted PathBuf as a bare string. Ensure
        // the untagged deserialiser still accepts it.
        let json = "\"/Users/alice/Downloads\"";
        let loc: Location = serde_json::from_str(json).unwrap();
        assert_eq!(loc, Location::local("/Users/alice/Downloads"));
    }

    #[test]
    fn breadcrumb_segments_for_local() {
        let loc = Location::local("/Users/alice/Downloads");
        assert_eq!(
            loc.breadcrumb_segments(),
            vec!["/", "Users", "alice", "Downloads"]
        );
    }

    #[test]
    fn breadcrumb_segments_for_remote() {
        let loc = Location::from_str("sftp://alice@host:22/var/log").unwrap();
        assert_eq!(
            loc.breadcrumb_segments(),
            vec!["sftp://alice@host:22", "var", "log"]
        );
    }

    #[test]
    fn from_impls_produce_local() {
        let a: Location = PathBuf::from("/x").into();
        let b: Location = Path::new("/x").into();
        assert_eq!(a, b);
        assert!(a.is_local());
    }

    #[test]
    fn from_str_impl_falls_back_on_error() {
        // The `From<&str>` impl (used by ergonomic call sites) never
        // fails — it recovers by treating the input as a bare local path.
        let loc: Location = "not://a valid uri".into();
        assert!(loc.is_local());
    }
}
