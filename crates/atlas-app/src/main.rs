//! Atlas — application binary.
//!
//! This is an intentionally thin wrapper. All UI types come from `atlas-ui`.
//! The file-system-backed Details view is wired here by creating the shell
//! and driving the initial pane location through the navigation controller.
//!
//! # Theme chain
//!
//! 1. `atlas_config::load()` reads `config.ui.theme` (e.g. `"atlas-dark"`).
//! 2. `ThemeLoader` resolves the ID: built-ins first, then the user themes dir.
//! 3. `ThemeWatcher::start` loads the initial tokens, returning an
//!    `Arc<ArcSwap<ThemeTokens>>` and a `Receiver<ThemeEvent>`.
//! 4. `shell.apply_theme(...)` pushes the initial tokens to the Slint `Theme`
//!    global via `invoke_from_event_loop`.
//! 5. A background thread drains the event channel; on each `Reloaded` it
//!    reads the fresh tokens from the `ArcSwap` and calls `apply_theme` again.

use std::{env, path::PathBuf, sync::Arc};

use anyhow::Result;
use arc_swap::ArcSwap;
use slint::ComponentHandle as _;
use tracing_subscriber::EnvFilter;

use atlas_ui::{
    actions::{ActionSink, UiAction},
    models::{PaletteModel, StatusModel},
    search::SearchController,
    shell::AppShell,
    theme::{ThemeLoader, ThemeTokens, ThemeWatcher},
    theming::ThemeEvent,
    AtlasWindow, NavigationController,
};

/// Application-level action sink that routes [`UiAction`]s to the appropriate
/// controller.
///
/// File-system operations (`FsCopy`, `FsMove`, etc.) are handled directly by
/// [`AppShell`]'s Slint callback wiring (see `wire_callbacks` in shell.rs),
/// so the sink only needs to handle the remaining lifecycle actions here.
/// The `Fs*` variants exist in [`UiAction`] for future atlas-keymap
/// integration (when keymap strings like `"fs::Copy"` are translated to typed
/// `UiAction` values by the keymap resolver).
struct AtlasActionSink {
    nav: Arc<NavigationController>,
}

impl AtlasActionSink {
    fn new(nav: Arc<NavigationController>) -> Self {
        Self { nav }
    }
}

impl ActionSink for AtlasActionSink {
    fn dispatch(&mut self, action: UiAction) {
        match action {
            // Navigation — actually drive the controller so the view updates.
            UiAction::Navigate { pane, path } => {
                tracing::debug!(pane, ?path, "navigating");
                self.nav.navigate(pane, path);
            }
            UiAction::BreadcrumbClicked { pane, segment } => {
                tracing::debug!(pane, segment, "breadcrumb clicked");
                self.nav.breadcrumb_clicked(pane, segment);
            }
            // Fs* actions are wired directly in AppShell::wire_callbacks via
            // Slint F-key callbacks; they do not flow through this sink in the
            // current implementation. Log at debug so the path is traceable.
            UiAction::FsCopy { .. }
            | UiAction::FsMove { .. }
            | UiAction::FsDelete { .. }
            | UiAction::FsRename { .. }
            | UiAction::FsMkdir { .. }
            | UiAction::FsCancel { .. }
            | UiAction::FsResolveConflict { .. }
            | UiAction::ToggleOpsPanel => {
                tracing::debug!(?action, "fs op action (handled by AppShell directly)");
            }
            // Bulk-rename actions are wired directly in AppShell::wire_callbacks.
            // The variants exist for future atlas-keymap integration.
            UiAction::OpenBulkRename
            | UiAction::BulkRenameQuery(_)
            | UiAction::BulkRenameConfirm { .. }
            | UiAction::BulkRenameClose => {
                tracing::debug!(?action, "bulk rename action (handled by AppShell directly)");
            }
            UiAction::SetDualPane(_) | UiAction::PaneFocusChanged(_) => {
                tracing::debug!(
                    ?action,
                    "pane focus/layout action (handled by AppShell directly)"
                );
            }
            _ => {
                tracing::info!(?action, "ui action");
            }
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,atlas=debug")),
        )
        .init();

    tracing::info!("starting atlas");

    // First-run: seed a heavily-commented config.toml so users have something
    // to edit. We only write when the file is missing; hot-reload will pick up
    // subsequent user edits automatically.
    seed_config_if_missing();

    let config = atlas_config::load().unwrap_or_default();
    let theme_id = config.ui.theme.clone();

    // Load default keymap and layer any user overrides from ~/.config/atlas/keymap.toml.
    let keymap = load_keymap_with_user_overrides();
    tracing::info!(layers = ?keymap.layers(), "keymap loaded");

    let window = AtlasWindow::new()?;
    let nav = NavigationController::new(&config.bookmarks);
    let search_ctrl = SearchController::new();

    // Try to reach the indexer daemon; if it isn't running, auto-launch it
    // and retry. Fall back to embedded-search (no index) on total failure.
    let index_client = connect_or_launch_indexd(&search_ctrl);
    search_ctrl.set_index_client(index_client);
    search_ctrl.attach_window(window.as_weak());

    let shell: Arc<AppShell> = AppShell::new(
        &window,
        AtlasActionSink::new(Arc::clone(&nav)),
        Arc::clone(&nav),
        Arc::clone(&search_ctrl),
    );

    let loader = ThemeLoader::new();
    let (theme_watcher, themes_arc, theme_events) = ThemeWatcher::start(loader, &theme_id)
        .unwrap_or_else(|e| {
            tracing::warn!("cannot load theme {theme_id:?}: {e}; falling back to atlas-dark");
            ThemeWatcher::start(ThemeLoader::new(), "atlas-dark")
                .expect("built-in atlas-dark must always load")
        });

    shell.apply_theme(&themes_arc.load());
    spawn_theme_event_thread(Arc::clone(&shell), Arc::clone(&themes_arc), theme_events);

    // Start the config hot-reload watcher so users can edit config.toml and
    // see changes take effect (currently the theme id and start path — more
    // consumers can subscribe from here as needed).
    let (config_watcher, config_arc, config_events) = match atlas_config::ConfigWatcher::start() {
        Ok(triple) => {
            let (w, a, e) = triple;
            (Some(w), Some(a), Some(e))
        }
        Err(err) => {
            tracing::warn!(%err, "config watcher failed to start; edits will not hot-reload");
            (None, None, None)
        }
    };
    if let (Some(arc), Some(events)) = (config_arc.clone(), config_events) {
        spawn_config_event_thread(Arc::clone(&shell), arc, events);
    }

    let start_path = config_arc
        .as_ref()
        .and_then(|a| a.load().general.start_path.clone())
        .or(config.general.start_path.clone())
        .unwrap_or_else(|| {
            env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/"))
        });
    search_ctrl.set_scope(Some(start_path.clone()));
    nav.navigate(0, start_path);
    shell.set_status(StatusModel::default());
    shell.set_palette(PaletteModel::default());

    // Keep `keymap` alive for the lifetime of the app so a future keymap-
    // dispatch integration can consume it. Suppress the unused warning until
    // the Slint FocusScope routing lands (tracked as a follow-up).
    let _keymap_handle = keymap;

    window.run()?;
    if let Some(w) = config_watcher {
        w.stop();
    }
    theme_watcher.stop();

    Ok(())
}

/// Write `atlas-config::skeleton_toml()` to the platform config path when the
/// file does not exist yet. Logs but does not fail if writing is impossible.
fn seed_config_if_missing() {
    let path = match atlas_config::config_file_path() {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(%err, "could not resolve config file path");
            return;
        }
    };
    if path.exists() {
        return;
    }
    if let Err(err) = atlas_config::ensure_config_dir() {
        tracing::warn!(%err, "could not create config directory");
        return;
    }
    if let Err(err) = std::fs::write(&path, atlas_config::skeleton_toml()) {
        tracing::warn!(%err, path = %path.display(), "could not seed default config");
        return;
    }
    tracing::info!(path = %path.display(), "seeded default config.toml");
}

/// Build the default keymap and layer any user overrides from
/// `~/.config/atlas/keymap.toml` on top.
fn load_keymap_with_user_overrides() -> atlas_keymap::Keymap {
    let mut keymap = atlas_keymap::Keymap::with_defaults();
    let path = match atlas_config::keymap_file_path() {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(%err, "could not resolve keymap file path");
            return keymap;
        }
    };
    if !path.exists() {
        return keymap;
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            if let Err(err) = keymap.apply_user_toml(&text) {
                tracing::warn!(%err, path = %path.display(), "user keymap.toml has errors; using defaults only");
            } else {
                tracing::info!(path = %path.display(), "loaded user keymap overrides");
            }
        }
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "could not read user keymap");
        }
    }
    keymap
}

/// Try to connect to `atlas-indexd`; if it isn't listening, spawn the sibling
/// binary and retry a few times before giving up.
fn connect_or_launch_indexd(
    search_ctrl: &Arc<SearchController>,
) -> Option<Arc<atlas_search::IndexClient>> {
    let handle = search_ctrl.runtime_handle();

    // Try once — daemon might already be running.
    if let Ok(client) = handle.block_on(atlas_search::IndexClient::connect_default()) {
        tracing::info!("connected to atlas-indexd");
        return Some(Arc::new(client));
    }

    // Attempt to spawn the sibling binary.
    let daemon_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("atlas-indexd")));
    let daemon_path = match daemon_path {
        Some(p) if p.exists() => p,
        _ => {
            tracing::warn!("atlas-indexd binary not found next to atlas-app; search index disabled");
            return None;
        }
    };

    tracing::info!(path = %daemon_path.display(), "spawning atlas-indexd");
    match std::process::Command::new(&daemon_path)
        .arg("run")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_child) => {
            // Retry connect with backoff.
            for attempt in 1..=5 {
                std::thread::sleep(std::time::Duration::from_millis(200 * attempt));
                if let Ok(client) = handle.block_on(atlas_search::IndexClient::connect_default()) {
                    tracing::info!(attempt, "connected to freshly-spawned atlas-indexd");
                    return Some(Arc::new(client));
                }
            }
            tracing::warn!("spawned atlas-indexd but could not connect after retries");
            None
        }
        Err(err) => {
            tracing::warn!(%err, path = %daemon_path.display(), "could not spawn atlas-indexd");
            None
        }
    }
}

/// Spawn a thread that drains [`atlas_config::ConfigEvent`]s and re-applies
/// user settings. Currently we log; a follow-up will route theme changes into
/// the theme watcher and start-path changes into the navigation controller.
fn spawn_config_event_thread(
    _shell: Arc<AppShell>,
    _config_arc: Arc<ArcSwap<atlas_config::Config>>,
    events: crossbeam_channel::Receiver<atlas_config::ConfigEvent>,
) {
    std::thread::Builder::new()
        .name(String::from("atlas-config-events"))
        .spawn(move || {
            for event in events {
                match event {
                    atlas_config::ConfigEvent::Reloaded => {
                        tracing::info!("config reloaded from disk");
                    }
                    atlas_config::ConfigEvent::LoadError(msg) => {
                        tracing::warn!(msg, "config file has errors; keeping previous values");
                    }
                }
            }
        })
        .expect("failed to spawn atlas-config-events thread");
}

/// Spawn a thread that drains [`ThemeEvent`]s and calls [`AppShell::apply_theme`]
/// on each successful reload.
fn spawn_theme_event_thread(
    shell: Arc<AppShell>,
    themes_arc: Arc<ArcSwap<ThemeTokens>>,
    events: crossbeam_channel::Receiver<ThemeEvent>,
) {
    std::thread::Builder::new()
        .name("atlas-theme-events".to_owned())
        .spawn(move || {
            for event in &events {
                match event {
                    ThemeEvent::Reloaded(ref id) => {
                        tracing::info!("theme reloaded: {id}");
                        shell.apply_theme(&themes_arc.load());
                    }
                    ThemeEvent::LoadError {
                        ref id,
                        ref message,
                    } => {
                        tracing::warn!("theme load error for {id}: {message}");
                    }
                }
            }
        })
        .expect("failed to spawn atlas-theme-events thread");
}
