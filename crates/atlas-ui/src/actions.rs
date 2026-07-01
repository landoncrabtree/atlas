//! UI action vocabulary.
//!
//! Every user gesture that produces a semantic operation is expressed as a
//! [`UiAction`] value and routed through an [`ActionSink`]. The concrete sink
//! wired at startup is a [`crate::shell::AppShell`]-provided adapter; the
//! real atlas-keymap integration is a follow-up todo.

use std::path::PathBuf;

use crate::models::ViewMode;

/// All actions the UI layer can emit.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum UiAction {
    /// Open or close the command palette.
    TogglePalette,
    /// Toggle the search panel open/closed.
    ToggleSearchPanel,
    /// Open the search panel (with focus on the query input).
    OpenSearchPanel,
    /// User changed the search query.
    SearchQueryChanged(String),
    /// User confirmed a search result.
    SearchConfirm(usize),
    /// User closed the search panel.
    SearchClose,
    /// Close the command palette without confirming.
    DismissPalette,
    /// User typed in the palette query field.
    PaletteQueryChanged(String),
    /// User confirmed a palette result.
    PaletteConfirm(String),
    /// Keyboard or click moved focus to a different pane.
    PaneFocusChanged(usize),
    /// User selected a tab in a pane.
    TabSelected { pane: usize, tab: usize },
    /// User closed a tab in a pane.
    TabClosed { pane: usize, tab: usize },
    /// User opened a new tab in a pane.
    NewTab { pane: usize },
    /// User submitted a path via the address bar.
    Navigate { pane: usize, path: PathBuf },
    /// User clicked a breadcrumb segment.
    BreadcrumbClicked { pane: usize, segment: usize },
    /// Toggle dual-pane mode.
    SetDualPane(bool),
    /// Switch the view rendering mode for a pane.
    SetViewMode { pane: usize, mode: ViewMode },
}

/// Consumer of UI actions.
///
/// Implemented by the top-level application coordinator. The shell calls
/// [`ActionSink::dispatch`] for every user gesture. Implementations must be
/// `Send` and `'static` so they can be wrapped in a [`std::sync::Arc`] and
/// moved across threads.
pub trait ActionSink: Send + 'static {
    /// Handle a UI action. This is called on the Slint event-loop thread.
    fn dispatch(&mut self, action: UiAction);
}
