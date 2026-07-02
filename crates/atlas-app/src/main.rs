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

use atlas_keymap::{ActionId, Chord, Dispatcher, Key, Modifiers, NamedKey, PrettyPlatform};
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

    // Wire remote pool + retry policy from config so the process-wide
    // singletons pick up the user's tuning before any pane opens.
    apply_remote_config(&config.remote);

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
    // config: reads config.search.max_visible_results
    search_ctrl.set_max_visible_results(config.search.max_visible_results);
    // config: reads config.search.min_query_length
    search_ctrl.set_min_query_length(config.search.min_query_length);
    // config: reads config.search.debounce_ms
    search_ctrl.set_debounce_ms(config.search.debounce_ms);
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
    shell.set_shortcut_footer_visible(config.ui.show_shortcuts);
    // config: reads config.ui.animations
    shell.set_animations_enabled(config.ui.animations);
    // config: reads config.ui.active_pane_border_px
    shell.set_active_pane_border_px(config.ui.active_pane_border_px);
    // config: reads config.remote.preview (cache_dir, max_bytes,
    // max_age_secs, max_open_bytes) so remote-file previews land in
    // the user-configured cache.
    shell.set_remote_preview_config(config.remote.preview.clone());
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

    // Seed Details-view column widths from `[view.details.column_widths]`
    // so the user's dragged-column preferences survive restarts.
    // config: reads config.view.details.column_widths
    shell.apply_column_widths(&config.view.details.column_widths);

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
    // Both palette confirms AND every Slint chord press route through this
    // dispatcher so the keymap TOML is the single source of truth for
    // keyboard shortcuts.
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

    // ── Route Slint key events through the dispatcher ────────────────────
    //
    // Every non-plain key press bubbles through `handle-key-chord` on the
    // Slint root; we normalise the platform-native modifier bools to
    // physical semantics, build an atlas-keymap `ChordSequence`, and
    // dispatch under the {Global, Pane} context stack. Handlers registered
    // in `build_dispatcher` do the actual work.
    //
    // `modal_active` is Slint's local union of modal-visibility flags
    // (palette / goto / search / bulk rename / ops progress). When a modal
    // is up we restrict the dispatch context to `[Global]` only — Pane
    // bindings (Cmd+A → pane::SelectAll, Cmd+C → fs::Copy, arrows →
    // pane::MoveDown, …) return false, the callback returns false, and
    // the key falls through to the modal's TextInput natively. This is
    // the modal-focus fix that keeps Cmd+A selecting the query text
    // rather than the pane's visible entries.
    {
        let dispatcher = Arc::clone(&dispatcher);
        shell.install_key_dispatcher(move |key, ctrl, alt, shift, cmd, modal_active| {
            let Some(seq) = build_sequence_from_slint(&key, ctrl, alt, shift, cmd) else {
                return false;
            };
            let contexts: Vec<String> = if modal_active {
                vec![String::from("Global")]
            } else {
                vec![String::from("Global"), String::from("Pane")]
            };
            let hit = dispatcher.handle_key(&seq, &contexts);
            if hit {
                tracing::debug!(chord = %seq, modal_active, "keymap: dispatched");
            }
            hit
        });
    }

    // Populate the bottom shortcut footer from the LIVE keymap. Data-driven
    // so user rebindings appear automatically; platform-native symbols so
    // Mac users see `⌘⇧P` and Windows users see `Ctrl+Shift+P` for the
    // same binding.
    refresh_shortcut_footer(&shell, &dispatcher);

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
    //         + config.view.details.column_widths (when the user resized any)
    match atlas_config::load() {
        Ok(mut latest) => {
            let mut needs_save = false;
            if latest.navigation.remember_last_location {
                if let Some(loc) = shell.pane_location(shell.focused_pane_id()) {
                    latest.navigation.last_location = Some(loc.clone());
                    needs_save = true;
                    tracing::info!(?loc, "persisted last_location on quit");
                }
            }
            if let Some(widths) = shell.column_widths_snapshot() {
                latest.view.details.column_widths = widths;
                needs_save = true;
                tracing::info!("persisted view.details.column_widths on quit");
            }
            if needs_save {
                if let Err(err) = atlas_config::save(&latest) {
                    tracing::warn!(%err, "could not persist config on quit");
                }
            }
        }
        Err(err) => {
            tracing::warn!(%err, "could not re-load config to persist state");
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

/// Push the user's remote-tuning knobs (pool + retry) into the
/// process-wide singletons owned by `atlas_remote`.
fn apply_remote_config(remote: &atlas_config::Remote) {
    let pool = atlas_remote::pool::global();
    pool.set_config(atlas_remote::PoolConfig {
        idle_ttl: std::time::Duration::from_millis(u64::from(remote.pool.idle_ttl_ms)),
        max_connections: remote.pool.max_connections.max(1) as usize,
    });
    // For now every scheme uses the same default; the per-scheme
    // overrides on `remote.timeout_ms` / `remote.retries` are read by
    // future per-VM policy customisation but the process-wide baseline
    // is a single struct.
    let policy = atlas_remote::RetryPolicy {
        timeout: std::time::Duration::from_millis(u64::from(remote.default_timeout_ms)),
        retries: remote.default_retries,
        backoff_initial: std::time::Duration::from_millis(u64::from(remote.backoff_initial_ms)),
        backoff_max: std::time::Duration::from_millis(u64::from(remote.backoff_max_ms)),
        backoff_multiplier: remote.backoff_multiplier,
    };
    atlas_remote::set_default_retry_policy(policy);
    tracing::info!(
        idle_ttl_ms = remote.pool.idle_ttl_ms,
        max_connections = remote.pool.max_connections,
        default_timeout_ms = remote.default_timeout_ms,
        default_retries = remote.default_retries,
        "wired remote pool + retry policy from config"
    );
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
                        shell.set_shortcut_footer_visible(cfg.ui.show_shortcuts);
                        shell.set_animations_enabled(cfg.ui.animations);
                        shell.set_active_pane_border_px(cfg.ui.active_pane_border_px);
                        shell.set_vim_mode(cfg.general.vim_mode);

                        // ── Search knobs ──────────────────────────────────
                        search_ctrl.set_max_results(cfg.search.fuzzy_max_results);
                        search_ctrl.set_max_visible_results(cfg.search.max_visible_results);
                        search_ctrl.set_min_query_length(cfg.search.min_query_length);
                        search_ctrl.set_debounce_ms(cfg.search.debounce_ms);
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
            // Merge saved-server entries into the goto palette so a single
            // Cmd+P search hits both local paths (source index 1) and remote
            // mounts (source index 3). Source indices come from
            // `atlas_ui::shell::build_palette_controller`.
            p2.open_multi(&[1, 3]);
        });
    }

    // ── Pane cursor movement ──────────────────────────────────────────────
    // ── File-list navigation (arrow keys + vim hjkl) ─────────────────────
    //
    // Route by view mode so hjkl / arrows work in every view (details,
    // grid, gallery, miller, tree). Nav is **focus-only** in every view:
    // moving the cursor updates the focused-index but leaves the existing
    // selection alone. This is the terminal-file-manager convention
    // (yazi / nnn / ranger / Total Commander) and lets the user build a
    // multi-selection by arrow-navigating + pressing Space per row.
    //
    // Left-click still single-selects (Finder/Explorer parity), Shift+arrow
    // extends the range from the anchor, and Cmd+A selects everything.
    {
        let s = Arc::clone(shell);
        d.register("pane::MoveDown", move || {
            let id = s.focused_pane_id();
            let mode = s.pane_view_mode(id);
            let Some(ctrl) = s.pane_by_id(id) else { return };
            match mode {
                atlas_ui::models::ViewMode::Details => ctrl.details.move_focus(1_i64),
                atlas_ui::models::ViewMode::Grid => ctrl.grid.move_focus(1_isize, 0),
                atlas_ui::models::ViewMode::Gallery => ctrl.gallery.move_focus(1_isize),
                atlas_ui::models::ViewMode::Tree => ctrl.tree.move_focus(1_isize),
                atlas_ui::models::ViewMode::Miller => ctrl.miller.move_focus(1_isize),
            }
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("pane::MoveUp", move || {
            let id = s.focused_pane_id();
            let mode = s.pane_view_mode(id);
            let Some(ctrl) = s.pane_by_id(id) else { return };
            match mode {
                atlas_ui::models::ViewMode::Details => ctrl.details.move_focus(-1_i64),
                atlas_ui::models::ViewMode::Grid => ctrl.grid.move_focus(-1_isize, 0),
                atlas_ui::models::ViewMode::Gallery => ctrl.gallery.move_focus(-1_isize),
                atlas_ui::models::ViewMode::Tree => ctrl.tree.move_focus(-1_isize),
                atlas_ui::models::ViewMode::Miller => ctrl.miller.move_focus(-1_isize),
            }
        });
    }
    // Shift+Arrow / Shift+j / Shift+k extends the range selection in
    // Details and Grid; degrades to plain move_focus for the
    // single-focus views (Gallery/Tree/Miller) where range extend has
    // no obvious meaning.
    {
        let s = Arc::clone(shell);
        d.register("pane::ExtendDown", move || {
            let id = s.focused_pane_id();
            let mode = s.pane_view_mode(id);
            let Some(ctrl) = s.pane_by_id(id) else { return };
            match mode {
                atlas_ui::models::ViewMode::Details => ctrl.details.extend_selection(1_i64),
                atlas_ui::models::ViewMode::Grid => ctrl.grid.extend_selection(1_isize, 0),
                atlas_ui::models::ViewMode::Gallery => ctrl.gallery.move_focus(1_isize),
                atlas_ui::models::ViewMode::Tree => ctrl.tree.move_focus(1_isize),
                atlas_ui::models::ViewMode::Miller => ctrl.miller.move_focus(1_isize),
            }
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("pane::ExtendUp", move || {
            let id = s.focused_pane_id();
            let mode = s.pane_view_mode(id);
            let Some(ctrl) = s.pane_by_id(id) else { return };
            match mode {
                atlas_ui::models::ViewMode::Details => ctrl.details.extend_selection(-1_i64),
                atlas_ui::models::ViewMode::Grid => ctrl.grid.extend_selection(-1_isize, 0),
                atlas_ui::models::ViewMode::Gallery => ctrl.gallery.move_focus(-1_isize),
                atlas_ui::models::ViewMode::Tree => ctrl.tree.move_focus(-1_isize),
                atlas_ui::models::ViewMode::Miller => ctrl.miller.move_focus(-1_isize),
            }
        });
    }
    // ── Multi-select actions (Space toggle, Cmd+A select all) ────────────
    //
    // Space = mark/unmark the focused entry, matching the "mark" idiom
    // from Total Commander / nnn / ranger / yazi. Cmd+A / Ctrl+A selects
    // every entry in the pane's directory; Shift+Cmd+A clears the
    // selection. These are only defined for Details/Grid — the
    // single-focus views (Gallery/Tree/Miller) have no concept of a
    // multi-selection.
    {
        let s = Arc::clone(shell);
        d.register("pane::ToggleSelection", move || {
            let id = s.focused_pane_id();
            let mode = s.pane_view_mode(id);
            let Some(ctrl) = s.pane_by_id(id) else { return };
            match mode {
                atlas_ui::models::ViewMode::Details => ctrl.details.toggle_focused(),
                atlas_ui::models::ViewMode::Grid => ctrl.grid.toggle_focused(),
                _ => {
                    tracing::debug!(?mode, "pane::ToggleSelection: not supported in this view");
                }
            }
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("pane::SelectAll", move || {
            let id = s.focused_pane_id();
            let mode = s.pane_view_mode(id);
            let Some(ctrl) = s.pane_by_id(id) else { return };
            match mode {
                atlas_ui::models::ViewMode::Details => ctrl.details.select_all(),
                atlas_ui::models::ViewMode::Grid => ctrl.grid.select_all(),
                _ => {
                    tracing::debug!(?mode, "pane::SelectAll: not supported in this view");
                }
            }
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("pane::DeselectAll", move || {
            let id = s.focused_pane_id();
            let mode = s.pane_view_mode(id);
            let Some(ctrl) = s.pane_by_id(id) else { return };
            match mode {
                atlas_ui::models::ViewMode::Details => ctrl.details.deselect_all(),
                atlas_ui::models::ViewMode::Grid => ctrl.grid.deselect_all(),
                _ => {
                    tracing::debug!(?mode, "pane::DeselectAll: not supported in this view");
                }
            }
        });
    }
    // ── Vim g g / shift-g (jump to top / bottom of the list) ─────────────
    {
        let s = Arc::clone(shell);
        d.register("pane::MoveToTop", move || {
            let id = s.focused_pane_id();
            let mode = s.pane_view_mode(id);
            let Some(ctrl) = s.pane_by_id(id) else { return };
            // Passing i64::MIN / isize::MIN makes clamp saturate at 0.
            match mode {
                atlas_ui::models::ViewMode::Details => ctrl.details.move_focus(i64::MIN),
                atlas_ui::models::ViewMode::Grid => ctrl.grid.move_focus(isize::MIN, 0),
                atlas_ui::models::ViewMode::Gallery => ctrl.gallery.move_focus(isize::MIN),
                atlas_ui::models::ViewMode::Tree => ctrl.tree.move_focus(isize::MIN),
                atlas_ui::models::ViewMode::Miller => ctrl.miller.move_focus(isize::MIN),
            }
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("pane::MoveToBottom", move || {
            let id = s.focused_pane_id();
            let mode = s.pane_view_mode(id);
            let Some(ctrl) = s.pane_by_id(id) else { return };
            match mode {
                atlas_ui::models::ViewMode::Details => ctrl.details.move_focus(i64::MAX),
                atlas_ui::models::ViewMode::Grid => ctrl.grid.move_focus(isize::MAX, 0),
                atlas_ui::models::ViewMode::Gallery => ctrl.gallery.move_focus(isize::MAX),
                atlas_ui::models::ViewMode::Tree => ctrl.tree.move_focus(isize::MAX),
                atlas_ui::models::ViewMode::Miller => ctrl.miller.move_focus(isize::MAX),
            }
        });
    }
    // ── Search-in-place (vim `/` — deferred; today is a no-op stub) ──────
    {
        d.register("pane::SearchInPlace", move || {
            tracing::info!(
                "pane::SearchInPlace: incremental in-list filter not implemented yet (v0.3); use Cmd+F / search::Toggle for now"
            );
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

    // ── App-level actions ─────────────────────────────────────────────────
    {
        let s = Arc::clone(shell);
        d.register("app::Quit", move || {
            tracing::info!("app::Quit — hiding window");
            if let Some(w) = s.window_weak().upgrade() {
                let _ = w.hide();
            }
        });
        d.register("app::OpenSettings", || {
            tracing::info!(
                "app::OpenSettings: no in-app settings UI yet — edit ~/.config/atlas/config.toml"
            );
        });
    }

    // ── File-list navigation (arrow keys + vim hjkl) ──────────────────────
    //
    // fs::View is the ONE "open" action: cd into folders, hand files off
    // to the OS default application (Preview.app for images, VS Code for
    // source files, …). Any number of keybinds may bind to it (Enter, l,
    // →, dblclick, context-menu Open); actions have no aliases — only
    // keybinds do.
    {
        let s = Arc::clone(shell);
        d.register("fs::View", move || {
            s.view_focused_entry(s.focused_pane_id());
        });
    }

    // ── Tab commands (Cmd+T new, Cmd+W close, Cmd+1..9 select) ───────────
    {
        let s = Arc::clone(shell);
        d.register("tab::New", move || {
            s.new_tab(s.focused_pane_id());
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("tab::Close", move || {
            let id = s.focused_pane_id();
            let active = s.active_tab_index(id).unwrap_or(0);
            s.close_tab(id, active);
        });
    }
    for (id, index) in [
        ("tab::Select1", 0_usize),
        ("tab::Select2", 1),
        ("tab::Select3", 2),
        ("tab::Select4", 3),
        ("tab::Select5", 4),
        ("tab::Select6", 5),
        ("tab::Select7", 6),
        ("tab::Select8", 7),
        ("tab::Select9", 8),
    ] {
        let s = Arc::clone(shell);
        d.register(id, move || {
            s.select_tab(s.focused_pane_id(), index);
        });
    }

    // ── File-system actions ─────────────────────────────────────────────
    //
    // Only clipboard-based Copy / Cut / Paste — the old F5/F6 pane-to-pane
    // shortcuts were dropped because they don't scale to N-pane workspaces
    // (which pane is the destination when there are 3+?). Clipboard flow
    // works identically in 1, 2, or 20 panes.
    let win_weak = shell.window_weak();
    {
        let s = Arc::clone(shell);
        d.register("fs::Copy", move || {
            let locs = s.selected_locations(s.focused_pane_id());
            s.clipboard().copy(locs);
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("fs::Cut", move || {
            let locs = s.selected_locations(s.focused_pane_id());
            s.clipboard().cut(locs);
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("fs::Paste", move || {
            let focused = s.focused_pane_id();
            let Some(dest) = s.pane_location_full(focused) else {
                tracing::warn!(?focused, "fs::Paste: no pane location");
                return;
            };
            s.clipboard().paste(dest);
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("fs::Duplicate", move || {
            // Duplicate every currently-selected entry (or the focused
            // entry if nothing is explicitly selected). Uses the ops
            // queue's RenameWithSuffix policy to append " (copy)".
            let id = s.focused_pane_id();
            let mut paths = s.selected_paths(id);
            if paths.is_empty() {
                if let Some(p) = s.focused_entry(id) {
                    paths.push(p);
                }
            }
            s.duplicate_paths(paths);
        });
    }
    {
        let w = win_weak.clone();
        d.register("fs::Delete", move || {
            if let Some(win) = w.upgrade() {
                win.invoke_fs_delete();
            }
        });
    }
    {
        let w = win_weak.clone();
        d.register("fs::Rename", move || {
            if let Some(win) = w.upgrade() {
                win.invoke_fs_rename();
            }
        });
    }
    {
        let w = win_weak.clone();
        d.register("fs::Mkdir", move || {
            if let Some(win) = w.upgrade() {
                win.invoke_fs_mkdir();
            }
        });
    }

    // ── Search / ops / bulk-rename toggles ────────────────────────────────
    {
        let search = shell.search();
        let search2 = Arc::clone(&search);
        d.register("search::Toggle", move || {
            if search.is_open() {
                search.close();
            } else {
                search.open();
            }
        });
        d.register("search::Open", move || {
            search2.open();
        });
    }
    {
        let ops = shell.ops();
        d.register("ops::TogglePanel", move || {
            ops.toggle_visible();
        });
    }
    {
        let s = Arc::clone(shell);
        d.register("rename::OpenBulk", move || {
            let focused = s.focused_pane_id();
            let paths = s.selected_paths(focused);
            s.bulk_rename().open(paths);
        });
    }

    // ── Dual-pane toggle (Cmd+\) ──────────────────────────────────────────
    {
        let s = Arc::clone(shell);
        d.register("workspace::ToggleDualPane", move || {
            let leaves = s.pane_id_for_index(1).is_some();
            if leaves {
                if let Some(id1) = s.pane_id_for_index(1) {
                    s.set_focused_pane_id(id1);
                    s.close_focused_pane();
                }
            } else {
                s.split_focused(atlas_ui::SplitDirection::Horizontal);
            }
        });
    }

    // ── Remote / connect (Cmd+K) — stub, real modal lands in phase 2.2 ────
    {
        let s = Arc::clone(shell);
        d.register("remote::Connect", move || {
            let focused = s.focused_pane_id();
            s.open_connect_modal(focused);
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
/// The user's font choice is *prepended* to the theme's own font-family
/// stack rather than replacing it, so glyphs the user's font can't render
/// (emoji, arrows, tab-close ✕) still resolve via the built-in fallback
/// chain instead of showing as tofu boxes.
///
/// Empty strings and non-positive sizes are treated as "unset" and leave
/// the theme's own defaults in place. Applied both at startup and whenever
/// `config.toml` hot-reloads.
///
/// config: reads config.ui.{font_family, monospace_font_family, font_size}
fn apply_font_overrides(tokens: &mut ThemeTokens, ui: &atlas_config::Ui) {
    if !ui.font_family.trim().is_empty() {
        tokens.typography.font_family =
            prepend_font(&ui.font_family, &tokens.typography.font_family);
    }
    if !ui.monospace_font_family.trim().is_empty() {
        tokens.typography.monospace_family = prepend_font(
            &ui.monospace_font_family,
            &tokens.typography.monospace_family,
        );
    }
    if ui.font_size > 0.0 && ui.font_size.is_finite() {
        tokens.typography.font_size_pt = ui.font_size;
    }
}

/// Prepend `user_font` to the comma-separated `fallback_chain`, de-duplicating
/// case-insensitively so repeated overrides don't stack.
fn prepend_font(user_font: &str, fallback_chain: &str) -> String {
    let user = user_font.trim();
    let already_present = fallback_chain
        .split(',')
        .any(|f| f.trim().eq_ignore_ascii_case(user));
    if already_present {
        fallback_chain.to_owned()
    } else {
        format!("{user}, {fallback_chain}")
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

/// Translate a raw Slint key event into an atlas-keymap [`ChordSequence`].
///
/// Slint 1.17 delivers modifier state as an opaque `KeyboardModifiers`
/// struct whose `control` / `meta` fields are *swapped* on macOS as a
/// cross-platform-shortcut convenience. This function undoes that so the
/// keymap always sees physical semantics — a user binding `ctrl-h` in
/// `~/.config/atlas/keymaps/default.toml` gets the physical ⌃ key regardless
/// of platform, and `cmd-p` maps to ⌘ on macOS and Ctrl on Linux/Windows.
///
/// The `key` string is:
///   - a single character (letter, digit, punctuation) — mapped to [`Key::Char`],
///   - a Slint NamedKey escape (delivered as a special multi-byte string) —
///     mapped to the corresponding [`NamedKey`],
///   - or one of `"f1"`..`"f24"` — mapped to [`Key::Function`].
///
/// Returns `None` for keys that don't produce a meaningful chord (e.g. bare
/// modifier presses, unknown escape sequences).
fn build_sequence_from_slint(
    key_text: &str,
    ctrl_raw: bool,
    alt_raw: bool,
    shift: bool,
    cmd_raw: bool,
) -> Option<atlas_keymap::ChordSequence> {
    use atlas_keymap::ChordSequence;

    // Slint's KeyboardModifiers::control and ::meta are swapped on macOS
    // (see https://github.com/slint-ui/slint/issues/2011). Normalise both
    // to physical: `cmd` = the ⌘/Super key, `ctrl` = the ⌃ key.
    #[cfg(target_os = "macos")]
    let (ctrl, cmd) = (cmd_raw, ctrl_raw);
    #[cfg(not(target_os = "macos"))]
    let (ctrl, cmd) = (ctrl_raw, cmd_raw);

    let key = slint_key_text_to_keymap_key(key_text)?;
    let chord = Chord {
        modifiers: Modifiers {
            ctrl,
            alt: alt_raw,
            shift,
            cmd,
        },
        key,
    };
    let mut seq = ChordSequence::default();
    seq.0.push(chord);
    Some(seq)
}

/// Map a Slint `KeyEvent.text` payload to an atlas-keymap [`Key`].
///
/// Slint encodes named / function keys as characters in the Unicode Private
/// Use Area (see `i_slint_common::key_codes`). This function matches those
/// PUA codepoints directly rather than going through `SharedString`
/// comparisons.
///
/// C0 control codes (Ctrl+letter can arrive as 0x01..0x1a on macOS text
/// bindings) are folded to their letter equivalent.
fn slint_key_text_to_keymap_key(text: &str) -> Option<Key> {
    let mut chars = text.chars();
    let first = chars.next()?;
    if chars.next().is_some() {
        // Multi-character text: only PUA-range single chars are keys we care
        // about here; anything else (e.g. multi-byte input) is not a chord.
        return None;
    }
    match first {
        // ── C0 named keys ─────────────────────────────────────────────
        '\u{0008}' => Some(Key::Named(NamedKey::Backspace)),
        '\u{0009}' => Some(Key::Named(NamedKey::Tab)),
        '\u{000a}' => Some(Key::Named(NamedKey::Enter)),
        '\u{001b}' => Some(Key::Named(NamedKey::Escape)),
        '\u{007f}' => Some(Key::Named(NamedKey::Delete)),
        '\u{0020}' => Some(Key::Named(NamedKey::Space)),
        // ── Private-Use-Area named keys (arrows, F-keys, nav) ─────────
        '\u{F700}' => Some(Key::Named(NamedKey::Up)),
        '\u{F701}' => Some(Key::Named(NamedKey::Down)),
        '\u{F702}' => Some(Key::Named(NamedKey::Left)),
        '\u{F703}' => Some(Key::Named(NamedKey::Right)),
        c @ '\u{F704}'..='\u{F71B}' => {
            let n = (c as u32 - 0xF704 + 1) as u8;
            Some(Key::Function(n))
        }
        '\u{F727}' => Some(Key::Named(NamedKey::Insert)),
        '\u{F729}' => Some(Key::Named(NamedKey::Home)),
        '\u{F72B}' => Some(Key::Named(NamedKey::End)),
        '\u{F72C}' => Some(Key::Named(NamedKey::PageUp)),
        '\u{F72D}' => Some(Key::Named(NamedKey::PageDown)),
        // ── Bare modifier presses — ignore ─────────────────────────────
        '\u{0010}'..='\u{0018}' => None,
        // ── C0 control codes: fold Ctrl+letter (0x01..0x1a) → letter. ──
        c @ '\u{0001}'..='\u{001a}' => Some(Key::Char(char::from(c as u8 + b'a' - 1))),
        // ── Plain printable character ──────────────────────────────────
        c => Some(Key::Char(c.to_ascii_lowercase())),
    }
}

/// Actions we advertise in the bottom shortcut footer, in display order.
///
/// Curated by hand — not every registered action deserves a chip. Pairs are
/// `(action_id, short_label)`; the chord is looked up live from the keymap
/// so user rebindings appear immediately. Actions with no binding in the
/// current keymap are silently dropped.
const FOOTER_ACTIONS: &[(&str, &str)] = &[
    ("fs::Copy", "Copy"),
    ("fs::Cut", "Cut"),
    ("fs::Paste", "Paste"),
    ("fs::Rename", "Rename"),
    ("fs::Mkdir", "New Folder"),
    ("fs::Delete", "Trash"),
    ("goto::Anything", "Goto"),
    ("command_palette::Toggle", "Palette"),
    ("search::Toggle", "Search"),
];

/// Read the live keymap through the dispatcher, render each
/// [`FOOTER_ACTIONS`] entry with the platform's native modifier symbols,
/// and push the result to the Slint shortcut footer.
///
/// Called at startup and any time the keymap changes on disk. Silently
/// skips actions that have no chord bound in either the Global or Pane
/// context (so if a user unbinds `fs::Copy`, the chip just disappears).
fn refresh_shortcut_footer(shell: &Arc<AppShell>, dispatcher: &Arc<Dispatcher>) {
    let contexts = [String::from("Global"), String::from("Pane")];
    let platform = PrettyPlatform::current();
    let keymap = dispatcher.keymap();
    let hints: Vec<(String, String)> = FOOTER_ACTIONS
        .iter()
        .filter_map(|(id, label)| {
            let action = ActionId::new(*id);
            let seq = keymap.chord_for_action(&action, &contexts)?;
            Some((seq.display_pretty(platform), (*label).to_owned()))
        })
        .collect();
    drop(keymap);
    tracing::debug!(count = hints.len(), "shortcut footer: refreshed hints");
    shell.set_shortcut_hints(hints);
}
