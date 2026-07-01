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

    // ── File-system operations (F-key dispatch) ───────────────────────────────
    //
    // These variants are emitted by the shell's F-key callback wiring and
    // handled directly by AppShell (which owns the OpsController).
    // They are also available for future atlas-keymap integration so that
    // keymap-driven action IDs (e.g. "fs::Copy") can be translated here.
    /// F5 — copy the source pane's selection to the target pane's directory.
    ///
    /// When `target_pane` is `None` (no dual-pane), the operation is skipped
    /// and a warning is logged. A destination-path dialog is a post-MVP
    /// follow-up.
    FsCopy {
        /// Pane whose selection provides the sources.
        source_pane: usize,
        /// Pane that provides the destination directory, if available.
        target_pane: Option<usize>,
    },
    /// F6 — move the source pane's selection to the target pane's directory.
    FsMove {
        /// Pane whose selection provides the sources.
        source_pane: usize,
        /// Pane that provides the destination directory, if available.
        target_pane: Option<usize>,
    },
    /// F8 — delete the focused pane's selection.
    ///
    /// When `to_trash` is `true` (default for F8), items are moved to the OS
    /// trash. Permanent deletion requires an explicit Shift+F8 binding
    /// (post-MVP).
    FsDelete {
        /// Pane whose selection is deleted.
        pane: usize,
        /// Send to trash rather than permanently delete.
        to_trash: bool,
    },
    /// F2 — rename the focused entry in a pane.
    ///
    /// The rename dialog UI is a post-MVP follow-up. The F2 handler currently
    /// logs the action and skips the operation.
    FsRename {
        /// Pane that contains the focused entry.
        pane: usize,
        /// Row index of the entry to rename.
        index: usize,
    },
    /// F7 — create a new directory inside the focused pane's location.
    FsMkdir {
        /// Pane in which the new directory is created.
        pane: usize,
    },
    /// Cancel a running operation by its `OpId`.
    FsCancel {
        /// The numeric `OpId` to cancel.
        op_id: u64,
    },
    /// Resolve a conflict for a running operation.
    ///
    /// `decision` is one of `"skip"`, `"overwrite"`, or `"rename"`.
    /// This is a placeholder for a future conflict-resolution dialog.
    FsResolveConflict {
        /// The numeric `OpId` whose conflict to resolve.
        op_id: u64,
        /// Resolution decision string.
        decision: String,
    },
    /// Show or hide the file-operations tray (Ctrl/Cmd+J).
    ToggleOpsPanel,
    /// Open a file (e.g. by double-click or Enter in the Miller/Tree view).
    OpenFile {
        /// Pane that contains the entry being opened.
        pane: usize,
        /// Path to the file being opened.
        path: PathBuf,
    },
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
