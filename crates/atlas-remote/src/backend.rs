//! [`Location`](atlas_core::Location) → [`LocationViewModel`] entry point.
//!
//! The [`open`] function is the single public dispatch that picks the right
//! backend based on [`atlas_core::BackendKind`]. Local locations bypass OpenDAL
//! and go straight to [`atlas_fs::InMemoryLocationViewModel`]; remote ones are
//! served by [`crate::opendal_vm::OpenDalLocationViewModel`].
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

use crate::opendal_vm::OpenDalLocationViewModel;

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
    /// OpenDAL rejected the configuration (missing bucket, bad host, …).
    #[error("opendal error: {0}")]
    OpenDal(#[from] opendal::Error),
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
/// [`BackendError::OpenDal`] for any configuration or transport error surfaced
/// by OpenDAL during operator construction.
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
    if matches!(kind, BackendKind::Local) {
        // A Location::Remote should never carry BackendKind::Local; treat it
        // as a caller bug rather than silently opening the wrong thing.
        return Err(BackendError::UnsupportedBackend(
            "local kind on remote location".to_owned(),
        ));
    }
    let vm = OpenDalLocationViewModel::open_live(uri.clone(), kind, credentials, opts)?;
    Ok(vm)
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
        let names: Vec<_> = vm.entries().into_iter().map(|e| e.name).collect();
        assert!(names.iter().any(|n| n == "hello.txt"));
    }
}
