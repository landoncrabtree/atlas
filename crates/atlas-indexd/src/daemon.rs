//! Core daemon implementation for atlas-indexd.

use std::cmp::Ordering as CmpOrdering;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use arc_swap::ArcSwap;
use crossbeam_channel::{Receiver, RecvTimeoutError};
use dashmap::DashMap;
use parking_lot::Mutex;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use atlas_config::Config;
use atlas_core::path::expand_tilde;
use atlas_index::{AtlasSchema, Hit, IndexReader, IndexWriter, Query, SearchOptions, SortBy};
use atlas_ipc::protocol::Notification;
use atlas_ipc::server::Server;
use atlas_watch::{DirectoryWatcher, FileEvent, RootId, WatcherBuilder};

use crate::handler::DaemonHandler;
use crate::ingest;
use crate::paths;
use crate::state::IndexRoot;

/// A snapshot of aggregate daemon statistics.
#[derive(Debug, Clone, Copy)]
pub struct DaemonStats {
    /// Total indexed documents across all roots.
    pub docs: u64,
    /// Total on-disk index size across all roots.
    pub on_disk_bytes: u64,
    /// Number of configured roots.
    pub num_roots: usize,
}

/// Background indexer daemon state.
pub struct Daemon {
    config: ArcSwap<Config>,
    schema: Arc<AtlasSchema>,
    roots: DashMap<PathBuf, Arc<IndexRoot>>,
    watcher: Mutex<Option<DirectoryWatcher>>,
    watcher_rx: Mutex<Option<Receiver<FileEvent>>>,
    notifications: broadcast::Sender<Notification>,
    cancel: CancellationToken,
    socket: PathBuf,
}

impl Daemon {
    /// Create a daemon, initialize watchers, and add configured roots.
    pub async fn start(config: Config, socket: PathBuf) -> Result<Arc<Self>> {
        std::fs::create_dir_all(paths::base_dir()?)?;
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let (watcher, watcher_rx) = WatcherBuilder::new()
            .debounce(Duration::from_millis(250))
            .recursive(true)
            .follow_symlinks(config.general.follow_symlinks)
            .build()?;

        let (notifications, _) = broadcast::channel(256);
        let daemon = Arc::new(Self {
            config: ArcSwap::from_pointee(config.clone()),
            schema: Arc::new(AtlasSchema::build()),
            roots: DashMap::new(),
            watcher: Mutex::new(Some(watcher)),
            watcher_rx: Mutex::new(Some(watcher_rx)),
            notifications,
            cancel: CancellationToken::new(),
            socket,
        });

        if config.indexer.enabled {
            for root in config.indexer.roots {
                if let Err(error) = daemon.add_root(root).await {
                    warn!(%error, "failed to add configured root");
                }
            }
        }

        Ok(daemon)
    }

    /// Run the IPC server and background maintenance tasks until cancelled.
    pub async fn run(self: Arc<Self>) -> Result<()> {
        let server = Server::bind(&self.socket, DaemonHandler::new(Arc::clone(&self))).await?;
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        if let Some(rx) = self.watcher_rx.lock().take() {
            let cancel = self.cancel.clone();
            tokio::task::spawn_blocking(move || bridge_events(rx, event_tx, cancel));
        }

        let event_daemon = Arc::clone(&self);
        let event_task = tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                crate::incremental::apply(&event_daemon, event).await;
                if event_daemon.cancel.is_cancelled() {
                    break;
                }
            }
        });

        let commit_daemon = Arc::clone(&self);
        let commit_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = commit_daemon.cancel.cancelled() => break,
                    _ = interval.tick() => commit_daemon.flush_pending_commits(),
                }
            }
        });

        let result = server.run(self.cancel.clone()).await;
        self.cancel.cancel();
        let _ = event_task.await;
        let _ = commit_task.await;
        self.shutdown().await;
        result?;
        Ok(())
    }

    /// Cancel background work, stop watchers, flush commits, and clean up the socket.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        self.flush_pending_commits();

        if let Some(watcher) = self.watcher.lock().take() {
            watcher.shutdown();
        }

        #[cfg(unix)]
        if self.socket.exists() {
            let _ = std::fs::remove_file(&self.socket);
        }
    }

    /// Add a root directory to the daemon and kick off an initial ingest.
    pub async fn add_root(&self, path: PathBuf) -> Result<()> {
        let expanded = expand_tilde(path);
        let canonical = expanded
            .canonicalize()
            .map_err(|error| atlas_core::AtlasError::io(Some(expanded.clone()), error))?;

        if self.roots.contains_key(&canonical) {
            return Ok(());
        }

        let index_dir = paths::index_root_dir(&canonical)?;
        std::fs::create_dir_all(&index_dir)?;

        let memory_budget = self.config.load().indexer.max_memory_mb as usize;
        let writer = IndexWriter::open(&index_dir, memory_budget)?;
        let reader = IndexReader::open(&index_dir)?;

        let root_id = self
            .watcher
            .lock()
            .as_ref()
            .context("watcher is not available")?
            .add_root(canonical.clone())?;

        let root = Arc::new(IndexRoot {
            path: canonical.clone(),
            root_id,
            index_dir,
            writer: parking_lot::Mutex::new(writer),
            reader: parking_lot::RwLock::new(reader),
            pending_writes: AtomicUsize::new(0),
            indexing: AtomicBool::new(false),
        });

        self.roots.insert(canonical, Arc::clone(&root));
        self.spawn_full_reindex(root);
        Ok(())
    }

    /// Remove a root directory from the daemon.
    pub async fn remove_root(&self, path: PathBuf) -> Result<()> {
        let key = self
            .resolve_root_path(&path)
            .with_context(|| format!("root {} is not indexed", path.display()))?;

        let (_, root) = self
            .roots
            .remove(&key)
            .ok_or_else(|| anyhow!("root {} disappeared during removal", key.display()))?;

        if let Some(watcher) = self.watcher.lock().as_ref() {
            watcher.remove_root(root.root_id)?;
        }

        if root.index_dir.exists() {
            std::fs::remove_dir_all(&root.index_dir)
                .map_err(|error| atlas_core::AtlasError::io(Some(root.index_dir.clone()), error))?;
        }

        Ok(())
    }

    /// Trigger a full reindex for one root or all roots.
    pub async fn reindex(&self, path: Option<PathBuf>) -> Result<()> {
        match path {
            Some(path) => {
                let key = self
                    .resolve_root_path(&path)
                    .with_context(|| format!("root {} is not indexed", path.display()))?;
                let root = self
                    .roots
                    .get(&key)
                    .map(|entry| Arc::clone(entry.value()))
                    .ok_or_else(|| anyhow!("root {} disappeared during reindex", key.display()))?;
                self.spawn_full_reindex(root);
            }
            None => {
                let roots: Vec<_> = self
                    .roots
                    .iter()
                    .map(|entry| Arc::clone(entry.value()))
                    .collect();
                for root in roots {
                    self.spawn_full_reindex(root);
                }
            }
        }
        Ok(())
    }

    /// Execute a search across every indexed root.
    pub fn search(&self, query: &Query, options: &SearchOptions) -> Result<Vec<Hit>> {
        let mut hits = Vec::new();
        let per_root_limit = options.limit.max(1);
        let root_count = self.stats().num_roots.max(1);
        let root_limit = per_root_limit.max(per_root_limit.saturating_mul(root_count));
        let per_root_options = SearchOptions {
            limit: root_limit,
            include_hidden: options.include_hidden,
            sort: options.sort,
        };

        for root in self.roots.iter() {
            hits.extend(root.reader.read().search(query, &per_root_options)?);
        }

        sort_hits(&mut hits, options.sort);
        hits.truncate(options.limit.max(1));
        debug!(schema = ?self.schema, hits = hits.len(), "search completed");
        Ok(hits)
    }

    /// Return aggregate daemon statistics.
    #[must_use]
    pub fn stats(&self) -> DaemonStats {
        let mut docs = 0_u64;
        let mut on_disk_bytes = 0_u64;

        for root in self.roots.iter() {
            match root.reader.read().stats() {
                Ok(stats) => {
                    docs += stats.num_docs;
                    on_disk_bytes += stats.on_disk_bytes;
                }
                Err(error) => {
                    warn!(root = %root.path.display(), %error, "failed to read index stats")
                }
            }
        }

        DaemonStats {
            docs,
            on_disk_bytes,
            num_roots: self.roots.len(),
        }
    }

    /// Subscribe to daemon notifications.
    pub fn notifications(&self) -> broadcast::Receiver<Notification> {
        self.notifications.subscribe()
    }

    /// Clone the notification sender.
    pub fn notifications_tx(&self) -> broadcast::Sender<Notification> {
        self.notifications.clone()
    }

    /// Look up a root by watch ID.
    pub fn root_by_id(&self, root_id: RootId) -> Option<Arc<IndexRoot>> {
        self.roots.iter().find_map(|entry| {
            if entry.value().root_id == root_id {
                Some(Arc::clone(entry.value()))
            } else {
                None
            }
        })
    }

    /// Spawn a full-root ingest task.
    pub fn spawn_full_reindex(&self, root: Arc<IndexRoot>) {
        let config = self.config.load_full();
        let notifications = self.notifications.clone();
        tokio::task::spawn_blocking(move || ingest::rebuild_root(root, config, notifications));
    }

    /// Spawn a subtree ingest task.
    pub fn spawn_subtree_reindex(&self, root: Arc<IndexRoot>, subtree: PathBuf) {
        let config = self.config.load_full();
        let notifications = self.notifications.clone();
        tokio::task::spawn_blocking(move || {
            ingest::rebuild_subtree(root, subtree, config, notifications)
        });
    }

    fn resolve_root_path(&self, path: &Path) -> Option<PathBuf> {
        let expanded = expand_tilde(path);
        let canonical = expanded.canonicalize().ok();
        self.roots.iter().find_map(|entry| {
            if entry.key().as_path() == expanded.as_path()
                || canonical
                    .as_ref()
                    .is_some_and(|candidate| candidate.as_path() == entry.key().as_path())
            {
                Some(entry.key().clone())
            } else {
                None
            }
        })
    }

    fn flush_pending_commits(&self) {
        for root in self.roots.iter() {
            let root = root.value();
            let pending = root.take_pending();
            if pending == 0 {
                continue;
            }

            let commit_result = (|| -> Result<()> {
                {
                    let mut writer = root.writer.lock();
                    writer.commit()?;
                }
                root.reader.read().reload()?;
                Ok(())
            })();

            if let Err(error) = commit_result {
                root.pending_writes.fetch_add(pending, Ordering::Relaxed);
                warn!(root = %root.path.display(), %error, "failed to commit index updates");
            }
        }
    }
}

fn bridge_events(
    rx: Receiver<FileEvent>,
    tx: mpsc::UnboundedSender<FileEvent>,
    cancel: CancellationToken,
) {
    loop {
        if cancel.is_cancelled() {
            break;
        }

        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => {
                if tx.send(event).is_err() {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn sort_hits(hits: &mut [Hit], sort: SortBy) {
    match sort {
        SortBy::Score => hits.sort_unstable_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(CmpOrdering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        }),
        SortBy::Name => {
            hits.sort_unstable_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)))
        }
        SortBy::Size => {
            hits.sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)))
        }
        SortBy::Mtime => hits.sort_unstable_by(|a, b| {
            b.mtime
                .unwrap_or_default()
                .cmp(&a.mtime.unwrap_or_default())
                .then_with(|| a.path.cmp(&b.path))
        }),
    }
}
