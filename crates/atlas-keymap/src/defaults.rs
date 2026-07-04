//! Default key bindings and action metadata shipped with Atlas.
//!
//! # Notes on conflict resolution
//!
//! `cmd-1` through `cmd-9` are assigned to `tab::Select1`–`tab::Select9`.
//! View mode switching therefore uses `cmd-alt-1` through `cmd-alt-5` to avoid
//! collision.

use crate::{ActionId, ActionMeta, Binding, ChordSequence, PrettyPlatform};

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

/// Returns the default bindings shipped with Atlas for the current platform.
///
/// The default keymap uses `cmd` shortcuts on macOS (matching every native
/// mac app) and `ctrl` shortcuts on Linux/Windows (matching GNOME/KDE/Windows
/// Explorer conventions). Users can override any binding in
/// `~/.config/atlas/keymaps/default.toml` — the keymap does NOT silently
/// remap `cmd` → `ctrl` at dispatch time; what the file says is what the
/// dispatcher matches.
#[must_use]
pub fn default_bindings() -> Vec<Binding> {
    default_bindings_for(PrettyPlatform::current())
}

/// Returns the default bindings for a specific platform. Split from
/// [`default_bindings`] so tests can exercise every platform's table
/// deterministically.
#[must_use]
pub fn default_bindings_for(platform: PrettyPlatform) -> Vec<Binding> {
    // The "primary" modifier is what the platform uses for global
    // application shortcuts. macOS uses ⌘, Linux/Windows use ⌃.
    // Vim-style `ctrl+hjkl` pane navigation stays literal on every
    // platform (users expect the physical Ctrl key).
    let m = match platform {
        PrettyPlatform::Mac => "cmd",
        PrettyPlatform::Windows | PrettyPlatform::Linux => "ctrl",
    };
    // Convenience: format a chord using the platform's primary modifier.
    let p = |modless: &str, action_ctx: (&str, &str)| {
        let (ctx, action) = action_ctx;
        b(&format!("{m}-{modless}"), ctx, action)
    };
    let ps = |modless: &str, action_ctx: (&str, &str)| {
        let (ctx, action) = action_ctx;
        b(&format!("{m}-shift-{modless}"), ctx, action)
    };
    let pa = |modless: &str, action_ctx: (&str, &str)| {
        let (ctx, action) = action_ctx;
        b(&format!("{m}-alt-{modless}"), ctx, action)
    };

    vec![
        // ── Palette / goto / app ──────────────────────────────────────────
        ps("p", ("Global", "command_palette::Toggle")),
        p("p", ("Global", "goto::Anything")),
        p(",", ("Global", "app::OpenSettings")),
        p("q", ("Global", "app::Quit")),
        // ── File-list navigation (Pane context) ───────────────────────────
        //
        // Vim `hjkl`, WASD, arrow keys, `,` / `.`, `Enter`, and `Backspace`
        // all navigate the focused pane's file list. They are always active
        // — there is no config-gated "vim mode" switch. Because Slint's
        // key-pressed handler is bubble-phase, typing in the address bar
        // TextInput consumes these letters BEFORE the dispatcher sees them,
        // so vim/wasd navigation and text entry never collide. When a modal
        // or focused text input is up, `keymap-bypass-active` flips true and
        // the dispatcher restricts to the `[Global]` context — Pane bindings
        // return false and the key falls through to the input natively.
        //
        // `fs::View` handles BOTH cd-into-folder and open-file-with-OS —
        // there is no separate "activate" action; the single action ID
        // is the source of truth and any number of chords may bind to
        // it (multiple keybinds → one action, never one keybind →
        // multiple aliased actions).
        b("j", "Pane", "pane::MoveDown"),
        b("k", "Pane", "pane::MoveUp"),
        b("h", "Pane", "pane::MoveLeft"),
        b("l", "Pane", "pane::MoveRight"),
        // WASD as first-class parallel bindings — always active regardless
        // of platform, no config gate. Same view-dispatch semantics as hjkl.
        b("w", "Pane", "pane::MoveUp"),
        b("a", "Pane", "pane::MoveLeft"),
        b("s", "Pane", "pane::MoveDown"),
        b("d", "Pane", "pane::MoveRight"),
        b(",", "Pane", "pane::GoUp"),
        b(".", "Pane", "fs::View"),
        b("down", "Pane", "pane::MoveDown"),
        b("up", "Pane", "pane::MoveUp"),
        b("right", "Pane", "pane::MoveRight"),
        b("left", "Pane", "pane::MoveLeft"),
        b("g g", "Pane", "pane::MoveToTop"),
        b("shift-g", "Pane", "pane::MoveToBottom"),
        b("/", "Pane", "pane::SearchInPlace"),
        b("enter", "Pane", "fs::View"),
        b("backspace", "Pane", "pane::GoUp"),
        b("alt-left", "Pane", "pane::Back"),
        b("alt-right", "Pane", "pane::Forward"),
        // Space toggles the focused entry's selected state (multi-select
        // support), Shift+Arrow/j/k/w/s extends the range selection while
        // moving focus vertically.
        b("space", "Pane", "pane::ToggleSelection"),
        b("shift-down", "Pane", "pane::ExtendDown"),
        b("shift-up", "Pane", "pane::ExtendUp"),
        b("shift-j", "Pane", "pane::ExtendDown"),
        b("shift-k", "Pane", "pane::ExtendUp"),
        b("shift-s", "Pane", "pane::ExtendDown"),
        b("shift-w", "Pane", "pane::ExtendUp"),
        p("a", ("Pane", "pane::SelectAll")),
        ps("a", ("Pane", "pane::DeselectAll")),
        // ── Tabs ──────────────────────────────────────────────────────────
        p("t", ("Global", "tab::New")),
        p("w", ("Global", "tab::Close")),
        ps("t", ("Global", "tab::Reopen")),
        p("1", ("Global", "tab::Select1")),
        p("2", ("Global", "tab::Select2")),
        p("3", ("Global", "tab::Select3")),
        p("4", ("Global", "tab::Select4")),
        p("5", ("Global", "tab::Select5")),
        p("6", ("Global", "tab::Select6")),
        p("7", ("Global", "tab::Select7")),
        p("8", ("Global", "tab::Select8")),
        p("9", ("Global", "tab::Select9")),
        ps("[", ("Pane", "tab::CyclePrev")),
        ps("]", ("Pane", "tab::CycleNext")),
        // ── View modes ────────────────────────────────────────────────────
        pa("1", ("Global", "view::Details")),
        pa("2", ("Global", "view::Grid")),
        pa("3", ("Global", "view::Gallery")),
        pa("4", ("Global", "view::Miller")),
        ps("e", ("Pane", "view::Cycle")),
        // ── File operations (Finder / Explorer / Nautilus conventions) ───
        //
        // Copy / cut / paste go through the OS clipboard so the user can
        // paste into Atlas, Finder, VS Code, TextEdit, anything.
        p("c", ("Pane", "fs::Copy")),
        p("x", ("Pane", "fs::Cut")),
        p("v", ("Pane", "fs::Paste")),
        // Delete → move to Trash. macOS uses ⌘⌫; Linux/Windows use the
        // plain Delete key (matching Nautilus / Explorer).
        match platform {
            PrettyPlatform::Mac => p("backspace", ("Pane", "fs::Delete")),
            PrettyPlatform::Linux | PrettyPlatform::Windows => b("delete", "Pane", "fs::Delete"),
        },
        // New folder — Finder ⌘⇧N, Nautilus/Explorer Ctrl+Shift+N.
        ps("n", ("Pane", "fs::Mkdir")),
        // Rename — F2 is universal (Windows, Nautilus, KDE, and even
        // Finder if the user has "Show tab bar" style prefs set).
        b("f2", "Pane", "fs::Rename"),
        // ── Pane split / close / focus ────────────────────────────────────
        p("d", ("Global", "pane::SplitRight")),
        ps("d", ("Global", "pane::SplitDown")),
        ps("w", ("Global", "pane::Close")),
        // Vim-style pane focus stays on physical Ctrl on macOS (users
        // muscle-memory-remember Ctrl+H/J/K/L from tmux/vim). On
        // Linux/Windows `Ctrl+H` is already the OS-native "toggle hidden
        // files" chord (Nautilus, Nemo, Thunar, Dolphin all use it),
        // so `pane::FocusLeft` shifts to `Ctrl+Shift+H` there to keep
        // vim-style navigation available without stomping the platform
        // convention.
        match platform {
            PrettyPlatform::Mac => b("ctrl-h", "Pane", "pane::FocusLeft"),
            PrettyPlatform::Linux | PrettyPlatform::Windows => {
                b("ctrl-shift-h", "Pane", "pane::FocusLeft")
            }
        },
        b("ctrl-j", "Pane", "pane::FocusDown"),
        b("ctrl-k", "Pane", "pane::FocusUp"),
        b("ctrl-l", "Pane", "pane::FocusRight"),
        // Per-pane hidden-files toggle. macOS uses `Cmd+.` (Finder's
        // `Cmd+Shift+.` is dotfile-only but Atlas has no separate
        // "system" vs "user" hidden concept, so we ship the shorter
        // one-key chord). Linux/Windows use `Ctrl+H` (Nautilus / Nemo /
        // Thunar / Dolphin). Per-pane means the focused pane flips
        // independently — split screens can show different states.
        match platform {
            PrettyPlatform::Mac => b("cmd-.", "Pane", "pane::ToggleHidden"),
            PrettyPlatform::Linux | PrettyPlatform::Windows => {
                b("ctrl-h", "Pane", "pane::ToggleHidden")
            }
        },
        // ── Search / ops / bulk-rename / dual-pane ────────────────────────
        b("escape", "Global", "ui::Cancel"),
        p("f", ("Global", "search::Toggle")),
        ps("f", ("Global", "search::Open")),
        p("j", ("Global", "ops::TogglePanel")),
        ps("f2", ("Pane", "rename::OpenBulk")),
        p("\\", ("Global", "workspace::ToggleDualPane")),
        // ── Remote / connect ─────────────────────────────────────────────
        //
        // Cmd+K on macOS (matching Finder's ⌘K "Connect to Server"),
        // Ctrl+Alt+K on Linux/Windows to avoid stomping Ctrl+K, which
        // browsers and editors overwhelmingly reserve for "focus search".
        match platform {
            PrettyPlatform::Mac => b("cmd-k", "Global", "remote::Connect"),
            PrettyPlatform::Linux | PrettyPlatform::Windows => {
                b("ctrl-alt-k", "Global", "remote::Connect")
            }
        },
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
        action!(
            "pane::MoveLeft",
            "Move Left",
            Some(
                "Directional Left. Semantics depend on the focused pane's \
                 view mode: Details/Miller → parent directory; Grid → previous \
                 column (row-wraps); Gallery → previous item."
                    .into()
            ),
            &["Pane"]
        ),
        action!(
            "pane::MoveRight",
            "Move Right",
            Some(
                "Directional Right. Semantics depend on the focused pane's \
                 view mode: Details/Miller → open focused entry (fs::View); \
                 Grid → next column (row-wraps); Gallery → next item."
                    .into()
            ),
            &["Pane"]
        ),
        action!("pane::MoveToTop", "Move to Top", None, &["Pane"]),
        action!("pane::MoveToBottom", "Move to Bottom", None, &["Pane"]),
        action!("pane::SearchInPlace", "Search in Place", None, &["Pane"]),
        action!("pane::GoUp", "Go Up (Parent Directory)", None, &["Pane"]),
        action!("pane::Back", "Navigate Back", None, &["Pane"]),
        action!("pane::Forward", "Navigate Forward", None, &["Pane"]),
        action!("pane::ToggleSelection", "Toggle Selection", None, &["Pane"]),
        action!(
            "pane::ExtendDown",
            "Extend Selection Down",
            Some("Move focus down while extending the range selection.".into()),
            &["Pane"]
        ),
        action!(
            "pane::ExtendUp",
            "Extend Selection Up",
            Some("Move focus up while extending the range selection.".into()),
            &["Pane"]
        ),
        action!("pane::SelectAll", "Select All", None, &["Pane"]),
        action!("pane::DeselectAll", "Deselect All", None, &["Pane"]),
        action!("tab::New", "New Tab", None, &["Global"]),
        action!("tab::Close", "Close Tab", None, &["Global"]),
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
        action!(
            "fs::View",
            "Open",
            Some("Open the focused entry: cd into folders, hand files off to the OS default handler.".into()),
            &["Pane"]
        ),
        action!("fs::Rename", "Rename", None, &["Pane"]),
        action!("fs::Mkdir", "New Folder", None, &["Pane"]),
        action!(
            "fs::Delete",
            "Move to Trash",
            Some("Move the selection to the OS trash.".into()),
            &["Pane"]
        ),
        action!(
            "fs::Copy",
            "Copy",
            Some("Copy the selection to the OS clipboard as file paths.".into()),
            &["Pane"]
        ),
        action!(
            "fs::Cut",
            "Cut",
            Some("Copy the selection to the clipboard; paste moves instead of copying.".into()),
            &["Pane"]
        ),
        action!(
            "fs::Paste",
            "Paste",
            Some("Paste files from the clipboard into the focused pane's directory.".into()),
            &["Pane"]
        ),
        action!(
            "fs::Duplicate",
            "Duplicate",
            Some("Copy the focused entry into the same directory with a `(copy)` suffix.".into()),
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
        // ── Per-pane hidden-files toggle ─────────────────────────────────
        action!(
            "pane::ToggleHidden",
            "Toggle Hidden Files",
            Some(
                "Show or hide dotfiles in the focused pane. Per-pane: \
                 one pane can hide dotfiles while another shows them. \
                 Runtime only — does not persist to config.toml."
                    .into()
            ),
            &["Pane"]
        ),
        // ── View cycle ───────────────────────────────────────────────────────
        action!("view::Cycle", "Cycle View Mode", None, &["Pane"]),
        // ── Tab cycle / reopen ────────────────────────────────────────────────
        action!("tab::CyclePrev", "Previous Tab", None, &["Pane"]),
        action!("tab::CycleNext", "Next Tab", None, &["Pane"]),
        action!("tab::Reopen", "Reopen Closed Tab", None, &["Global"]),
        // ── Search / ops / rename / dual-pane ─────────────────────────────────
        action!(
            "ui::Cancel",
            "Cancel / Close Panel",
            Some("Close the active right-side dock panel when one is visible.".into()),
            &["Global"]
        ),
        action!(
            "search::Toggle",
            "Toggle Search Panel",
            Some("Show, hide, or swap to the right-side search panel.".into()),
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
            Some("Show, hide, or swap to the right-side operations panel.".into()),
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
        action!(
            "remote::Connect",
            "Connect to Server",
            Some(
                "Open the connect-to-server modal to attach a remote filesystem \
                 (SFTP, S3, WebDAV, FTP) to the focused pane."
                    .into()
            ),
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
    fn test_platform_specific_primary_modifier() {
        // On macOS, `tab::New` binds to `cmd-t`; on Linux/Windows, `ctrl-t`.
        // This test guards against the platform picker regressing.
        for (platform, expected_seq) in [
            (PrettyPlatform::Mac, "cmd-t"),
            (PrettyPlatform::Linux, "ctrl-t"),
            (PrettyPlatform::Windows, "ctrl-t"),
        ] {
            let bindings = default_bindings_for(platform);
            let tab_new = bindings
                .iter()
                .find(|b| b.action.as_str() == "tab::New")
                .expect("tab::New must have a default binding");
            assert_eq!(
                tab_new.sequence.display(),
                expected_seq,
                "{:?}: tab::New should bind to {}",
                platform,
                expected_seq
            );
        }
    }

    #[test]
    fn test_vim_pane_focus_stays_on_ctrl_every_platform() {
        // Vim-style pane navigation uses physical Ctrl on every platform;
        // muscle memory from tmux/vim expects this.
        for platform in [
            PrettyPlatform::Mac,
            PrettyPlatform::Linux,
            PrettyPlatform::Windows,
        ] {
            let bindings = default_bindings_for(platform);
            let focus_right = bindings
                .iter()
                .find(|b| b.action.as_str() == "pane::FocusRight")
                .expect("pane::FocusRight must exist");
            assert_eq!(
                focus_right.sequence.display(),
                "ctrl-l",
                "{:?}: pane::FocusRight must stay ctrl-l on every platform",
                platform
            );
        }
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
            "pane::ToggleHidden",
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
            "pane::ToggleHidden",
            "view::Cycle",
            "tab::CyclePrev",
            "tab::CycleNext",
            "tab::Reopen",
            "remote::Connect",
        ] {
            assert!(action_ids.contains(id), "missing ActionMeta for {id:?}");
        }
    }

    #[test]
    fn test_toggle_hidden_binding_per_platform() {
        // Regression test: ensure the platform-specific chord for
        // `pane::ToggleHidden` stays aligned with OS conventions and
        // that `pane::FocusLeft` moves off `ctrl-h` on Linux/Windows
        // to avoid stomping the OS-native hidden-files chord.
        for (platform, expected_toggle, expected_focus_left) in [
            (PrettyPlatform::Mac, "cmd-.", "ctrl-h"),
            (PrettyPlatform::Linux, "ctrl-h", "ctrl-shift-h"),
            (PrettyPlatform::Windows, "ctrl-h", "ctrl-shift-h"),
        ] {
            let bindings = default_bindings_for(platform);
            let toggle = bindings
                .iter()
                .find(|b| b.action.as_str() == "pane::ToggleHidden")
                .unwrap_or_else(|| {
                    panic!("pane::ToggleHidden missing on {platform:?}");
                });
            assert_eq!(
                toggle.sequence.display(),
                expected_toggle,
                "pane::ToggleHidden chord for {platform:?}"
            );
            assert_eq!(toggle.context, "Pane");

            let focus_left = bindings
                .iter()
                .find(|b| b.action.as_str() == "pane::FocusLeft")
                .unwrap_or_else(|| panic!("pane::FocusLeft missing on {platform:?}"));
            assert_eq!(
                focus_left.sequence.display(),
                expected_focus_left,
                "pane::FocusLeft chord for {platform:?}"
            );
        }
    }

    #[test]
    fn test_remote_connect_binding_per_platform() {
        for (platform, expected_seq) in [
            (PrettyPlatform::Mac, "cmd-k"),
            (PrettyPlatform::Linux, "ctrl-alt-k"),
            (PrettyPlatform::Windows, "ctrl-alt-k"),
        ] {
            let bindings = default_bindings_for(platform);
            let hit = bindings
                .iter()
                .find(|b| b.action.as_str() == "remote::Connect")
                .expect("remote::Connect must have a default binding");
            assert_eq!(
                hit.sequence.display(),
                expected_seq,
                "{platform:?}: remote::Connect should bind to {expected_seq}",
            );
            assert_eq!(hit.context, "Global");
        }
    }
}
