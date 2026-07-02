//! Remote-backend UI bridges.
//!
//! Housing for controllers that mediate between the Slint UI and
//! [`atlas_remote`] backends. Today this is just the Connect-to-Server
//! modal ([`connect::ConnectController`]); the connection-pool /
//! server-catalogue viewer will land here as separate modules.

pub mod connect;
pub mod preview;

pub use connect::ConnectController;
pub use preview::{OpenHandler, PreviewCache, PreviewError, PreviewOutcome, RealOpener};
