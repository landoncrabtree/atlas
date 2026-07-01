//! `atlas-indexd` library crate — exposes internal modules for integration
//! testing and potential embedding.
//!
//! External callers (tests, future clients) interact with the daemon through
//! the `atlas-ipc` socket protocol. This library target makes the daemon's
//! internal API accessible so integration tests can start the daemon in-process
//! without spawning a subprocess.

pub mod cli;
pub mod daemon;
pub mod handler;
pub mod incremental;
pub mod ingest;
pub mod launchd;
pub mod paths;
pub mod state;
