//! Platform-specific socket path resolution, listener, and connector helpers.

use std::path::{Path, PathBuf};

use interprocess::local_socket::tokio::{
    prelude::*, Listener as LocalListener, RecvHalf, SendHalf, Stream as LocalStream,
};
#[cfg(unix)]
use interprocess::local_socket::{GenericFilePath, ToFsName};
#[cfg(windows)]
use interprocess::local_socket::{GenericNamespaced, ToNsName};
use interprocess::local_socket::{ListenerOptions, Name};

use crate::error::{IpcError, Result};

/// Opaque listener handle.
pub struct Listener {
    inner: LocalListener,
}

/// Opaque connected stream, split into read/write halves.
pub struct Stream {
    /// Read half of the connected local socket stream.
    pub recv: RecvHalf,
    /// Write half of the connected local socket stream.
    pub send: SendHalf,
}

/// Return the default socket path for atlas-indexd.
///
/// Override by setting the `ATLAS_IPC_SOCKET` environment variable.
pub fn default_socket_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("ATLAS_IPC_SOCKET") {
        return Ok(PathBuf::from(path));
    }

    #[cfg(target_os = "macos")]
    {
        let dirs = directories::ProjectDirs::from("dev", "atlas", "atlas")
            .ok_or_else(|| IpcError::Io(std::io::Error::other("could not determine home dir")))?;
        Ok(dirs.data_dir().join("indexd.sock"))
    }

    #[cfg(target_os = "linux")]
    {
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
            let user = std::env::var("USER").unwrap_or_else(|_| "atlas".into());
            format!("/tmp/atlas-{user}")
        });
        Ok(PathBuf::from(runtime).join("atlas").join("indexd.sock"))
    }

    #[cfg(target_os = "windows")]
    {
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "atlas".into());
        Ok(PathBuf::from(format!(r"\\.\pipe\atlas-indexd-{user}")))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err(IpcError::Io(std::io::Error::other(
            "unsupported platform for default_socket_path",
        )))
    }
}

#[cfg(unix)]
fn local_socket_name(path: &Path) -> std::io::Result<Name<'_>> {
    path.to_fs_name::<GenericFilePath>()
}

#[cfg(windows)]
fn local_socket_name(path: &Path) -> std::io::Result<Name<'static>> {
    // Named Pipe names on Windows must outlive the temporary Cow<str>
    // that `Path::to_string_lossy` returns. Own the string, then leak
    // it into a &'static str so the resulting `Name` isn't borrowing
    // a stack local. The leak is deliberate: this function is called
    // once per IPC connect/listen, so total leaked bytes are O(number
    // of connect calls in the process's lifetime) — bounded and small.
    let owned: String = path.to_string_lossy().into_owned();
    let leaked: &'static str = Box::leak(owned.into_boxed_str());
    leaked.to_ns_name::<GenericNamespaced>()
}

/// Create a [`Listener`] at `path`.
///
/// On Unix, removes a stale socket file if one already exists.
pub async fn listen(path: &Path) -> Result<Listener> {
    #[cfg(unix)]
    {
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let name = local_socket_name(path).map_err(IpcError::Io)?;
    let opts = ListenerOptions::new().name(name);
    let inner = opts.create_tokio().map_err(IpcError::Io)?;
    Ok(Listener { inner })
}

/// Connect to the listener at `path`.
///
/// Returns [`IpcError::NotRunning`] if nothing is listening.
pub async fn connect(path: &Path) -> Result<Stream> {
    let name = local_socket_name(path).map_err(IpcError::Io)?;
    let stream = LocalStream::connect(name).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::ConnectionRefused
            || error.kind() == std::io::ErrorKind::NotFound
        {
            IpcError::NotRunning {
                path: path.to_owned(),
            }
        } else {
            IpcError::Io(error)
        }
    })?;
    let (recv, send) = stream.split();
    Ok(Stream { recv, send })
}

impl Listener {
    /// Accept the next incoming connection.
    pub async fn accept(&self) -> Result<Stream> {
        let stream = self.inner.accept().await.map_err(IpcError::Io)?;
        let (recv, send) = stream.split();
        Ok(Stream { recv, send })
    }
}
