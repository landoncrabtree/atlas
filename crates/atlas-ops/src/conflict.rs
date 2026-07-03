//! Conflict resolution policies and helpers.
//!
//! # Backend awareness
//!
//! [`resolve_conflict`] (sync) handles the local-destination case: it
//! probes `dest.exists()` and generates rename candidates via the
//! local filesystem. Cross-backend flows in [`crate::execute`] can
//! therefore not reuse it verbatim — they need to stat a remote path
//! and generate a rename candidate without touching the local
//! filesystem. See the `resolve_cross_backend_conflict` helper in
//! [`crate::execute`] for the async, backend-aware variant.
//!
//! # Prompt policy safety on async threads
//!
//! [`resolve_conflict`] performs a blocking [`crossbeam_channel::Receiver::recv`]
//! when `policy == ConflictPolicy::Prompt`. Callers on a tokio runtime
//! must therefore route through [`resolve_conflict_async`] which
//! offloads the recv to `spawn_blocking` — otherwise the runtime
//! worker would stall until the UI answered.

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use atlas_core::AtlasError;

use crate::op::{OpEvent, OpId};

/// Conflict behavior for destination path collisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Skip the conflicting item.
    Skip,
    /// Replace the destination.
    Overwrite,
    /// Choose a unique suffixed destination name.
    RenameWithSuffix,
    /// Ask the consumer to resolve the conflict.
    Prompt,
}

/// Resolution chosen for a single destination conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictDecision {
    /// Skip the item.
    Skip,
    /// Replace the destination.
    Overwrite,
    /// Copy or move to a different destination path.
    RenameTo(PathBuf),
    /// Cancel the whole operation.
    Cancel,
}

/// One-shot responder used to unblock a prompt-based conflict.
#[derive(Debug, Clone)]
pub struct ConflictResponder {
    tx: crossbeam_channel::Sender<ConflictDecision>,
}

impl ConflictResponder {
    pub(crate) fn pair() -> (Self, crossbeam_channel::Receiver<ConflictDecision>) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        (Self { tx }, rx)
    }

    /// Sends the chosen resolution back to the blocked worker.
    pub fn resolve(self, decision: ConflictDecision) {
        let _ = self.tx.send(decision);
    }
}

/// Finds the next available name using the pattern `name (copy).ext`,
/// `name (copy 2).ext`, and so on.
#[must_use]
pub fn rename_with_suffix(dest_dir: &Path, base_name: &str) -> PathBuf {
    let candidate_path = Path::new(base_name);
    let stem = candidate_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(base_name);
    let extension = candidate_path.extension().and_then(|value| value.to_str());

    let mut index = 1_u32;
    loop {
        let suffix = if index == 1 {
            format!("{stem} (copy)")
        } else {
            format!("{stem} (copy {index})")
        };
        let file_name = match extension {
            Some(ext) if !ext.is_empty() => format!("{suffix}.{ext}"),
            _ => suffix,
        };
        let candidate = dest_dir.join(file_name);
        if !candidate.exists() {
            return candidate;
        }
        index += 1;
    }
}

/// Resolves a single source/destination conflict according to the configured policy.
pub(crate) fn resolve_conflict(
    id: OpId,
    source: &Path,
    dest: &Path,
    policy: ConflictPolicy,
    event_tx: &crossbeam_channel::Sender<OpEvent>,
) -> atlas_core::Result<ConflictDecision> {
    match policy {
        ConflictPolicy::Skip => Ok(ConflictDecision::Skip),
        ConflictPolicy::Overwrite => Ok(ConflictDecision::Overwrite),
        ConflictPolicy::RenameWithSuffix => {
            let parent = dest.parent().ok_or_else(|| {
                atlas_core::AtlasError::InvalidPath(format!(
                    "destination has no parent: {}",
                    dest.display()
                ))
            })?;
            let file_name = dest
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| {
                    atlas_core::AtlasError::InvalidPath(format!(
                        "destination has invalid file name: {}",
                        dest.display()
                    ))
                })?;
            Ok(ConflictDecision::RenameTo(rename_with_suffix(
                parent, file_name,
            )))
        }
        ConflictPolicy::Prompt => {
            let (resolver, rx) = ConflictResponder::pair();
            event_tx
                .send(OpEvent::Conflict {
                    id,
                    source: source.to_path_buf(),
                    dest: dest.to_path_buf(),
                    resolver,
                })
                .map_err(|error| atlas_core::AtlasError::Other(anyhow!(error.to_string())))?;
            rx.recv()
                .map_err(|error| atlas_core::AtlasError::Other(anyhow!(error.to_string())))
        }
    }
}

/// Async wrapper around [`resolve_conflict`] safe to call from a tokio
/// runtime thread.
///
/// For non-prompt policies this is a straight passthrough. For the
/// `Prompt` case the blocking `recv()` runs inside
/// [`tokio::task::spawn_blocking`] so the runtime worker isn't stalled
/// waiting for the UI to answer.
pub(crate) async fn resolve_conflict_async(
    id: OpId,
    source: PathBuf,
    dest: PathBuf,
    policy: ConflictPolicy,
    event_tx: crossbeam_channel::Sender<OpEvent>,
) -> atlas_core::Result<ConflictDecision> {
    if policy != ConflictPolicy::Prompt {
        return resolve_conflict(id, &source, &dest, policy, &event_tx);
    }
    tokio::task::spawn_blocking(move || resolve_conflict(id, &source, &dest, policy, &event_tx))
        .await
        .map_err(|err| AtlasError::Other(anyhow!(err)))?
}

/// Build a Prompt-style responder pair without owning a policy value.
///
/// Cross-backend flows that manage their own existence-probe already
/// know the destination is occupied; they only need the recv side of
/// the responder to await a user decision. Using this helper keeps
/// `Cancelled → send OpEvent::Conflict → recv` symmetric with the
/// sync path in [`resolve_conflict`], and lets the caller await the
/// answer on [`tokio::task::spawn_blocking`].
pub(crate) fn emit_prompt(
    id: OpId,
    source: PathBuf,
    dest: PathBuf,
    event_tx: &crossbeam_channel::Sender<OpEvent>,
) -> atlas_core::Result<crossbeam_channel::Receiver<ConflictDecision>> {
    let (resolver, rx) = ConflictResponder::pair();
    event_tx
        .send(OpEvent::Conflict {
            id,
            source,
            dest,
            resolver,
        })
        .map_err(|error| AtlasError::Other(anyhow!(error.to_string())))?;
    Ok(rx)
}
