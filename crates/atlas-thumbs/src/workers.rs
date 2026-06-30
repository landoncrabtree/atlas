//! Worker pool for asynchronous thumbnail generation with in-flight deduplication.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use ahash::AHashSet;
use crossbeam_channel::{select, unbounded, Receiver, Sender};
use parking_lot::Mutex;

use crate::cache::{CachedThumb, SqliteCache, ThumbFormat};
use crate::generate::generate_thumbnail;
use crate::key::CacheKey;

/// A request to generate or retrieve a thumbnail.
#[derive(Debug, Clone)]
pub struct ThumbRequest {
    /// Absolute or relative path to the source file.
    pub path: PathBuf,
    /// Target dimension for the longer side in pixels.
    pub target_dim: u32,
}

/// The outcome of a thumbnail request.
#[derive(Debug, Clone)]
pub enum ThumbResult {
    /// Thumbnail is available from cache or fresh generation.
    Hit {
        /// The original request.
        request: ThumbRequest,
        /// The cached thumbnail data.
        thumb: CachedThumb,
    },
    /// Thumbnail generation failed.
    Miss {
        /// The original request.
        request: ThumbRequest,
        /// Human-readable error description.
        error: String,
    },
}

/// Worker pool for generating thumbnails and emitting results over channels.
pub struct Generator {
    request_tx: Sender<ThumbRequest>,
    result_rx: Receiver<ThumbResult>,
    _done_tx: Sender<()>,
    handles: Vec<thread::JoinHandle<()>>,
    _in_flight: Arc<Mutex<AHashSet<CacheKey>>>,
}

impl Generator {
    /// Starts a worker pool.
    ///
    /// If `worker_count` is `0`, the logical CPU count is used.
    /// The cache is trimmed in the background toward `max_cache_bytes` every 60 seconds.
    #[must_use]
    pub fn start(cache: Arc<SqliteCache>, worker_count: usize, max_cache_bytes: u64) -> Self {
        let worker_count = if worker_count == 0 {
            num_cpus::get().max(1)
        } else {
            worker_count
        };

        let (request_tx, request_rx) = unbounded::<ThumbRequest>();
        let (result_tx, result_rx) = unbounded::<ThumbResult>();
        let (done_tx, done_rx) = unbounded::<()>();
        let in_flight = Arc::new(Mutex::new(AHashSet::new()));
        let mut handles = Vec::with_capacity(worker_count + 1);

        for _ in 0..worker_count {
            let worker_request_rx = request_rx.clone();
            let worker_result_tx = result_tx.clone();
            let worker_cache = Arc::clone(&cache);
            let worker_in_flight = Arc::clone(&in_flight);
            let worker_done_rx = done_rx.clone();

            handles.push(thread::spawn(move || {
                worker_loop(
                    worker_request_rx,
                    worker_result_tx,
                    worker_cache,
                    worker_in_flight,
                    worker_done_rx,
                );
            }));
        }

        {
            let trim_cache = Arc::clone(&cache);
            let trim_done_rx = done_rx.clone();
            handles.push(thread::spawn(move || {
                trim_loop(trim_cache, max_cache_bytes, trim_done_rx);
            }));
        }

        Self {
            request_tx,
            result_rx,
            _done_tx: done_tx,
            handles,
            _in_flight: in_flight,
        }
    }

    /// Enqueues a thumbnail request and returns immediately.
    pub fn request(&self, req: ThumbRequest) {
        let _ = self.request_tx.send(req);
    }

    /// Returns a cloneable receiver for thumbnail results.
    #[must_use]
    pub fn results(&self) -> Receiver<ThumbResult> {
        self.result_rx.clone()
    }

    /// Signals all workers to stop and waits for them to finish.
    pub fn shutdown(self) {
        let Self {
            request_tx: _,
            result_rx: _,
            _done_tx,
            mut handles,
            _in_flight: _,
        } = self;
        drop(_done_tx);
        for handle in handles.drain(..) {
            let _ = handle.join();
        }
    }

    /// Returns the in-flight set for tests.
    #[cfg(test)]
    pub(crate) fn in_flight_for_test(&self) -> Arc<Mutex<AHashSet<CacheKey>>> {
        Arc::clone(&self._in_flight)
    }
}

fn worker_loop(
    request_rx: Receiver<ThumbRequest>,
    result_tx: Sender<ThumbResult>,
    cache: Arc<SqliteCache>,
    in_flight: Arc<Mutex<AHashSet<CacheKey>>>,
    done_rx: Receiver<()>,
) {
    loop {
        select! {
            recv(request_rx) -> message => {
                let request = match message {
                    Ok(request) => request,
                    Err(_) => break,
                };
                process_one(request, &cache, &in_flight, &result_tx);
            }
            recv(done_rx) -> _ => break,
        }
    }
}

fn process_one(
    req: ThumbRequest,
    cache: &SqliteCache,
    in_flight: &Mutex<AHashSet<CacheKey>>,
    result_tx: &Sender<ThumbResult>,
) {
    let key = match build_cache_key(&req) {
        Ok(key) => key,
        Err(error) => {
            let _ = result_tx.send(ThumbResult::Miss {
                request: req,
                error: error.to_string(),
            });
            return;
        }
    };

    {
        let mut guard = in_flight.lock();
        if guard.contains(&key) {
            return;
        }
        guard.insert(key.clone());
    }

    let result = do_work(&req, &key, cache);
    in_flight.lock().remove(&key);
    let _ = result_tx.send(result);
}

fn do_work(req: &ThumbRequest, key: &CacheKey, cache: &SqliteCache) -> ThumbResult {
    match cache.get(key) {
        Ok(Some(thumb)) => {
            return ThumbResult::Hit {
                request: req.clone(),
                thumb,
            };
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(path = ?req.path, %error, "thumbnail cache read error");
        }
    }

    match generate_thumbnail(&req.path, req.target_dim, ThumbFormat::Webp) {
        Ok(thumb) => {
            if let Err(error) = cache.put(key, &thumb) {
                tracing::warn!(path = ?req.path, %error, "thumbnail cache write error");
            }
            ThumbResult::Hit {
                request: req.clone(),
                thumb,
            }
        }
        Err(error) => ThumbResult::Miss {
            request: req.clone(),
            error: error.to_string(),
        },
    }
}

fn trim_loop(cache: Arc<SqliteCache>, max_bytes: u64, done_rx: Receiver<()>) {
    loop {
        select! {
            recv(done_rx) -> _ => break,
            default(Duration::from_secs(60)) => {
                match cache.evict_lru_until_under_bytes(max_bytes) {
                    Ok(report) if report.removed > 0 => {
                        tracing::debug!(
                            removed = report.removed,
                            bytes_freed = report.bytes_freed,
                            "thumbnail cache LRU trim removed entries"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(%error, "thumbnail cache LRU trim error");
                    }
                }
            }
        }
    }
}

fn build_cache_key(req: &ThumbRequest) -> std::io::Result<CacheKey> {
    let metadata = std::fs::metadata(&req.path)?;
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos() as i128);

    Ok(CacheKey {
        path: req.path.canonicalize().unwrap_or_else(|_| req.path.clone()),
        mtime_ns,
        size: metadata.len(),
        target_dim: req.target_dim,
    })
}
