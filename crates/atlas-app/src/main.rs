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
//!
//! # Keymap dispatch chain
//!
//! 1. `load_keymap_with_user_overrides()` builds a [`atlas_keymap::Keymap`].
//! 2. [`atlas_keymap::Dispatcher::new`] wraps it; handlers for common action
//!    IDs are registered (palette, navigation, cursor movement).
//! 3. The command palette's `on_dispatch` callback calls
//!    [`Dispatcher::dispatch_action`] so palette-triggered actions invoke
//!    the same handlers as keyboard-triggered ones.
//! 4. The Slint `FocusScope` → [`Dispatcher::handle_key`] routing is tracked
//!    in the `gap-keymap-slint-routing` follow-up.

use std::{env, path::PathBuf, sync::Arc};

use anyhow::Result;
use arc_swap::ArcSwap;
use slint::ComponentHandle as _;
use tracing_subscriber::EnvFilter;

use atlas_keymap::{ActionId, Dispatcher};
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
    seed_keymap_if_missing();

    let config = atlas_config::load().unwrap_or_default();
    let theme_id = config.ui.theme.clone();

    // Load default keymap and layer any user overrides from ~/.config/atlas/keymap.toml.
    let keymap = load_keymap_with_user_overrides();
    tracing::info!(layers = ?keymap.layers(), "keymap loaded");

    let window = AtlasWindow::new()?;
    // Force the initial window size: Slint's preferred-width is treated as a
    // hint the WM can ignore, so on macOS we sometimes open at min-width
    // (720). Explicitly set 1440x900 which is comfortable for dual-pane +
    // Miller. Users can resize freely afterwards; we don't auto-grow.
    window
        .window()
        .set_size(slint::PhysicalSize::new(1440, 900));
    let nav = NavigationController::with_config(&config);
    let search_ctrl = SearchController::new();

    // config: reads config.search.fuzzy_max_results
    search_ctrl.set_max_results(config.search.fuzzy_max_results);
    // config: reads config.search.default_globs_exclude
    search_ctrl.set_exclude_globs(config.search.default_globs_exclude.clone());
    // config: reads config.search.content_search_threads
    search_ctrl.set_content_search_threads(config.search.content_search_threads);

    // config: reads config.indexer.enabled — skip daemon launch when disabled.
    let index_client = if config.indexer.enabled {
        connect_or_launch_indexd(&search_ctrl, &config)
    } else {
        tracing::info!("indexer disabled by config (indexer.enabled = false)");
        None
    };

    // config: reads config.indexer.roots — after daemon connect, tell it to
    // watch every configured root so path-index search covers them without
    // requiring the user to seed the daemon separately.
    if let Some(client) = &index_client {
        if !config.indexer.roots.is_empty() {
            let client = Arc::clone(client);
            let roots = config.indexer.roots.clone();
            let handle = search_ctrl.runtime_handle();
            handle.spawn(async move {
                for root in roots {
                    match client.add_root(root.clone()).await {
                        Ok(()) => {
                            tracing::info!(?root, "indexer: added configured root");
                        }
                        Err(err) => {
                            tracing::warn!(?root, %err, "indexer: add_root failed");
                        }
                    }
                }
            });
        }
    }

    search_ctrl.set_index_client(index_client);
    search_ctrl.attach_window(window.as_weak());

    // config: reads config.thumbnails.generation_threads / cache_max_size_mb
    let thumb_worker_count = config.thumbnails.generation_threads.unwrap_or(0);
    // 0 means "use num_cpus" inside ThumbRequester
    let thumb_max_cache_bytes = (config.thumbnails.cache_max_size_mb as u64).max(1) * 1024 * 1024;
    // config: reads config.thumbnails.enabled + generate_for_size_up_to_mb
    let thumbs_enabled = config.thumbnails.enabled;
    let thumb_max_file_bytes = if config.thumbnails.generate_for_size_up_to_mb > 0 {
        (config.thumbnails.generate_for_size_up_to_mb as u64) * 1024 * 1024
    } else {
        0
    };

    let bookmark_pairs: Vec<(String, PathBuf)> = config
        .bookmarks
        .iter()
        .map(|b| (b.name.clone(), b.path.clone()))
        .collect();

    let shell: Arc<AppShell> = AppShell::new(
        &window,
        AtlasActionSink::new(Arc::clone(&nav)),
        Arc::clone(&nav),
        Arc::clone(&search_ctrl),
        thumb_worker_count,
        thumb_max_cache_bytes,
        thumbs_enabled,
        thumb_max_file_bytes,
        bookmark_pairs,
    );

    let loader = ThemeLoader::new();
    let (theme_watcher, themes_arc, theme_events) = ThemeWatcher::start(loader, &theme_id)
        .unwrap_or_else(|e| {
            tracing::warn!("cannot load theme {theme_id:?}: {e}; falling back to atlas-dark");
            ThemeWatcher::start(ThemeLoader::new(), "atlas-dark")
                .expect("built-in atlas-dark must always load")
        });

    // Push config-driven typography onto the theme so users can override
    // fonts + size without hand-authoring a full theme TOML.
    // config: reads config.ui.{font_family,font_size,monospace_font_family}
    {
        let mut overlaid = (**themes_arc.load()).clone();
        apply_font_overrides(&mut overlaid, &config.ui);
        apply_density_override(&mut overlaid, config.ui.density);
        shell.apply_theme(&overlaid);
    }
    // Wire chrome visibility from config.ui.
    shell.set_status_bar_visible(config.ui.show_status_bar);
    shell.set_breadcrumbs_visible(config.ui.show_breadcrumbs);
    // config: reads config.ui.animations
    shell.set_animations_enabled(config.ui.animations);
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
        spawn_config_event_thread(
            Arc::clone(&shell),
            arc,
            events,
            Arc::clone(&nav),
            Arc::clone(&search_ctrl),
            Arc::clone(&themes_arc),
        );
    }

    // If remember_last_location is enabled and a last_location was saved on a
    // previous quit, prefer it over start_path so the app re-opens where the
    // user left off.
    // config: reads config.navigation.{remember_last_location,last_location}
    let last_location = if config.navigation.remember_last_location {
        config_arc
            .as_ref()
            .and_then(|a| a.load().navigation.last_location.clone())
            .or(config.navigation.last_location.clone())
    } else {
        None
    };
    let start_path = last_location
        .or_else(|| {
            config_arc
                .as_ref()
                .and_then(|a| a.load().general.start_path.clone())
        })
        .or(config.general.start_path.clone())
        .unwrap_or_else(|| {
            env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/"))
        });
    search_ctrl.set_scope(Some(start_path.clone()));
    nav.navigate_pane(shell.focused_pane_id(), start_path);
    shell.set_status(StatusModel::default());
    shell.set_palette(PaletteModel::default());

    // Apply the config's default view mode to the focused (initial) pane.
    shell.set_view_mode(
        shell.focused_pane_id(),
        config_view_mode(config.view.default_mode),
    );

    // Push config-driven UI settings into the Slint window.
    shell.set_vim_mode(config.general.vim_mode); // config: reads config.general.vim_mode

    // TODO(config-sweep): ui.font_family / ui.font_size / ui.monospace_font_family —
    //   requires pushing new properties into the Theme Slint global. Tracked in
    //   gap-ui-fonts.

    // Open in dual-pane layout when the config asks for it (default: true).
    // The new pane inherits pane 0's location via AppShell::split_focused.
    if config.general.dual_pane {
        if let Some(new_id) = shell.split_focused(atlas_ui::SplitDirection::Horizontal) {
            shell.set_view_mode(new_id, config_view_mode(config.view.default_mode));
        }
    }

    // Build the keymap dispatcher and register handlers for common action IDs.
    // The palette on_dispatch callback routes through this dispatcher so that
    // palette-triggered actions use the same code paths as keyboard-triggered ones.
    // The Slint FocusScope → Dispatcher::handle_key wiring is tracked separately
    // in the `gap-keymap-slint-routing` follow-up todo.
    let dispatcher = build_dispatcher(keymap, &shell, &nav);

    // Wire palette confirm → dispatcher so picking an action from the palette
    // has the same effect as pressing its keyboard chord.
    {
        let d = Arc::clone(&dispatcher);
        shell
            .palette_controller()
            .set_on_dispatch(move |action_id| {
                d.dispatch_action(&ActionId::new(action_id));
            });
    }

    // ── Quit confirmation ────────────────────────────────────────────────
    // When config.general.confirm_on_quit = true, intercept OS close events
    // and show an in-app modal. `quit_confirmed` flips to true once the user
    // clicks "Quit" in the modal, which lets the second close_requested slip
    // straight through to HideWindow.
    // config: reads config.general.confirm_on_quit
    let quit_confirmed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let cfg_arc = config_arc.clone();
        let default_confirm = config.general.confirm_on_quit;
        let win_weak = window.as_weak();
        let quit_confirmed = Arc::clone(&quit_confirmed);
        window.window().on_close_requested(move || {
            let confirm = cfg_arc
                .as_ref()
                .map(|a| a.load().general.confirm_on_quit)
                .unwrap_or(default_confirm);
            if !confirm || quit_confirmed.load(std::sync::atomic::Ordering::Relaxed) {
                slint::CloseRequestResponse::HideWindow
            } else if let Some(w) = win_weak.upgrade() {
                w.set_confirm_quit_visible(true);
                slint::CloseRequestResponse::KeepWindowShown
            } else {
                slint::CloseRequestResponse::HideWindow
            }
        });
    }
    {
        let win_weak = window.as_weak();
        let quit_confirmed = Arc::clone(&quit_confirmed);
        window.on_confirm_quit_accept(move || {
            quit_confirmed.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(w) = win_weak.upgrade() {
                w.set_confirm_quit_visible(false);
                let _ = w.hide();
            }
        });
    }
    {
        let win_weak = window.as_weak();
        window.on_confirm_quit_cancel(move || {
            if let Some(w) = win_weak.upgrade() {
                w.set_confirm_quit_visible(false);
            }
        });
    }

    window.run()?;
    if let Some(w) = config_watcher {
        w.stop();
    }

    // On quit, persist the focused pane's active-tab location so the next
    // launch re-opens the same directory when navigation.remember_last_location
    // is true. We re-load the config from disk to preserve any concurrent edits
    // the user may have made while Atlas was running.
    // config: writes config.navigation.last_location
    match atlas_config::load() {
        Ok(mut latest) => {
            if latest.navigation.remember_last_location {
                if let Some(loc) = shell.pane_location(shell.focused_pane_id()) {
                    latest.navigation.last_location = Some(loc.clone());
                    if let Err(err) = atlas_config::save(&latest) {
                        tracing::warn!(%err, "could not persist last_location on quit");
                    } else {
                        tracing::info!(?loc, "persisted last_location on quit");
                    }
                }
            }
        }
        Err(err) => {
            tracing::warn!(%err, "could not re-load config to persist last_location");
        }
    }

    // Keep dispatcher alive until the event loop exits.
    drop(dispatcher);
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

/// Write the default keymap to the platform keymap path when the file does
/// not exist yet.  Logs but does not fail if writing is impossible.
///
/// Mirrors [`seed_config_if_missing`].  The keymap path resolves to
/// `~/.config/atlas/keymaps/default.toml` (or `$XDG_CONFIG_HOME/atlas/…` /
/// `%APPDATA%\Atlas\keymaps\default.toml`).
fn seed_keymap_if_missing() {
    let path = match atlas_config::keymap_file_path() {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(%err, "could not resolve keymap file path");
            return;
        }
    };
    if path.exists() {
        return;
    }
    if let Err(err) = atlas_keymap::write_default_keymap_to(&path) {
        tracing::warn!(%err, path = %path.display(), "could not seed default keymap");
        return;
    }
    tracing::info!(path = %path.display(), "seeded default keymap");
}

/// Convert `atlas_config::ViewMode` into the UI-side `atlas_ui::models::ViewMode`.
fn config_view_mode(m: atlas_config::ViewMode) -> atlas_ui::models::ViewMode {
    match m {
        atlas_config::ViewMode::Details => atlas_ui::models::ViewMode::Details,
        atlas_config::ViewMode::Grid => atlas_ui::models::ViewMode::Grid,
        atlas_config::ViewMode::Gallery => atlas_ui::models::ViewMode::Gallery,
        atlas_config::ViewMode::Miller => atlas_ui::models::ViewMode::Miller,
        atlas_config::ViewMode::Tree => atlas_ui::models::ViewMode::Tree,
    }
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
///
/// The `config` is used to log indexer settings; actual `indexer.roots` wiring
/// requires `IndexClient::add_root` which is not yet implemented — tracked in
/// `gap-indexer-add-roots`.
fn connect_or_launch_indexd(
    search_ctrl: &Arc<SearchController>,
    _config: &atlas_config::Config,
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
            tracing::warn!(
                "atlas-indexd binary not found next to atlas-app; search index disabled"
            );
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
/// user settings:
///
/// - **Theme changed** — loads the new theme via a fresh [`ThemeLoader`],
///   stores it in `themes_arc`, and calls `shell.apply_theme`. The file-watcher
///   for the new theme file is updated automatically because `ThemeWatcher` polls
///   the user themes dir on every file event.
/// - **`start_path` changed** — updates the search scope and, when
///   `navigation.remember_last_location` is `false`, navigates pane 0 to the
///   new start path.
fn spawn_config_event_thread(
    shell: Arc<AppShell>,
    config_arc: Arc<ArcSwap<atlas_config::Config>>,
    events: crossbeam_channel::Receiver<atlas_config::ConfigEvent>,
    nav: Arc<NavigationController>,
    search_ctrl: Arc<SearchController>,
    themes_arc: Arc<ArcSwap<ThemeTokens>>,
) {
    std::thread::Builder::new()
        .name(String::from("atlas-config-events"))
        .spawn(move || {
            let theme_loader = ThemeLoader::new();
            // Capture the initial values so we can detect changes.
            let initial = config_arc.load();
            let mut last_theme = initial.ui.theme.clone();
            let mut last_start = initial.general.start_path.clone();
            drop(initial);

            for event in events {
                match event {
                    atlas_config::ConfigEvent::Reloaded => {
                        tracing::info!("config reloaded from disk");
                        let cfg = config_arc.load();

                        // ── Theme ─────────────────────────────────────────
                        if cfg.ui.theme != last_theme {
                            match theme_loader.load(&cfg.ui.theme) {
                                Ok(tokens) => {
                                    last_theme = cfg.ui.theme.clone();
                                    themes_arc.store(Arc::new(tokens));
                                    let mut overlaid =
                                        (**themes_arc.load()).clone();
                                    apply_font_overrides(&mut overlaid, &cfg.ui);
                                    apply_density_override(&mut overlaid, cfg.ui.density);
                                    shell.apply_theme(&overlaid);
                                    tracing::info!(theme = %cfg.ui.theme, "config reload: theme updated");
                                }
                                Err(err) => {
                                    tracing::warn!(
                                        %err,
                                        theme = %cfg.ui.theme,
                                        "config reload: cannot load new theme; keeping previous"
                                    );
                                }
                            }
                        } else {
                            // Theme id unchanged but font/size/density may have
                            // changed; re-apply overlays so users see updates.
                            let mut overlaid = (**themes_arc.load()).clone();
                            apply_font_overrides(&mut overlaid, &cfg.ui);
                            apply_density_override(&mut overlaid, cfg.ui.density);
                            shell.apply_theme(&overlaid);
                        }

                        // ── Start path / search scope ─────────────────────
                        let new_start = cfg.general.start_path.clone();
                        if new_start != last_start {
                            last_start = new_start.clone();
                            let scope = new_start.or_else(|| {
                                env::var("HOME").ok().map(PathBuf::from)
                            });
                            search_ctrl.set_scope(scope.clone());
                            if !cfg.navigation.remember_last_location {
                                if let Some(path) = scope {
                                    tracing::info!(
                                        ?path,
                                        "config reload: navigating pane 0 to new start_path"
                                    );
                                    nav.navigate_pane(shell.focused_pane_id(), path);
                                }
                            }
                        }

                        // ── Chrome visibility ─────────────────────────────
                        shell.set_status_bar_visible(cfg.ui.show_status_bar);
                        shell.set_breadcrumbs_visible(cfg.ui.show_breadcrumbs);
                        shell.set_animations_enabled(cfg.ui.animations);

                        // ── Search knobs ──────────────────────────────────
                        search_ctrl.set_max_results(cfg.search.fuzzy_max_results);
                        search_ctrl.set_exclude_globs(cfg.search.default_globs_exclude.clone());
                        search_ctrl.set_content_search_threads(cfg.search.content_search_threads);
                    }
                    atlas_config::ConfigEvent::LoadError(msg) => {
                        tracing::warn!(msg, "config file has errors; keeping previous values");
                    }
                }
            }
        })
        .expect("failed to spawn atlas-config-events thread");
}

/// Build a [`Dispatcher`] wrapping `keymap` and register handlers for the
/// action IDs that are unconditionally wired at startup.
///
/// Handlers that require Slint-side key routing (all chord-triggered bindings)
/// are registered here now so that palette-driven dispatch works immediately.
/// The `FocusScope` → [`Dispatcher::handle_key`] wiring is tracked separately
/// in `gap-keymap-slint-routing`.
fn build_dispatcher(
    keymap: atlas_keymap::Keymap,
    shell: &Arc<AppShell>,
    _nav: &Arc<NavigationController>,
) -> Arc<Dispatcher> {
    let d = Dispatcher::new(keymap);

    // ── Command palette ───────────────────────────────────────────────────
    {
        let palette = shell.palette_controller();
        let p2 = Arc::clone(&palette);
        d.register("command_palette::Toggle", move || {
            if palette.is_visible() {
                palette.close();
            } else {
                palette.open(0);
            }
        });
        d.register("goto::Anything", move || {
            p2.open(1);
        });
    }

    // ── Pane cursor movement ──────────────────────────────────────────────
    {
        let s = Arc::clone(shell);
        d.register("pane::MoveDown", move || {
            let id = s.focused_pane_id();
            if let Some(ctrl) = s.pane_by_id(id) {
                ctrl.details.move_focus(1);
            }
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("pane::MoveUp", move || {
            let id = s.focused_pane_id();
            if let Some(ctrl) = s.pane_by_id(id) {
                ctrl.details.move_focus(-1);
            }
        });
    }

    // ── Pane history / directory navigation ──────────────────────────────
    {
        let s = Arc::clone(shell);
        d.register("pane::GoUp", move || {
            s.go_up(s.focused_pane_id());
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("pane::Back", move || {
            s.back_focused();
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("pane::Forward", move || {
            s.forward_focused();
        });
    }

    // ── View mode switching (Cmd+Alt+1..5 by default) ────────────────────
    for (id, mode) in [
        ("view::Details", atlas_ui::models::ViewMode::Details),
        ("view::Grid", atlas_ui::models::ViewMode::Grid),
        ("view::Gallery", atlas_ui::models::ViewMode::Gallery),
        ("view::Miller", atlas_ui::models::ViewMode::Miller),
        ("view::Tree", atlas_ui::models::ViewMode::Tree),
    ] {
        let s = Arc::clone(shell);
        d.register(id, move || {
            s.set_view_mode(s.focused_pane_id(), mode);
        });
    }

    // ── Pane split / close ───────────────────────────────────────────────
    {
        let s = Arc::clone(shell);
        d.register("pane::SplitRight", move || {
            s.split_focused(atlas_ui::SplitDirection::Horizontal);
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("pane::SplitDown", move || {
            s.split_focused(atlas_ui::SplitDirection::Vertical);
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("pane::Close", move || {
            s.close_focused_pane();
        });
    }

    // ── Pane focus (directional, geometry-aware) ─────────────────────────
    {
        let s = Arc::clone(shell);
        d.register("pane::FocusLeft", move || {
            s.focus_direction(atlas_ui::Cardinal::Left);
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("pane::FocusRight", move || {
            s.focus_direction(atlas_ui::Cardinal::Right);
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("pane::FocusUp", move || {
            s.focus_direction(atlas_ui::Cardinal::Up);
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("pane::FocusDown", move || {
            s.focus_direction(atlas_ui::Cardinal::Down);
        });
    }

    // ── View cycle (Cmd+Shift+E) ──────────────────────────────────────────
    {
        let s = Arc::clone(shell);
        d.register("view::Cycle", move || {
            s.cycle_view_mode();
        });
    }

    // ── Tab cycle (Cmd+Shift+[ / Cmd+Shift+]) ─────────────────────────────
    {
        let s = Arc::clone(shell);
        d.register("tab::CyclePrev", move || {
            s.cycle_tab(s.focused_pane_id(), -1);
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("tab::CycleNext", move || {
            s.cycle_tab(s.focused_pane_id(), 1);
        });
    }

    // ── Reopen closed tab ─────────────────────────────────────────────────
    {
        // TODO(v0.3): needs a closed-tabs stack on AppShell.
        d.register("tab::Reopen", || {
            tracing::warn!("tab::Reopen: not yet implemented (requires closed-tabs stack — v0.3)");
        });
    }

    tracing::info!(handlers = d.handler_count(), "keymap dispatcher ready");
    d
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

/// Overlay user-configured typography onto the resolved theme tokens.
///
/// Empty strings and non-positive sizes are treated as "unset" and leave the
/// theme's own defaults in place; any populated field wins. This gives users
/// a clean escape hatch for fonts without maintaining a full custom theme
/// TOML. Applied both at startup and whenever `config.toml` hot-reloads.
///
/// config: reads config.ui.{font_family, monospace_font_family, font_size}
fn apply_font_overrides(tokens: &mut ThemeTokens, ui: &atlas_config::Ui) {
    if !ui.font_family.trim().is_empty() {
        tokens.typography.font_family = ui.font_family.clone();
    }
    if !ui.monospace_font_family.trim().is_empty() {
        tokens.typography.monospace_family = ui.monospace_font_family.clone();
    }
    if ui.font_size > 0.0 && ui.font_size.is_finite() {
        tokens.typography.font_size_pt = ui.font_size;
    }
}

/// Overlay the configured `ui.density` onto the theme's row-height token that
/// the file-list views actually read (`row_h_default_px`). `Compact` and
/// `Spacious` copy from the corresponding tokens; `Comfortable` keeps whatever
/// the theme already provides.
///
/// config: reads config.ui.density
fn apply_density_override(tokens: &mut ThemeTokens, density: atlas_config::Density) {
    match density {
        atlas_config::Density::Compact => {
            tokens.chrome.row_h_default_px = tokens.chrome.row_h_compact_px;
        }
        atlas_config::Density::Spacious => {
            tokens.chrome.row_h_default_px = tokens.chrome.row_h_spacious_px;
        }
        atlas_config::Density::Comfortable => {}
    }
}
