//! WebDAV backend built on `reqwest` + `quick-xml`.
//!
//! WebDAV is a small extension of HTTP so a roll-your-own client is
//! surprisingly compact — we send:
//!
//! * `PROPFIND` (depth 1) → parse multi-status XML for listings and
//!   stat.
//! * `GET` → read.
//! * `PUT` → write.
//! * `MKCOL` → create directory.
//! * `MOVE` (with `Destination:` header) → rename.
//! * `DELETE` → delete.
//!
//! Authentication is HTTP Basic (`Credentials::Password`) or
//! anonymous. The scheme in the source URI selects http vs https:
//! `webdav+http://` → HTTP, `webdav+https://` / `webdavs://` → HTTPS.
//! The bare `webdav://` scheme defaults to HTTP for compatibility
//! with the mock server; production users on Nextcloud / SharePoint
//! should use `webdavs://`.

use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use atlas_core::RemoteUri;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, Method, StatusCode};

use crate::backend::{BackendError, Credentials};
use crate::error::{RemoteError, RemoteErrorKind, RemoteMetadata, RemoteMode, RemoteResult};

use super::common::{basename, ensure_dir_slash, normalized_list_path, BackendClient, RemoteEntry};

/// Handle to a WebDAV endpoint.
pub(crate) struct WebDavBackend {
    client: Client,
    /// Base URL WITHOUT trailing `/` (we append per-op).
    base_url: String,
    root: String,
    /// Auth header value, `None` for anonymous.
    auth_header: Option<String>,
}

impl WebDavBackend {
    /// Build the reqwest client and stash the base URL + auth header.
    /// No network I/O — errors here are just config validation.
    pub(crate) fn new(uri: &RemoteUri, credentials: Credentials) -> Result<Self, BackendError> {
        let host = uri
            .host
            .as_deref()
            .ok_or_else(|| BackendError::InvalidCredentials {
                backend: "webdav",
                detail: "missing host".to_owned(),
            })?;
        let scheme = if matches!(uri.scheme.as_str(), "webdavs" | "webdav+https") {
            "https"
        } else {
            "http"
        };
        let base_url = if let Some(port) = uri.port {
            format!("{scheme}://{host}:{port}")
        } else {
            format!("{scheme}://{host}")
        };

        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| BackendError::InvalidCredentials {
                backend: "webdav",
                detail: format!("build reqwest client: {e}"),
            })?;

        let auth_header = match credentials {
            Credentials::Password(pw) => {
                let user = uri.username.as_deref().unwrap_or("");
                let creds = format!("{user}:{pw}");
                Some(format!("Basic {}", B64.encode(creds.as_bytes())))
            }
            Credentials::Anonymous => None,
            Credentials::SshKey(_, _) | Credentials::Iam { .. } => {
                return Err(BackendError::InvalidCredentials {
                    backend: "webdav",
                    detail: "only Password or Anonymous credentials are valid for WebDAV"
                        .to_owned(),
                });
            }
        };

        let root = normalized_list_path(&uri.path);

        Ok(Self {
            client,
            base_url,
            root,
            auth_header,
        })
    }

    /// Build an absolute HTTP URL for the given root-relative path.
    fn url(&self, path: &str) -> String {
        let clean_root = self.root.trim_end_matches('/');
        let clean_path = path.trim_start_matches('/');
        if clean_root.is_empty() {
            format!("{}/{clean_path}", self.base_url)
        } else if clean_path.is_empty() {
            format!("{}/{clean_root}", self.base_url)
        } else {
            format!("{}/{clean_root}/{clean_path}", self.base_url)
        }
    }

    fn auth_headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(v) = &self.auth_header {
            if let Ok(hv) = HeaderValue::from_str(v) {
                h.insert(AUTHORIZATION, hv);
            }
        }
        h
    }

    async fn request(
        &self,
        method: Method,
        url: &str,
        body: Option<Vec<u8>>,
        extra: Option<(&str, &str)>,
    ) -> RemoteResult<reqwest::Response> {
        let mut req = self
            .client
            .request(method, url)
            .headers(self.auth_headers());
        if let Some((k, v)) = extra {
            req = req.header(k, v);
        }
        if let Some(b) = body {
            req = req.body(b);
        }
        req.send()
            .await
            .map_err(|e| RemoteError::unexpected(format!("webdav http: {e}")))
    }
}

fn map_status(status: StatusCode, path: &str) -> Option<RemoteError> {
    if status.is_success() {
        return None;
    }
    let kind = match status {
        StatusCode::NOT_FOUND => RemoteErrorKind::NotFound,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => RemoteErrorKind::PermissionDenied,
        StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED => {
            RemoteErrorKind::Unsupported
        }
        StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED => RemoteErrorKind::AlreadyExists,
        _ => RemoteErrorKind::Unexpected,
    };
    Some(RemoteError::new(
        kind,
        format!(
            "webdav {} on {}: {}",
            status.as_u16(),
            path,
            status.canonical_reason().unwrap_or("")
        ),
    ))
}

#[async_trait]
impl BackendClient for WebDavBackend {
    async fn list(&self, path: &str) -> RemoteResult<Vec<RemoteEntry>> {
        let url = self.url(path);
        let url = ensure_dir_slash(&url);
        let body = br#"<?xml version="1.0" encoding="utf-8"?><D:propfind xmlns:D="DAV:"><D:allprop/></D:propfind>"#.to_vec();
        let resp = self
            .request(
                Method::from_bytes(b"PROPFIND").expect("PROPFIND method"),
                &url,
                Some(body),
                Some(("Depth", "1")),
            )
            .await?;
        let status = resp.status();
        if !status.is_success() {
            if let Some(err) = map_status(status, &url) {
                return Err(err);
            }
        }
        let body = resp
            .text()
            .await
            .map_err(|e| RemoteError::unexpected(format!("webdav list body: {e}")))?;
        let mut entries = parse_multistatus(&body);

        // Drop the entry that corresponds to the listing root itself
        // (WebDAV always includes it in a Depth:1 PROPFIND).
        let root_href = url_path_component(&url);
        entries.retain(|e| {
            e.path != root_href && e.path.trim_end_matches('/') != root_href.trim_end_matches('/')
        });

        // Rebase every href onto our root-relative form so the outer
        // vm layer can compute names uniformly.
        let root_rel = self.root.trim_end_matches('/');
        for e in &mut entries {
            let stripped = strip_prefix_ci(&e.path, &root_href);
            let stripped = stripped.trim_start_matches('/');
            e.path = if root_rel.is_empty() {
                stripped.to_owned()
            } else {
                format!("{root_rel}/{stripped}")
            };
        }
        Ok(entries)
    }

    async fn read(&self, path: &str) -> RemoteResult<Vec<u8>> {
        let url = self.url(path);
        let resp = self.request(Method::GET, &url, None, None).await?;
        let status = resp.status();
        if let Some(err) = map_status(status, &url) {
            return Err(err);
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| RemoteError::unexpected(format!("webdav body: {e}")))?;
        Ok(bytes.to_vec())
    }

    async fn stat(&self, path: &str) -> RemoteResult<RemoteMetadata> {
        let url = self.url(path);
        let body = br#"<?xml version="1.0" encoding="utf-8"?><D:propfind xmlns:D="DAV:"><D:allprop/></D:propfind>"#.to_vec();
        let resp = self
            .request(
                Method::from_bytes(b"PROPFIND").expect("PROPFIND method"),
                &url,
                Some(body),
                Some(("Depth", "0")),
            )
            .await?;
        let status = resp.status();
        if let Some(err) = map_status(status, &url) {
            return Err(err);
        }
        let text = resp
            .text()
            .await
            .map_err(|e| RemoteError::unexpected(format!("webdav stat body: {e}")))?;
        let entries = parse_multistatus(&text);
        let first = entries
            .into_iter()
            .next()
            .ok_or_else(|| RemoteError::unexpected("webdav stat: empty multistatus"))?;
        Ok(RemoteMetadata::new(first.mode, first.size, first.modified))
    }

    async fn write(&self, path: &str, bytes: Vec<u8>) -> RemoteResult<()> {
        let url = self.url(path);
        let mut req = self
            .client
            .put(&url)
            .headers(self.auth_headers())
            .body(bytes);
        req = req.header(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        let resp = req
            .send()
            .await
            .map_err(|e| RemoteError::unexpected(format!("webdav put: {e}")))?;
        let status = resp.status();
        if let Some(err) = map_status(status, &url) {
            return Err(err);
        }
        Ok(())
    }

    async fn create_dir(&self, path: &str) -> RemoteResult<()> {
        let url = ensure_dir_slash(&self.url(path));
        let resp = self
            .request(
                Method::from_bytes(b"MKCOL").expect("MKCOL method"),
                &url,
                None,
                None,
            )
            .await?;
        let status = resp.status();
        if let Some(err) = map_status(status, &url) {
            return Err(err);
        }
        Ok(())
    }

    async fn rename(&self, from: &str, to: &str) -> RemoteResult<()> {
        let from_url = self.url(from);
        let to_url = self.url(to);
        let resp = self
            .request(
                Method::from_bytes(b"MOVE").expect("MOVE method"),
                &from_url,
                None,
                Some(("Destination", to_url.as_str())),
            )
            .await?;
        let status = resp.status();
        if let Some(err) = map_status(status, &from_url) {
            return Err(err);
        }
        Ok(())
    }

    async fn delete(&self, path: &str) -> RemoteResult<()> {
        let url = self.url(path);
        let resp = self.request(Method::DELETE, &url, None, None).await?;
        let status = resp.status();
        if let Some(err) = map_status(status, &url) {
            return Err(err);
        }
        Ok(())
    }
}

/// Extract the path portion of a URL, keeping the leading slash.
fn url_path_component(url: &str) -> String {
    // Strip scheme://host[:port] prefix.
    let after_scheme = url.split_once("://").map(|x| x.1).unwrap_or(url);
    let rest = after_scheme.split_once('/').map(|x| x.1).unwrap_or("");
    format!("/{rest}")
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> &'a str {
    if s.to_ascii_lowercase()
        .starts_with(&prefix.to_ascii_lowercase())
    {
        &s[prefix.len()..]
    } else {
        s
    }
}

/// Parse a WebDAV multistatus XML response into `RemoteEntry` rows.
///
/// We look at `d:response > d:href` and the inner
/// `d:propstat > d:prop` for `d:getcontentlength`, `d:getlastmodified`,
/// and `d:resourcetype > d:collection`.
fn parse_multistatus(xml: &str) -> Vec<RemoteEntry> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut out = Vec::new();

    let mut in_response = false;
    let mut in_prop = false;
    let mut in_resource_type = false;

    let mut current_href: Option<String> = None;
    let mut current_size: u64 = 0;
    let mut current_mode: RemoteMode = RemoteMode::File;
    let mut current_modified: Option<std::time::SystemTime> = None;
    let mut capture: Option<&'static str> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e))
                if is_local(e.name().as_ref(), b"response") =>
            {
                in_response = true;
                current_href = None;
                current_size = 0;
                current_mode = RemoteMode::File;
                current_modified = None;
            }
            Ok(Event::End(e)) if is_local(e.name().as_ref(), b"response") && in_response => {
                if let Some(href) = current_href.take() {
                    let decoded = urlencoding::decode(&href)
                        .map(std::borrow::Cow::into_owned)
                        .unwrap_or(href);
                    out.push(RemoteEntry {
                        path: decoded,
                        mode: current_mode,
                        size: if matches!(current_mode, RemoteMode::Dir) {
                            0
                        } else {
                            current_size
                        },
                        modified: current_modified,
                        symlink_target: None,
                    });
                }
                in_response = false;
            }
            Ok(Event::Start(ref e)) if in_response && is_local(e.name().as_ref(), b"prop") => {
                in_prop = true;
            }
            Ok(Event::End(ref e)) if in_response && is_local(e.name().as_ref(), b"prop") => {
                in_prop = false;
            }
            Ok(Event::Start(ref e)) if in_prop && is_local(e.name().as_ref(), b"resourcetype") => {
                in_resource_type = true;
            }
            Ok(Event::End(ref e)) if in_prop && is_local(e.name().as_ref(), b"resourcetype") => {
                in_resource_type = false;
            }
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e))
                if in_resource_type && is_local(e.name().as_ref(), b"collection") =>
            {
                current_mode = RemoteMode::Dir;
            }
            Ok(Event::Start(ref e)) if in_response => {
                let name = e.name();
                let local = local_name(name.as_ref());
                capture = match local {
                    b"href" => Some("href"),
                    b"getcontentlength" => Some("size"),
                    b"getlastmodified" => Some("mtime"),
                    _ => None,
                };
            }
            Ok(Event::Text(t)) if in_response && capture.is_some() => {
                let raw = t.unescape().unwrap_or_default().into_owned();
                match capture {
                    Some("href") => current_href = Some(raw),
                    Some("size") => current_size = raw.trim().parse().unwrap_or(0),
                    Some("mtime") => {
                        // RFC1123: "Wed, 21 Oct 2015 07:28:00 GMT"
                        if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(raw.trim()) {
                            let secs = dt.timestamp();
                            if secs >= 0 {
                                current_modified =
                                    Some(UNIX_EPOCH + Duration::from_secs(secs as u64));
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(_)) => {
                capture = None;
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// True if the QName's local part equals `expected` (ignore namespace).
fn is_local(qname: &[u8], expected: &[u8]) -> bool {
    local_name(qname).eq_ignore_ascii_case(expected)
}

fn local_name(qname: &[u8]) -> &[u8] {
    match qname.iter().rposition(|&b| b == b':') {
        Some(idx) => &qname[idx + 1..],
        None => qname,
    }
}

// Silence unused imports pending future work (thumbnails, atlas-ops).
#[allow(dead_code)]
const _: fn(&str) -> String = basename;
