//! Cache key type with stable fingerprinting.

use std::hash::{BuildHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::OnceLock;

/// A cache key that uniquely identifies a thumbnail request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// Absolute path to the source file (canonicalized if possible).
    pub path: PathBuf,
    /// Modification time in nanoseconds since Unix epoch (0 if unknown).
    pub mtime_ns: i128,
    /// File size in bytes.
    pub size: u64,
    /// Target dimension (longer side), e.g. 128, 256, 512.
    pub target_dim: u32,
}

static HASH_STATE: OnceLock<ahash::RandomState> = OnceLock::new();

fn hash_state() -> &'static ahash::RandomState {
    HASH_STATE.get_or_init(|| {
        ahash::RandomState::with_seeds(
            0x6c62_272e_07bb_0142,
            0x62b8_2175_6295_c58d,
            0x517c_c1b7_2722_0a95,
            0xdb17_3427_4b5a_d4b1,
        )
    })
}

impl CacheKey {
    /// Returns a stable 16-character hex fingerprint suitable as a SQLite primary key.
    ///
    /// Uses a fixed-seed AHash so the fingerprint is consistent within the same binary version.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut hasher = hash_state().build_hasher();
        self.path.to_string_lossy().as_ref().hash(&mut hasher);
        self.mtime_ns.hash(&mut hasher);
        self.size.hash(&mut hasher);
        self.target_dim.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }
}
