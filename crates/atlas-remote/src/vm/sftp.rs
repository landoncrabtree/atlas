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
//! # Known-hosts strategy
//!
//! Production always rejects unknown host keys. Setting
//! `ATLAS_SFTP_KNOWN_HOSTS_STRATEGY=accept` in the environment
//! disables that check — for integration tests against ephemeral
//! mock servers only. Never set in production.

use std::sync::Arc;
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

use super::common::{join_path, normalized_list_path, BackendClient, RemoteEntry};

/// Whether the process is in "accept-unknown-host-key" test mode.
fn accept_any_host_key() -> bool {
    matches!(
        std::env::var("ATLAS_SFTP_KNOWN_HOSTS_STRATEGY").as_deref(),
        Ok("accept")
    )
}

/// Server-key handler for the russh client. When the accept-any env
/// var is set we accept every host key; otherwise we reject any key
/// we haven't been told about (a full known-hosts implementation is
/// a later phase — for now, unknown = reject).
struct HostKeyHandler {
    accept_any: bool,
}

#[async_trait]
impl russh::client::Handler for HostKeyHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(self.accept_any)
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
    live: OnceCell<Live>,
}

impl SftpBackend {
    /// Validate the URI + credentials shape. Returns immediately;
    /// no network I/O happens here.
    pub(crate) fn new(uri: &RemoteUri, credentials: Credentials) -> Result<Self, BackendError> {
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
            accept_any: accept_any_host_key(),
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
            let mode = ftype_to_mode(attrs.file_type());
            let size = attrs.size.unwrap_or(0);
            let modified = attrs
                .mtime
                .map(|secs| UNIX_EPOCH + Duration::from_secs(u64::from(secs)));
            let child_path = if listing_root.is_empty() {
                name.clone()
            } else {
                format!("{listing_root}/{name}")
            };
            out.push(RemoteEntry {
                path: child_path,
                mode,
                size,
                modified,
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
}
