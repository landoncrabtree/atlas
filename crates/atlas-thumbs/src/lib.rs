//! `atlas-thumbs` provides SQLite-cached thumbnail generation for Atlas.
//!
//! ## Architecture
//!
//! ```text
//! caller ──request──▶ Generator ──queue──▶ workers ──▶ SqliteCache (WAL SQLite)
//!        ◀─result────                               ◀─ generate_thumbnail
//! ```
//!
//! - [`Generator`] manages a worker pool and exposes a channel API.
//! - [`SqliteCache`] provides insertion, lookup, invalidation, and eviction.
//! - [`decode_to_rgba`] handles raster images and SVG.
//! - [`generate_thumbnail`] decodes, resizes, and WebP or PNG encodes.

/// SQLite-backed thumbnail cache types.
pub mod cache;
/// Image decoding helpers.
pub mod decode;
/// Error types and result aliases.
pub mod error;
/// Thumbnail generation pipeline.
pub mod generate;
/// Stable cache key fingerprinting.
pub mod key;
/// Worker pool for asynchronous generation.
pub mod workers;

pub use cache::{CachedThumb, EvictionReport, SqliteCache, ThumbFormat};
pub use decode::{can_thumbnail, decode_thumbnailable_extensions, decode_to_rgba};
pub use error::{Result, ThumbError};
pub use generate::generate_thumbnail;
pub use key::CacheKey;
pub use workers::{Generator, ThumbRequest, ThumbResult};

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use image::RgbaImage;
    use tempfile::TempDir;

    use super::*;
    use crate::key::CacheKey;
    use crate::workers::ThumbRequest;

    fn tmp() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn make_png(dir: &TempDir, w: u32, h: u32, name: &str) -> PathBuf {
        let path = dir.path().join(name);
        let img = RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        img.save_with_format(&path, image::ImageFormat::Png)
            .expect("save png");
        path
    }

    fn make_key(path: PathBuf, target_dim: u32) -> CacheKey {
        let meta = std::fs::metadata(&path).expect("stat");
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos() as i128);
        CacheKey {
            path: path.canonicalize().unwrap_or(path),
            mtime_ns,
            size: meta.len(),
            target_dim,
        }
    }

    #[test]
    fn fingerprint_is_stable() {
        let key = CacheKey {
            path: PathBuf::from("/home/user/photo.jpg"),
            mtime_ns: 1_700_000_000_000_000_000,
            size: 12_345,
            target_dim: 128,
        };
        assert_eq!(key.fingerprint(), key.fingerprint());
    }

    #[test]
    fn fingerprint_changes_with_dim() {
        let base = CacheKey {
            path: PathBuf::from("/home/user/photo.jpg"),
            mtime_ns: 0,
            size: 0,
            target_dim: 128,
        };
        let other = CacheKey {
            target_dim: 256,
            ..base.clone()
        };
        assert_ne!(base.fingerprint(), other.fingerprint());
    }

    #[test]
    fn cache_put_get_roundtrip() {
        let dir = tmp();
        let db = dir.path().join("thumbs.db");
        let cache = SqliteCache::open(&db).expect("open");

        let path = make_png(&dir, 100, 100, "img.png");
        let key = make_key(path, 128);

        let thumb = CachedThumb {
            format: ThumbFormat::Png,
            width: 100,
            height: 100,
            bytes: vec![0_u8; 50].into(),
        };
        cache.put(&key, &thumb).expect("put");

        let got = cache.get(&key).expect("get").expect("cached thumb");
        assert_eq!(got.width, 100);
        assert_eq!(got.height, 100);
        assert_eq!(got.bytes.len(), 50);
    }

    #[test]
    fn cache_size_accounting() {
        let dir = tmp();
        let cache = SqliteCache::open(&dir.path().join("thumbs.db")).expect("open");

        let path = make_png(&dir, 50, 50, "img.png");
        let key = make_key(path, 64);
        let blob = vec![7_u8; 200];

        let thumb = CachedThumb {
            format: ThumbFormat::Webp,
            width: 50,
            height: 50,
            bytes: blob.into(),
        };
        cache.put(&key, &thumb).expect("put");
        assert_eq!(cache.total_size_bytes().expect("total"), 200);
    }

    #[test]
    fn invalidate_path_removes_only_matching() {
        let dir = tmp();
        let cache = SqliteCache::open(&dir.path().join("thumbs.db")).expect("open");

        let p1 = make_png(&dir, 10, 10, "a.png");
        let p2 = make_png(&dir, 10, 10, "b.png");

        let k1 = make_key(p1.clone(), 128);
        let k2 = make_key(p2.clone(), 128);

        let stub = CachedThumb {
            format: ThumbFormat::Png,
            width: 10,
            height: 10,
            bytes: vec![0_u8; 10].into(),
        };
        cache.put(&k1, &stub).expect("put k1");
        cache.put(&k2, &stub).expect("put k2");

        let removed = cache
            .invalidate_path(&p1.canonicalize().unwrap_or(p1))
            .expect("invalidate");
        assert_eq!(removed, 1);
        assert_eq!(cache.count().expect("count"), 1);
    }

    #[test]
    fn evict_lru_removes_oldest_first() {
        let dir = tmp();
        let cache = SqliteCache::open(&dir.path().join("thumbs.db")).expect("open");

        for i in 0_u32..3 {
            let path = make_png(&dir, 10, 10, &format!("img{i}.png"));
            let key = make_key(path, 128);
            let thumb = CachedThumb {
                format: ThumbFormat::Png,
                width: 10,
                height: 10,
                bytes: vec![0_u8; 1_000].into(),
            };
            cache.put(&key, &thumb).expect("put");
            std::thread::sleep(Duration::from_millis(2));
        }

        assert_eq!(cache.total_size_bytes().expect("total"), 3_000);
        let report = cache.evict_lru_until_under_bytes(2_001).expect("evict");
        assert_eq!(report.removed, 1);
        assert_eq!(report.bytes_freed, 1_000);
        assert_eq!(cache.count().expect("count"), 2);
    }

    #[test]
    fn can_thumbnail_is_case_insensitive() {
        assert!(can_thumbnail(std::path::Path::new("photo.JPEG")));
        assert!(!can_thumbnail(std::path::Path::new("notes.txt")));
    }

    #[test]
    fn decode_extensions_include_svg() {
        assert!(decode_thumbnailable_extensions().contains(&"svg"));
    }

    #[test]
    fn decode_raster_png() {
        let dir = tmp();
        let path = make_png(&dir, 80, 60, "test.png");
        let img = decode_to_rgba(&path).expect("decode");
        assert_eq!(img.width(), 80);
        assert_eq!(img.height(), 60);
    }

    #[test]
    fn decode_svg() {
        let dir = tmp();
        let svg_path = dir.path().join("test.svg");
        std::fs::write(
            &svg_path,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="48">
               <rect width="64" height="48" fill="blue"/>
             </svg>"#,
        )
        .expect("write svg");

        let img = decode_to_rgba(&svg_path).expect("decode svg");
        assert_eq!(img.width(), 64);
        assert_eq!(img.height(), 48);
    }

    #[test]
    fn unsupported_format_returns_error() {
        let dir = tmp();
        let path = dir.path().join("file.xyz");
        std::fs::write(&path, b"garbage").expect("write");
        let err = decode_to_rgba(&path).expect_err("unsupported format error");
        assert!(matches!(err, ThumbError::UnsupportedFormat(_)));
    }

    #[test]
    fn generate_respects_target_dim_landscape() {
        let dir = tmp();
        let path = make_png(&dir, 800, 400, "landscape.png");
        let thumb = generate_thumbnail(&path, 128, ThumbFormat::Png).expect("generate");
        assert!(thumb.width.max(thumb.height) <= 128);
        assert_eq!(thumb.width.max(thumb.height), 128);
    }

    #[test]
    fn generate_respects_target_dim_portrait() {
        let dir = tmp();
        let path = make_png(&dir, 400, 800, "portrait.png");
        let thumb = generate_thumbnail(&path, 256, ThumbFormat::Png).expect("generate");
        assert_eq!(thumb.height, 256);
        assert!(thumb.width <= 256);
    }

    #[test]
    fn generate_no_upscale() {
        let dir = tmp();
        let path = make_png(&dir, 50, 50, "small.png");
        let thumb = generate_thumbnail(&path, 256, ThumbFormat::Png).expect("generate");
        assert_eq!(thumb.width, 50);
        assert_eq!(thumb.height, 50);
    }

    #[test]
    fn generator_five_requests_and_cache_hits() {
        let dir = tmp();
        let db = dir.path().join("thumbs.db");
        let cache = Arc::new(SqliteCache::open(&db).expect("open"));
        let generator = Generator::start(Arc::clone(&cache), 2, u64::MAX);
        let rx = generator.results();

        let paths: Vec<PathBuf> = (0..5)
            .map(|i| make_png(&dir, 100, 100, &format!("img{i}.png")))
            .collect();

        for path in &paths {
            generator.request(ThumbRequest {
                path: path.clone(),
                target_dim: 64,
            });
        }

        let mut first_pass = Vec::new();
        while first_pass.len() < 5 {
            match rx.recv_timeout(Duration::from_secs(30)) {
                Ok(result) => first_pass.push(result),
                Err(_) => panic!("timed out waiting for first pass results"),
            }
        }
        assert_eq!(first_pass.len(), 5);
        assert!(first_pass
            .iter()
            .all(|result| matches!(result, ThumbResult::Hit { .. })));

        for path in &paths {
            generator.request(ThumbRequest {
                path: path.clone(),
                target_dim: 64,
            });
        }

        let mut second_pass = Vec::new();
        while second_pass.len() < 5 {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(result) => second_pass.push(result),
                Err(_) => panic!("timed out waiting for second pass results"),
            }
        }
        assert_eq!(second_pass.len(), 5);
        assert!(second_pass
            .iter()
            .all(|result| matches!(result, ThumbResult::Hit { .. })));

        generator.shutdown();
    }

    #[test]
    fn generator_dedup_in_flight() {
        let dir = tmp();
        let db = dir.path().join("thumbs.db");
        let cache = Arc::new(SqliteCache::open(&db).expect("open"));
        let generator = Generator::start(Arc::clone(&cache), 1, u64::MAX);
        let rx = generator.results();

        let path = make_png(&dir, 64, 64, "dedup.png");
        let key = make_key(path.clone(), 128);
        generator.in_flight_for_test().lock().insert(key.clone());

        generator.request(ThumbRequest {
            path: path.clone(),
            target_dim: 128,
        });
        generator.request(ThumbRequest {
            path: path.clone(),
            target_dim: 128,
        });

        std::thread::sleep(Duration::from_millis(200));
        assert!(
            rx.try_recv().is_err(),
            "no results expected while key is in flight"
        );

        generator.in_flight_for_test().lock().remove(&key);
        generator.request(ThumbRequest {
            path: path.clone(),
            target_dim: 128,
        });

        let result = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("should get exactly one result");
        assert!(matches!(result, ThumbResult::Hit { .. }));

        std::thread::sleep(Duration::from_millis(100));
        assert!(rx.try_recv().is_err(), "no extra results expected");

        generator.shutdown();
    }
}
