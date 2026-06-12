//! Configuration for the async / background-sync WAL writer.
//!
//! Split out of the monolithic `wal.rs` (lines ~1618-1668) as part of the
//! Phase-4 wal decomposition. Pairs with `wal::config::WalConfig` (sync /
//! archive knobs) but lives in its own sub-module since it is only consumed
//! by `AsyncWalWriter` and `SegmentSyncManager`.

use std::path::PathBuf;

/// Configuration for the async WAL writer.
///
/// Controls backpressure behavior when the background sync thread falls behind.
#[derive(Debug, Clone)]
pub struct AsyncWalConfig {
    /// Maximum number of pending segments before blocking writers.
    ///
    /// When this limit is reached, `sync_async()` will block until the oldest
    /// segment is synced. Default: 4
    pub max_pending_segments: usize,

    /// Maximum total bytes in pending segments before blocking writers.
    ///
    /// Provides byte-based backpressure in addition to segment count.
    /// Default: 256MB
    pub max_pending_bytes: u64,

    /// Directory for pending segments awaiting sync.
    ///
    /// Pending segments are named `wal_pending_{timestamp}.segment` and are
    /// moved to the archive directory after successful sync.
    /// Default: "{data_dir}/wal_pending"
    pub pending_dir: PathBuf,

    /// Interval between sync thread checks when idle.
    ///
    /// The sync thread will sleep for this duration when there are no
    /// pending segments to sync. Default: 10ms
    pub idle_check_interval_ms: u64,
}

impl Default for AsyncWalConfig {
    fn default() -> Self {
        Self {
            max_pending_segments: 4,
            max_pending_bytes: 256 * 1024 * 1024, // 256 MB
            pending_dir: PathBuf::from("wal_pending"),
            idle_check_interval_ms: 10,
        }
    }
}

impl AsyncWalConfig {
    /// Create config with custom pending directory.
    pub fn with_pending_dir(pending_dir: impl Into<PathBuf>) -> Self {
        Self {
            pending_dir: pending_dir.into(),
            ..Default::default()
        }
    }
}
