//! S3-compatible backend built on `object_store`.
//!
//! The `object_store` crate (from Apache Arrow) gives us a uniform
//! interface for AWS S3 plus every S3-compatible service under the
//! sun (MinIO, R2, moto, LocalStack, GCS via `--features gcs`). We
//! only pull in `aws`; other providers can be added later without
//! any new UI plumbing.
//!
//! # Environment-variable overrides
//!
//! Non-AWS endpoints (moto, MinIO, R2, custom gateways) can be
//! configured via:
//!
//! * `ATLAS_S3_ENDPOINT` — full URL, e.g. `http://127.0.0.1:5000`
//! * `ATLAS_S3_REGION` — region string, e.g. `us-east-1`
//!
//! These are unset in production and normal AWS defaults apply.

use std::sync::Arc;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use atlas_core::RemoteUri;
use bytes::Bytes;
use futures::StreamExt;
use object_store::aws::AmazonS3Builder;
use object_store::path::Path as ObjectPath;
use object_store::{Error as ObjectError, ObjectStore, PutPayload};

use crate::backend::{BackendError, Credentials};
use crate::error::{RemoteError, RemoteErrorKind, RemoteMetadata, RemoteMode, RemoteResult};

use super::common::{basename, ensure_dir_slash, normalized_list_path, BackendClient, RemoteEntry};

/// S3 backend handle.
pub(crate) struct S3Backend {
    store: Arc<dyn ObjectStore>,
    root: String,
}

impl S3Backend {
    /// Build the S3 client from URI + credentials + env overrides.
    /// No network I/O — errors here are just config validation.
    pub(crate) fn new(uri: &RemoteUri, credentials: Credentials) -> Result<Self, BackendError> {
        let bucket = uri
            .host
            .as_deref()
            .ok_or_else(|| BackendError::InvalidCredentials {
                backend: "s3",
                detail: "missing bucket (host component of URI)".to_owned(),
            })?;

        let mut builder = AmazonS3Builder::new().with_bucket_name(bucket);

        // Env-var overrides for test / non-AWS endpoints.
        if let Ok(endpoint) = std::env::var("ATLAS_S3_ENDPOINT") {
            if !endpoint.is_empty() {
                let allow_http = endpoint.starts_with("http://");
                builder = builder.with_endpoint(endpoint);
                if allow_http {
                    builder = builder.with_allow_http(true);
                }
            }
        }
        if let Ok(region) = std::env::var("ATLAS_S3_REGION") {
            if !region.is_empty() {
                builder = builder.with_region(region);
            }
        }

        match credentials {
            Credentials::Iam {
                access_key_id,
                secret_key,
                session_token,
            } => {
                builder = builder
                    .with_access_key_id(access_key_id)
                    .with_secret_access_key(secret_key);
                if let Some(tok) = session_token {
                    builder = builder.with_token(tok);
                }
            }
            Credentials::Anonymous => {
                builder = builder.with_skip_signature(true);
            }
            Credentials::Password(_) | Credentials::SshKey(_, _) => {
                return Err(BackendError::InvalidCredentials {
                    backend: "s3",
                    detail: "S3 requires IAM or Anonymous credentials".to_owned(),
                });
            }
        }

        let store = builder
            .build()
            .map_err(|e| BackendError::InvalidCredentials {
                backend: "s3",
                detail: format!("build S3 client: {e}"),
            })?;

        let root = normalized_list_path(&uri.path);

        Ok(Self {
            store: Arc::new(store),
            root,
        })
    }

    fn abs(&self, path: &str) -> String {
        let clean_root = self.root.trim_end_matches('/');
        let clean_path = path.trim_start_matches('/');
        if clean_root.is_empty() {
            clean_path.to_owned()
        } else if clean_path.is_empty() {
            clean_root.to_owned()
        } else {
            format!("{clean_root}/{clean_path}")
        }
    }
}

/// Turn an `object_store::Error` into our normalised kind.
fn map_os_err(err: ObjectError) -> RemoteError {
    let msg = err.to_string();
    let kind = match &err {
        ObjectError::NotFound { .. } => RemoteErrorKind::NotFound,
        ObjectError::PermissionDenied { .. } | ObjectError::Unauthenticated { .. } => {
            RemoteErrorKind::PermissionDenied
        }
        ObjectError::AlreadyExists { .. } => RemoteErrorKind::AlreadyExists,
        ObjectError::NotSupported { .. } | ObjectError::NotImplemented => {
            RemoteErrorKind::Unsupported
        }
        _ => RemoteErrorKind::Unexpected,
    };
    RemoteError::new(kind, msg)
}

fn systime_from_utc(dt: chrono::DateTime<chrono::Utc>) -> Option<std::time::SystemTime> {
    let secs = dt.timestamp();
    if secs >= 0 {
        Some(UNIX_EPOCH + std::time::Duration::from_secs(secs as u64))
    } else {
        None
    }
}

#[async_trait]
impl BackendClient for S3Backend {
    async fn list(&self, path: &str) -> RemoteResult<Vec<RemoteEntry>> {
        let prefix_str = self.abs(path);
        let prefix_dir = ensure_dir_slash(&prefix_str);
        let prefix = if prefix_dir == "/" {
            None
        } else {
            Some(ObjectPath::from(prefix_dir.trim_end_matches('/')))
        };

        // Use list_with_delimiter to get both file entries AND common
        // prefixes (synthetic "directories"), matching what users
        // expect in a file explorer.
        let result = self
            .store
            .list_with_delimiter(prefix.as_ref())
            .await
            .map_err(map_os_err)?;

        let mut out = Vec::with_capacity(result.objects.len() + result.common_prefixes.len());

        for obj in result.objects {
            let path_str = obj.location.to_string();
            let is_dir_marker = path_str.ends_with('/') && obj.size == 0;
            let name = basename(&path_str);
            if name.is_empty() {
                continue;
            }
            let mode = if is_dir_marker {
                RemoteMode::Dir
            } else {
                RemoteMode::File
            };
            out.push(RemoteEntry {
                path: path_str,
                mode,
                size: obj.size as u64,
                modified: systime_from_utc(obj.last_modified),
            });
        }

        for cp in result.common_prefixes {
            out.push(RemoteEntry {
                path: cp.to_string(),
                mode: RemoteMode::Dir,
                size: 0,
                modified: None,
            });
        }

        Ok(out)
    }

    async fn read(&self, path: &str) -> RemoteResult<Vec<u8>> {
        let abs = self.abs(path);
        let loc = ObjectPath::from(abs);
        let get = self.store.get(&loc).await.map_err(map_os_err)?;
        let bytes = get.bytes().await.map_err(map_os_err)?;
        Ok(bytes.to_vec())
    }

    async fn stat(&self, path: &str) -> RemoteResult<RemoteMetadata> {
        let abs = self.abs(path);
        let loc = ObjectPath::from(abs.clone());
        match self.store.head(&loc).await {
            Ok(meta) => {
                let mode = if abs.ends_with('/') && meta.size == 0 {
                    RemoteMode::Dir
                } else {
                    RemoteMode::File
                };
                Ok(RemoteMetadata::new(
                    mode,
                    meta.size as u64,
                    systime_from_utc(meta.last_modified),
                ))
            }
            Err(ObjectError::NotFound { .. }) => {
                // Object doesn't exist; check whether it's a virtual
                // directory (common prefix with at least one child).
                let dir = ensure_dir_slash(&abs);
                let prefix = ObjectPath::from(dir.trim_end_matches('/'));
                let mut stream = self.store.list(Some(&prefix));
                if stream.next().await.is_some() {
                    Ok(RemoteMetadata::new(RemoteMode::Dir, 0, None))
                } else {
                    Err(RemoteError::not_found(format!("s3 stat: {abs}")))
                }
            }
            Err(e) => Err(map_os_err(e)),
        }
    }

    async fn write(&self, path: &str, bytes: Vec<u8>) -> RemoteResult<()> {
        let abs = self.abs(path);
        let loc = ObjectPath::from(abs);
        self.store
            .put(&loc, PutPayload::from_bytes(Bytes::from(bytes)))
            .await
            .map(|_| ())
            .map_err(map_os_err)
    }

    async fn create_dir(&self, path: &str) -> RemoteResult<()> {
        // S3 has no real directories — synthesise one via a zero-byte
        // marker with a trailing slash.
        let abs = self.abs(path);
        let marker = ensure_dir_slash(&abs);
        let loc = ObjectPath::from(marker);
        self.store
            .put(&loc, PutPayload::from_bytes(Bytes::new()))
            .await
            .map(|_| ())
            .map_err(map_os_err)
    }

    async fn rename(&self, from: &str, to: &str) -> RemoteResult<()> {
        let from = ObjectPath::from(self.abs(from));
        let to = ObjectPath::from(self.abs(to));
        self.store.rename(&from, &to).await.map_err(map_os_err)
    }

    async fn delete(&self, path: &str) -> RemoteResult<()> {
        let loc = ObjectPath::from(self.abs(path));
        self.store.delete(&loc).await.map_err(map_os_err)
    }
}
