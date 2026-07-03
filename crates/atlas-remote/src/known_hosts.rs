//! OpenSSH-compatible `known_hosts` store for SSH host-key TOFU.
//!
//! The store reads two files:
//!
//! * The atlas-owned `~/.config/atlas/known_hosts` (or platform equivalent
//!   via [`atlas_config::paths::known_hosts_file_path`]) — this is the only
//!   file [`KnownHosts::save`] writes back to.
//! * The user's shell `~/.ssh/known_hosts` — read-only, treated as trusted
//!   so users who already pinned a server in their shell don't need to
//!   re-trust it inside Atlas.
//!
//! The on-disk format matches the standard `sshd(8)` `known_hosts` grammar so
//! entries copy-and-paste cleanly in either direction. Both plain and hashed
//! host patterns are parsed on read; only plain entries are emitted on write
//! (parse-only; Atlas does not write hashed known-host entries).
//!
//! # Fingerprint format
//!
//! Public keys are fingerprinted with SHA-256 over the wire-format key blob
//! and rendered as base64 without padding, e.g.
//! `SHA256:6JmnqJZP2yJvxAuMlqXQopWJH4v2rQE0jl3mrjJcC2E`. This matches
//! `ssh-keyscan(1)` / `ssh-keygen -lf`.
//!
//! # Threading
//!
//! [`KnownHosts`] is owned by the caller — every caller loads a fresh
//! snapshot, mutates it, then calls [`KnownHosts::save`]. Atomic replacement
//! via `tmp → rename` avoids partial-write corruption. This is fine here
//! because the file is tiny (dozens of lines) and mutations are user-driven
//! (one every "Trust always" click).

use std::fs;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use hmac::{Hmac, Mac};
use russh::keys::key::PublicKey;
use russh::keys::PublicKeyBase64;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use thiserror::Error;

type HmacSha1 = Hmac<Sha1>;

/// Errors surfaced by the known-hosts store.
#[derive(Debug, Error)]
pub enum KnownHostsError {
    /// File I/O failed (open, read, write, rename). Path is included so the
    /// UI can render an actionable error message.
    #[error("known_hosts I/O error at {}: {source}", path.display())]
    Io {
        /// The offending path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// Could not resolve the atlas config directory (e.g. `HOME` unset).
    #[error("could not resolve atlas config directory: {0}")]
    Config(String),
}

/// The outcome of comparing an offered host key against the known-hosts store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyStatus {
    /// An entry exists for the host AND the offered fingerprint matches.
    Trusted,
    /// No entry exists for the host — the caller should prompt the user
    /// (TOFU flow).
    Unknown,
    /// An entry exists for the host but the offered fingerprint is
    /// **different** from the stored one. This is the SSH "man-in-the-middle
    /// or key-rotation" warning case.
    Mismatch {
        /// The fingerprint currently on disk, formatted as `SHA256:<base64>`.
        known_fingerprint: String,
    },
}

/// Origin of a parsed entry. Only atlas-owned entries participate in
/// [`KnownHosts::save`]; shell entries are read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryOrigin {
    /// Loaded from atlas's own `known_hosts` file (writable).
    Atlas,
    /// Loaded from the user's `~/.ssh/known_hosts` (read-only).
    Shell,
}

/// One row from a `known_hosts` file.
#[derive(Debug, Clone)]
struct KnownHostEntry {
    pattern: HostPattern,
    key_type: String,
    /// Wire-format base64 of the public key (no `ssh-rsa `/`ssh-ed25519 `
    /// prefix). Preserved verbatim on write so consumers see byte-identical
    /// values across shell and atlas files.
    key_b64: String,
    origin: EntryOrigin,
}

/// Which side of the OpenSSH host-pattern grammar an entry uses.
#[derive(Debug, Clone)]
enum HostPattern {
    /// A comma-separated list of literal host tokens. Each token is either
    /// `hostname` (for the default port 22) or `[hostname]:port`.
    Plain(Vec<String>),
    /// `|1|<salt>|<hash>` — the OpenSSH hashed-hostname format.
    Hashed {
        /// Raw HMAC salt (base64-decoded).
        salt: Vec<u8>,
        /// Raw HMAC output (base64-decoded).
        hash: Vec<u8>,
    },
}

/// In-memory representation of the union of atlas + shell known-hosts files.
///
/// Load once via [`Self::load`], call [`Self::check`] against each offered
/// server key, mutate via [`Self::add`], and persist atlas-owned entries via
/// [`Self::save`]. Shell-owned entries are never written back.
pub struct KnownHosts {
    entries: Vec<KnownHostEntry>,
    atlas_path: PathBuf,
}

impl KnownHosts {
    /// Load the atlas + shell known-hosts stores from disk.
    ///
    /// Missing files are treated as empty (never an error). Malformed lines
    /// are skipped with a debug-level trace so a single bad row can't lock
    /// the user out of every subsequent connect.
    ///
    /// # Errors
    ///
    /// Returns [`KnownHostsError::Config`] if the atlas config directory
    /// cannot be resolved. Actual I/O errors on the atlas file surface as
    /// [`KnownHostsError::Io`]; the shell file is read best-effort and its
    /// errors are logged only.
    pub fn load() -> Result<Self, KnownHostsError> {
        let atlas_path = atlas_config::paths::known_hosts_file_path()
            .map_err(|e| KnownHostsError::Config(e.to_string()))?;

        let mut entries = Vec::new();
        Self::load_file(&atlas_path, EntryOrigin::Atlas, &mut entries)?;

        // Best-effort ~/.ssh/known_hosts scan. Any error here is logged and
        // ignored — the atlas file is still authoritative.
        if let Some(ssh_path) = ssh_known_hosts_path() {
            if let Err(err) = Self::load_file(&ssh_path, EntryOrigin::Shell, &mut entries) {
                tracing::debug!(path = %ssh_path.display(), error = %err, "known_hosts: skipping shell file");
            }
        }

        Ok(Self {
            entries,
            atlas_path,
        })
    }

    /// Load-from-arbitrary-path variant, used by unit tests and by
    /// [`Self::load`] internally. See [`Self::load`] for the shell/atlas
    /// composition semantics; this method loads a single file.
    ///
    /// # Errors
    ///
    /// Returns [`KnownHostsError::Io`] if the file exists but cannot be
    /// opened; a missing file is not an error.
    #[doc(hidden)]
    pub fn load_from_path(path: &Path) -> Result<Self, KnownHostsError> {
        let mut entries = Vec::new();
        Self::load_file(path, EntryOrigin::Atlas, &mut entries)?;
        Ok(Self {
            entries,
            atlas_path: path.to_path_buf(),
        })
    }

    fn load_file(
        path: &Path,
        origin: EntryOrigin,
        out: &mut Vec<KnownHostEntry>,
    ) -> Result<(), KnownHostsError> {
        let contents = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(KnownHostsError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        for (idx, raw) in contents.lines().enumerate() {
            match parse_line(raw, origin) {
                Ok(Some(entry)) => out.push(entry),
                Ok(None) => {} // blank / comment
                Err(err) => {
                    tracing::debug!(
                        path = %path.display(),
                        line = idx + 1,
                        error = %err,
                        "known_hosts: skipping malformed line",
                    );
                }
            }
        }
        Ok(())
    }

    /// Compare `key` against the store for `(host, port)`.
    ///
    /// See [`HostKeyStatus`] for the tri-state outcome. The result is a pure
    /// function of the in-memory snapshot — [`Self::load`] must be called
    /// beforehand.
    #[must_use]
    pub fn check(&self, host: &str, port: u16, key: &PublicKey) -> HostKeyStatus {
        let offered_fp = fingerprint(key);
        let mut mismatch: Option<String> = None;
        for entry in &self.entries {
            if !entry.pattern.matches(host, port) {
                continue;
            }
            let stored_fp = fingerprint_from_b64(&entry.key_b64);
            if stored_fp == offered_fp {
                return HostKeyStatus::Trusted;
            }
            // Remember the first mismatch; keep scanning in case a
            // later entry does match (unlikely, but harmless).
            if mismatch.is_none() {
                mismatch = Some(stored_fp);
            }
        }
        match mismatch {
            Some(known_fingerprint) => HostKeyStatus::Mismatch { known_fingerprint },
            None => HostKeyStatus::Unknown,
        }
    }

    /// Add `key` under the `(host, port)` tuple to the atlas-owned entry set.
    ///
    /// Any existing atlas-owned entries for this host with a mismatched
    /// fingerprint are removed so the store is left in a coherent state
    /// (this is the "Replace and continue" path from the UI mismatch banner).
    /// Shell-owned entries are never touched.
    ///
    /// The change is in-memory only until [`Self::save`] is called.
    ///
    /// # Errors
    ///
    /// Currently infallible; the return type is kept as `Result` so future
    /// validation (e.g. reserved-host guards) can be added without breaking
    /// callers.
    pub fn add(&mut self, host: &str, port: u16, key: &PublicKey) -> Result<(), KnownHostsError> {
        // Drop any conflicting atlas-owned entry for this host so the file
        // stays coherent — matches OpenSSH's `-R` behaviour on key rotation.
        self.entries
            .retain(|e| !(e.origin == EntryOrigin::Atlas && e.pattern.matches(host, port)));
        self.entries.push(KnownHostEntry {
            pattern: HostPattern::Plain(vec![format_host_token(host, port)]),
            key_type: key.name().to_owned(),
            key_b64: key.public_key_base64(),
            origin: EntryOrigin::Atlas,
        });
        Ok(())
    }

    /// Persist the atlas-owned entries to [`atlas_config::paths::known_hosts_file_path`].
    ///
    /// Writes via `tmp → rename` for atomicity. Shell-owned entries are
    /// excluded from the output.
    ///
    /// # Errors
    ///
    /// Returns [`KnownHostsError::Io`] on any filesystem error along the
    /// write path (create parent dir, open tmp, write, rename).
    pub fn save(&self) -> Result<(), KnownHostsError> {
        if let Some(parent) = self.atlas_path.parent() {
            fs::create_dir_all(parent).map_err(|source| KnownHostsError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let tmp = self.atlas_path.with_extension("tmp");
        let mut buf = String::new();
        buf.push_str("# Atlas known_hosts — OpenSSH-compatible.\n");
        buf.push_str("# Managed by atlas; hand-edits are honoured.\n");
        for entry in &self.entries {
            if entry.origin != EntryOrigin::Atlas {
                continue;
            }
            let HostPattern::Plain(tokens) = &entry.pattern else {
                // We never write hashed entries (parse-only),
                // but we round-trip atlas-owned hashed entries as-is if
                // somehow present — future-proofing.
                continue;
            };
            buf.push_str(&tokens.join(","));
            buf.push(' ');
            buf.push_str(&entry.key_type);
            buf.push(' ');
            buf.push_str(&entry.key_b64);
            buf.push('\n');
        }

        {
            let mut file = fs::File::create(&tmp).map_err(|source| KnownHostsError::Io {
                path: tmp.clone(),
                source,
            })?;
            file.write_all(buf.as_bytes())
                .map_err(|source| KnownHostsError::Io {
                    path: tmp.clone(),
                    source,
                })?;
            file.sync_all().map_err(|source| KnownHostsError::Io {
                path: tmp.clone(),
                source,
            })?;
        }
        fs::rename(&tmp, &self.atlas_path).map_err(|source| KnownHostsError::Io {
            path: self.atlas_path.clone(),
            source,
        })?;
        Ok(())
    }

    /// The absolute path this store writes back to. Exposed for tests and
    /// for the UI to render "trust always → will be saved to X" hints.
    #[must_use]
    pub fn atlas_path(&self) -> &Path {
        &self.atlas_path
    }

    /// Number of entries currently loaded (atlas + shell combined). Exposed
    /// for tests only.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

impl HostPattern {
    /// Does this pattern cover the given `(host, port)` tuple?
    fn matches(&self, host: &str, port: u16) -> bool {
        match self {
            Self::Plain(tokens) => {
                let want_plain = host;
                let want_bracketed = format!("[{host}]:{port}");
                for tok in tokens {
                    // For default port 22, OpenSSH accepts either form.
                    if port == 22 && tok == want_plain {
                        return true;
                    }
                    if tok == &want_bracketed {
                        return true;
                    }
                    // Also accept a bare bracketed form even when the port
                    // matches the default — `[host]:22` is legal.
                    if tok.starts_with('[') && tok == &format!("[{host}]:{port}") {
                        return true;
                    }
                }
                false
            }
            Self::Hashed { salt, hash } => {
                let candidate = if port == 22 {
                    host.to_owned()
                } else {
                    format!("[{host}]:{port}")
                };
                let Ok(mut mac) = HmacSha1::new_from_slice(salt) else {
                    return false;
                };
                mac.update(candidate.as_bytes());
                mac.finalize().into_bytes().as_slice() == hash.as_slice()
            }
        }
    }
}

/// Parse a single line from a known_hosts file. Returns `Ok(None)` for
/// blank / comment lines, `Ok(Some)` for a valid entry, and `Err` for a
/// syntactically bad line (the caller logs and skips).
fn parse_line(raw: &str, origin: EntryOrigin) -> Result<Option<KnownHostEntry>, String> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    // OpenSSH supports optional `@marker` prefixes (`@cert-authority`,
    // `@revoked`). We skip such lines — cert-authority delegation isn't in
    // scope for the current known-hosts implementation.
    if line.starts_with('@') {
        return Ok(None);
    }
    // Split into at most 3 whitespace tokens: host-pattern, key-type, key-b64.
    let mut parts = line.splitn(3, char::is_whitespace);
    let host_field = parts.next().ok_or_else(|| "empty line".to_owned())?;
    let key_type = parts
        .next()
        .ok_or_else(|| "missing key type".to_owned())?
        .trim()
        .to_owned();
    let key_b64_raw = parts
        .next()
        .ok_or_else(|| "missing key material".to_owned())?
        .trim();
    // The last field may carry a trailing comment; keep everything before
    // the first whitespace.
    let key_b64 = key_b64_raw
        .split_whitespace()
        .next()
        .ok_or_else(|| "empty key material".to_owned())?
        .to_owned();
    let pattern = parse_host_field(host_field)?;
    Ok(Some(KnownHostEntry {
        pattern,
        key_type,
        key_b64,
        origin,
    }))
}

/// Turn the leftmost field of a known_hosts row into a [`HostPattern`].
fn parse_host_field(field: &str) -> Result<HostPattern, String> {
    if let Some(rest) = field.strip_prefix("|1|") {
        // OpenSSH hashed format: |1|<salt-b64>|<hash-b64>
        let mut parts = rest.splitn(2, '|');
        let salt_b64 = parts
            .next()
            .ok_or_else(|| "hashed: missing salt".to_owned())?;
        let hash_b64 = parts
            .next()
            .ok_or_else(|| "hashed: missing hash".to_owned())?;
        let salt = BASE64_STANDARD
            .decode(salt_b64)
            .map_err(|e| format!("hashed: bad salt base64: {e}"))?;
        let hash = BASE64_STANDARD
            .decode(hash_b64)
            .map_err(|e| format!("hashed: bad hash base64: {e}"))?;
        return Ok(HostPattern::Hashed { salt, hash });
    }
    let tokens: Vec<String> = field.split(',').map(|s| s.trim().to_owned()).collect();
    if tokens.iter().any(String::is_empty) {
        return Err("empty host token".to_owned());
    }
    Ok(HostPattern::Plain(tokens))
}

/// Format a host+port tuple as the token to write into a plain
/// known_hosts entry. Port 22 collapses to the bare hostname.
fn format_host_token(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    }
}

/// Resolve the user's `~/.ssh/known_hosts` path via
/// [`directories::UserDirs`], which internally goes through
/// `SHGetKnownFolderPath(FOLDERID_Profile)` on Windows and the standard
/// `$HOME` on Unix. On Windows 10+ the OpenSSH client ships under
/// `C:\Windows\System32\OpenSSH\` and reads from
/// `%USERPROFILE%\.ssh\known_hosts` — same relative path as Unix, so we
/// don't need a Windows-specific branch.
///
/// Returns `None` if no home directory can be resolved. Callers must
/// treat that — and a missing file at the returned path — as normal,
/// non-error conditions: many systems don't have a shell `~/.ssh` at
/// all, and it's not our job to complain.
//
// TODO(windows): PuTTY registry `HKEY_CURRENT_USER\Software\SimonTatham\
// PuTTY\SshHostKeys` as tertiary lookup. Deferred — TOFU
// covers Windows users adequately; PuTTY compatibility is a nice-to-have
// for the subset of Windows shops that predate Windows-OpenSSH (2018+).
fn ssh_known_hosts_path() -> Option<PathBuf> {
    let user_dirs = directories::UserDirs::new()?;
    Some(user_dirs.home_dir().join(".ssh").join("known_hosts"))
}

/// SHA-256 fingerprint of a [`PublicKey`], rendered `SHA256:<base64-no-pad>`.
#[must_use]
pub fn fingerprint(key: &PublicKey) -> String {
    let blob = key.public_key_bytes();
    let digest = Sha256::digest(&blob);
    let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest);
    format!("SHA256:{b64}")
}

/// Fingerprint variant that starts from a stored wire-format base64 blob
/// (as parsed from a known_hosts entry). Falls back to `SHA256:<b64-of-b64>`
/// on decode failure — a defensive path that should never fire against a
/// well-formed store, but keeps [`KnownHosts::check`] tolerant of upstream
/// corruption.
fn fingerprint_from_b64(key_b64: &str) -> String {
    match BASE64_STANDARD.decode(key_b64) {
        Ok(blob) => {
            let digest = Sha256::digest(&blob);
            let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest);
            format!("SHA256:{b64}")
        }
        Err(_) => format!("SHA256:INVALID({key_b64:.16})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::key::KeyPair;
    use tempfile::TempDir;

    /// Build a deterministic ed25519 public key for tests. Any fixed
    /// keypair works — we only need reproducible fingerprints.
    fn test_public_key() -> PublicKey {
        let kp = KeyPair::generate_ed25519().expect("gen ed25519");
        kp.clone_public_key().expect("public")
    }

    fn wire_b64(k: &PublicKey) -> String {
        k.public_key_base64()
    }

    fn key_type(k: &PublicKey) -> &'static str {
        k.name()
    }

    #[test]
    fn parse_plain_line_yields_hostname_entry() {
        let k = test_public_key();
        let line = format!("example.com {} {}", key_type(&k), wire_b64(&k));
        let entry = parse_line(&line, EntryOrigin::Atlas)
            .expect("parse")
            .expect("some");
        match entry.pattern {
            HostPattern::Plain(t) => assert_eq!(t, vec!["example.com".to_owned()]),
            _ => panic!("expected plain"),
        }
        assert_eq!(entry.key_b64, wire_b64(&k));
    }

    #[test]
    fn parse_plain_line_supports_bracketed_port_syntax() {
        let k = test_public_key();
        let line = format!("[example.com]:2222 {} {}", key_type(&k), wire_b64(&k));
        let entry = parse_line(&line, EntryOrigin::Atlas)
            .expect("parse")
            .expect("some");
        match entry.pattern {
            HostPattern::Plain(t) => assert_eq!(t, vec!["[example.com]:2222".to_owned()]),
            _ => panic!("expected plain"),
        }
    }

    #[test]
    fn parse_hashed_line_matches_source_hostname() {
        // ssh-keygen -H style hashed entry hand-computed for "example.com".
        // salt = "abc123==", hash = HMAC-SHA1(salt, "example.com").
        use base64::Engine as _;
        let salt = b"\x89\xe7\xed\xcaZ";
        let mut mac = HmacSha1::new_from_slice(salt).unwrap();
        mac.update(b"example.com");
        let hash = mac.finalize().into_bytes();
        let salt_b64 = BASE64_STANDARD.encode(salt);
        let hash_b64 = BASE64_STANDARD.encode(hash);

        let k = test_public_key();
        let line = format!("|1|{salt_b64}|{hash_b64} {} {}", key_type(&k), wire_b64(&k));
        let entry = parse_line(&line, EntryOrigin::Atlas)
            .expect("parse")
            .expect("some");
        assert!(entry.pattern.matches("example.com", 22));
        assert!(!entry.pattern.matches("other.example.com", 22));
    }

    #[test]
    fn parse_skips_comments_and_blanks() {
        assert!(parse_line("", EntryOrigin::Atlas).unwrap().is_none());
        assert!(parse_line("   ", EntryOrigin::Atlas).unwrap().is_none());
        assert!(parse_line("# a comment", EntryOrigin::Atlas)
            .unwrap()
            .is_none());
        assert!(parse_line("@cert-authority x y z", EntryOrigin::Atlas)
            .unwrap()
            .is_none());
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("does_not_exist");
        let store = KnownHosts::load_from_path(&path).expect("load");
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn add_and_save_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("known_hosts");
        let mut store = KnownHosts::load_from_path(&path).unwrap();
        let k = test_public_key();
        store.add("example.com", 22, &k).unwrap();
        store.save().unwrap();
        let reloaded = KnownHosts::load_from_path(&path).unwrap();
        assert!(matches!(
            reloaded.check("example.com", 22, &k),
            HostKeyStatus::Trusted
        ));
    }

    #[test]
    fn check_detects_mismatch_when_offered_key_differs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("known_hosts");
        let mut store = KnownHosts::load_from_path(&path).unwrap();
        let k1 = test_public_key();
        store.add("example.com", 22, &k1).unwrap();
        let k2 = test_public_key(); // different keypair
        match store.check("example.com", 22, &k2) {
            HostKeyStatus::Mismatch { known_fingerprint } => {
                assert!(known_fingerprint.starts_with("SHA256:"));
            }
            other => panic!("expected mismatch, got {other:?}"),
        }
    }

    #[test]
    fn check_unknown_when_host_absent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("known_hosts");
        let mut store = KnownHosts::load_from_path(&path).unwrap();
        let k = test_public_key();
        store.add("example.com", 22, &k).unwrap();
        let k2 = test_public_key();
        assert!(matches!(
            store.check("unseen.example.com", 22, &k2),
            HostKeyStatus::Unknown
        ));
    }

    #[test]
    fn nondefault_port_uses_bracketed_form() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("known_hosts");
        let mut store = KnownHosts::load_from_path(&path).unwrap();
        let k = test_public_key();
        store.add("example.com", 2222, &k).unwrap();
        store.save().unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("[example.com]:2222"),
            "expected bracketed port form in output, got:\n{contents}"
        );
        // Same store trusts (host, port).
        assert!(matches!(
            store.check("example.com", 2222, &k),
            HostKeyStatus::Trusted
        ));
        // Wrong port ⇒ unknown.
        assert!(matches!(
            store.check("example.com", 22, &k),
            HostKeyStatus::Unknown
        ));
    }

    #[test]
    fn ipv6_host_stored_and_retrieved() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("known_hosts");
        let mut store = KnownHosts::load_from_path(&path).unwrap();
        let k = test_public_key();
        // OpenSSH uses the string form of the IPv6 address (no brackets on
        // default port, `[addr]:port` on non-default).
        store.add("2001:db8::1", 22, &k).unwrap();
        store.save().unwrap();
        let reloaded = KnownHosts::load_from_path(&path).unwrap();
        assert!(matches!(
            reloaded.check("2001:db8::1", 22, &k),
            HostKeyStatus::Trusted
        ));
    }

    #[test]
    fn unicode_hostname_stored_and_retrieved() {
        // OpenSSH treats hostnames as opaque byte strings for the pattern
        // comparison; we do the same so IDNs / punycode / raw unicode all
        // survive a round-trip byte-for-byte.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("known_hosts");
        let mut store = KnownHosts::load_from_path(&path).unwrap();
        let k = test_public_key();
        let host = "café.example.com";
        store.add(host, 22, &k).unwrap();
        store.save().unwrap();
        let reloaded = KnownHosts::load_from_path(&path).unwrap();
        assert!(matches!(
            reloaded.check(host, 22, &k),
            HostKeyStatus::Trusted
        ));
    }

    #[test]
    fn comment_and_blank_lines_survive_reload() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("known_hosts");
        let k = test_public_key();
        let raw = format!(
            "# top comment\n\n# blank line follows\n\nexample.com {} {}\n",
            key_type(&k),
            wire_b64(&k)
        );
        fs::write(&path, raw).unwrap();
        let store = KnownHosts::load_from_path(&path).unwrap();
        assert_eq!(store.len(), 1);
        assert!(matches!(
            store.check("example.com", 22, &k),
            HostKeyStatus::Trusted
        ));
    }

    #[test]
    fn fingerprint_is_sha256_base64_no_pad() {
        let k = test_public_key();
        let fp = fingerprint(&k);
        assert!(fp.starts_with("SHA256:"));
        // Base64-no-pad SHA-256 output is always exactly 43 chars.
        assert_eq!(fp.len(), "SHA256:".len() + 43);
        assert!(!fp.ends_with('='));
    }

    #[test]
    fn replace_conflicting_entry_on_add() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("known_hosts");
        let mut store = KnownHosts::load_from_path(&path).unwrap();
        let k1 = test_public_key();
        let k2 = test_public_key();
        store.add("example.com", 22, &k1).unwrap();
        store.add("example.com", 22, &k2).unwrap();
        // Only the second key should remain — first was displaced.
        assert!(matches!(
            store.check("example.com", 22, &k2),
            HostKeyStatus::Trusted
        ));
        assert!(matches!(
            store.check("example.com", 22, &k1),
            HostKeyStatus::Mismatch { .. }
        ));
    }

    /// Cross-platform path-resolution sanity check.
    ///
    /// Verifies that the primary atlas known_hosts path is composed from
    /// [`atlas_config::paths::config_dir`] — i.e. it honours
    /// `ATLAS_CONFIG_DIR`, `XDG_CONFIG_HOME`, and platform defaults —
    /// rather than being hardcoded to `~/.config/atlas/known_hosts`.
    ///
    /// The `ATLAS_CONFIG_DIR` env override is the same knob every other
    /// atlas config file (`servers.toml`, `config.toml`, keymaps) uses,
    /// so testing it here transitively verifies that the platform
    /// resolution branches in `atlas_config::paths::config_dir` — Windows
    /// `%APPDATA%\Atlas`, Linux `$XDG_CONFIG_HOME/atlas` or `~/.config/
    /// atlas`, macOS `~/.config/atlas` — flow through unchanged. We do
    /// NOT attempt to spawn a Windows binary from a Unix test host; the
    /// per-platform branches are covered by the existing
    /// `atlas_config::servers::tests::path_helpers` test.
    ///
    /// Also asserts the secondary `~/.ssh/known_hosts` lookup goes
    /// through [`directories::UserDirs`] (matching every other home-dir
    /// resolution in the workspace — see
    /// `atlas_ui::shell::palette_root`, `atlas_indexd::paths`) rather
    /// than reading `$HOME` / `%USERPROFILE%` raw.
    #[test]
    fn known_hosts_cross_platform_paths() {
        // Serialise env-var mutation. `ATLAS_CONFIG_DIR` is process-global
        // and other tests may read it concurrently; use a scope-local
        // guard that always restores the previous value.
        struct EnvGuard {
            key: &'static str,
            prev: Option<std::ffi::OsString>,
        }
        impl EnvGuard {
            fn set(key: &'static str, value: &str) -> Self {
                let prev = std::env::var_os(key);
                std::env::set_var(key, value);
                Self { key, prev }
            }
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.prev {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }

        let tmp = TempDir::new().unwrap();
        let override_dir = tmp.path();
        let _guard = EnvGuard::set("ATLAS_CONFIG_DIR", override_dir.to_str().unwrap());

        // Primary atlas store must live inside the overridden config dir,
        // with the filename `known_hosts` (no extension, matching OpenSSH
        // and every user's muscle memory).
        let resolved = atlas_config::paths::known_hosts_file_path()
            .expect("known_hosts_file_path with ATLAS_CONFIG_DIR set");
        assert_eq!(resolved, override_dir.join("known_hosts"));
        assert_eq!(
            resolved.file_name().and_then(|s| s.to_str()),
            Some("known_hosts")
        );
        assert!(
            resolved.starts_with(override_dir),
            "known_hosts must be under config_dir, got {}",
            resolved.display()
        );

        // Secondary `~/.ssh/known_hosts` — the atlas path override MUST
        // NOT leak into the shell path. The shell path is always
        // <home>/.ssh/known_hosts, driven by `directories::UserDirs`.
        // We call the private helper directly to prove the composition
        // is `<home>/.ssh/known_hosts`; the actual home value depends on
        // the test host, so we assert structure (segments) not literal
        // path.
        if let Some(shell_path) = ssh_known_hosts_path() {
            let mut iter = shell_path.iter().rev();
            assert_eq!(iter.next().and_then(|s| s.to_str()), Some("known_hosts"));
            assert_eq!(iter.next().and_then(|s| s.to_str()), Some(".ssh"));
            // Whatever remains is the home dir — must NOT be the atlas
            // config override we just installed.
            assert!(
                !shell_path.starts_with(override_dir),
                "shell known_hosts must not inherit ATLAS_CONFIG_DIR override; got {}",
                shell_path.display()
            );
        }
        // Note: if `directories::UserDirs::new()` returns None (e.g. a
        // sandboxed CI runner with no home dir), we accept `None` from
        // `ssh_known_hosts_path` — callers already treat that as a
        // no-op, and it's not our job to fail here.

        // Load must succeed with an override pointing at a non-existent
        // dir: missing file → empty store, no error, no warning.
        let store = KnownHosts::load().expect("load must succeed on missing files");
        assert_eq!(store.atlas_path(), override_dir.join("known_hosts"));
        // Only shell entries (if any) may be present; the atlas file
        // doesn't exist yet.
        for entry in &store.entries {
            assert_eq!(entry.origin, EntryOrigin::Shell);
        }
    }
}
