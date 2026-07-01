//! Thumbnail request/result bridge between [`atlas_thumbs::Generator`] and Slint.
//!
//! [`ThumbRequester`] wraps a [`Generator`] worker pool, deduplicates in-flight
//! requests by `(path, dim)`, decodes WebP/PNG bytes to raw RGBA pixels
//! (`Send`-safe), and routes results back to the correct grid cell indices via
//! [`slint::invoke_from_event_loop`].
//!
//! # Send-safety note
//!
//! `slint::Image` is **not** `Send` (it holds `VRc<OpaqueImageVTable>` which
//! contains a raw pointer). All cross-thread shared state therefore stores
//! `DecodedPixels` (raw RGBA8 bytes) and converts to `slint::Image` only
//! inside `invoke_from_event_loop` closures that run on the Slint event thread.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use ahash::{AHashMap, AHashSet};
use atlas_thumbs::{can_thumbnail, Generator, SqliteCache, ThumbRequest, ThumbResult};
use crossbeam_channel::RecvTimeoutError;
use parking_lot::Mutex;
use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::AtlasWindow;

/// Default target dimension for thumbnails (longer side, in pixels).
pub const DEFAULT_TARGET_DIM: u32 = 256;

/// Key for deduplication: (canonical path, target dimension).
type ThumbKey = (PathBuf, u32);

/// Raw RGBA8 pixel data decoded from a WebP/PNG thumbnail.
///
/// Stored in shared state instead of `slint::Image` so it can safely cross
/// thread boundaries (`slint::Image` is not `Send`).
#[derive(Clone)]
pub struct DecodedPixels {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Raw RGBA8 bytes — length must equal `width * height * 4`.
    pub rgba: Vec<u8>,
}

/// Convert raw decoded pixels to a `slint::Image`.
///
/// Must be called on the Slint event thread (inside `invoke_from_event_loop`).
pub fn decoded_to_slint(d: &DecodedPixels) -> slint::Image {
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&d.rgba, d.width, d.height);
    slint::Image::from_rgba8(buf)
}

/// Shared mutable state owned jointly by [`ThumbRequester`] and its drain thread.
///
/// All fields are `Send` because `DecodedPixels` contains only `Vec<u8>`.
struct Shared {
    /// Maps `(path, dim)` → list of grid cell indices waiting for it.
    pending: Mutex<AHashMap<ThumbKey, Vec<usize>>>,
    /// Keys currently enqueued or in the worker pool.
    in_flight: Mutex<AHashSet<ThumbKey>>,
    /// Per-cell flag indicating whether a decoded thumbnail is available.
    has_thumbs: Mutex<Vec<bool>>,
    /// Per-cell decoded pixels; `None` while pending or absent.
    decoded: Mutex<Vec<Option<DecodedPixels>>>,
}

/// Wraps [`Generator`] to deduplicate requests and push decoded thumbnails
/// to the Slint UI thread.
pub struct ThumbRequester {
    generator: Arc<Generator>,
    shared: Arc<Shared>,
    pane: usize,
    stop: Arc<AtomicBool>,
    drain_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    window: slint::Weak<AtlasWindow>,
}

impl ThumbRequester {
    /// Construct a requester and start the result-drain background thread.
    ///
    /// `worker_count` sets the thumbnail generation thread pool size
    /// (config: `thumbnails.generation_threads`); pass `0` to use
    /// `num_cpus` clamped to 4.  `max_cache_bytes` caps the LRU eviction
    /// target (config: `thumbnails.cache_max_size_mb`).
    #[must_use]
    pub fn new(
        cache: Arc<SqliteCache>,
        pane: usize,
        window: slint::Weak<AtlasWindow>,
        worker_count: usize,
        max_cache_bytes: u64,
    ) -> Arc<Self> {
        // config: reads config.thumbnails.generation_threads
        let workers = if worker_count == 0 {
            num_cpus::get().clamp(1, 4)
        } else {
            worker_count.clamp(1, 16)
        };
        // config: reads config.thumbnails.cache_max_size_mb
        let generator = Arc::new(Generator::start(cache, workers, max_cache_bytes));

        let shared = Arc::new(Shared {
            pending: Mutex::new(AHashMap::new()),
            in_flight: Mutex::new(AHashSet::new()),
            has_thumbs: Mutex::new(Vec::new()),
            decoded: Mutex::new(Vec::new()),
        });

        let stop = Arc::new(AtomicBool::new(false));

        let requester = Arc::new(Self {
            generator,
            shared,
            pane,
            stop: Arc::clone(&stop),
            drain_handle: Mutex::new(None),
            window,
        });

        requester.start_drain_thread();
        requester
    }

    /// Reset internal state for a new directory load.
    ///
    /// Clears all pending/in-flight maps and resizes the pixel/flag vectors.
    pub fn reset(&self, len: usize) {
        self.shared.pending.lock().clear();
        self.shared.in_flight.lock().clear();
        *self.shared.has_thumbs.lock() = vec![false; len];
        *self.shared.decoded.lock() = vec![None; len];
    }

    /// Returns cloned snapshots of the current decoded pixel and flag vectors.
    ///
    /// Called by the controller to push the initial (all-empty) state to Slint
    /// immediately after a location change.
    pub fn snapshot(&self) -> (Vec<Option<DecodedPixels>>, Vec<bool>) {
        (
            self.shared.decoded.lock().clone(),
            self.shared.has_thumbs.lock().clone(),
        )
    }

    /// Enqueue a thumbnail request for `path` at the given `cell_index`.
    ///
    /// Deduplicates: if the same `(path, dim)` is already in-flight, only the
    /// new `cell_index` is recorded; no extra generator request is made.
    /// Non-thumbnailable paths are silently ignored.
    pub fn request(&self, path: PathBuf, target_dim: u32, cell_index: usize) {
        if !can_thumbnail(&path) {
            return;
        }

        let key: ThumbKey = (path.clone(), target_dim);

        {
            let mut in_flight = self.shared.in_flight.lock();
            let mut pending = self.shared.pending.lock();

            // If already queued, just append the new cell index.
            if in_flight.contains(&key) {
                pending.entry(key).or_default().push(cell_index);
                return;
            }

            in_flight.insert(key.clone());
            pending.entry(key).or_default().push(cell_index);
        }

        self.generator.request(ThumbRequest { path, target_dim });
    }

    fn start_drain_thread(self: &Arc<Self>) {
        let shared = Arc::clone(&self.shared);
        let result_rx = self.generator.results();
        let stop = Arc::clone(&self.stop);
        let pane = self.pane;
        let window = self.window.clone();

        let handle = std::thread::Builder::new()
            .name(format!("atlas-grid-thumbs-pane{pane}"))
            .spawn(move || drain_loop(&shared, &result_rx, &stop, pane, &window))
            .expect("failed to spawn grid thumb drain thread");

        *self.drain_handle.lock() = Some(handle);
    }
}

impl Drop for ThumbRequester {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.drain_handle.lock().take() {
            let _ = handle.join();
        }
    }
}

/// Decode WebP/PNG bytes from a [`atlas_thumbs::CachedThumb`] to raw RGBA8 pixels.
fn thumb_to_pixels(thumb: &atlas_thumbs::CachedThumb) -> Option<DecodedPixels> {
    let dyn_img = image::load_from_memory(&thumb.bytes)
        .map_err(|e| tracing::warn!(error = %e, "failed to decode thumbnail bytes"))
        .ok()?;

    let rgba = dyn_img.into_rgba8();
    let (width, height) = rgba.dimensions();
    Some(DecodedPixels {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

fn drain_loop(
    shared: &Arc<Shared>,
    result_rx: &crossbeam_channel::Receiver<ThumbResult>,
    stop: &Arc<AtomicBool>,
    pane: usize,
    window: &slint::Weak<AtlasWindow>,
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let result = match result_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };

        let (path, target_dim, maybe_pixels) = match result {
            ThumbResult::Hit { request, thumb } => {
                let pixels = thumb_to_pixels(&thumb);
                (request.path, request.target_dim, pixels)
            }
            ThumbResult::Miss { request, error } => {
                tracing::debug!(path = ?request.path, %error, "thumbnail miss");
                (request.path, request.target_dim, None)
            }
        };

        let key: ThumbKey = (path, target_dim);

        let indices: Vec<usize> = {
            let mut pending = shared.pending.lock();
            shared.in_flight.lock().remove(&key);
            pending.remove(&key).unwrap_or_default()
        };

        let Some(pixels) = maybe_pixels else { continue };

        // Update shared state (Send-safe: DecodedPixels contains only Vec<u8>).
        {
            let mut decoded = shared.decoded.lock();
            let mut has = shared.has_thumbs.lock();
            for &idx in &indices {
                if let Some(slot) = decoded.get_mut(idx) {
                    *slot = Some(pixels.clone());
                }
                if let Some(flag) = has.get_mut(idx) {
                    *flag = true;
                }
            }
        }

        // Snapshot and push to UI.  Converting to slint::Image happens inside
        // invoke_from_event_loop so it runs on the Slint event thread.
        let decoded_snap = shared.decoded.lock().clone();
        let has_snap = shared.has_thumbs.lock().clone();
        let window = window.clone();

        let _ = slint::invoke_from_event_loop(move || {
            let Some(w) = window.upgrade() else { return };

            let thumbs: Vec<slint::Image> = decoded_snap
                .iter()
                .map(|d| d.as_ref().map(decoded_to_slint).unwrap_or_default())
                .collect();

            let thumb_model = slint::ModelRc::new(slint::VecModel::from(thumbs));
            let has_model = slint::ModelRc::new(slint::VecModel::from(has_snap));

            if pane == 0 {
                w.set_pane0_grid_thumbnails(thumb_model);
                w.set_pane0_grid_has_thumbs(has_model);
            } else {
                w.set_pane1_grid_thumbnails(thumb_model);
                w.set_pane1_grid_has_thumbs(has_model);
            }
        });
    }
}
