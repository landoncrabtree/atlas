//! Conflict resolution policies and helpers.

use std::path::{Path, PathBuf};

use anyhow::anyhow;

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
