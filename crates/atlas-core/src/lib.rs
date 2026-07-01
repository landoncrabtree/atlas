//! Atlas core — shared types, traits, and events.

pub mod error;
pub mod location;
pub mod path;

pub use error::{AtlasError, Result};
pub use location::{BackendKind, Location, LocationParseError, RemoteUri};
