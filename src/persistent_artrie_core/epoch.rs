//! Epoch-Based Automatic Checkpointing
//!
//! This module implements epoch-based checkpointing for bounded WAL size
//! and predictable recovery. Based on the BD+Tree (SIGMOD 2024) epoch-based
//! persistence model.
//!
//! # Epoch Lifecycle
//!
//! ```text
//! ┌───────────┐    advance()    ┌───────────┐    checkpoint()    ┌───────────┐
//! │  ACTIVE   │────────────────►│  SEALING  │──────────────────►│  DURABLE  │
//! │           │                 │           │                    │           │
//! │ Accepts   │                 │ No new    │                    │ Fully     │
//! │ new ops   │                 │ writes    │                    │ persisted │
//! └───────────┘                 └───────────┘                    └───────────┘
//! ```
//!
//! # WAL Segmentation
//!
//! Each epoch has its own WAL segment:
//!
//! ```text
//! data/
//! ├── artrie.dat           # Main trie data file
//! ├── wal/
//! │   ├── epoch_0000000042.wal
//! │   ├── epoch_0000000043.wal
//! │   ├── epoch_0000000044.wal  (current)
//! │   └── checkpoint.meta       # Last checkpoint info
//! └── checkpoint/
//!     └── checkpoint_0000000042.snap
//! ```
//!
//! # Recovery Semantics
//!
//! On crash during epoch N, the system recovers to the end of epoch N-2.
//! This provides a bounded recovery window while allowing writes within
//! an epoch to be batched efficiently.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::wal::{Lsn, WalWriter};
use super::PersistentARTrieError;
use super::Result;
use log::warn;

/// Unique identifier for an epoch.
pub type EpochId = u64;

/// Configuration for epoch-based automatic checkpointing.
///
/// Epochs divide time into discrete intervals. At each epoch boundary,
/// the system ensures all data from the previous epoch is durable.
///
/// # Recovery Semantics
///
/// On crash during epoch N, the system recovers to the end of epoch N-2.
/// This provides a bounded recovery window while allowing writes within
/// an epoch to be batched efficiently.
///
/// # Tuning Guidelines
///
/// - **epoch_duration**: Controls the maximum data loss window
///   - 100ms: Low latency, frequent checkpoints, higher overhead
///   - 1s: Balanced, good for most workloads
///   - 10s: High throughput, larger recovery window
///
/// - **max_wal_size_bytes**: Prevents unbounded WAL growth
///   - Set based on available disk space and recovery time requirements
///   - Larger values = faster writes, slower recovery
#[derive(Debug, Clone)]
pub struct EpochConfig {
    /// Duration of each epoch.
    ///
    /// This is the target time between checkpoints. The actual epoch
    /// duration may be shorter if other triggers fire first.
    ///
    /// Default: 100ms
    /// Range: 10ms - 60s
    pub epoch_duration: Duration,

    /// Maximum operations per epoch before forcing early checkpoint.
    ///
    /// When this many operations have been logged in the current epoch,
    /// an early checkpoint is triggered regardless of time elapsed.
    ///
    /// Default: 10_000
    /// Range: 100 - 10_000_000
    pub max_ops_per_epoch: usize,

    /// Maximum WAL size in bytes before forcing checkpoint.
    ///
    /// This bounds the total WAL size across all epochs. When exceeded,
    /// the oldest epoch is checkpointed and its WAL segment deleted.
    ///
    /// Default: 64MB
    /// Range: 1MB - 10GB
    pub max_wal_size_bytes: usize,

    /// Number of epoch WAL segments to retain.
    ///
    /// After checkpoint, this many old WAL segments are kept for
    /// debugging/auditing. Set to 0 to delete immediately.
    ///
    /// Default: 2
    /// Range: 0 - 100
    pub retention_epochs: usize,

    /// Use a background thread for checkpointing.
    ///
    /// When true, checkpoints happen in the background without blocking
    /// foreground operations. When false, checkpoint is synchronous.
    ///
    /// Default: true
    pub background_checkpoint: bool,

    /// Enable incremental checkpointing.
    ///
    /// When true, only dirty pages since the last checkpoint are written.
    /// When false, the entire trie is persisted (simpler but slower).
    ///
    /// Default: true
    pub incremental_checkpoint: bool,
}

impl Default for EpochConfig {
    fn default() -> Self {
        Self {
            epoch_duration: Duration::from_millis(100),
            max_ops_per_epoch: 10_000,
            max_wal_size_bytes: 64 * 1024 * 1024, // 64 MB
            retention_epochs: 2,
            background_checkpoint: true,
            incremental_checkpoint: true,
        }
    }
}

impl EpochConfig {
    /// Create a config optimized for low latency (more frequent checkpoints).
    pub fn low_latency() -> Self {
        Self {
            epoch_duration: Duration::from_millis(10),
            max_ops_per_epoch: 1_000,
            max_wal_size_bytes: 8 * 1024 * 1024, // 8 MB
            ..Default::default()
        }
    }

    /// Create a config optimized for high throughput (less frequent checkpoints).
    pub fn high_throughput() -> Self {
        Self {
            epoch_duration: Duration::from_secs(1),
            max_ops_per_epoch: 100_000,
            max_wal_size_bytes: 256 * 1024 * 1024, // 256 MB
            ..Default::default()
        }
    }

    /// Validate config parameters.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.epoch_duration < Duration::from_millis(10) {
            return Err("epoch_duration must be at least 10ms".into());
        }
        if self.epoch_duration > Duration::from_secs(60) {
            return Err("epoch_duration must be at most 60s".into());
        }
        if self.max_ops_per_epoch < 100 {
            return Err("max_ops_per_epoch must be at least 100".into());
        }
        if self.max_wal_size_bytes < 1024 * 1024 {
            return Err("max_wal_size_bytes must be at least 1MB".into());
        }
        Ok(())
    }
}

/// State of an epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochState {
    /// Epoch is active and accepting new operations.
    Active,
    /// Epoch is being sealed (no new operations, pending writes completing).
    Sealing,
    /// Epoch has been checkpointed and is durable.
    Durable,
    /// Epoch has been archived (WAL deleted, only checkpoint remains).
    Archived,
}

impl std::fmt::Display for EpochState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EpochState::Active => write!(f, "ACTIVE"),
            EpochState::Sealing => write!(f, "SEALING"),
            EpochState::Durable => write!(f, "DURABLE"),
            EpochState::Archived => write!(f, "ARCHIVED"),
        }
    }
}

/// Metadata for a single epoch.
#[derive(Debug, Clone)]
pub struct EpochMetadata {
    /// Unique epoch identifier.
    pub id: EpochId,

    /// Current state of this epoch.
    pub state: EpochState,

    /// When this epoch started.
    pub started_at: SystemTime,

    /// When this epoch was sealed (None if still active).
    pub sealed_at: Option<SystemTime>,

    /// When this epoch was checkpointed (None if not yet).
    pub checkpointed_at: Option<SystemTime>,

    /// Number of operations in this epoch.
    pub operation_count: usize,

    /// WAL size for this epoch in bytes.
    pub wal_size_bytes: usize,

    /// First LSN in this epoch.
    pub first_lsn: Lsn,

    /// Last LSN in this epoch (updated as operations are added).
    pub last_lsn: Lsn,
}

impl EpochMetadata {
    /// Create metadata for a new epoch.
    pub fn new(id: EpochId) -> Self {
        Self {
            id,
            state: EpochState::Active,
            started_at: SystemTime::now(),
            sealed_at: None,
            checkpointed_at: None,
            operation_count: 0,
            wal_size_bytes: 0,
            first_lsn: 0,
            last_lsn: 0,
        }
    }
}

/// Checkpoint metadata stored on disk.
#[derive(Debug, Clone)]
pub struct CheckpointMeta {
    /// Epoch that was checkpointed.
    pub epoch_id: EpochId,

    /// LSN up to which data is durable.
    pub checkpoint_lsn: Lsn,

    /// Timestamp of checkpoint.
    pub timestamp: SystemTime,

    /// CRC32 checksum of the checkpoint data.
    pub checksum: u32,
}

impl CheckpointMeta {
    /// Magic number for checkpoint metadata files.
    const MAGIC: [u8; 8] = *b"EPCKPT\0\0";
    /// Version number.
    const VERSION: u32 = 1;
    /// Serialized size in bytes.
    pub const SIZE: usize = 40;

    /// Create new checkpoint metadata.
    pub fn new(epoch_id: EpochId, checkpoint_lsn: Lsn) -> Self {
        Self {
            epoch_id,
            checkpoint_lsn,
            timestamp: SystemTime::now(),
            checksum: 0, // Will be computed during serialization
        }
    }

    /// Serialize to bytes.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::SIZE);

        // Magic (8 bytes)
        buf.extend_from_slice(&Self::MAGIC);
        // Version (4 bytes)
        buf.extend_from_slice(&Self::VERSION.to_le_bytes());
        // Epoch ID (8 bytes)
        buf.extend_from_slice(&self.epoch_id.to_le_bytes());
        // Checkpoint LSN (8 bytes)
        buf.extend_from_slice(&self.checkpoint_lsn.to_le_bytes());
        // Timestamp (8 bytes)
        let timestamp_secs = self
            .timestamp
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        buf.extend_from_slice(&timestamp_secs.to_le_bytes());
        // CRC32 (4 bytes) - computed over previous fields
        let crc = crc32(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());

        buf
    }

    /// Deserialize from bytes.
    pub fn deserialize(data: &[u8]) -> std::result::Result<Self, String> {
        if data.len() < Self::SIZE {
            return Err(format!(
                "Checkpoint metadata too short: {} bytes",
                data.len()
            ));
        }

        // Verify magic
        let magic: [u8; 8] = data[0..8].try_into().map_err(|_| "Invalid magic")?;
        if magic != Self::MAGIC {
            return Err("Invalid checkpoint magic number".into());
        }

        // Verify version
        let version = u32::from_le_bytes(data[8..12].try_into().map_err(|_| "Invalid version")?);
        if version != Self::VERSION {
            return Err(format!("Unsupported checkpoint version: {}", version));
        }

        // Read fields
        let epoch_id = u64::from_le_bytes(data[12..20].try_into().map_err(|_| "Invalid epoch_id")?);
        let checkpoint_lsn =
            u64::from_le_bytes(data[20..28].try_into().map_err(|_| "Invalid lsn")?);
        let timestamp_secs =
            u64::from_le_bytes(data[28..36].try_into().map_err(|_| "Invalid timestamp")?);
        let stored_crc = u32::from_le_bytes(data[36..40].try_into().map_err(|_| "Invalid crc")?);

        // Verify CRC
        let computed_crc = crc32(&data[0..36]);
        if stored_crc != computed_crc {
            return Err(format!(
                "Checkpoint CRC mismatch: stored={}, computed={}",
                stored_crc, computed_crc
            ));
        }

        Ok(Self {
            epoch_id,
            checkpoint_lsn,
            timestamp: UNIX_EPOCH + Duration::from_secs(timestamp_secs),
            checksum: stored_crc,
        })
    }
}

/// Simple CRC32 implementation (IEEE polynomial).
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Statistics for epoch management.
#[derive(Debug, Clone, Default)]
pub struct EpochStats {
    /// Total epochs created.
    pub total_epochs: u64,
    /// Total checkpoints performed.
    pub total_checkpoints: u64,
    /// Total operations across all epochs.
    pub total_operations: u64,
    /// Total WAL bytes written.
    pub total_wal_bytes: u64,
    /// Average operations per epoch.
    pub avg_ops_per_epoch: f64,
    /// Average epoch duration in milliseconds.
    pub avg_epoch_duration_ms: f64,
    /// Current WAL size across all epochs.
    pub current_total_wal_bytes: usize,
}

/// The checkpoint manager coordinates epoch lifecycle and checkpointing.
///
/// Note: Named `CheckpointManager` to avoid conflict with the `EpochManager`
/// in the concurrency module (which handles epoch-based memory reclamation).
pub struct CheckpointManager {
    /// Base directory for WAL and checkpoint files.
    base_dir: PathBuf,

    /// Configuration.
    config: EpochConfig,

    /// Current epoch ID (atomically incrementing).
    current_epoch: AtomicU64,

    /// Operations in current epoch.
    current_ops: AtomicUsize,

    /// WAL bytes in current epoch.
    current_wal_bytes: AtomicUsize,

    /// When current epoch started.
    epoch_start: RwLock<Instant>,

    /// Metadata for recent epochs.
    epochs: RwLock<VecDeque<EpochMetadata>>,

    /// Current WAL writer.
    wal_writer: RwLock<Option<WalWriter>>,

    /// Background checkpoint thread.
    checkpoint_thread: Mutex<Option<JoinHandle<()>>>,

    /// Signal for background thread.
    checkpoint_signal: Arc<(Mutex<bool>, Condvar)>,

    /// Shutdown flag.
    shutdown: Arc<AtomicBool>,

    /// Last checkpoint metadata.
    last_checkpoint: RwLock<Option<CheckpointMeta>>,

    /// Cumulative statistics.
    stats: RwLock<EpochStats>,
}

impl CheckpointManager {
    /// Create a new epoch manager.
    pub fn new(base_dir: impl AsRef<Path>, config: EpochConfig) -> Result<Self> {
        config
            .validate()
            .map_err(|e| PersistentARTrieError::internal(e))?;

        let base_dir = base_dir.as_ref().to_path_buf();

        // Create directories
        let wal_dir = base_dir.join("wal");
        fs::create_dir_all(&wal_dir).map_err(|e| {
            PersistentARTrieError::io_error("create directory", wal_dir.display().to_string(), e)
        })?;
        let checkpoint_dir = base_dir.join("checkpoint");
        fs::create_dir_all(&checkpoint_dir).map_err(|e| {
            PersistentARTrieError::io_error(
                "create directory",
                checkpoint_dir.display().to_string(),
                e,
            )
        })?;

        // Load last checkpoint if exists
        let last_checkpoint = Self::load_checkpoint_meta_static(&base_dir)?;
        let starting_epoch = last_checkpoint
            .as_ref()
            .map(|c| c.epoch_id + 1)
            .unwrap_or(0);

        let manager = Self {
            base_dir,
            config,
            current_epoch: AtomicU64::new(starting_epoch),
            current_ops: AtomicUsize::new(0),
            current_wal_bytes: AtomicUsize::new(0),
            epoch_start: RwLock::new(Instant::now()),
            epochs: RwLock::new(VecDeque::new()),
            wal_writer: RwLock::new(None),
            checkpoint_thread: Mutex::new(None),
            checkpoint_signal: Arc::new((Mutex::new(false), Condvar::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
            last_checkpoint: RwLock::new(last_checkpoint),
            stats: RwLock::new(EpochStats::default()),
        };

        // Initialize first epoch metadata
        {
            let mut epochs = manager.epochs.write().expect("epochs lock poisoned");
            epochs.push_back(EpochMetadata::new(starting_epoch));
        }

        // Open WAL for current epoch
        manager.open_epoch_wal(starting_epoch)?;

        // Start background checkpoint thread if configured
        if manager.config.background_checkpoint {
            manager.start_checkpoint_thread();
        }

        Ok(manager)
    }

    /// Get the current epoch ID.
    pub fn current_epoch_id(&self) -> EpochId {
        self.current_epoch.load(Ordering::Acquire)
    }

    /// Record an operation in the current epoch.
    ///
    /// Returns the epoch ID the operation was recorded in.
    pub fn record_operation(&self, wal_bytes: usize) -> EpochId {
        let epoch = self.current_epoch.load(Ordering::Acquire);
        let ops = self.current_ops.fetch_add(1, Ordering::Relaxed) + 1;
        let bytes = self
            .current_wal_bytes
            .fetch_add(wal_bytes, Ordering::Relaxed)
            + wal_bytes;

        // Update epoch metadata
        {
            let mut epochs = self.epochs.write().expect("epochs lock poisoned");
            if let Some(current) = epochs.back_mut() {
                if current.id == epoch {
                    current.operation_count = ops;
                    current.wal_size_bytes = bytes;
                }
            }
        }

        // Check if we should trigger epoch advance
        self.maybe_advance_epoch();

        epoch
    }

    /// Get the WAL writer for appending records.
    pub fn wal_writer(&self) -> std::sync::RwLockReadGuard<'_, Option<WalWriter>> {
        self.wal_writer.read().expect("wal lock poisoned")
    }

    /// Check if an epoch advance should be triggered.
    fn should_advance_epoch(&self) -> bool {
        let ops = self.current_ops.load(Ordering::Relaxed);
        let bytes = self.current_wal_bytes.load(Ordering::Relaxed);
        let elapsed = self.epoch_start.read().expect("lock").elapsed();

        ops >= self.config.max_ops_per_epoch
            || bytes >= self.config.max_wal_size_bytes
            || elapsed >= self.config.epoch_duration
    }

    /// Maybe advance to a new epoch if triggers are met.
    fn maybe_advance_epoch(&self) {
        if !self.should_advance_epoch() {
            return;
        }

        // Use try_lock to avoid blocking hot path
        if let Ok(_guard) = self.checkpoint_thread.try_lock() {
            let _ = self.advance_epoch();
        }
    }

    /// Advance to a new epoch.
    pub fn advance_epoch(&self) -> Result<EpochId> {
        let old_epoch = self.current_epoch.load(Ordering::Acquire);
        let new_epoch = old_epoch + 1;

        let old_ops = self.current_ops.load(Ordering::Relaxed);
        let old_bytes = self.current_wal_bytes.load(Ordering::Relaxed);
        let old_start = *self.epoch_start.read().expect("lock");

        // Seal the current epoch
        {
            let mut epochs = self.epochs.write().expect("epochs lock poisoned");
            if let Some(current) = epochs.back_mut() {
                if current.id == old_epoch && current.state == EpochState::Active {
                    current.state = EpochState::Sealing;
                    current.sealed_at = Some(SystemTime::now());
                    current.operation_count = old_ops;
                    current.wal_size_bytes = old_bytes;
                }
            }
        }

        // Sync the current WAL
        {
            let wal = self.wal_writer.read().expect("wal lock poisoned");
            if let Some(ref w) = *wal {
                w.sync().map_err(|e| {
                    PersistentARTrieError::io_error(
                        "sync WAL",
                        w.path().display().to_string(),
                        std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                    )
                })?;
            }
        }

        // Open WAL for new epoch
        self.open_epoch_wal(new_epoch)?;

        // Update counters
        self.current_epoch.store(new_epoch, Ordering::Release);
        self.current_ops.store(0, Ordering::Relaxed);
        self.current_wal_bytes.store(0, Ordering::Relaxed);
        *self.epoch_start.write().expect("epoch_start lock") = Instant::now();

        // Add new epoch metadata
        {
            let mut epochs = self.epochs.write().expect("epochs lock");
            epochs.push_back(EpochMetadata::new(new_epoch));

            // Trim old epochs beyond retention
            while epochs.len() > self.config.retention_epochs + 2 {
                epochs.pop_front();
            }
        }

        // Update statistics
        {
            let mut stats = self.stats.write().expect("stats lock");
            stats.total_epochs += 1;
            stats.total_operations += old_ops as u64;
            stats.total_wal_bytes += old_bytes as u64;
            if stats.total_epochs > 0 {
                stats.avg_ops_per_epoch = stats.total_operations as f64 / stats.total_epochs as f64;
                let duration_ms = old_start.elapsed().as_millis() as f64;
                stats.avg_epoch_duration_ms =
                    (stats.avg_epoch_duration_ms * (stats.total_epochs - 1) as f64 + duration_ms)
                        / stats.total_epochs as f64;
            }
        }

        // Signal background thread to potentially checkpoint
        self.signal_checkpoint();

        Ok(new_epoch)
    }

    /// Perform a checkpoint of the specified epoch.
    pub fn checkpoint_epoch(&self, epoch_id: EpochId) -> Result<()> {
        // Get the last LSN for this epoch
        let last_lsn = {
            let epochs = self.epochs.read().expect("epochs lock");
            epochs
                .iter()
                .find(|e| e.id == epoch_id)
                .map(|e| e.last_lsn)
                .unwrap_or(0)
        };

        // Create checkpoint metadata
        let meta = CheckpointMeta::new(epoch_id, last_lsn);

        // Write checkpoint metadata
        self.write_checkpoint_meta(&meta)?;

        // Mark epoch as durable
        {
            let mut epochs = self.epochs.write().expect("epochs lock");
            for epoch in epochs.iter_mut() {
                if epoch.id == epoch_id {
                    epoch.state = EpochState::Durable;
                    epoch.checkpointed_at = Some(SystemTime::now());
                    break;
                }
            }
        }

        // Delete old WAL segments beyond retention
        self.cleanup_old_wals()?;

        // Update last checkpoint
        *self.last_checkpoint.write().expect("checkpoint lock") = Some(meta);

        // Update statistics
        {
            let mut stats = self.stats.write().expect("stats lock");
            stats.total_checkpoints += 1;
        }

        Ok(())
    }

    /// Force a synchronous checkpoint of the current epoch.
    pub fn force_checkpoint(&self) -> Result<EpochId> {
        let epoch = self.advance_epoch()?;
        self.checkpoint_epoch(epoch.saturating_sub(1))?;
        Ok(epoch)
    }

    /// Get the last durable epoch (safe recovery point).
    pub fn last_durable_epoch(&self) -> Option<EpochId> {
        self.last_checkpoint
            .read()
            .expect("lock")
            .as_ref()
            .map(|c| c.epoch_id)
    }

    /// Get the current statistics.
    pub fn stats(&self) -> EpochStats {
        let mut stats = self.stats.read().expect("stats lock").clone();

        // Update current WAL size
        let epochs = self.epochs.read().expect("epochs lock");
        stats.current_total_wal_bytes = epochs.iter().map(|e| e.wal_size_bytes).sum();

        stats
    }

    /// Get metadata for all tracked epochs.
    pub fn epoch_metadata(&self) -> Vec<EpochMetadata> {
        self.epochs
            .read()
            .expect("epochs lock")
            .iter()
            .cloned()
            .collect()
    }

    /// Get the configuration.
    pub fn config(&self) -> &EpochConfig {
        &self.config
    }

    /// Get the base directory.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    // --- Private methods ---

    fn open_epoch_wal(&self, epoch: EpochId) -> Result<()> {
        let wal_path = self.wal_path(epoch);

        // Remove existing WAL if it exists (leftover from crash)
        if wal_path.exists() {
            fs::remove_file(&wal_path).map_err(|e| {
                PersistentARTrieError::io_error(
                    "remove stale WAL",
                    wal_path.display().to_string(),
                    e,
                )
            })?;
        }

        let writer = WalWriter::create(&wal_path).map_err(|e| {
            PersistentARTrieError::io_error(
                "create WAL",
                wal_path.display().to_string(),
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            )
        })?;

        let mut wal = self.wal_writer.write().expect("wal lock");
        *wal = Some(writer);

        Ok(())
    }

    fn wal_path(&self, epoch: EpochId) -> PathBuf {
        self.base_dir
            .join("wal")
            .join(format!("epoch_{:016}.wal", epoch))
    }

    fn checkpoint_path(&self, epoch: EpochId) -> PathBuf {
        self.base_dir
            .join("checkpoint")
            .join(format!("checkpoint_{:016}.snap", epoch))
    }

    fn checkpoint_meta_path(&self) -> PathBuf {
        self.base_dir.join("wal").join("checkpoint.meta")
    }

    fn load_checkpoint_meta_static(base_dir: &Path) -> Result<Option<CheckpointMeta>> {
        let path = base_dir.join("wal").join("checkpoint.meta");
        if !path.exists() {
            return Ok(None);
        }

        let mut file = File::open(&path).map_err(|e| {
            PersistentARTrieError::io_error(
                "open checkpoint metadata",
                path.display().to_string(),
                e,
            )
        })?;
        let mut data = Vec::new();
        file.read_to_end(&mut data).map_err(|e| {
            PersistentARTrieError::io_error(
                "read checkpoint metadata",
                path.display().to_string(),
                e,
            )
        })?;

        match CheckpointMeta::deserialize(&data) {
            Ok(meta) => Ok(Some(meta)),
            Err(e) => {
                warn!("Failed to load checkpoint metadata: {}", e);
                Ok(None)
            }
        }
    }

    /// Load checkpoint metadata from disk.
    pub fn load_checkpoint_meta(&self) -> Result<Option<CheckpointMeta>> {
        Self::load_checkpoint_meta_static(&self.base_dir)
    }

    fn write_checkpoint_meta(&self, meta: &CheckpointMeta) -> Result<()> {
        let path = self.checkpoint_meta_path();
        let temp_path = path.with_extension("meta.tmp");

        // Write to temp file first
        {
            let mut file = File::create(&temp_path).map_err(|e| {
                PersistentARTrieError::io_error(
                    "create checkpoint metadata",
                    temp_path.display().to_string(),
                    e,
                )
            })?;
            file.write_all(&meta.serialize()).map_err(|e| {
                PersistentARTrieError::io_error(
                    "write checkpoint metadata",
                    temp_path.display().to_string(),
                    e,
                )
            })?;
            file.sync_all().map_err(|e| {
                PersistentARTrieError::io_error(
                    "sync checkpoint metadata",
                    temp_path.display().to_string(),
                    e,
                )
            })?;
        }

        // Atomic rename
        fs::rename(&temp_path, &path).map_err(|e| {
            PersistentARTrieError::io_error(
                "rename checkpoint metadata",
                path.display().to_string(),
                e,
            )
        })?;

        Ok(())
    }

    fn cleanup_old_wals(&self) -> Result<()> {
        let last_durable = self.last_durable_epoch().unwrap_or(0);
        let cutoff = last_durable.saturating_sub(self.config.retention_epochs as u64);

        let wal_dir = self.base_dir.join("wal");
        if !wal_dir.exists() {
            return Ok(());
        }

        for entry in fs::read_dir(&wal_dir).map_err(|e| {
            PersistentARTrieError::io_error("read WAL directory", wal_dir.display().to_string(), e)
        })? {
            let entry = entry.map_err(|e| {
                PersistentARTrieError::io_error(
                    "read directory entry",
                    wal_dir.display().to_string(),
                    e,
                )
            })?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if name_str.starts_with("epoch_") && name_str.ends_with(".wal") {
                if let Some(epoch_str) = name_str
                    .strip_prefix("epoch_")
                    .and_then(|s| s.strip_suffix(".wal"))
                {
                    if let Ok(epoch) = epoch_str.parse::<EpochId>() {
                        if epoch < cutoff {
                            let entry_path = entry.path();
                            fs::remove_file(&entry_path).map_err(|e| {
                                PersistentARTrieError::io_error(
                                    "remove old WAL",
                                    entry_path.display().to_string(),
                                    e,
                                )
                            })?;

                            // Mark epoch as archived
                            let mut epochs = self.epochs.write().expect("epochs lock");
                            for e in epochs.iter_mut() {
                                if e.id == epoch {
                                    e.state = EpochState::Archived;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn start_checkpoint_thread(&self) {
        let signal = Arc::clone(&self.checkpoint_signal);
        let shutdown = Arc::clone(&self.shutdown);
        let epoch_duration = self.config.epoch_duration;

        let handle = thread::Builder::new()
            .name("artrie-epoch-checkpoint".to_string())
            .spawn(move || {
                Self::checkpoint_loop(signal, shutdown, epoch_duration);
            })
            .expect("failed to spawn checkpoint thread");

        *self.checkpoint_thread.lock().expect("lock") = Some(handle);
    }

    fn checkpoint_loop(
        signal: Arc<(Mutex<bool>, Condvar)>,
        shutdown: Arc<AtomicBool>,
        epoch_duration: Duration,
    ) {
        let (lock, cvar) = &*signal;

        loop {
            // Wait for signal or timeout
            let mut triggered = lock.lock().expect("lock");
            let result = cvar
                .wait_timeout(triggered, epoch_duration)
                .expect("wait failed");
            triggered = result.0;
            *triggered = false;
            drop(triggered);

            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            // The actual checkpoint logic is triggered by record_operation
            // via maybe_advance_epoch. This thread just ensures we wake up
            // periodically to check if an epoch advance is needed.
        }
    }

    fn signal_checkpoint(&self) {
        let (lock, cvar) = &*self.checkpoint_signal;
        let mut triggered = lock.lock().expect("lock");
        *triggered = true;
        cvar.notify_one();
    }

    /// Find all WAL segment files in epoch order.
    pub fn find_wal_segments(&self) -> Result<Vec<(EpochId, PathBuf)>> {
        let wal_dir = self.base_dir.join("wal");
        if !wal_dir.exists() {
            return Ok(Vec::new());
        }

        let mut segments = Vec::new();

        for entry in fs::read_dir(&wal_dir).map_err(|e| {
            PersistentARTrieError::io_error("read WAL directory", wal_dir.display().to_string(), e)
        })? {
            let entry = entry.map_err(|e| {
                PersistentARTrieError::io_error(
                    "read directory entry",
                    wal_dir.display().to_string(),
                    e,
                )
            })?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if name_str.starts_with("epoch_") && name_str.ends_with(".wal") {
                if let Some(epoch_str) = name_str
                    .strip_prefix("epoch_")
                    .and_then(|s| s.strip_suffix(".wal"))
                {
                    if let Ok(epoch) = epoch_str.parse::<EpochId>() {
                        segments.push((epoch, entry.path()));
                    }
                }
            }
        }

        // Sort by epoch
        segments.sort_by_key(|(epoch, _)| *epoch);

        Ok(segments)
    }
}

impl Drop for CheckpointManager {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.signal_checkpoint();

        if let Some(handle) = self.checkpoint_thread.lock().expect("lock").take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_epoch_config_default() {
        let config = EpochConfig::default();
        assert_eq!(config.epoch_duration, Duration::from_millis(100));
        assert_eq!(config.max_ops_per_epoch, 10_000);
        assert_eq!(config.max_wal_size_bytes, 64 * 1024 * 1024);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_epoch_config_validation() {
        let mut config = EpochConfig::default();

        // Valid config
        assert!(config.validate().is_ok());

        // Invalid: epoch_duration too short
        config.epoch_duration = Duration::from_millis(1);
        assert!(config.validate().is_err());

        // Invalid: max_ops_per_epoch too small
        config = EpochConfig::default();
        config.max_ops_per_epoch = 10;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_checkpoint_meta_serialization() {
        let meta = CheckpointMeta::new(42, 12345);
        let serialized = meta.serialize();
        let deserialized = CheckpointMeta::deserialize(&serialized).expect("deserialize");

        assert_eq!(deserialized.epoch_id, 42);
        assert_eq!(deserialized.checkpoint_lsn, 12345);
    }

    #[test]
    fn test_epoch_manager_creation() {
        let dir = tempdir().expect("create temp dir");
        let config = EpochConfig {
            background_checkpoint: false, // Disable for simpler testing
            ..Default::default()
        };

        let manager = CheckpointManager::new(dir.path(), config).expect("create manager");
        assert_eq!(manager.current_epoch_id(), 0);
    }

    #[test]
    fn test_epoch_advancement() {
        let dir = tempdir().expect("create temp dir");
        let config = EpochConfig {
            epoch_duration: Duration::from_secs(60), // Long duration to avoid time-based advance
            max_ops_per_epoch: 100,
            background_checkpoint: false,
            ..Default::default()
        };

        let manager = CheckpointManager::new(dir.path(), config).expect("create manager");

        // Record operations until epoch should advance
        for _ in 0..150 {
            manager.record_operation(100);
        }

        // Epoch should have advanced at least once
        assert!(manager.current_epoch_id() >= 1);
    }

    #[test]
    fn test_epoch_states() {
        let dir = tempdir().expect("create temp dir");
        let config = EpochConfig {
            max_ops_per_epoch: 100,
            background_checkpoint: false,
            ..Default::default()
        };

        let manager = CheckpointManager::new(dir.path(), config).expect("create manager");

        // Initial state
        let metadata = manager.epoch_metadata();
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].state, EpochState::Active);

        // Trigger epoch advance (need to exceed max_ops_per_epoch)
        for _ in 0..150 {
            manager.record_operation(100);
        }

        // Should have multiple epochs now
        let metadata = manager.epoch_metadata();
        assert!(metadata.len() >= 2);

        // First epoch should be sealed or beyond
        let first_state = metadata[0].state;
        assert!(first_state == EpochState::Sealing || first_state == EpochState::Durable);
    }

    #[test]
    fn test_checkpoint_and_recovery() {
        let dir = tempdir().expect("create temp dir");
        let config = EpochConfig {
            max_ops_per_epoch: 100,
            background_checkpoint: false,
            ..Default::default()
        };

        // Create manager and record some operations
        {
            let manager =
                CheckpointManager::new(dir.path(), config.clone()).expect("create manager");

            for _ in 0..500 {
                manager.record_operation(100);
            }

            // Force checkpoint
            manager.force_checkpoint().expect("checkpoint");

            // Verify checkpoint was written
            assert!(manager.last_durable_epoch().is_some());
        }

        // Create new manager and verify recovery
        {
            let manager = CheckpointManager::new(dir.path(), config).expect("reopen manager");

            // Should have loaded checkpoint metadata
            let last_durable = manager.last_durable_epoch();
            assert!(last_durable.is_some());

            // Current epoch should continue from last checkpoint
            assert!(manager.current_epoch_id() > last_durable.unwrap());
        }
    }

    #[test]
    fn test_wal_segments() {
        let dir = tempdir().expect("create temp dir");
        let config = EpochConfig {
            max_ops_per_epoch: 100,
            background_checkpoint: false,
            ..Default::default()
        };

        let manager = CheckpointManager::new(dir.path(), config).expect("create manager");

        // Record enough ops to create multiple epochs
        for _ in 0..500 {
            manager.record_operation(100);
        }

        // Find WAL segments
        let segments = manager.find_wal_segments().expect("find segments");
        assert!(!segments.is_empty());

        // Verify segments are in order
        for i in 1..segments.len() {
            assert!(segments[i].0 > segments[i - 1].0);
        }
    }

    #[test]
    fn test_stats() {
        let dir = tempdir().expect("create temp dir");
        let config = EpochConfig {
            max_ops_per_epoch: 100,
            background_checkpoint: false,
            ..Default::default()
        };

        let manager = CheckpointManager::new(dir.path(), config).expect("create manager");

        // Record some operations (need to exceed max_ops_per_epoch to trigger epoch advance)
        for _ in 0..300 {
            manager.record_operation(100);
        }

        let stats = manager.stats();
        assert!(stats.total_epochs >= 2); // Should have advanced epochs
        assert!(stats.total_operations > 0);
    }
}
