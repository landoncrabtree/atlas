//! Root-directory identifier type.

/// Opaque identifier for a watched root directory.
///
/// Each call to [`DirectoryWatcher::add_root`] returns a unique `RootId` that is
/// embedded in every [`FileEvent`](crate::FileEvent) originating from that root.
/// Consumers can compare `RootId`s to route events without maintaining their own
/// path-to-owner maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RootId(pub u64);
