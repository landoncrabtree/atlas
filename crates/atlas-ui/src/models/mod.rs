//! Pure-Rust view models for the Atlas UI shell.
//!
//! These types are independent of the Slint runtime and can be constructed,
//! cloned, and tested without a display server. The [`crate::shell::AppShell`]
//! adapter translates them into Slint property values on the event-loop thread.

pub mod palette;
pub mod pane;
pub mod status;
pub mod tab;
pub mod workspace;

pub use palette::{PaletteModel, PaletteResult};
pub use pane::{PaneModel, ViewMode};
pub use status::{IndexerState, StatusModel};
pub use tab::TabModel;
pub use workspace::WorkspaceModel;
