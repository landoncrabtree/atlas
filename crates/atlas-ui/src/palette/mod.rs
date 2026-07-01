//! Command palette — fuzzy action picker and goto-anything file picker.
//!
//! # Architecture
//!
//! - [`PaletteSource`] trait: enumerate candidate items.
//! - [`PaletteMatcher`]: thin nucleo wrapper that scores and ranks items.
//! - [`PaletteController`]: orchestrates open/close/query/confirm and pushes
//!   [`crate::models::PaletteModel`] updates to the window.

pub mod controller;
pub mod matcher;
pub mod source;

pub use controller::PaletteController;
pub use source::{
    ActionsSource, GotoPathsSource, ItemSink, PaletteItem, PaletteItemKind, PaletteSource,
    PathIndex, WalkerPathIndex,
};
