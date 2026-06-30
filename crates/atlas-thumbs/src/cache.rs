//! SQLite-backed thumbnail cache.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
use parking_lot::Mutex;
use rusqlite::{params, Connection};

use crate::error::{Result, ThumbError};
use crate::key::CacheKey;

/// Thumbnail image format stored in the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbFormat {
    /// WebP encoded thumbnail.
    Webp,
    /// PNG encoded thumbnail.
    Png,
}

impl ThumbFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Webp => "webp",
            Self::Png => "png",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "webp" => Some(Self::Webp),
            "png" => Some(Self::Png),
            _ => None,
        }
    }
}

/// A cached thumbnail and its metadata.
#[derive(Debug, Clone)]
pub struct CachedThumb {
    /// Image format of the encoded bytes.
    pub format: ThumbFormat,
    /// Pixel width of the thumbnail.
    pub width: u32,
    /// Pixel height of the thumbnail.
    pub height: u32,
    /// Encoded image bytes.
    pub bytes: Arc<[u8]>,
}

/// Statistics returned after an LRU eviction pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct EvictionReport {
    /// Number of entries removed.
    pub removed: u64,
    /// Total bytes freed.
    pub bytes_freed: u64,
}

/// SQLite-backed cache for generated thumbnails.
///
/// Thread-safe access is provided by wrapping the SQLite connection in a mutex.
pub struct SqliteCache {
    conn: Mutex<Connection>,
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

impl SqliteCache {
    /// Opens or creates a cache database at the given path.
    ///
    /// Applies WAL mode and performance pragmas, then ensures the schema exists.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;
        apply_pragmas(&conn)?;
        create_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Opens the default platform cache at `<cache_dir>/atlas/thumbs.db`.
    ///
    /// Creates parent directories as needed.
    pub fn open_default() -> Result<Self> {
        let dirs = ProjectDirs::from("dev", "atlas", "atlas")
            .ok_or_else(|| ThumbError::Io(std::io::Error::other("no valid home directory")))?;
        let path = dirs.cache_dir().join("thumbs.db");
        Self::open(&path)
    }

    /// Looks up a cached thumbnail by key.
    ///
    /// Updates `last_used` within the same transaction on a cache hit.
    pub fn get(&self, key: &CacheKey) -> Result<Option<CachedThumb>> {
        let fingerprint = key.fingerprint();
        let now_ms = unix_ms();
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;

        let result = tx.query_row(
            "SELECT format, width, height, bytes FROM thumbnails WHERE fingerprint = ?1",
            params![fingerprint],
            |row| {
                let format_str: String = row.get(0)?;
                let width: u32 = row.get(1)?;
                let height: u32 = row.get(2)?;
                let bytes: Vec<u8> = row.get(3)?;
                Ok((format_str, width, height, bytes))
            },
        );

        match result {
            Ok((format_str, width, height, bytes)) => {
                tx.execute(
                    "UPDATE thumbnails SET last_used = ?1 WHERE fingerprint = ?2",
                    params![now_ms, key.fingerprint()],
                )?;
                tx.commit()?;
                let format = ThumbFormat::from_str(&format_str).ok_or_else(|| {
                    ThumbError::Decode(format!("unknown format in cache: {format_str}"))
                })?;
                Ok(Some(CachedThumb {
                    format,
                    width,
                    height,
                    bytes: bytes.into(),
                }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Inserts or replaces a thumbnail in the cache.
    pub fn put(&self, key: &CacheKey, thumb: &CachedThumb) -> Result<()> {
        let fingerprint = key.fingerprint();
        let now_ms = unix_ms();
        let byte_size = thumb.bytes.len() as i64;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO thumbnails              (fingerprint, path, mtime_ns, size, target_dim, format, width, height, bytes, byte_size, created_at, last_used)              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                fingerprint,
                key.path.to_string_lossy().as_ref(),
                key.mtime_ns as i64,
                key.size as i64,
                key.target_dim as i64,
                thumb.format.as_str(),
                thumb.width as i64,
                thumb.height as i64,
                thumb.bytes.as_ref(),
                byte_size,
                now_ms,
                now_ms,
            ],
        )?;
        Ok(())
    }

    /// Removes all cached thumbnails whose source path matches `path`.
    ///
    /// Returns the number of rows removed.
    pub fn invalidate_path(&self, path: &Path) -> Result<usize> {
        let path_str = path.to_string_lossy();
        let conn = self.conn.lock();
        let removed = conn.execute(
            "DELETE FROM thumbnails WHERE path = ?1",
            params![path_str.as_ref()],
        )?;
        Ok(removed)
    }

    /// Removes least-recently-used entries until the total cache size is under `max_bytes`.
    ///
    /// Returns an [`EvictionReport`] describing how many entries and bytes were freed.
    pub fn evict_lru_until_under_bytes(&self, max_bytes: u64) -> Result<EvictionReport> {
        let mut report = EvictionReport::default();
        let conn = self.conn.lock();

        loop {
            let total: i64 = conn.query_row(
                "SELECT COALESCE(SUM(byte_size), 0) FROM thumbnails",
                [],
                |row| row.get(0),
            )?;
            if total as u64 <= max_bytes {
                break;
            }

            let oldest = conn.query_row(
                "SELECT fingerprint, byte_size FROM thumbnails ORDER BY last_used ASC LIMIT 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            );

            match oldest {
                Ok((fingerprint, byte_size)) => {
                    conn.execute(
                        "DELETE FROM thumbnails WHERE fingerprint = ?1",
                        params![fingerprint],
                    )?;
                    report.removed += 1;
                    report.bytes_freed += byte_size as u64;
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => break,
                Err(error) => return Err(error.into()),
            }
        }

        Ok(report)
    }

    /// Returns the total number of bytes stored in the cache.
    pub fn total_size_bytes(&self) -> Result<u64> {
        let conn = self.conn.lock();
        let total: i64 = conn.query_row(
            "SELECT COALESCE(SUM(byte_size), 0) FROM thumbnails",
            [],
            |row| row.get(0),
        )?;
        Ok(total as u64)
    }

    /// Returns the total number of entries in the cache.
    pub fn count(&self) -> Result<u64> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM thumbnails", [], |row| row.get(0))?;
        Ok(count as u64)
    }
}

fn apply_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;         PRAGMA synchronous = NORMAL;         PRAGMA temp_store = MEMORY;         PRAGMA mmap_size = 67108864;",
    )?;
    Ok(())
}

fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS thumbnails (
            fingerprint TEXT PRIMARY KEY,
            path        TEXT NOT NULL,
            mtime_ns    INTEGER NOT NULL,
            size        INTEGER NOT NULL,
            target_dim  INTEGER NOT NULL,
            format      TEXT NOT NULL,
            width       INTEGER NOT NULL,
            height      INTEGER NOT NULL,
            bytes       BLOB NOT NULL,
            byte_size   INTEGER NOT NULL,
            created_at  INTEGER NOT NULL,
            last_used   INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS thumbnails_last_used ON thumbnails(last_used);
        CREATE INDEX IF NOT EXISTS thumbnails_path ON thumbnails(path);",
    )?;
    Ok(())
}
