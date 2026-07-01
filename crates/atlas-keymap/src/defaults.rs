//! Default key bindings and action metadata shipped with Atlas.
//!
//! # Notes on conflict resolution
//!
//! `cmd-1` through `cmd-9` are assigned to `tab::Select1`–`tab::Select9`.
//! View mode switching therefore uses `cmd-alt-1` through `cmd-alt-5` to avoid
//! collision.

use crate::{ActionId, ActionMeta, Binding, ChordSequence};

/// Parse a chord sequence, panicking with a descriptive message on failure.
/// Only used at startup with hard-coded strings — a panic here is a programmer error.
fn seq(s: &str) -> ChordSequence {
    ChordSequence::from_str(s)
        .unwrap_or_else(|error| panic!("default binding string {s:?} failed to parse: {error}"))
}

fn b(sequence: &str, context: &str, action: &str) -> Binding {
    Binding {
        sequence: seq(sequence),
        context: context.to_owned(),
        action: ActionId::new(action),
    }
}

/// Returns the default bindings shipped with Atlas.
pub fn default_bindings() -> Vec<Binding> {
    vec![
        b("cmd-shift-p", "Global", "command_palette::Toggle"),
        b("cmd-p", "Global", "goto::Anything"),
        b("cmd-,", "Global", "app::OpenSettings"),
        b("cmd-q", "Global", "app::Quit"),
        b("j", "Pane", "pane::MoveDown"),
        b("k", "Pane", "pane::MoveUp"),
        b("h", "Pane", "pane::MoveLeft"),
        b("l", "Pane", "pane::MoveRight"),
        b("g g", "Pane", "pane::MoveToTop"),
        b("shift-g", "Pane", "pane::MoveToBottom"),
        b("/", "Pane", "pane::SearchInPlace"),
        b("enter", "Pane", "pane::Activate"),
        b("backspace", "Pane", "pane::GoUp"),
        b("alt-left", "Pane", "pane::Back"),
        b("alt-right", "Pane", "pane::Forward"),
        b("space", "Pane", "pane::ToggleSelection"),
        b("cmd-a", "Pane", "pane::SelectAll"),
        b("cmd-shift-a", "Pane", "pane::DeselectAll"),
        b("cmd-t", "Global", "tab::New"),
        b("cmd-w", "Global", "tab::Close"),
        b("cmd-shift-t", "Global", "tab::ReopenClosed"),
        b("cmd-1", "Global", "tab::Select1"),
        b("cmd-2", "Global", "tab::Select2"),
        b("cmd-3", "Global", "tab::Select3"),
        b("cmd-4", "Global", "tab::Select4"),
        b("cmd-5", "Global", "tab::Select5"),
        b("cmd-6", "Global", "tab::Select6"),
        b("cmd-7", "Global", "tab::Select7"),
        b("cmd-8", "Global", "tab::Select8"),
        b("cmd-9", "Global", "tab::Select9"),
        b("cmd-alt-1", "Global", "view::Details"),
        b("cmd-alt-2", "Global", "view::Grid"),
        b("cmd-alt-3", "Global", "view::Gallery"),
        b("cmd-alt-4", "Global", "view::Miller"),
        b("cmd-alt-5", "Global", "view::Tree"),
        b("f2", "Pane", "fs::Rename"),
        b("f3", "Pane", "fs::View"),
        b("f4", "Pane", "fs::Edit"),
        b("f5", "Pane", "fs::Copy"),
        b("f6", "Pane", "fs::Move"),
        b("f7", "Pane", "fs::Mkdir"),
        b("f8", "Pane", "fs::Delete"),
        b("cmd-c", "Pane", "fs::CopyToClipboard"),
        b("cmd-x", "Pane", "fs::CutToClipboard"),
        b("cmd-v", "Pane", "fs::PasteFromClipboard"),
        // ── Pane split / close ────────────────────────────────────────────────
        b("cmd-d", "Global", "pane::SplitRight"),
        b("cmd-shift-d", "Global", "pane::SplitDown"),
        b("cmd-shift-w", "Global", "pane::Close"),
        // ── Pane focus (vim-style) ─────────────────────────────────────────
        b("ctrl-h", "Pane", "pane::FocusLeft"),
        b("ctrl-j", "Pane", "pane::FocusDown"),
        b("ctrl-k", "Pane", "pane::FocusUp"),
        b("ctrl-l", "Pane", "pane::FocusRight"),
        // ── View cycle ───────────────────────────────────────────────────────
        b("cmd-shift-e", "Pane", "view::Cycle"),
        // ── Tab cycle ────────────────────────────────────────────────────────
        b("cmd-shift-[", "Pane", "tab::CyclePrev"),
        b("cmd-shift-]", "Pane", "tab::CycleNext"),
        // tab::Reopen supersedes tab::ReopenClosed on cmd-shift-t (last wins).
        b("cmd-shift-t", "Global", "tab::Reopen"),
        // ── Search / ops / rename / dual-pane ─────────────────────────────────
        b("cmd-f", "Global", "search::Toggle"),
        b("cmd-shift-f", "Global", "search::Open"),
        b("cmd-j", "Global", "ops::TogglePanel"),
        b("cmd-shift-f2", "Pane", "rename::OpenBulk"),
        b("cmd-\\", "Global", "workspace::ToggleDualPane"),
        b("cmd-shift-n", "Pane", "fs::Mkdir"),
    ]
}

/// Returns metadata for all default actions.
pub fn default_actions() -> Vec<ActionMeta> {
    macro_rules! action {
        ($id:expr, $title:expr, $desc:expr, $contexts:expr) => {
            ActionMeta {
                id: ActionId::new($id),
                title: $title.into(),
                description: $desc,
                contexts: $contexts
                    .iter()
                    .map(|context: &&str| (*context).to_string())
                    .collect(),
            }
        };
    }

    vec![
        action!(
            "command_palette::Toggle",
            "Toggle Command Palette",
            Some("Open or close the command palette.".into()),
            &["Global"]
        ),
        action!(
            "goto::Anything",
            "Go to Anything",
            Some("Quickly open any file or directory.".into()),
            &["Global"]
        ),
        action!("app::OpenSettings", "Open Settings", None, &["Global"]),
        action!("app::Quit", "Quit Atlas", None, &["Global"]),
        action!("pane::MoveDown", "Move Down", None, &["Pane"]),
        action!("pane::MoveUp", "Move Up", None, &["Pane"]),
        action!("pane::MoveLeft", "Move Left", None, &["Pane"]),
        action!("pane::MoveRight", "Move Right", None, &["Pane"]),
        action!("pane::MoveToTop", "Move to Top", None, &["Pane"]),
        action!("pane::MoveToBottom", "Move to Bottom", None, &["Pane"]),
        action!("pane::SearchInPlace", "Search in Place", None, &["Pane"]),
        action!("pane::Activate", "Activate", None, &["Pane"]),
        action!("pane::GoUp", "Go Up (Parent Directory)", None, &["Pane"]),
        action!("pane::Back", "Navigate Back", None, &["Pane"]),
        action!("pane::Forward", "Navigate Forward", None, &["Pane"]),
        action!("pane::ToggleSelection", "Toggle Selection", None, &["Pane"]),
        action!("pane::SelectAll", "Select All", None, &["Pane"]),
        action!("pane::DeselectAll", "Deselect All", None, &["Pane"]),
        action!("tab::New", "New Tab", None, &["Global"]),
        action!("tab::Close", "Close Tab", None, &["Global"]),
        action!("tab::ReopenClosed", "Reopen Closed Tab", None, &["Global"]),
        action!("tab::Select1", "Select Tab 1", None, &["Global"]),
        action!("tab::Select2", "Select Tab 2", None, &["Global"]),
        action!("tab::Select3", "Select Tab 3", None, &["Global"]),
        action!("tab::Select4", "Select Tab 4", None, &["Global"]),
        action!("tab::Select5", "Select Tab 5", None, &["Global"]),
        action!("tab::Select6", "Select Tab 6", None, &["Global"]),
        action!("tab::Select7", "Select Tab 7", None, &["Global"]),
        action!("tab::Select8", "Select Tab 8", None, &["Global"]),
        action!("tab::Select9", "Select Tab 9", None, &["Global"]),
        action!("view::Details", "View: Details", None, &["Global"]),
        action!("view::Grid", "View: Grid", None, &["Global"]),
        action!("view::Gallery", "View: Gallery", None, &["Global"]),
        action!("view::Miller", "View: Miller Columns", None, &["Global"]),
        action!("view::Tree", "View: Tree", None, &["Global"]),
        action!("fs::Rename", "Rename", None, &["Pane"]),
        action!("fs::View", "View File", None, &["Pane"]),
        action!("fs::Edit", "Edit File", None, &["Pane"]),
        action!("fs::Copy", "Copy", None, &["Pane"]),
        action!("fs::Move", "Move", None, &["Pane"]),
        action!("fs::Mkdir", "New Directory", None, &["Pane"]),
        action!("fs::Delete", "Delete", None, &["Pane"]),
        action!("fs::CopyToClipboard", "Copy to Clipboard", None, &["Pane"]),
        action!("fs::CutToClipboard", "Cut to Clipboard", None, &["Pane"]),
        action!(
            "fs::PasteFromClipboard",
            "Paste from Clipboard",
            None,
            &["Pane"]
        ),
        // ── Pane split / close ────────────────────────────────────────────────
        action!("pane::SplitRight", "Split Pane Right", None, &["Global"]),
        action!("pane::SplitDown", "Split Pane Down", None, &["Global"]),
        action!("pane::Close", "Close Pane", None, &["Global"]),
        // ── Pane focus (vim-style) ─────────────────────────────────────────
        action!("pane::FocusLeft", "Focus Left Pane", None, &["Pane"]),
        action!("pane::FocusDown", "Focus Below Pane", None, &["Pane"]),
        action!("pane::FocusUp", "Focus Above Pane", None, &["Pane"]),
        action!("pane::FocusRight", "Focus Right Pane", None, &["Pane"]),
        // ── View cycle ───────────────────────────────────────────────────────
        action!("view::Cycle", "Cycle View Mode", None, &["Pane"]),
        // ── Tab cycle / reopen ────────────────────────────────────────────────
        action!("tab::CyclePrev", "Previous Tab", None, &["Pane"]),
        action!("tab::CycleNext", "Next Tab", None, &["Pane"]),
        action!("tab::Reopen", "Reopen Closed Tab", None, &["Global"]),
        // ── Search / ops / rename / dual-pane ─────────────────────────────────
        action!(
            "search::Toggle",
            "Toggle Search Panel",
            Some("Show or hide the right-hand search panel.".into()),
            &["Global"]
        ),
        action!(
            "search::Open",
            "Open Search Panel",
            Some("Show the search panel and focus the query input.".into()),
            &["Global"]
        ),
        action!(
            "ops::TogglePanel",
            "Toggle Operations Panel",
            Some("Show or hide the bottom operations tray.".into()),
            &["Global"]
        ),
        action!(
            "rename::OpenBulk",
            "Open Bulk Rename",
            Some("Open the bulk-rename modal with the focused pane's selection.".into()),
            &["Pane"]
        ),
        action!(
            "workspace::ToggleDualPane",
            "Toggle Dual Pane",
            Some("Add a second pane, or close it if one already exists.".into()),
            &["Global"]
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_default_bindings_parse() {
        let bindings = default_bindings();
        assert!(!bindings.is_empty());
    }

    #[test]
    fn test_all_actions_covered() {
        let bindings = default_bindings();
        let actions = default_actions();
        let action_ids: HashSet<&str> = actions.iter().map(|action| action.id.as_str()).collect();

        for binding in &bindings {
            assert!(
                action_ids.contains(binding.action.as_str()),
                "binding action {:?} has no corresponding ActionMeta",
                binding.action
            );
        }
    }

    #[test]
    fn test_new_pane_bindings_present() {
        let bindings = default_bindings();
        let action_set: HashSet<&str> = bindings.iter().map(|b| b.action.as_str()).collect();
        for id in [
            "pane::SplitRight",
            "pane::SplitDown",
            "pane::Close",
            "pane::FocusLeft",
            "pane::FocusDown",
            "pane::FocusUp",
            "pane::FocusRight",
            "view::Cycle",
            "tab::CyclePrev",
            "tab::CycleNext",
            "tab::Reopen",
        ] {
            assert!(action_set.contains(id), "missing binding for {id:?}");
        }
    }

    #[test]
    fn test_new_actions_in_default_actions() {
        let actions = default_actions();
        let action_ids: HashSet<&str> = actions.iter().map(|a| a.id.as_str()).collect();
        for id in [
            "pane::SplitRight",
            "pane::SplitDown",
            "pane::Close",
            "pane::FocusLeft",
            "pane::FocusDown",
            "pane::FocusUp",
            "pane::FocusRight",
            "view::Cycle",
            "tab::CyclePrev",
            "tab::CycleNext",
            "tab::Reopen",
        ] {
            assert!(action_ids.contains(id), "missing ActionMeta for {id:?}");
        }
    }
}
