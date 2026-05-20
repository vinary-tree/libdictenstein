//! WAL archive / segment configuration.
//!
//! Holds `WalConfig` (archive mode + segment-management knobs) plus its
//! `Default` and helper-constructor impls. Originally inline in
//! `persistent_artrie_core/wal.rs` lines 60-132; split out as part of the
//! Phase-4 wal decomposition.
//!
//! `WalHeader` and `AsyncWalConfig` will join this module in a future
//! incremental split.

use std::path::PathBuf;

/// WAL configuration for archive mode and segment management.
///
/// Archive mode provides crash recovery by preserving WAL segments instead
/// of truncating them. This allows rebuilding the entire dataset from
/// archived segments if the base file is corrupted.
///
/// # Example
///
/// ```rust,ignore
/// let config = WalConfig {
///     archive_enabled: true,
///     archive_dir: PathBuf::from("./wal_archive"),
///     max_segments: 10,
///     max_archive_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
/// };
/// ```
#[derive(Debug, Clone)]
pub struct WalConfig {
    /// Enable archive mode (rename WAL instead of truncate)
    ///
    /// When enabled, checkpoint rotates the WAL to archive instead of
    /// truncating it. This preserves all operations for potential recovery.
    pub archive_enabled: bool,

    /// Directory for archived WAL segments
    ///
    /// Default: "{data_dir}/wal_archive"
    pub archive_dir: PathBuf,

    /// Maximum number of archived segments to keep
    ///
    /// Older segments are pruned when this limit is exceeded.
    /// Default: 10
    pub max_segments: usize,

    /// Maximum total bytes in archived segments
    ///
    /// Older segments are pruned when this limit is exceeded.
    /// Default: 10 GB
    pub max_archive_bytes: u64,
}

impl Default for WalConfig {
    fn default() -> Self {
        Self {
            archive_enabled: true,
            archive_dir: PathBuf::from("wal_archive"),
            max_segments: 10,
            max_archive_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
        }
    }
}

impl WalConfig {
    /// Create a new configuration with archive mode disabled
    pub fn no_archive() -> Self {
        Self {
            archive_enabled: false,
            ..Default::default()
        }
    }

    /// Create a new configuration with custom archive directory
    pub fn with_archive_dir(archive_dir: impl Into<PathBuf>) -> Self {
        Self {
            archive_dir: archive_dir.into(),
            ..Default::default()
        }
    }
}
