//! [`Location`](atlas_core::Location) → [`LocationViewModel`] entry point.
//!
//! The [`open`] function is the single public dispatch that picks the right
//! backend based on [`atlas_core::BackendKind`]. Local locations bypass any
//! network client and go straight to [`atlas_fs::InMemoryLocationViewModel`];
//! remote ones are served by [`crate::vm::RemoteLocationViewModel`] over one
//! of the four per-protocol clients under `crate::vm::{sftp,ftp,webdav,s3}`.
//!
//! Credentials for remote backends are supplied by the caller via [`Credentials`].
//! Callers that stored credentials in the OS keychain can look them up first
//! with [`crate::secrets::retrieve`] and pass the resulting string as
//! [`Credentials::Password`] (or IAM secret components).

use std::path::PathBuf;
use std::sync::Arc;

use atlas_core::{BackendKind, Location, RemoteUri};
use atlas_fs::{InMemoryLocationViewModel, LocationViewModel, OpenOptions};
use thiserror::Error;

use crate::vm::{
    common::BackendClient, ftp::FtpBackend, s3::S3Backend, sftp::SftpBackend,
    webdav::WebDavBackend, RemoteLocationViewModel,
};

/// Credentials used to open a remote backend.
#[derive(Debug, Clone)]
pub enum Credentials {
    /// Static password / API secret.
    Password(String),
    /// SSH private-key auth. Optional passphrase for the key.
    SshKey(PathBuf, Option<String>),
    /// IAM-style credentials (S3-compatible).
    Iam {
        /// Access key id.
        access_key_id: String,
        /// Secret access key.
        secret_key: String,
        /// Optional STS session token.
        session_token: Option<String>,
    },
    /// Anonymous / no credentials (public buckets, guest FTP).
    Anonymous,
}

/// Errors returned by [`open`].
#[derive(Debug, Error)]
pub enum BackendError {
    /// The scheme in the [`RemoteUri`] is not supported by this build.
    #[error("unsupported backend: {0}")]
    UnsupportedBackend(String),
    /// The credentials supplied are not accepted by the chosen backend.
    #[error("invalid credentials for {backend}: {detail}")]
    InvalidCredentials {
        /// Backend that rejected the credentials.
        backend: &'static str,
        /// Human-readable detail.
        detail: String,
    },
    /// Any other backend-side error (network, config, …).
    #[error("backend error: {0}")]
    Backend(String),
}

/// Open `location` with the appropriate backend and return an
/// [`Arc<dyn LocationViewModel>`] the UI can subscribe to.
///
/// For [`BackendKind::Local`] the `credentials` argument is ignored and the
/// call delegates to [`InMemoryLocationViewModel::open_live`].
///
/// # Errors
///
/// Returns [`BackendError::UnsupportedBackend`] when a build without the
/// requested service feature is asked to open that scheme, and
/// [`BackendError::InvalidCredentials`] when the supplied [`Credentials`]
/// aren't accepted by the chosen backend (e.g. IAM against SFTP, SSH key
/// against S3). Any error surfacing during the actual network handshake
/// (auth rejection, unreachable host, DNS failure, …) is delivered
/// asynchronously via the returned view model's subscribe channel as a
/// [`atlas_fs::ViewModelEvent::Error`].
pub fn open(
    location: &Location,
    credentials: Credentials,
    opts: OpenOptions,
) -> Result<Arc<dyn LocationViewModel>, BackendError> {
    match location {
        Location::Local(path) => Ok(InMemoryLocationViewModel::open_live(path.clone(), opts)),
        Location::Remote(uri, kind) => open_remote(uri, *kind, credentials, opts),
    }
}

fn open_remote(
    uri: &RemoteUri,
    kind: BackendKind,
    credentials: Credentials,
    opts: OpenOptions,
) -> Result<Arc<dyn LocationViewModel>, BackendError> {
    let pool = crate::pool::global();
    let key = crate::pool::PoolKey::new(
        kind,
        uri.host.clone().unwrap_or_default(),
        uri.port,
        uri.username.clone(),
        &credentials,
    );
    let client = pool.get_or_open(&key, || {
        let built: Arc<dyn BackendClient> = match kind {
            BackendKind::Local => {
                return Err(BackendError::UnsupportedBackend(
                    "local kind on remote location".to_owned(),
                ));
            }
            BackendKind::Sftp => Arc::new(SftpBackend::new(uri, credentials.clone())?),
            BackendKind::Ftp => Arc::new(FtpBackend::new(uri, credentials.clone())?),
            BackendKind::WebDav => Arc::new(WebDavBackend::new(uri, credentials.clone())?),
            BackendKind::S3 => Arc::new(S3Backend::new(uri, credentials.clone())?),
        };
        Ok(built)
    })?;

    Ok(RemoteLocationViewModel::from_client(
        uri.clone(),
        kind,
        client,
        opts,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::Location;
    use tempfile::TempDir;

    #[test]
    fn open_local_delegates_to_in_memory_view_model() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("hello.txt"), b"hi").expect("write");

        let location = Location::local(dir.path());
        let vm = open(&location, Credentials::Anonymous, OpenOptions::default())
            .expect("open local backend");

        // Give the loader thread a moment to enumerate the (tiny) directory.
        let sub = vm.subscribe();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !vm.is_loaded() && std::time::Instant::now() < deadline {
            let _ = sub.recv_timeout(std::time::Duration::from_millis(50));
        }
        assert!(vm.is_loaded(), "local vm should load");
        let names: Vec<_> = vm.entries().iter().map(|e| e.name.clone()).collect();
        assert!(names.iter().any(|n| n == "hello.txt"));
    }
}
