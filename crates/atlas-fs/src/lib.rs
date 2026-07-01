//! Atlas filesystem layer — async streaming walker, metadata, and sort/filter
//! primitives.
//!
//! This crate provides the non-blocking, channel-based I/O foundation that
//! Atlas views consume. Directory enumeration never blocks the UI: results are
//! streamed to consumers over [`crossbeam_channel`] receivers rather than
//! awaited as a whole.
//!
//! # Building blocks
//!
//! - [`list_directory`] — stream the contents of a single directory.
//! - [`walk`] — recursively walk one or more roots in parallel (via `ignore`).
//! - [`SortSpec`] / [`compare`] / [`sort_in_place`] — ordering, with natural
//!   numeric name sort.
//! - [`Filter`] / [`CompiledFilter`] — substring, glob, and regex filtering.
//! - [`LocationViewModel`] / [`InMemoryLocationViewModel`] — an observable,
//!   sorted-and-filtered snapshot for UI binding.

mod entry;
mod filter;
mod lister;
mod sort;
mod view_model;
mod walker;
pub(crate) mod watched;

pub use entry::{Entry, EntryKind, Metadata};
pub use filter::{CompiledFilter, Filter};
pub use lister::{list_directory, ListEvent, ListRequest};
pub use sort::{compare, sort_in_place, SortKey, SortOrder, SortSpec};
pub use view_model::{InMemoryLocationViewModel, LocationViewModel, OpenOptions, ViewModelEvent};
pub use walker::{walk, WalkRequest};
