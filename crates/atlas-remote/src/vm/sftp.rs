//! SFTP backend built on `russh` + `russh-sftp`.
//!
//! Pure-Rust SSH stack (no `openssh` / `nix` C-side dependency), which
//! makes it viable on Windows as well as macOS/Linux. Runs against
//! real OpenSSH servers as well as the paramiko-based mock we use in
//! integration tests.
//!
//! # Design: lazy connect
//!
//! The constructor [`SftpBackend::new`] is **synchronous** — it
//! validates only what we can check locally (host present, username
//! present, credential type acceptable) and stores the config. The
//! actual TCP + SSH + SFTP handshake happens lazily on the first
//! async operation via a `tokio::sync::OnceCell`. This lets
//! [`crate::backend::open`] stay a sync function while auth errors
//! still surface — they arrive via [`atlas_fs::ViewModelEvent::Error`]
//! from the background lister, which is exactly the path the connect
//! controller already listens on.
//!
//! # Authentication
//!
//! * SSH private key (`Credentials::SshKey(path, passphrase)`)
//! * Password (`Credentials::Password`)
//! * Anonymous — issues a "none" auth attempt; only succeeds against
//!   servers that permit it.
//!
//! # Host-key trust (TOFU)
//!
//! Host-key handling is delegated to [`crate::known_hosts::KnownHosts`]
//! and [`crate::host_key::HostKeyResolver`]. The [`SftpBackend`] carries a
//! [`KnownHostsMode`] discriminator:
//!
//! * `Strict` — reject any host key not already in the store. No prompt.
//! * `Prompt` — default. Consult the store; on cache miss ask the resolver
//!   (falling back to reject when no resolver is attached).
//! * `AutoTrust` — accept every host key. Integration-test opt-in only;
//!   selected via [`SftpBackend::with_options`] from
//!   `crates/atlas-remote/tests/common/mock.rs`.

use std::sync::{Arc, RwLock};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use atlas_core::RemoteUri;
use russh::client::Handle as RusshHandle;
use russh::keys::key;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType, StatusCode};
use tokio::sync::OnceCell;

use crate::backend::{BackendError, Credentials};
use crate::error::{RemoteError, RemoteErrorKind, RemoteMetadata, RemoteMode, RemoteResult};
use crate::host_key::{HostKeyDecision, HostKeyRequest, HostKeyResolver, KnownHostsMode};
use crate::known_hosts::{fingerprint, HostKeyStatus, KnownHosts};

use super::common::{join_path, normalized_list_path, BackendClient, RemoteEntry};

/// Per-connection options for the SFTP backend.
///
/// Callers who need the default production behaviour (strict +
/// atlas-known-hosts + resolver-driven TOFU) can use [`SftpBackend::new`];
/// tests and specialised UI flows use [`SftpBackend::with_options`] to
/// inject a custom [`KnownHostsMode`] or attach a [`HostKeyResolver`].
#[derive(Clone, Default)]
pub struct SftpOptions {
    /// Trust strategy for the SSH handshake. Defaults to
    /// [`KnownHostsMode::Prompt`].
    pub known_hosts_mode: KnownHostsMode,
    /// Optional resolver for interactive TOFU prompts. Required when
    /// `known_hosts_mode = Prompt` and the server is not already trusted;
    /// production callers attach one supplied by `ConnectController`.
    pub resolver: Option<HostKeyResolver>,
}

static DEFAULT_SFTP_OPTIONS: RwLock<Option<SftpOptions>> = RwLock::new(None);

/// Install a process-wide default [`SftpOptions`] used by
/// [`SftpBackend::new`] (and therefore by every code path that constructs
/// SFTP clients via `RemoteLocationViewModel::open_live`).
///
/// Intended as a Rust-level test seam: integration tests install an
/// [`KnownHostsMode::AutoTrust`] default so they don't require a real
/// known_hosts entry for the throwaway paramiko mock. Production code
/// never calls this — production `ConnectController` flows use
/// [`SftpBackend::with_options`] directly and leave this global unset,
/// preserving the strict + resolver-driven TOFU semantics for anything
/// that opens SFTP without going through the modal.
pub fn set_default_sftp_options(options: SftpOptions) {
    if let Ok(mut guard) = DEFAULT_SFTP_OPTIONS.write() {
        *guard = Some(options);
    }
}

/// Clear the process-wide default installed by
/// [`set_default_sftp_options`]. Intended for teardown in tests that
/// briefly override the default and want to restore production semantics.
pub fn clear_default_sftp_options() {
    if let Ok(mut guard) = DEFAULT_SFTP_OPTIONS.write() {
        *guard = None;
    }
}

fn current_default_options() -> SftpOptions {
    DEFAULT_SFTP_OPTIONS
        .read()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_default()
}

/// Server-key handler for the russh client.
///
/// The handler consults [`crate::known_hosts::KnownHosts`] on every
/// handshake and — depending on the configured [`KnownHostsMode`] and
/// whether a [`HostKeyResolver`] is attached — either accepts silently,
/// prompts the user, or rejects.
struct HostKeyHandler {
    host: String,
    port: u16,
    mode: KnownHostsMode,
    resolver: Option<HostKeyResolver>,
}

#[async_trait]
impl russh::client::Handler for HostKeyHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &key::PublicKey,
    ) -> Result<bool, Self::Error> {
        if matches!(self.mode, KnownHostsMode::AutoTrust) {
            tracing::debug!(
                host = %self.host,
                "sftp: AutoTrust mode accepted host key",
            );
            return Ok(true);
        }

        let store = match KnownHosts::load() {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(error = %err, "sftp: known_hosts load failed; treating as empty");
                // Fall back to an "empty" store via load_from_path on a
                // definitely-missing path. Any I/O failure at this point
                // means the atlas config dir is unreachable — we still
                // want to give the resolver a chance to prompt.
                let missing = std::path::PathBuf::from("/nonexistent");
                KnownHosts::load_from_path(&missing).unwrap_or_else(|_| {
                    // Both paths failed; fall through with an empty
                    // in-memory store constructed via the same route.
                    KnownHosts::load_from_path(&missing)
                        .expect("load_from_path on missing path yields empty store")
                })
            }
        };
        let status = store.check(&self.host, self.port, server_public_key);
        let offered_fp = fingerprint(server_public_key);
        tracing::debug!(
            host = %self.host,
            port = self.port,
            offered = %offered_fp,
            status = ?status,
            "sftp: host-key check",
        );

        match &status {
            HostKeyStatus::Trusted => Ok(true),
            HostKeyStatus::Unknown | HostKeyStatus::Mismatch { .. } => {
                if matches!(self.mode, KnownHostsMode::Strict) {
                    tracing::warn!(
                        host = %self.host,
                        "sftp: rejecting host key (Strict mode)",
                    );
                    return Ok(false);
                }
                let Some(resolver) = self.resolver.as_ref() else {
                    tracing::warn!(
                        host = %self.host,
                        "sftp: no resolver attached; rejecting untrusted host key",
                    );
                    return Ok(false);
                };
                let request = HostKeyRequest {
                    host: self.host.clone(),
                    port: self.port,
                    offered_fingerprint: offered_fp,
                    current_status: status.clone(),
                };
                let decision = resolver.resolve(request).await;
                match decision {
                    HostKeyDecision::TrustOnce => Ok(true),
                    HostKeyDecision::TrustAlways => {
                        // Persist the accepted key. Any failure here is
                        // logged but does not block the connection —
                        // the user already said "trust always" and it
                        // would be jarring to error out on a filesystem
                        // hiccup after the accept.
                        match KnownHosts::load() {
                            Ok(mut store) => {
                                if let Err(err) =
                                    store.add(&self.host, self.port, server_public_key)
                                {
                                    tracing::warn!(error = %err, "sftp: known_hosts add failed");
                                } else if let Err(err) = store.save() {
                                    tracing::warn!(error = %err, "sftp: known_hosts save failed");
                                } else {
                                    tracing::info!(
                                        host = %self.host,
                                        "sftp: host key persisted to known_hosts",
                                    );
                                }
                            }
                            Err(err) => {
                                tracing::warn!(error = %err, "sftp: could not reload known_hosts for save");
                            }
                        }
                        Ok(true)
                    }
                    HostKeyDecision::Cancel => {
                        tracing::info!(host = %self.host, "sftp: user cancelled host-key prompt");
                        Ok(false)
                    }
                }
            }
        }
    }
}

/// Once-established SSH+SFTP context. Held inside a `OnceCell` so
/// that connection setup happens exactly once per backend instance.
struct Live {
    /// The SFTP session. `russh-sftp`'s methods are `&self` and
    /// internally serialise over a single channel, so no outer
    /// mutex is needed.
    sftp: SftpSession,
    /// Keep the SSH session alive alongside the SFTP session so its
    /// background task servicing the channel isn't dropped.
    _session: RusshHandle<HostKeyHandler>,
}

/// SFTP backend: config + lazily-established connection.
pub(crate) struct SftpBackend {
    host: String,
    port: u16,
    user: String,
    credentials: Credentials,
    /// Base path — normalised: no leading `/`, may end with `/`.
    root: String,
    /// Host-key trust configuration used by [`HostKeyHandler`].
    options: SftpOptions,
    live: OnceCell<Live>,
}

impl SftpBackend {
    /// Validate the URI + credentials shape. Returns immediately;
    /// no network I/O happens here.
    pub(crate) fn new(uri: &RemoteUri, credentials: Credentials) -> Result<Self, BackendError> {
        Self::with_options(uri, credentials, current_default_options())
    }

    /// Same as [`Self::new`] but accepts caller-supplied trust options.
    /// This is the seam integration tests use to pass
    /// [`KnownHostsMode::AutoTrust`], and the connect controller uses to
    /// attach an interactive [`HostKeyResolver`].
    pub(crate) fn with_options(
        uri: &RemoteUri,
        credentials: Credentials,
        options: SftpOptions,
    ) -> Result<Self, BackendError> {
        let host = uri
            .host
            .as_deref()
            .ok_or_else(|| BackendError::InvalidCredentials {
                backend: "sftp",
                detail: "missing host".to_owned(),
            })?
            .to_owned();
        let port = uri.port.unwrap_or(22);
        let user = uri
            .username
            .as_deref()
            .ok_or_else(|| BackendError::InvalidCredentials {
                backend: "sftp",
                detail: "missing username".to_owned(),
            })?;
        if user.is_empty() {
            return Err(BackendError::InvalidCredentials {
                backend: "sftp",
                detail: "missing username".to_owned(),
            });
        }
        if let Credentials::Iam { .. } = credentials {
            return Err(BackendError::InvalidCredentials {
                backend: "sftp",
                detail: "IAM credentials not supported for SFTP".to_owned(),
            });
        }

        Ok(Self {
            host,
            port,
            user: user.to_owned(),
            credentials,
            root: normalized_list_path(&uri.path),
            options,
            live: OnceCell::new(),
        })
    }

    /// Lazily establish the SSH connection + SFTP subsystem.
    async fn live(&self) -> RemoteResult<&Live> {
        self.live
            .get_or_try_init(|| async { self.connect().await })
            .await
    }

    async fn connect(&self) -> RemoteResult<Live> {
        let config = Arc::new(russh::client::Config::default());
        let handler = HostKeyHandler {
            host: self.host.clone(),
            port: self.port,
            mode: self.options.known_hosts_mode,
            resolver: self.options.resolver.clone(),
        };
        let mut session = russh::client::connect(config, (self.host.as_str(), self.port), handler)
            .await
            .map_err(|e| RemoteError::new(RemoteErrorKind::Network, format!("ssh connect: {e}")))?;

        let auth_ok = match &self.credentials {
            Credentials::SshKey(path, passphrase) => {
                let keypair =
                    russh::keys::load_secret_key(path, passphrase.as_deref()).map_err(|e| {
                        RemoteError::new(
                            RemoteErrorKind::PermissionDenied,
                            format!("load SSH key {}: {e}", path.display()),
                        )
                    })?;
                session
                    .authenticate_publickey(&self.user, Arc::new(keypair))
                    .await
                    .map_err(|e| {
                        RemoteError::new(
                            RemoteErrorKind::PermissionDenied,
                            format!("publickey auth: {e}"),
                        )
                    })?
            }
            Credentials::Password(password) => session
                .authenticate_password(&self.user, password.clone())
                .await
                .map_err(|e| {
                    RemoteError::new(
                        RemoteErrorKind::PermissionDenied,
                        format!("password auth: {e}"),
                    )
                })?,
            Credentials::Anonymous => session
                .authenticate_password(&self.user, "")
                .await
                .map_err(|e| {
                    RemoteError::new(
                        RemoteErrorKind::PermissionDenied,
                        format!("anonymous auth: {e}"),
                    )
                })?,
            Credentials::Iam { .. } => unreachable!("filtered out in ::new"),
        };
        if !auth_ok {
            return Err(RemoteError::new(
                RemoteErrorKind::PermissionDenied,
                "authentication rejected by server".to_owned(),
            ));
        }

        let channel = session.channel_open_session().await.map_err(|e| {
            RemoteError::new(
                RemoteErrorKind::Network,
                format!("open session channel: {e}"),
            )
        })?;
        channel.request_subsystem(true, "sftp").await.map_err(|e| {
            RemoteError::new(
                RemoteErrorKind::Network,
                format!("request sftp subsystem: {e}"),
            )
        })?;
        let sftp = SftpSession::new(channel.into_stream()).await.map_err(|e| {
            RemoteError::new(RemoteErrorKind::Network, format!("open sftp session: {e}"))
        })?;

        Ok(Live {
            sftp,
            _session: session,
        })
    }

    /// Resolve a caller-supplied root-relative path into the
    /// absolute path the SFTP server expects. Empty caller path
    /// means the backend root.
    fn abs(&self, path: &str) -> String {
        let joined = join_path(self.root.trim_end_matches('/'), path);
        if joined.is_empty() {
            ".".to_owned()
        } else {
            joined
        }
    }
}

/// Turn a russh-sftp error into a `RemoteError` with a matching kind.
fn map_sftp_err(err: russh_sftp::client::error::Error) -> RemoteError {
    use russh_sftp::client::error::Error as Se;
    match err {
        Se::Status(ref s) => {
            let kind = match s.status_code {
                StatusCode::NoSuchFile => RemoteErrorKind::NotFound,
                StatusCode::PermissionDenied => RemoteErrorKind::PermissionDenied,
                StatusCode::OpUnsupported => RemoteErrorKind::Unsupported,
                _ => RemoteErrorKind::Unexpected,
            };
            RemoteError::new(
                kind,
                format!("sftp status {:?}: {}", s.status_code, s.error_message),
            )
        }
        other => RemoteError::unexpected(format!("sftp: {other}")),
    }
}

/// Turn a russh-sftp `FileAttributes` into our normalised metadata.
fn attrs_to_metadata(attrs: &russh_sftp::client::fs::Metadata) -> RemoteMetadata {
    let mode = if attrs.is_dir() {
        RemoteMode::Dir
    } else if attrs.is_regular() {
        RemoteMode::File
    } else {
        RemoteMode::Other
    };
    let size = attrs.size.unwrap_or(0);
    let modified = attrs
        .mtime
        .map(|secs| UNIX_EPOCH + Duration::from_secs(u64::from(secs)));
    RemoteMetadata::new(mode, size, modified)
}

fn ftype_to_mode(ft: FileType) -> RemoteMode {
    match ft {
        FileType::Dir => RemoteMode::Dir,
        FileType::File => RemoteMode::File,
        FileType::Symlink | FileType::Other => RemoteMode::Other,
    }
}

#[async_trait]
impl BackendClient for SftpBackend {
    async fn list(&self, path: &str) -> RemoteResult<Vec<RemoteEntry>> {
        let abs = self.abs(path);
        let listing_root = if abs == "." {
            self.root.trim_end_matches('/').to_owned()
        } else {
            abs.trim_end_matches('/').to_owned()
        };
        let live = self.live().await?;
        let read = live.sftp.read_dir(&abs).await.map_err(map_sftp_err)?;
        let mut out = Vec::new();
        for entry in read {
            let name = entry.file_name();
            let attrs = entry.metadata();
            let child_path = if listing_root.is_empty() {
                name.clone()
            } else {
                format!("{listing_root}/{name}")
            };
            // For symlinks: read the target string and stat-follow to
            // resolve the target's kind + size. This makes the entry
            // dispatch transparently — a symlink pointing to a
            // directory shows up as `RemoteMode::Dir`, one pointing
            // to a file shows up as `RemoteMode::File`, and broken
            // symlinks fall through as `RemoteMode::Other` with the
            // raw target populated.
            let (mode, size, modified, symlink_target) = if attrs.file_type().is_symlink() {
                let raw_target = live.sftp.read_link(child_path.clone()).await.ok();
                match live.sftp.metadata(child_path.clone()).await {
                    Ok(target_attrs) => {
                        let target_mode = if target_attrs.is_dir() {
                            RemoteMode::Dir
                        } else if target_attrs.is_regular() {
                            RemoteMode::File
                        } else {
                            RemoteMode::Other
                        };
                        let target_size = target_attrs.size.unwrap_or(0);
                        let target_modified = target_attrs
                            .mtime
                            .map(|secs| UNIX_EPOCH + Duration::from_secs(u64::from(secs)));
                        (target_mode, target_size, target_modified, raw_target)
                    }
                    Err(err) => {
                        tracing::debug!(
                            path = %child_path,
                            %err,
                            "sftp list: broken symlink, keeping mode=Other",
                        );
                        (RemoteMode::Other, 0, None, raw_target)
                    }
                }
            } else {
                let mode = ftype_to_mode(attrs.file_type());
                let size = attrs.size.unwrap_or(0);
                let modified = attrs
                    .mtime
                    .map(|secs| UNIX_EPOCH + Duration::from_secs(u64::from(secs)));
                (mode, size, modified, None)
            };
            out.push(RemoteEntry {
                path: child_path,
                mode,
                size,
                modified,
                symlink_target,
            });
        }
        Ok(out)
    }

    async fn read(&self, path: &str) -> RemoteResult<Vec<u8>> {
        let abs = self.abs(path);
        let live = self.live().await?;
        let mut file = live.sftp.open(&abs).await.map_err(map_sftp_err)?;
        let mut buf = Vec::new();
        use tokio::io::AsyncReadExt;
        file.read_to_end(&mut buf)
            .await
            .map_err(|e| RemoteError::unexpected(format!("sftp read: {e}")))?;
        Ok(buf)
    }

    async fn stat(&self, path: &str) -> RemoteResult<RemoteMetadata> {
        let abs = self.abs(path);
        let live = self.live().await?;
        let attrs = live.sftp.metadata(&abs).await.map_err(map_sftp_err)?;
        Ok(attrs_to_metadata(&attrs))
    }

    async fn write(&self, path: &str, bytes: Vec<u8>) -> RemoteResult<()> {
        let abs = self.abs(path);
        let live = self.live().await?;
        let mut file = live.sftp.create(&abs).await.map_err(map_sftp_err)?;
        use tokio::io::AsyncWriteExt;
        file.write_all(&bytes)
            .await
            .map_err(|e| RemoteError::unexpected(format!("sftp write: {e}")))?;
        file.shutdown()
            .await
            .map_err(|e| RemoteError::unexpected(format!("sftp close: {e}")))?;
        Ok(())
    }

    async fn create_dir(&self, path: &str) -> RemoteResult<()> {
        let abs = self.abs(path.trim_end_matches('/'));
        let live = self.live().await?;
        live.sftp.create_dir(&abs).await.map_err(map_sftp_err)
    }

    async fn rename(&self, from: &str, to: &str) -> RemoteResult<()> {
        let from = self.abs(from);
        let to = self.abs(to);
        let live = self.live().await?;
        live.sftp.rename(&from, &to).await.map_err(map_sftp_err)
    }

    async fn delete(&self, path: &str) -> RemoteResult<()> {
        let abs = self.abs(path);
        let live = self.live().await?;
        // Try file first, fall back to dir if the server reports the
        // wrong-type error.
        match live.sftp.remove_file(&abs).await {
            Ok(()) => Ok(()),
            Err(e) => {
                if let russh_sftp::client::error::Error::Status(s) = &e {
                    if matches!(
                        s.status_code,
                        StatusCode::Failure | StatusCode::OpUnsupported
                    ) {
                        return live.sftp.remove_dir(&abs).await.map_err(map_sftp_err);
                    }
                }
                Err(map_sftp_err(e))
            }
        }
    }

    async fn read_link(&self, path: &str) -> RemoteResult<String> {
        let abs = self.abs(path);
        let live = self.live().await?;
        live.sftp.read_link(&abs).await.map_err(map_sftp_err)
    }
}
