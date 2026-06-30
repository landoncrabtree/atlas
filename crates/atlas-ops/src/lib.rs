//! Atlas file operations queue — copy, move, delete, rename, mkdir with
//! progress reporting, cancellation, conflict resolution, and undo.
//!
//! # Quick start
//! ```no_run
//! use atlas_ops::{ConflictPolicy, OpKind, OperationQueue, QueueOptions};
//!
//! let (queue, _events) = OperationQueue::start(QueueOptions::default());
//! let _id = queue.submit(OpKind::Mkdir {
//!     path: "/example/mydir".into(),
//!     parents: true,
//! });
//! let _ = ConflictPolicy::Skip;
//! ```

pub mod conflict;
pub mod op;
pub(crate) mod primitives;
pub mod queue;
pub mod undo;

pub use conflict::{ConflictDecision, ConflictPolicy, ConflictResponder};
pub use op::{OpEvent, OpId, OpKind, OpKindDescriptor, OpStatus, Operation, ProgressSnapshot};
pub use queue::{OperationQueue, QueueOptions};
pub use undo::{UndoEntry, UndoStack, UndoToken};

#[doc(hidden)]
pub use primitives::move_::move_via_copy_delete_for_tests;
