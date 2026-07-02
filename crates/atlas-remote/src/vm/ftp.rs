//! FTP backend built on `suppaftp`.
//!
//! We use the *sync* client wrapped in `tokio::task::spawn_blocking`.
//! The async variant of suppaftp pins to `async-std`, which would
//! force a second async runtime into the process. spawn_blocking
//! keeps the runtime story simple at the cost of one OS thread per
//! concurrent FTP op — fine for the use case (interactive browsing +
//! occasional transfers).
//!
//! Every operation acquires the shared `Mutex<FtpStream>` because
//! FTP is a stateful connection-oriented protocol: only one
//! transfer at a time.

use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use atlas_core::RemoteUri;
use suppaftp::{types::FileType as FtpFileType, types::Response, FtpError, FtpStream, Status};
use tokio::sync::OnceCell;

use crate::backend::{BackendError, Credentials};
use crate::error::{RemoteError, RemoteErrorKind, RemoteMetadata, RemoteMode, RemoteResult};

use super::common::{basename, join_path, normalized_list_path, BackendClient, RemoteEntry};

/// FTP backend: config + lazily-established `FtpStream`.
///
/// FTP is stateful (CWD, transfer mode) so we hold a single
/// `Mutex<FtpStream>` and serialise operations. The stream itself is
/// initialised lazily on the first async operation via a
/// `tokio::sync::OnceCell` — same rationale as [`super::sftp`].
pub(crate) struct FtpBackend {
    host: String,
    port: u16,
    user: String,
    password: String,
    root: String,
    stream: OnceCell<Arc<Mutex<FtpStream>>>,
}

impl FtpBackend {
    /// Validate the URI + credentials shape. No network I/O.
    pub(crate) fn new(uri: &RemoteUri, credentials: Credentials) -> Result<Self, BackendError> {
        let host = uri
            .host
            .as_deref()
            .ok_or_else(|| BackendError::InvalidCredentials {
                backend: "ftp",
                detail: "missing host".to_owned(),
            })?
            .to_owned();
        let port = uri.port.unwrap_or(21);
        let user = uri
            .username
            .clone()
            .unwrap_or_else(|| "anonymous".to_owned());

        let password = match credentials {
            Credentials::Password(p) => p,
            Credentials::Anonymous => String::new(),
            Credentials::SshKey(_, _) | Credentials::Iam { .. } => {
                return Err(BackendError::InvalidCredentials {
                    backend: "ftp",
                    detail: "only Password or Anonymous credentials are valid for FTP".to_owned(),
                });
            }
        };

        Ok(Self {
            host,
            port,
            user,
            password,
            root: normalized_list_path(&uri.path),
            stream: OnceCell::new(),
        })
    }

    async fn live(&self) -> RemoteResult<Arc<Mutex<FtpStream>>> {
        let stream = self
            .stream
            .get_or_try_init(|| async { self.connect().await })
            .await?;
        Ok(Arc::clone(stream))
    }

    async fn connect(&self) -> RemoteResult<Arc<Mutex<FtpStream>>> {
        let host_port = format!("{}:{}", self.host, self.port);
        let user = self.user.clone();
        let password = self.password.clone();
        let stream = tokio::task::spawn_blocking(move || -> RemoteResult<FtpStream> {
            let mut s = FtpStream::connect(&host_port).map_err(map_ftp_err)?;
            s.login(&user, &password).map_err(map_ftp_err)?;
            s.transfer_type(suppaftp::types::FileType::Binary)
                .map_err(map_ftp_err)?;
            Ok(s)
        })
        .await
        .map_err(|e| RemoteError::unexpected(format!("ftp connect join: {e}")))??;
        Ok(Arc::new(Mutex::new(stream)))
    }

    fn abs(&self, path: &str) -> String {
        let joined = join_path(self.root.trim_end_matches('/'), path);
        if joined.is_empty() {
            "/".to_owned()
        } else if joined.starts_with('/') {
            joined
        } else {
            format!("/{joined}")
        }
    }
}

fn map_ftp_err(err: FtpError) -> RemoteError {
    let msg = err.to_string();
    let kind = match &err {
        FtpError::UnexpectedResponse(resp) => match resp.status {
            Status::NotAvailable | Status::FileUnavailable | Status::BadFilename => {
                RemoteErrorKind::NotFound
            }
            Status::NotLoggedIn
            | Status::NeedPassword
            | Status::InvalidCredentials
            | Status::LoginNeedAccount => RemoteErrorKind::PermissionDenied,
            Status::NotImplemented | Status::NotImplementedParameter => {
                RemoteErrorKind::Unsupported
            }
            _ => RemoteErrorKind::Unexpected,
        },
        FtpError::ConnectionError(_) => RemoteErrorKind::Network,
        _ => RemoteErrorKind::Unexpected,
    };
    RemoteError::new(kind, msg)
}

/// Silence the compiler about status variants we don't map.
const _: fn(&Response) = |_r| {};

#[async_trait]
impl BackendClient for FtpBackend {
    async fn list(&self, path: &str) -> RemoteResult<Vec<RemoteEntry>> {
        let stream = self.live().await?;
        let abs = self.abs(path);
        let listing_root = abs.trim_end_matches('/').to_owned();
        tokio::task::spawn_blocking(move || -> RemoteResult<Vec<RemoteEntry>> {
            let mut guard = stream.lock().expect("ftp mutex poisoned");
            let target = if abs.trim_matches('/').is_empty() {
                None
            } else {
                Some(abs.as_str())
            };
            // Prefer MLSD (machine-readable) when the server supports it;
            // fall back to LIST for older daemons.
            let entries = match guard.mlsd(target) {
                Ok(rows) => parse_mlsd(&rows, &listing_root),
                Err(_) => {
                    let rows = guard.list(target).map_err(map_ftp_err)?;
                    parse_list(&rows, &listing_root)
                }
            };
            Ok(entries)
        })
        .await
        .map_err(|e| RemoteError::unexpected(format!("ftp list join: {e}")))?
    }

    async fn read(&self, path: &str) -> RemoteResult<Vec<u8>> {
        let stream = self.live().await?;
        let abs = self.abs(path);
        tokio::task::spawn_blocking(move || -> RemoteResult<Vec<u8>> {
            let mut guard = stream.lock().expect("ftp mutex poisoned");
            let bytes = guard
                .retr_as_buffer(&abs)
                .map_err(map_ftp_err)?
                .into_inner();
            Ok(bytes)
        })
        .await
        .map_err(|e| RemoteError::unexpected(format!("ftp read join: {e}")))?
    }

    async fn stat(&self, path: &str) -> RemoteResult<RemoteMetadata> {
        let stream = self.live().await?;
        let abs = self.abs(path);
        tokio::task::spawn_blocking(move || -> RemoteResult<RemoteMetadata> {
            let mut guard = stream.lock().expect("ftp mutex poisoned");
            // Try SIZE first — succeeds only for regular files.
            let size_res = guard.size(&abs);
            match size_res {
                Ok(sz) => {
                    let modified = guard.mdtm(&abs).ok().and_then(|dt| {
                        let secs = dt.and_utc().timestamp();
                        if secs >= 0 {
                            Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
                        } else {
                            None
                        }
                    });
                    Ok(RemoteMetadata::new(RemoteMode::File, sz as u64, modified))
                }
                Err(_) => {
                    let pwd = guard.pwd().map_err(map_ftp_err)?;
                    match guard.cwd(&abs) {
                        Ok(()) => {
                            let _ = guard.cwd(&pwd);
                            Ok(RemoteMetadata::new(RemoteMode::Dir, 0, None))
                        }
                        Err(e) => Err(map_ftp_err(e)),
                    }
                }
            }
        })
        .await
        .map_err(|e| RemoteError::unexpected(format!("ftp stat join: {e}")))?
    }

    async fn write(&self, path: &str, bytes: Vec<u8>) -> RemoteResult<()> {
        let stream = self.live().await?;
        let abs = self.abs(path);
        tokio::task::spawn_blocking(move || -> RemoteResult<()> {
            let mut guard = stream.lock().expect("ftp mutex poisoned");
            let mut cursor = Cursor::new(bytes);
            guard
                .put_file(&abs, &mut cursor)
                .map(|_| ())
                .map_err(map_ftp_err)
        })
        .await
        .map_err(|e| RemoteError::unexpected(format!("ftp write join: {e}")))?
    }

    async fn create_dir(&self, path: &str) -> RemoteResult<()> {
        let stream = self.live().await?;
        let abs = self.abs(path.trim_end_matches('/'));
        tokio::task::spawn_blocking(move || -> RemoteResult<()> {
            let mut guard = stream.lock().expect("ftp mutex poisoned");
            guard.mkdir(&abs).map_err(map_ftp_err)
        })
        .await
        .map_err(|e| RemoteError::unexpected(format!("ftp mkdir join: {e}")))?
    }

    async fn rename(&self, from: &str, to: &str) -> RemoteResult<()> {
        let stream = self.live().await?;
        let from = self.abs(from);
        let to = self.abs(to);
        tokio::task::spawn_blocking(move || -> RemoteResult<()> {
            let mut guard = stream.lock().expect("ftp mutex poisoned");
            guard.rename(&from, &to).map_err(map_ftp_err)
        })
        .await
        .map_err(|e| RemoteError::unexpected(format!("ftp rename join: {e}")))?
    }

    async fn delete(&self, path: &str) -> RemoteResult<()> {
        let stream = self.live().await?;
        let abs = self.abs(path);
        tokio::task::spawn_blocking(move || -> RemoteResult<()> {
            let mut guard = stream.lock().expect("ftp mutex poisoned");
            // Try `rm` (DELE) first; fall back to rmdir on failure so
            // callers don't have to know the entry type in advance.
            match guard.rm(&abs) {
                Ok(()) => Ok(()),
                Err(_) => guard.rmdir(&abs).map_err(map_ftp_err),
            }
        })
        .await
        .map_err(|e| RemoteError::unexpected(format!("ftp delete join: {e}")))?
    }
}

/// Parse an MLSD response into `RemoteEntry` rows. MLSD lines look
/// like `type=file;size=42;modify=20241231235959; filename.txt`.
fn parse_mlsd(rows: &[String], listing_root: &str) -> Vec<RemoteEntry> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        // Split at the space that separates fact-list and filename.
        let Some(sep) = row.find(' ') else { continue };
        let facts = &row[..sep];
        let name = row[sep + 1..].trim();
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        let mut mode = RemoteMode::Other;
        let mut size = 0u64;
        let mut modified = None;
        for fact in facts.split(';') {
            let Some((k, v)) = fact.split_once('=') else {
                continue;
            };
            match k.to_ascii_lowercase().as_str() {
                "type" => {
                    mode = match v.to_ascii_lowercase().as_str() {
                        "file" => RemoteMode::File,
                        "dir" | "cdir" | "pdir" => RemoteMode::Dir,
                        _ => RemoteMode::Other,
                    };
                }
                "size" => size = v.parse().unwrap_or(0),
                "modify" => {
                    // YYYYMMDDHHMMSS(.fff) — parse via chrono
                    let ts = &v[..v.len().min(14)];
                    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(ts, "%Y%m%d%H%M%S") {
                        let secs = dt.and_utc().timestamp();
                        if secs >= 0 {
                            modified = Some(UNIX_EPOCH + Duration::from_secs(secs as u64));
                        }
                    }
                }
                _ => {}
            }
        }
        let path = if listing_root.is_empty() || listing_root == "/" {
            name.to_owned()
        } else {
            format!("{}/{name}", listing_root.trim_end_matches('/'))
        };
        out.push(RemoteEntry {
            path,
            mode,
            size,
            modified,
            symlink_target: None,
        });
    }
    out
}

/// Parse a Unix-style `LIST` response (very best-effort). Lines look like:
/// `drwxr-xr-x   2 user group     4096 Jan  1 12:00 name`.
fn parse_list(rows: &[String], listing_root: &str) -> Vec<RemoteEntry> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let parts: Vec<&str> = row.split_whitespace().collect();
        if parts.len() < 9 {
            continue;
        }
        let perms = parts[0];
        let mode = if perms.starts_with('d') {
            RemoteMode::Dir
        } else if perms.starts_with('-') {
            RemoteMode::File
        } else {
            RemoteMode::Other
        };
        let size = parts[4].parse::<u64>().unwrap_or(0);
        let name = parts[8..].join(" ");
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        let path = if listing_root.is_empty() || listing_root == "/" {
            name.clone()
        } else {
            format!("{}/{}", listing_root.trim_end_matches('/'), name)
        };
        out.push(RemoteEntry {
            path,
            mode,
            size,
            modified: None,
            symlink_target: None,
        });
    }
    out
}

// Silence unused-import warnings — these helpers are here for future
// work (recursive listing, symlink walking, etc.).
#[allow(dead_code)]
const _: fn(&str) -> String = basename;
#[allow(dead_code)]
const _: fn(FtpFileType) = |_| {};
