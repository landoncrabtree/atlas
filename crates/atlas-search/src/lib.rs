//! Atlas search — unified search facade.
pub mod content;
pub mod fuzzy;
pub mod index_client;
pub mod unified;

pub use content::*;
pub use fuzzy::*;
pub use index_client::{IndexClient, IndexClientError};
pub use unified::{
    run as run_unified, UnifiedHandle, UnifiedRequest, UnifiedResult, UnifiedSource, UnifiedSummary,
};
