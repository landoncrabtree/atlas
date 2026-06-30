//! Filesystem-watching hot reload for the Atlas configuration.
//!
//! [`ConfigWatcher::start`] spawns a background thread that uses
//! `notify-debouncer-full` to watch the config *directory* (more reliable
//! than watching a single file on macOS) and filters events to
//! `config.toml`.  On each change the config is reloaded, the
//! [`arc_swap::ArcSwap`] is updated atomically, and a [`ConfigEvent`] is
//! sent on the subscriber channel.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use notify_debouncer_full::new_debouncer;
use notify_debouncer_full::notify::RecursiveMode;

use atlas_core::Result;

use super::load::{load, load_from_file};
use super::paths::{config_file_path, ensure_config_dir};
use super::schema::Config;

// ── Public types ────────────────────────────────────────────────────────────

/// Events emitted to subscribers after each filesystem change.
#[derive(Debug, Clone)]
pub enum ConfigEvent {
    /// A new valid config was loaded and the shared value was updated.
    Reloaded,
    /// The config file changed but could not be parsed.
    ///
    /// The shared [`ArcSwap`] retains the last successfully loaded value.
    /// The inner [`String`] is the human-readable error message.
    LoadError(String),
}

/// Handle to the background configuration watcher.
///
/// Dropping this value (or calling [`stop`][ConfigWatcher::stop]) terminates
/// the watcher thread cleanly.
pub struct ConfigWatcher {
    stop_tx: crossbeam_channel::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ConfigWatcher {
    /// Start watching `config.toml`.
    ///
    /// Returns a tuple of:
    /// - `Self` — the watcher handle (drop or call [`stop`][Self::stop] to
    ///   shut down).
    /// - `Arc<ArcSwap<Config>>` — always points at the most recently loaded
    ///   valid config.
    /// - `Receiver<ConfigEvent>` — subscribe to reload/error notifications.
    pub fn start() -> Result<(
        Self,
        Arc<ArcSwap<Config>>,
        crossbeam_channel::Receiver<ConfigEvent>,
    )> {
        // Ensure the config directory exists so we can watch it.
        ensure_config_dir()?;

        // Load the initial config value (never fails — returns Default on error).
        let initial = load().unwrap_or_default();
        let shared: Arc<ArcSwap<Config>> = Arc::new(ArcSwap::from_pointee(initial));

        let (event_tx, event_rx) = crossbeam_channel::unbounded::<ConfigEvent>();
        let (stop_tx, stop_rx) = crossbeam_channel::bounded::<()>(1);

        let shared_clone = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("atlas-config-watcher".to_string())
            .spawn(move || watcher_thread(shared_clone, event_tx, stop_rx))
            .map_err(|e| anyhow::anyhow!("failed to spawn watcher thread: {e}"))?;

        Ok((
            Self {
                stop_tx,
                thread: Some(thread),
            },
            shared,
            event_rx,
        ))
    }

    /// Stop the watcher and wait for the background thread to finish.
    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        // Best-effort: the thread may have already exited.
        let _ = self.stop_tx.try_send(());
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

// ── Background thread ───────────────────────────────────────────────────────

fn watcher_thread(
    shared: Arc<ArcSwap<Config>>,
    event_tx: crossbeam_channel::Sender<ConfigEvent>,
    stop_rx: crossbeam_channel::Receiver<()>,
) {
    // Bridge notify callback → crossbeam channel.
    let (notify_tx, notify_rx) =
        crossbeam_channel::unbounded::<notify_debouncer_full::DebounceEventResult>();

    let mut debouncer = match new_debouncer(Duration::from_millis(300), None, move |result| {
        let _ = notify_tx.send(result);
    }) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("failed to create file-system debouncer: {e}");
            return;
        }
    };

    let config_dir = match super::paths::config_dir() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("cannot determine config directory for watching: {e}");
            return;
        }
    };

    if let Err(e) = debouncer.watch(&config_dir, RecursiveMode::NonRecursive) {
        tracing::error!(
            "failed to watch config directory {}: {e}",
            config_dir.display()
        );
        return;
    }

    let config_file = match config_file_path() {
        Ok(p) => {
            // Canonicalize so symlinks (e.g. /var → /private/var on macOS) don't
            // prevent matching against paths reported by the FSEvents backend.
            std::fs::canonicalize(&p).unwrap_or(p)
        }
        Err(e) => {
            tracing::error!("cannot determine config file path: {e}");
            return;
        }
    };

    // Cache just the file name for a fast, symlink-safe membership check.
    let config_filename = config_file
        .file_name()
        .map(std::ffi::OsString::from)
        .unwrap_or_default();

    loop {
        crossbeam_channel::select! {
            recv(stop_rx) -> _ => break,

            recv(notify_rx) -> msg => {
                let events = match msg {
                    Ok(Ok(evs)) => evs,
                    Ok(Err(errs)) => {
                        for e in errs {
                            tracing::warn!("file-watch error: {e:?}");
                        }
                        continue;
                    }
                    Err(_) => break, // notify channel closed
                };

                let relevant = events.iter().any(|de| {
                    de.event.paths.iter().any(|p| {
                        // Match by file name (symlink-safe) since we watch a
                        // specific non-recursive directory.
                        p.file_name().map(|n| n.to_os_string()) == Some(config_filename.clone())
                    })
                });

                if !relevant {
                    continue;
                }

                match load_from_file(&config_file) {
                    Ok(new_cfg) => {
                        shared.store(Arc::new(new_cfg));
                        let _ = event_tx.send(ConfigEvent::Reloaded);
                    }
                    Err(e) => {
                        tracing::warn!("config reload failed: {e}");
                        let _ = event_tx.send(ConfigEvent::LoadError(e.to_string()));
                    }
                }
            }
        }
    }
}
