//! Thumbnail request/result bridge for the Gallery view.
//!
//! This mirrors the Grid view thumbnail requester so Gallery can reuse the same
//! asynchronous atlas-thumbs pipeline without modifying the existing Grid code.
//! A follow-up refactor can extract the shared core into a common module.

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

/// Target dimension for strip thumbnails.
pub const STRIP_TARGET_DIM: u32 = 96;
/// Target dimension for the large Gallery preview.
pub const PREVIEW_TARGET_DIM: u32 = 1024;

type ThumbKey = (PathBuf, u32);

/// Raw RGBA8 pixel data decoded from a thumbnail.
#[derive(Clone, Debug)]
pub struct DecodedPixels {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Raw RGBA8 bytes.
    pub rgba: Vec<u8>,
}

/// Convert raw decoded pixels to a Slint image.
#[must_use]
pub fn decoded_to_slint(decoded: &DecodedPixels) -> slint::Image {
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &decoded.rgba,
        decoded.width,
        decoded.height,
    );
    slint::Image::from_rgba8(buffer)
}

/// Logical destination for a Gallery thumbnail result.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GalleryThumbTarget {
    /// Strip thumbnail for a specific entry index.
    Strip(usize),
    /// Preview image keyed by its entry path.
    Preview(PathBuf),
}

/// Result delivered back to the Gallery controller.
#[derive(Clone, Debug)]
pub struct GalleryThumbEvent {
    /// Path associated with the thumbnail request.
    pub path: PathBuf,
    /// Requested target dimension.
    pub target_dim: u32,
    /// Decoded pixels when generation succeeded.
    pub pixels: Option<DecodedPixels>,
    /// Logical targets waiting on this `(path, dim)` pair.
    pub targets: Vec<GalleryThumbTarget>,
}

struct Shared {
    pending: Mutex<AHashMap<ThumbKey, Vec<GalleryThumbTarget>>>,
    in_flight: Mutex<AHashSet<ThumbKey>>,
}

/// Background thumbnail requester used by the Gallery controller.
pub struct GalleryThumbRequester {
    generator: Arc<Generator>,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    drain_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl GalleryThumbRequester {
    /// Construct a requester and start its drain thread.
    #[must_use]
    pub fn new(
        cache: Arc<SqliteCache>,
        thread_name: String,
        on_result: Arc<dyn Fn(GalleryThumbEvent) + Send + Sync>,
    ) -> Arc<Self> {
        let worker_count = num_cpus::get().clamp(1, 4);
        let generator = Arc::new(Generator::start(cache, worker_count, 500 * 1024 * 1024));
        let shared = Arc::new(Shared {
            pending: Mutex::new(AHashMap::new()),
            in_flight: Mutex::new(AHashSet::new()),
        });
        let stop = Arc::new(AtomicBool::new(false));

        let requester = Arc::new(Self {
            generator,
            shared,
            stop,
            drain_handle: Mutex::new(None),
        });
        requester.start_drain_thread(thread_name, on_result);
        requester
    }

    /// Clear pending and in-flight state for a new directory snapshot.
    pub fn reset(&self) {
        self.shared.pending.lock().clear();
        self.shared.in_flight.lock().clear();
    }

    /// Request a thumbnail for `path`.
    pub fn request(&self, path: PathBuf, target_dim: u32, target: GalleryThumbTarget) {
        if !can_thumbnail(&path) {
            return;
        }

        let key = (path.clone(), target_dim);
        {
            let mut in_flight = self.shared.in_flight.lock();
            let mut pending = self.shared.pending.lock();
            if in_flight.contains(&key) {
                pending.entry(key).or_default().push(target);
                return;
            }
            in_flight.insert(key.clone());
            pending.entry(key).or_default().push(target);
        }

        self.generator.request(ThumbRequest { path, target_dim });
    }

    fn start_drain_thread(
        self: &Arc<Self>,
        thread_name: String,
        on_result: Arc<dyn Fn(GalleryThumbEvent) + Send + Sync>,
    ) {
        let shared = Arc::clone(&self.shared);
        let stop = Arc::clone(&self.stop);
        let result_rx = self.generator.results();
        let handle = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || drain_loop(&shared, &result_rx, &stop, &on_result))
            .expect("failed to spawn gallery thumbnail drain thread");
        *self.drain_handle.lock() = Some(handle);
    }
}

impl Drop for GalleryThumbRequester {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.drain_handle.lock().take() {
            let _ = handle.join();
        }
    }
}

fn thumb_to_pixels(thumb: &atlas_thumbs::CachedThumb) -> Option<DecodedPixels> {
    let dyn_img = image::load_from_memory(&thumb.bytes)
        .map_err(|error| tracing::warn!(%error, "failed to decode thumbnail bytes"))
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
    on_result: &Arc<dyn Fn(GalleryThumbEvent) + Send + Sync>,
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let result = match result_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };

        let (path, target_dim, pixels) = match result {
            ThumbResult::Hit { request, thumb } => {
                (request.path, request.target_dim, thumb_to_pixels(&thumb))
            }
            ThumbResult::Miss { request, error } => {
                tracing::debug!(path = ?request.path, %error, "gallery thumbnail miss");
                (request.path, request.target_dim, None)
            }
        };

        let key = (path.clone(), target_dim);
        let targets = {
            let mut pending = shared.pending.lock();
            shared.in_flight.lock().remove(&key);
            pending.remove(&key).unwrap_or_default()
        };
        if targets.is_empty() {
            continue;
        }

        on_result(GalleryThumbEvent {
            path,
            target_dim,
            pixels,
            targets,
        });
    }
}
