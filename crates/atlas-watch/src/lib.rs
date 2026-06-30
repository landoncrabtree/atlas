//! `atlas-watch` — cross-platform debounced directory watcher.
//!
//! # Overview
//!
//! This crate provides [`DirectoryWatcher`], a persistent watcher that can monitor
//! one or more directory trees simultaneously. Each watched root is identified by a
//! [`RootId`] so that a single consumer can route incoming [`FileEvent`]s to the
//! appropriate handler without keeping its own path-to-owner map.
//!
//! ## Quick start
//!
//! ```no_run
//! use atlas_watch::{WatcherBuilder, FileEventKind};
//! use std::{path::PathBuf, time::Duration};
//!
//! let (watcher, rx) = WatcherBuilder::new()
//!     .debounce(Duration::from_millis(200))
//!     .build()
//!     .expect("failed to create watcher");
//!
//! let root_id = watcher
//!     .add_root(PathBuf::from("/home/user/projects"))
//!     .expect("failed to add root");
//!
//! for event in rx {
//!     if event.root == root_id {
//!         match event.kind {
//!             FileEventKind::Created => { /* handle create */ }
//!             FileEventKind::Removed => { /* handle remove */ }
//!             _ => {}
//!         }
//!     }
//! }
//! ```

mod event;
mod ids;
mod watcher;

pub use event::{FileEvent, FileEventKind};
pub use ids::RootId;
pub use watcher::{DirectoryWatcher, WatcherBuilder};
