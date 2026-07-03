//! Cross-backend and remote-native op primitives used by
//! [`crate::execute::execute_op`].
//!
//! This module is the single seam where atlas-ops talks to
//! `atlas-remote`. Every helper here opens a
//! [`RemoteLocationViewModel`] via [`RemoteLocationViewModel::open_live`]
//! per invocation — a future phase will layer a connection pool on
//! top so we don't reconnect per op.
//!
//! # Routing rules
//!
//! * *Local ↔ local* — handled by the sync primitives in
//!   [`crate::primitives`], never reaches this module.
//! * *Same-backend remote* — uses the backend's native op via
//!   `RemoteLocationViewModel::{rename, delete, write, create_dir}`
//!   for the common cases and falls back to
//!   [`atlas_remote::stream_copy`] for content copy.
//! * *Cross-backend* — always goes through
//!   [`atlas_remote::stream_copy`]: reader from source, writer on
//!   destination. Directory sources are enumerated with
//!   [`atlas_remote::enumerate_recursive`].
//!
//! # Progress fidelity
//!
//! Every long-running remote transfer wires
//! [`atlas_remote::stream::StreamProgress`] into the queue's shared
//! progress channel. A dedicated bridge thread converts backend byte
//! counters into [`crate::op::OpEvent::Progress`] events so the ops
//! panel updates the progress bar without polling.
//!
//! # Cancellation
//!
//! Every helper polls the [`AtomicU8`] flag word between logical
//! sub-steps (per-file, per-listing round-trip). Cross-backend
//! `stream_copy` doesn't accept a cancellation token yet — the
//! current implementation checks the flag before each new source
//! file. Long single-file transfers rely on the outer op cancel to
//! surface after the current in-flight file finishes. A follow-up
//! phase will thread cancellation into stream_copy directly.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use atlas_core::{AtlasError, Location};
use atlas_fs::OpenOptions;
use atlas_remote::{
    enumerate_recursive, stream_copy, BackendError, Credentials, RemoteError, RemoteErrorKind,
    RemoteLocationViewModel, RemoteMode, StreamProgress, WalkEntry,
};
use crossbeam_channel::Sender;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use std::collections::HashMap;

use crate::op::{OpEvent, OpId, Operation, ProgressSnapshot, FLAG_CANCEL, FLAG_PAUSE};

/// Process-wide credential cache keyed by (scheme, user, host, port).
/// Populated by atlas-ui when the user successfully connects to a
/// remote server; consumed by atlas-ops when it needs to re-open the
/// same server for a copy / move / delete operation.
///
/// Bypasses the OS keychain for the current session so cross-backend
/// paste and drag-drop work without repeated macOS access dialogs. The
/// cache is intentionally in-memory only — persistence stays in
/// `~/.config/atlas/servers.toml` + the keychain entry, populated
/// separately by the connect controller.
static SESSION_CREDENTIALS: Lazy<RwLock<HashMap<String, Credentials>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// Compute the cache key for a remote URI. Same host+user+port maps
/// to the same credentials regardless of path.
///
/// The port is normalised via the backend's default when absent — so
/// `sftp://user@host` and `sftp://user@host:22` produce the same key,
/// even if the caller synthesised the URI outside the connect
/// controller (which is where `RemoteUri::with_default_port` is
/// normally applied). This is a belt-and-suspenders defence against a
/// future refactor bypassing that call site: without it, a URI that
/// leaks in with `port: None` would silently miss the credentials
/// cache and force the OS keychain prompt every time.
fn cred_key(uri: &atlas_core::RemoteUri) -> String {
    let effective_port = uri.port.or_else(|| {
        atlas_core::BackendKind::from_scheme(&uri.scheme).and_then(|k| k.default_port())
    });
    format!(
        "{}://{}@{}:{}",
        uri.scheme,
        uri.username.clone().unwrap_or_default(),
        uri.host.clone().unwrap_or_default(),
        effective_port.map(|p| p.to_string()).unwrap_or_default(),
    )
}

/// One-time snapshot of counted items + bytes for a subtree.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RemoteCounts {
    pub(crate) items: u64,
    pub(crate) bytes: u64,
}

/// Handle to an open remote view model.
pub(crate) struct RemoteHandle {
    pub(crate) vm: Arc<RemoteLocationViewModel>,
    /// URI path portion (starts with `/`).
    pub(crate) root: String,
    /// URI display (scheme + host + path) — used in errors.
    pub(crate) display: String,
}

/// Record credentials for a remote host so subsequent atlas-ops can
/// reconnect without touching the OS keychain. Called from the connect
/// controller once the initial handshake succeeds.
pub fn cache_session_credentials(uri: &atlas_core::RemoteUri, credentials: Credentials) {
    let key = cred_key(uri);
    SESSION_CREDENTIALS.write().insert(key, credentials);
}

/// Drop the cached credentials for a remote host (e.g. after the user
/// disconnects). Silently no-ops if no entry exists.
pub fn clear_session_credentials(uri: &atlas_core::RemoteUri) {
    let key = cred_key(uri);
    SESSION_CREDENTIALS.write().remove(&key);
}

/// Resolve the effective [`Credentials`] for `uri` without prompting the
/// user. Consults the session cache first (populated on successful
/// connect); falls back to the OS keychain via the persisted
/// `credential_ref`; and finally to [`Credentials::Anonymous`].
///
/// This is the shared entry point used by every `atlas-ops` re-open
/// call and, out-of-crate, by `atlas-ui` for silent remote-pane
/// navigation and the preview cache — both of which need the same
/// credentials the user already authorised at connect time.
///
/// # Errors
///
/// Only returns [`AtlasError`] if the session cache lookup itself
/// fails (never, in practice). Keychain lookup failures are logged and
/// degraded to [`Credentials::Anonymous`] so the caller can decide
/// whether an anonymous retry makes sense.
pub fn credentials_for(uri: &atlas_core::RemoteUri) -> Result<Credentials, AtlasError> {
    // 1) Session-scoped in-memory cache — the fast path taken during
    // interactive Cmd+C / Cmd+V without keychain prompts.
    if let Some(cred) = SESSION_CREDENTIALS.read().get(&cred_key(uri)).cloned() {
        return Ok(cred);
    }

    // 2) Persistent keychain lookup via saved credential_ref.
    if let Some(cref) = &uri.credential_ref {
        match atlas_remote::retrieve_secret(cref) {
            Ok(secret) => Ok(Credentials::Password(secret)),
            Err(err) => {
                tracing::warn!(
                    credential = %cref,
                    error = %err,
                    "credential lookup failed; falling back to anonymous"
                );
                Ok(Credentials::Anonymous)
            }
        }
    } else {
        Ok(Credentials::Anonymous)
    }
}

fn map_backend_error(display: &str, err: BackendError) -> AtlasError {
    match err {
        BackendError::InvalidCredentials { detail, .. } => {
            AtlasError::auth_required(display.to_owned(), detail)
        }
        other => AtlasError::Other(anyhow::anyhow!(other)),
    }
}

/// Convert a [`RemoteError`] into an [`AtlasError`], flagging
/// permission-denied variants as [`AtlasError::AuthRequired`] so the
/// ops panel can offer a "Reconnect" chip.
pub(crate) fn translate_remote_error(display: &str, err: RemoteError) -> AtlasError {
    map_remote_error(display, err)
}

fn map_remote_error(display: &str, err: RemoteError) -> AtlasError {
    if matches!(err.kind(), RemoteErrorKind::PermissionDenied) {
        return AtlasError::auth_required(display.to_owned(), err.to_string());
    }
    AtlasError::Other(anyhow::anyhow!(err))
}

/// Open a live [`RemoteLocationViewModel`] for the given remote
/// location. Credentials come from
/// [`atlas_remote::retrieve_secret`] if a `credential_ref` is set,
/// otherwise anonymous.
pub(crate) async fn open_remote(location: &Location) -> Result<RemoteHandle, AtlasError> {
    let (uri, kind) = match location {
        Location::Remote(uri, kind) => (uri.clone(), *kind),
        Location::Local(_) => {
            return Err(AtlasError::InvalidPath(format!(
                "open_remote called with local path {}",
                location.display_path()
            )));
        }
    };
    let display = location.display_path();
    let credentials = credentials_for(&uri)?;
    // The `handle.root` string is the caller-supplied path we will
    // pass to backend methods (`reader`, `writer`, `stat`, ...). The
    // VM itself is opened rooted at `/` so `abs()` inside the backend
    // is a simple identity join; without this, ops that open a VM at
    // a nested URI path double-prepend that path onto every call.
    let root = uri.path.clone();
    let mut vm_uri = uri;
    vm_uri.path = "/".into();
    let vm = RemoteLocationViewModel::open_live(vm_uri, kind, credentials, OpenOptions::default())
        .map_err(|err| map_backend_error(&display, err))?;
    Ok(RemoteHandle { vm, root, display })
}

/// Check the cancel / pause flag and yield `Cancelled` if requested.
pub(crate) async fn check_flags_async(flags: &AtomicU8) -> Result<(), AtlasError> {
    loop {
        let current = flags.load(Ordering::Relaxed);
        if current & FLAG_CANCEL != 0 {
            return Err(AtlasError::Cancelled);
        }
        if current & FLAG_PAUSE != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            continue;
        }
        return Ok(());
    }
}

/// Recursively count items and byte totals underneath a remote path.
/// For a file this is a single stat call. For a directory this walks
/// the entire tree — expensive on large remote stores, but matches
/// how the local ops report totals up-front.
pub(crate) async fn count_remote(handle: &RemoteHandle) -> Result<RemoteCounts, AtlasError> {
    let stat = handle
        .vm
        .stat(&handle.root)
        .await
        .map_err(|err| map_remote_error(&handle.display, err))?;
    match stat.mode() {
        RemoteMode::File => Ok(RemoteCounts {
            items: 1,
            bytes: stat.content_length(),
        }),
        RemoteMode::Dir => {
            let entries = enumerate_recursive(handle.vm.client(), &handle.root)
                .await
                .map_err(|err| map_remote_error(&handle.display, err))?;
            let mut counts = RemoteCounts { items: 1, bytes: 0 };
            for entry in &entries {
                counts.items = counts.items.saturating_add(1);
                if matches!(entry.kind, RemoteMode::File) {
                    counts.bytes = counts.bytes.saturating_add(entry.size);
                }
            }
            Ok(counts)
        }
        RemoteMode::Other => Ok(RemoteCounts { items: 1, bytes: 0 }),
    }
}

/// Cheap size lookup for a single local path — mirrors what
/// [`crate::primitives::copy::count_path`] does but returns just a
/// bytes value. Used when the source is local and we need to seed
/// progress totals before diving into `stream_copy`.
pub(crate) fn count_local(path: &std::path::Path) -> Result<RemoteCounts, AtlasError> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|source| AtlasError::io(Some(path.to_path_buf()), source))?;
    if meta.is_dir() {
        let mut counts = RemoteCounts { items: 0, bytes: 0 };
        for entry in walkdir::WalkDir::new(path).follow_links(false) {
            let entry = entry.map_err(|err| {
                AtlasError::io(
                    Some(path.to_path_buf()),
                    std::io::Error::other(err.to_string()),
                )
            })?;
            let entry_meta = entry.metadata().map_err(|source| {
                AtlasError::io(Some(entry.path().to_path_buf()), source.into())
            })?;
            counts.items = counts.items.saturating_add(1);
            if entry_meta.is_file() {
                counts.bytes = counts.bytes.saturating_add(entry_meta.len());
            }
        }
        Ok(counts)
    } else if meta.is_file() {
        Ok(RemoteCounts {
            items: 1,
            bytes: meta.len(),
        })
    } else {
        Ok(RemoteCounts { items: 1, bytes: 0 })
    }
}

/// Copy a single file from a remote source into a remote destination.
/// Uses `stream_copy` under the hood so it works whether the two
/// endpoints share a backend or not.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn copy_remote_file_to_remote(
    id: OpId,
    src: &RemoteHandle,
    src_path: &str,
    dst: &RemoteHandle,
    dst_path: &str,
    total_bytes: Option<u64>,
    event_tx: &Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    flags: &AtomicU8,
) -> Result<u64, AtlasError> {
    check_flags_async(flags).await?;
    let mut reader = src
        .vm
        .reader(src_path, total_bytes)
        .await
        .map_err(|err| map_remote_error(&src.display, err))?;
    let mut writer = dst
        .vm
        .writer(dst_path)
        .await
        .map_err(|err| map_remote_error(&dst.display, err))?;
    stream_copy_with_progress(
        id,
        &mut reader,
        &mut writer,
        total_bytes,
        event_tx,
        op_arc,
        src_path,
    )
    .await
}

/// Copy a local file to a remote destination.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn copy_local_file_to_remote(
    id: OpId,
    src_path: &std::path::Path,
    dst: &RemoteHandle,
    dst_path: &str,
    event_tx: &Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    flags: &AtomicU8,
) -> Result<u64, AtlasError> {
    check_flags_async(flags).await?;
    let src_path_owned: PathBuf = src_path.to_path_buf();
    let (file, total_bytes) = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
        let file = std::fs::File::open(&src_path_owned)?;
        let len = file.metadata()?.len();
        Ok((file, len))
    })
    .await
    .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))?
    .map_err(|source| AtlasError::io(Some(src_path.to_path_buf()), source))?;
    let mut reader = futures::io::AllowStdIo::new(file);
    let mut writer = dst
        .vm
        .writer(dst_path)
        .await
        .map_err(|err| map_remote_error(&dst.display, err))?;
    let display = src_path.to_string_lossy().into_owned();
    stream_copy_with_progress(
        id,
        &mut reader,
        &mut writer,
        Some(total_bytes),
        event_tx,
        op_arc,
        &display,
    )
    .await
}

/// Copy a remote file to a local destination.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn copy_remote_file_to_local(
    id: OpId,
    src: &RemoteHandle,
    src_path: &str,
    dst_path: &std::path::Path,
    total_bytes: Option<u64>,
    event_tx: &Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    flags: &AtomicU8,
) -> Result<u64, AtlasError> {
    check_flags_async(flags).await?;
    // Ensure parent exists.
    if let Some(parent) = dst_path.parent() {
        let parent_owned = parent.to_path_buf();
        tokio::task::spawn_blocking(move || std::fs::create_dir_all(&parent_owned))
            .await
            .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))?
            .map_err(|source| AtlasError::io(Some(dst_path.to_path_buf()), source))?;
    }
    let mut reader = src
        .vm
        .reader(src_path, total_bytes)
        .await
        .map_err(|err| map_remote_error(&src.display, err))?;
    let dst_owned = dst_path.to_path_buf();
    let file = tokio::task::spawn_blocking(move || std::fs::File::create(&dst_owned))
        .await
        .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))?
        .map_err(|source| AtlasError::io(Some(dst_path.to_path_buf()), source))?;
    let mut writer = futures::io::AllowStdIo::new(file);
    let display = src.display.clone();
    let result = stream_copy_with_progress(
        id,
        &mut reader,
        &mut writer,
        total_bytes,
        event_tx,
        op_arc,
        &display,
    )
    .await;
    if result.is_err() {
        // Best-effort cleanup of the partial file.
        let cleanup = dst_path.to_path_buf();
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_file(cleanup)).await;
    }
    result
}

/// Bridge [`stream_copy`] into the ops event stream.
#[allow(clippy::too_many_arguments)]
async fn stream_copy_with_progress<R, W>(
    id: OpId,
    reader: &mut R,
    writer: &mut W,
    total_bytes: Option<u64>,
    event_tx: &Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    current_path_display: &str,
) -> Result<u64, AtlasError>
where
    R: futures::io::AsyncRead + Unpin,
    W: futures::io::AsyncWrite + Unpin,
{
    let (progress_tx, progress_rx) = crossbeam_channel::unbounded::<StreamProgress>();
    // Baseline byte count before this file started.
    let base_bytes_done = op_arc.lock().progress.bytes_done;
    // Publish current path immediately.
    {
        let mut op = op_arc.lock();
        op.progress.current_path = Some(PathBuf::from(current_path_display));
    }
    let event_tx_clone = event_tx.clone();
    let op_arc_clone = Arc::clone(op_arc);
    let display_owned = current_path_display.to_string();
    let bridge = tokio::task::spawn_blocking(move || {
        let mut last_seen: u64 = 0;
        while let Ok(ev) = progress_rx.recv() {
            let delta = ev.bytes_transferred.saturating_sub(last_seen);
            last_seen = ev.bytes_transferred;
            let snapshot = {
                let mut op = op_arc_clone.lock();
                op.progress.bytes_done = op.progress.bytes_done.saturating_add(delta);
                if op.progress.bytes_total < op.progress.bytes_done {
                    op.progress.bytes_total = op.progress.bytes_done;
                }
                op.progress.current_path = Some(PathBuf::from(&display_owned));
                op.progress.clone()
            };
            let _ = event_tx_clone.send(OpEvent::Progress { id, snapshot });
        }
    });
    let copy_result = stream_copy(reader, writer, None, total_bytes, Some(&progress_tx)).await;
    drop(progress_tx);
    let _ = bridge.await;
    let transferred = copy_result
        .map_err(|source| AtlasError::io(Some(PathBuf::from(current_path_display)), source))?;
    // Consolidate: after `stream_copy` the bytes counter should reflect
    // the full file. If the bridge missed the final tick because we drained
    // the sender before the last event landed, catch up now.
    {
        let mut op = op_arc.lock();
        let target = base_bytes_done.saturating_add(transferred);
        if op.progress.bytes_done < target {
            op.progress.bytes_done = target;
            if op.progress.bytes_total < op.progress.bytes_done {
                op.progress.bytes_total = op.progress.bytes_done;
            }
        }
    }
    Ok(transferred)
}

/// Recursively copy a remote source subtree into a remote destination.
///
/// `dst_path` is the destination path on `dst`'s backend — usually
/// `dst.root`, but callers can pass a renamed path if the base
/// destination collides.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn copy_remote_tree_to_remote(
    id: OpId,
    src: &RemoteHandle,
    dst: &RemoteHandle,
    dst_path: &str,
    event_tx: &Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    flags: &AtomicU8,
) -> Result<(), AtlasError> {
    let entries = enumerate_recursive(src.vm.client(), &src.root)
        .await
        .map_err(|err| map_remote_error(&src.display, err))?;
    // Create the root directory at the destination first.
    dst.vm
        .create_dir(dst_path)
        .await
        .map_err(|err| map_remote_error(&dst.display, err))?;
    increment_items(op_arc, 1);
    let src_root = trim_trailing_slash(&src.root);
    let dst_root = trim_trailing_slash(dst_path);
    copy_walk_entries(
        id, &entries, src, &src_root, dst, &dst_root, event_tx, op_arc, flags,
    )
    .await?;
    Ok(())
}

/// Recursively copy a local source subtree into a remote destination.
///
/// `dst_path` is the destination path on the remote backend —
/// callers pass a renamed path when the base destination collides.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn copy_local_tree_to_remote(
    id: OpId,
    src_root: &std::path::Path,
    dst: &RemoteHandle,
    dst_path: &str,
    event_tx: &Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    flags: &AtomicU8,
) -> Result<(), AtlasError> {
    dst.vm
        .create_dir(dst_path)
        .await
        .map_err(|err| map_remote_error(&dst.display, err))?;
    increment_items(op_arc, 1);
    let dst_root_trim = trim_trailing_slash(dst_path);
    let root_owned = src_root.to_path_buf();
    let entries = tokio::task::spawn_blocking(move || collect_local_walk(&root_owned))
        .await
        .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))??;
    for (rel, kind) in entries {
        check_flags_async(flags).await?;
        let dst_child = join_remote(&dst_root_trim, &rel);
        let src_child = src_root.join(&rel);
        match kind {
            LocalKind::Dir => {
                dst.vm
                    .create_dir(&dst_child)
                    .await
                    .map_err(|err| map_remote_error(&dst.display, err))?;
                increment_items(op_arc, 1);
            }
            LocalKind::File => {
                copy_local_file_to_remote(id, &src_child, dst, &dst_child, event_tx, op_arc, flags)
                    .await?;
                increment_items(op_arc, 1);
            }
        }
    }
    Ok(())
}

/// Recursively copy a remote source subtree into a local directory
/// at `dst_root`. Callers pass a renamed `dst_root` when the base
/// destination collides.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn copy_remote_tree_to_local(
    id: OpId,
    src: &RemoteHandle,
    dst_root: &std::path::Path,
    event_tx: &Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    flags: &AtomicU8,
) -> Result<(), AtlasError> {
    let dst_owned = dst_root.to_path_buf();
    tokio::task::spawn_blocking(move || std::fs::create_dir_all(&dst_owned))
        .await
        .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))?
        .map_err(|source| AtlasError::io(Some(dst_root.to_path_buf()), source))?;
    increment_items(op_arc, 1);
    let entries = enumerate_recursive(src.vm.client(), &src.root)
        .await
        .map_err(|err| map_remote_error(&src.display, err))?;
    let src_root_trim = trim_trailing_slash(&src.root);
    for entry in &entries {
        check_flags_async(flags).await?;
        let child_dst = dst_root.join(&entry.relative_path);
        let child_src = join_remote(&src_root_trim, &entry.relative_path);
        match entry.kind {
            RemoteMode::Dir => {
                let owned = child_dst.clone();
                tokio::task::spawn_blocking(move || std::fs::create_dir_all(&owned))
                    .await
                    .map_err(|err| AtlasError::Other(anyhow::anyhow!(err)))?
                    .map_err(|source| AtlasError::io(Some(child_dst.clone()), source))?;
                increment_items(op_arc, 1);
            }
            RemoteMode::File => {
                copy_remote_file_to_local(
                    id,
                    src,
                    &child_src,
                    &child_dst,
                    Some(entry.size),
                    event_tx,
                    op_arc,
                    flags,
                )
                .await?;
                increment_items(op_arc, 1);
            }
            RemoteMode::Other => {}
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn copy_walk_entries(
    id: OpId,
    entries: &[WalkEntry],
    src: &RemoteHandle,
    src_root_trim: &str,
    dst: &RemoteHandle,
    dst_root_trim: &str,
    event_tx: &Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
    flags: &AtomicU8,
) -> Result<(), AtlasError> {
    for entry in entries {
        check_flags_async(flags).await?;
        let child_src = join_remote(src_root_trim, &entry.relative_path);
        let child_dst = join_remote(dst_root_trim, &entry.relative_path);
        match entry.kind {
            RemoteMode::Dir => {
                dst.vm
                    .create_dir(&child_dst)
                    .await
                    .map_err(|err| map_remote_error(&dst.display, err))?;
                increment_items(op_arc, 1);
            }
            RemoteMode::File => {
                copy_remote_file_to_remote(
                    id,
                    src,
                    &child_src,
                    dst,
                    &child_dst,
                    Some(entry.size),
                    event_tx,
                    op_arc,
                    flags,
                )
                .await?;
                increment_items(op_arc, 1);
            }
            RemoteMode::Other => {}
        }
    }
    Ok(())
}

/// Delete a remote subtree. Depth-first — children first, then the
/// parent — so backends that don't allow rm-rf on non-empty
/// directories still succeed.
pub(crate) async fn delete_remote(
    handle: &RemoteHandle,
    flags: &AtomicU8,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
) -> Result<(), AtlasError> {
    check_flags_async(flags).await?;
    let stat = handle
        .vm
        .stat(&handle.root)
        .await
        .map_err(|err| map_remote_error(&handle.display, err))?;
    match stat.mode() {
        RemoteMode::File | RemoteMode::Other => {
            handle
                .vm
                .delete(&handle.root)
                .await
                .map_err(|err| map_remote_error(&handle.display, err))?;
            increment_items(op_arc, 1);
        }
        RemoteMode::Dir => {
            let entries = enumerate_recursive(handle.vm.client(), &handle.root)
                .await
                .map_err(|err| map_remote_error(&handle.display, err))?;
            let root_trim = trim_trailing_slash(&handle.root);
            // Delete files first, deepest paths first.
            let mut files: Vec<_> = entries
                .iter()
                .filter(|e| matches!(e.kind, RemoteMode::File))
                .collect();
            files.sort_by_key(|e| std::cmp::Reverse(e.relative_path.len()));
            for entry in files {
                check_flags_async(flags).await?;
                let path = join_remote(&root_trim, &entry.relative_path);
                handle
                    .vm
                    .delete(&path)
                    .await
                    .map_err(|err| map_remote_error(&handle.display, err))?;
                increment_items(op_arc, 1);
            }
            let mut dirs: Vec<_> = entries
                .iter()
                .filter(|e| matches!(e.kind, RemoteMode::Dir))
                .collect();
            dirs.sort_by_key(|e| std::cmp::Reverse(e.relative_path.len()));
            for entry in dirs {
                check_flags_async(flags).await?;
                let path = join_remote(&root_trim, &entry.relative_path);
                handle
                    .vm
                    .delete(&path)
                    .await
                    .map_err(|err| map_remote_error(&handle.display, err))?;
                increment_items(op_arc, 1);
            }
            handle
                .vm
                .delete(&handle.root)
                .await
                .map_err(|err| map_remote_error(&handle.display, err))?;
            increment_items(op_arc, 1);
        }
    }
    Ok(())
}

/// Create a directory at a remote location.
pub(crate) async fn mkdir_remote(handle: &RemoteHandle) -> Result<(), AtlasError> {
    handle
        .vm
        .create_dir(&handle.root)
        .await
        .map_err(|err| map_remote_error(&handle.display, err))
}

/// Rename a remote entry in place.
pub(crate) async fn rename_remote(
    handle: &RemoteHandle,
    new_name: &str,
) -> Result<String, AtlasError> {
    if new_name.contains('/') {
        return Err(AtlasError::InvalidPath(new_name.to_owned()));
    }
    let parent = parent_path(&handle.root);
    let new_full = join_remote(&trim_trailing_slash(&parent), new_name);
    handle
        .vm
        .rename(&handle.root, &new_full)
        .await
        .map_err(|err| map_remote_error(&handle.display, err))?;
    Ok(new_full)
}

/// Same-backend move via the backend's native rename verb.
pub(crate) async fn move_remote_same_backend(
    src: &RemoteHandle,
    dst_path: &str,
) -> Result<(), AtlasError> {
    src.vm
        .rename(&src.root, dst_path)
        .await
        .map_err(|err| map_remote_error(&src.display, err))
}

fn increment_items(op_arc: &Arc<parking_lot::Mutex<Operation>>, delta: u64) {
    let mut op = op_arc.lock();
    op.progress.items_done = op.progress.items_done.saturating_add(delta);
    if op.progress.items_total < op.progress.items_done {
        op.progress.items_total = op.progress.items_done;
    }
}

fn trim_trailing_slash(path: &str) -> String {
    if path == "/" {
        return path.to_owned();
    }
    path.trim_end_matches('/').to_owned()
}

/// Join two remote path fragments with a single `/`. Handles the
/// root case (`/` + `foo` → `/foo`).
pub(crate) fn join_remote(root_trim: &str, relative: &str) -> String {
    let relative = relative.trim_start_matches('/');
    if root_trim.is_empty() || root_trim == "/" {
        format!("/{relative}")
    } else if relative.is_empty() {
        root_trim.to_owned()
    } else {
        format!("{root_trim}/{relative}")
    }
}

fn parent_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => "/".to_owned(),
        Some(idx) => trimmed[..idx].to_owned(),
        None => "/".to_owned(),
    }
}

#[derive(Clone, Copy)]
enum LocalKind {
    Dir,
    File,
}

fn collect_local_walk(root: &std::path::Path) -> Result<Vec<(String, LocalKind)>, AtlasError> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root).min_depth(1).follow_links(false) {
        let entry = entry.map_err(|err| {
            AtlasError::io(
                Some(root.to_path_buf()),
                std::io::Error::other(err.to_string()),
            )
        })?;
        let rel = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| {
                AtlasError::InvalidPath(format!(
                    "walk emitted entry outside root: {}",
                    entry.path().display()
                ))
            })?
            .to_string_lossy()
            .into_owned();
        let file_type = entry.file_type();
        if file_type.is_dir() {
            out.push((rel, LocalKind::Dir));
        } else if file_type.is_file() {
            out.push((rel, LocalKind::File));
        }
        // Skip other kinds — symlinks etc. — a future phase can handle them.
    }
    Ok(out)
}

/// Publish an initial progress snapshot to the event stream so the
/// UI shows a "starting…" bar before the first byte flies.
pub(crate) fn emit_initial_progress(
    id: OpId,
    event_tx: &Sender<OpEvent>,
    op_arc: &Arc<parking_lot::Mutex<Operation>>,
) {
    let snapshot: ProgressSnapshot = op_arc.lock().progress.clone();
    let _ = event_tx.send(OpEvent::Progress { id, snapshot });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_remote_handles_root() {
        assert_eq!(join_remote("", "foo"), "/foo");
        assert_eq!(join_remote("/", "foo"), "/foo");
        assert_eq!(join_remote("/tmp", "a.txt"), "/tmp/a.txt");
        assert_eq!(join_remote("/tmp", "sub/a.txt"), "/tmp/sub/a.txt");
        assert_eq!(join_remote("/tmp", ""), "/tmp");
    }

    #[test]
    fn parent_path_walks_up_one_level() {
        assert_eq!(parent_path("/"), "/");
        assert_eq!(parent_path("/foo"), "/");
        assert_eq!(parent_path("/foo/bar"), "/foo");
        assert_eq!(parent_path("/foo/bar/"), "/foo");
    }

    #[test]
    fn credential_cache_round_trip() {
        let uri = atlas_core::RemoteUri {
            scheme: "sftp".into(),
            host: Some("host.session-test.example".into()),
            port: Some(2222),
            path: "/only-cache-here".into(),
            username: Some("alice".into()),
            credential_ref: None,
        };
        // Path must be irrelevant to the key.
        let uri_other_path = atlas_core::RemoteUri {
            path: "/somewhere-else".into(),
            ..uri.clone()
        };
        cache_session_credentials(&uri, Credentials::Password("hunter2".into()));

        let got = credentials_for(&uri_other_path).unwrap();
        match got {
            Credentials::Password(s) => assert_eq!(s, "hunter2"),
            other => panic!("expected cached password, got {other:?}"),
        }

        clear_session_credentials(&uri);
        // No credential_ref, cache empty — falls back to Anonymous.
        let cleared = credentials_for(&uri).unwrap();
        assert!(matches!(cleared, Credentials::Anonymous));
    }

    #[test]
    fn cred_key_normalises_missing_port_via_scheme() {
        // Regression: `port: None` and `port: Some(default_port_for_scheme)`
        // must produce the same cred_key so a URI that slipped past
        // `RemoteUri::with_default_port` still hits the session
        // credentials cache. Previously the two forms produced
        // `sftp://alice@host:` and `sftp://alice@host:22` respectively
        // and `credentials_for` missed every cache lookup.
        let without_port = atlas_core::RemoteUri {
            scheme: "sftp".into(),
            host: Some("host.cred-key-test.example".into()),
            port: None,
            username: Some("alice".into()),
            path: "/some/path".into(),
            credential_ref: None,
        };
        let with_default = atlas_core::RemoteUri {
            port: Some(22),
            ..without_port.clone()
        };
        assert_eq!(
            cred_key(&without_port),
            cred_key(&with_default),
            "cred_key must be identical whether the URI carries None or Some(default)"
        );

        // Explicit non-default port stays distinct.
        let explicit_alt = atlas_core::RemoteUri {
            port: Some(2222),
            ..without_port.clone()
        };
        assert_ne!(cred_key(&without_port), cred_key(&explicit_alt));

        // FTP + WebDAV get the same treatment via their respective defaults.
        let ftp = atlas_core::RemoteUri {
            scheme: "ftp".into(),
            host: Some("ftp.example".into()),
            port: None,
            username: Some("anon".into()),
            path: "/".into(),
            credential_ref: None,
        };
        let ftp_explicit = atlas_core::RemoteUri {
            port: Some(21),
            ..ftp.clone()
        };
        assert_eq!(cred_key(&ftp), cred_key(&ftp_explicit));
    }
}
