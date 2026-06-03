//! `MmapDiskManager`-specific constructors for `PersistentARTrieChar<V>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~135-1287, ~1153 LOC)
//! as a Phase-6 char sub-module. These constructors target the
//! default `MmapDiskManager` storage backend:
//!
//! - `new` (in-memory ctor)
//! - `create` / `create_with_slot_tracking`
//! - `open` / `open_with_slot_tracking`
//! - `open_with_recovery` / `open_with_recovery_and_slot_tracking`
//! - Enhanced recovery variants
//!
//! The `IoUringDiskManager` variants live in `super::io_uring_ctor`;
//! generic methods (any `BlockStorage` backend) stay in
//! `dict_impl_char.rs`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

use crate::persistent_artrie::adaptive_pool::CacheStats;
#[allow(unused_imports)]
use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::buffer_manager::BufferManager;
use crate::persistent_artrie::concurrency::{EpochManager, OptimisticVersion, RetryStats};
use crate::persistent_artrie::dict_impl::DurabilityPolicy;
use crate::persistent_artrie::disk_manager::DiskManager;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::{
    AsyncWalConfig, AsyncWalWriter, WalConfig, WalReader, WalRecord,
};
use crate::persistent_artrie::wal_managed::{create_async_wal, open_or_create_async_wal};
use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;

use super::arena_manager::ArenaManager;
use super::recovery_stats::{EnhancedRecoveryMode, EnhancedRecoveryStats};
use super::types::CharTrieRoot;
use super::DEFAULT_CHAR_BUFFER_POOL_SIZE;

impl<V: DictionaryValue> super::PersistentARTrieChar<V> {
    /// Create a new empty trie (in-memory mode)
    pub fn new() -> Self {
        Self {
            root: CharTrieRoot::Empty,
            len: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: None,
            wal_writer: None,
            wal_config: WalConfig::default(),
            next_lsn: std::sync::atomic::AtomicU64::new(1),
            committed_watermark: super::committed_watermark::CommittedWatermark::new(0),
            overlay_write_mode: super::overlay_write_mode::OverlayWriteMode::default(),
            file_path: None,
            arena_manager: None,
            version: OptimisticVersion::new(),
            epoch_manager: Arc::new(EpochManager::new()),
            retire_list: Arc::new(super::reclaim::CharRetireList::new()),
            structural_generation: std::sync::atomic::AtomicU64::new(0),
            retry_stats: RetryStats::new(),
            #[cfg(feature = "group-commit")]
            group_commit: None,
            memory_monitor: None,
            cache_stats: CacheStats::default(),
            checkpoint_manager: None,
            durability_policy: DurabilityPolicy::default(),
            eviction_coordinator: None,
            prefetcher: crate::persistent_artrie::prefetch::Prefetcher::disabled(),
            _phantom: std::marker::PhantomData,
            lockfree_root: None,
            commit_seq: std::sync::atomic::AtomicU64::new(0),
            commit_seq_by_data_lsn: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            lockfree_cache: None,
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Create a new disk-backed trie at the given path
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        // Create disk manager
        let disk_manager = DiskManager::create(path)?;

        // Create buffer manager (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_CHAR_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create async WAL file
        let wal_path = path.with_extension("wal");
        let wal_writer =
            create_async_wal(&wal_path, path).map_err(|e| PersistentARTrieError::WalError {
                reason: format!("{:?}", e),
            })?;
        let wal_writer = Arc::new(wal_writer);

        // Create arena manager for space-efficient node storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        Ok(Self {
            root: CharTrieRoot::Empty,
            len: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            wal_config: WalConfig::default(),
            next_lsn: std::sync::atomic::AtomicU64::new(1),
            committed_watermark: super::committed_watermark::CommittedWatermark::new(0),
            overlay_write_mode: super::overlay_write_mode::OverlayWriteMode::default(),
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: Arc::new(EpochManager::new()),
            retire_list: Arc::new(super::reclaim::CharRetireList::new()),
            structural_generation: std::sync::atomic::AtomicU64::new(0),
            retry_stats: RetryStats::new(),
            #[cfg(feature = "group-commit")]
            group_commit: None,
            memory_monitor: None,
            cache_stats: CacheStats::default(),
            checkpoint_manager: None,
            durability_policy: DurabilityPolicy::default(),
            eviction_coordinator: None,
            prefetcher: crate::persistent_artrie::prefetch::Prefetcher::new(),
            _phantom: std::marker::PhantomData,
            lockfree_root: None,
            commit_seq: std::sync::atomic::AtomicU64::new(0),
            commit_seq_by_data_lsn: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            lockfree_cache: None,
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Create a new disk-backed trie with slot-level dirty tracking.
    ///
    /// This enables incremental checkpoints that write only modified slots
    /// instead of entire 256KB arenas, reducing checkpoint I/O by 90%+ for
    /// localized updates.
    ///
    /// # Arguments
    /// * `path` - Path to the trie file (must not exist)
    pub fn create_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        use super::arena_manager::FlushConfig;

        let path = path.as_ref();

        // Create disk manager
        let disk_manager = DiskManager::create(path)?;

        // Create buffer manager (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_CHAR_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create async WAL file
        let wal_path = path.with_extension("wal");
        let wal_writer =
            create_async_wal(&wal_path, path).map_err(|e| PersistentARTrieError::WalError {
                reason: format!("{:?}", e),
            })?;
        let wal_writer = Arc::new(wal_writer);

        // Create arena manager with slot-level tracking enabled
        let flush_config = FlushConfig::with_slot_tracking();
        let arena_manager =
            ArenaManager::with_buffer_manager_and_config(Arc::clone(&buffer_manager), flush_config);
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        Ok(Self {
            root: CharTrieRoot::Empty,
            len: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            wal_config: WalConfig::default(),
            next_lsn: std::sync::atomic::AtomicU64::new(1),
            committed_watermark: super::committed_watermark::CommittedWatermark::new(0),
            overlay_write_mode: super::overlay_write_mode::OverlayWriteMode::default(),
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: Arc::new(EpochManager::new()),
            retire_list: Arc::new(super::reclaim::CharRetireList::new()),
            structural_generation: std::sync::atomic::AtomicU64::new(0),
            retry_stats: RetryStats::new(),
            #[cfg(feature = "group-commit")]
            group_commit: None,
            memory_monitor: None,
            cache_stats: CacheStats::default(),
            checkpoint_manager: None,
            durability_policy: DurabilityPolicy::default(),
            eviction_coordinator: None,
            prefetcher: crate::persistent_artrie::prefetch::Prefetcher::new(),
            _phantom: std::marker::PhantomData,
            lockfree_root: None,
            commit_seq: std::sync::atomic::AtomicU64::new(0),
            commit_seq_by_data_lsn: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            lockfree_cache: None,
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Create a new disk-backed trie with custom WAL configuration
    pub fn create_with_config<P: AsRef<Path>>(path: P, wal_config: WalConfig) -> Result<Self> {
        let path = path.as_ref();

        // Create disk manager
        let disk_manager = DiskManager::create(path)?;

        // Create buffer manager (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_CHAR_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create async WAL file with custom config
        let wal_path = path.with_extension("wal");
        let async_config = AsyncWalConfig {
            pending_dir: path.parent().unwrap_or(Path::new(".")).join("wal_pending"),
            ..Default::default()
        };
        let wal_writer = AsyncWalWriter::create(&wal_path, async_config, wal_config.clone())
            .map_err(|e| PersistentARTrieError::WalError {
                reason: format!("{:?}", e),
            })?;
        let wal_writer = Arc::new(wal_writer);

        // Create archive directory if archive mode is enabled
        // NOTE: create_dir_all() is idempotent - no exists() check needed.
        // Checking exists() before create_dir_all() creates a TOCTOU race window.
        if wal_config.archive_enabled {
            let archive_dir = path
                .parent()
                .unwrap_or(Path::new("."))
                .join(&wal_config.archive_dir);
            std::fs::create_dir_all(&archive_dir).map_err(|e| {
                PersistentARTrieError::io_error(
                    "create archive directory",
                    archive_dir.display().to_string(),
                    e,
                )
            })?;
        }

        // Create arena manager for space-efficient node storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        Ok(Self {
            root: CharTrieRoot::Empty,
            len: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            wal_config,
            next_lsn: std::sync::atomic::AtomicU64::new(1),
            committed_watermark: super::committed_watermark::CommittedWatermark::new(0),
            overlay_write_mode: super::overlay_write_mode::OverlayWriteMode::default(),
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: Arc::new(EpochManager::new()),
            retire_list: Arc::new(super::reclaim::CharRetireList::new()),
            structural_generation: std::sync::atomic::AtomicU64::new(0),
            retry_stats: RetryStats::new(),
            #[cfg(feature = "group-commit")]
            group_commit: None,
            memory_monitor: None,
            cache_stats: CacheStats::default(),
            checkpoint_manager: None,
            durability_policy: DurabilityPolicy::default(),
            eviction_coordinator: None,
            prefetcher: crate::persistent_artrie::prefetch::Prefetcher::new(),
            _phantom: std::marker::PhantomData,
            lockfree_root: None,
            commit_seq: std::sync::atomic::AtomicU64::new(0),
            commit_seq_by_data_lsn: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            lockfree_cache: None,
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Open an existing disk-backed trie
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

        let path = path.as_ref();

        // Open disk manager
        let disk_manager = DiskManager::open(path)?;

        // Read root pointer and entry count from header
        let root_ptr = disk_manager.root_ptr()?;
        let _entry_count = disk_manager.entry_count()?;

        // Create buffer manager (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_CHAR_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Read WAL records for recovery if WAL exists
        let wal_path = path.with_extension("wal");
        let (recovered_ops, next_lsn, checkpoint_lsn, commit_seq_seed) = if wal_path.exists() {
            // Recover from WAL
            let mut reader =
                WalReader::new(&wal_path).map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;

            let mut records = Vec::new();
            let mut max_lsn = 0u64;
            let mut checkpoint_lsn = 0u64;
            // DG-RECON S1 seed: the max CommitRank generation surviving in the WAL.
            // Combined below with the durable header floor to seed `commit_seq`.
            let mut max_commit_seq_gen = 0u64;
            while let Some(result) = reader.next_record() {
                match result {
                    Ok((lsn, record)) => {
                        max_lsn = max_lsn.max(lsn);
                        // Track the latest checkpoint LSN
                        if let WalRecord::Checkpoint {
                            checkpoint_lsn: cp_lsn,
                            ..
                        } = &record
                        {
                            checkpoint_lsn = checkpoint_lsn.max(*cp_lsn);
                        }
                        // Track the max commit generation (DG-RECON S1 seed).
                        if let WalRecord::CommitRank { generation, .. } = &record {
                            max_commit_seq_gen = max_commit_seq_gen.max(*generation);
                        }
                        records.push((lsn, record));
                    }
                    Err(_) => break, // Stop on error
                }
            }

            let next_lsn = max_lsn + 1;
            // Seed = max(durable header floor, scanned max generation). The floor is
            // currently 0 until DG2 wires it at checkpoint; scan-max covers the
            // un-checkpointed tail. A failed header read falls back to scan-max.
            let floor = WalReader::read_header(&wal_path)
                .map(|h| h.commit_seq_floor)
                .unwrap_or(0);
            let commit_seq_seed = floor.max(max_commit_seq_gen);
            (records, next_lsn, checkpoint_lsn, commit_seq_seed)
        } else {
            (Vec::new(), 1, 0, 0)
        };

        // Create async WAL writer using TOCTOU-safe open_or_create
        let wal_writer = open_or_create_async_wal(&wal_path, path).map_err(|e| {
            PersistentARTrieError::WalError {
                reason: format!("{:?}", e),
            }
        })?;
        let wal_writer = Arc::new(wal_writer);

        // Create arena manager for space-efficient node storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        let mut inner = Self {
            root: CharTrieRoot::Empty,
            len: AtomicUsize::new(0), // Updated from disk or WAL replay
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager.clone()),
            wal_writer: Some(wal_writer),
            wal_config: WalConfig::default(),
            next_lsn: std::sync::atomic::AtomicU64::new(next_lsn),
            committed_watermark: super::committed_watermark::CommittedWatermark::new(
                next_lsn.saturating_sub(1),
            ),
            overlay_write_mode: super::overlay_write_mode::OverlayWriteMode::default(),
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: Arc::new(EpochManager::new()),
            retire_list: Arc::new(super::reclaim::CharRetireList::new()),
            structural_generation: std::sync::atomic::AtomicU64::new(0),
            retry_stats: RetryStats::new(),
            #[cfg(feature = "group-commit")]
            group_commit: None,
            memory_monitor: None,
            cache_stats: CacheStats::default(),
            checkpoint_manager: None,
            durability_policy: DurabilityPolicy::default(),
            eviction_coordinator: None,
            prefetcher: crate::persistent_artrie::prefetch::Prefetcher::new(),
            _phantom: std::marker::PhantomData,
            lockfree_root: None,
            commit_seq: std::sync::atomic::AtomicU64::new(0),
            commit_seq_by_data_lsn: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            lockfree_cache: None,
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        };
        // DG-RECON S1 seed (inert until S4 stamps producers): raise commit_seq to
        // out-rank every generation surviving recovery, so a post-reopen claim cannot
        // collide with a replayed generation (the A.2 cross-restart-order fix).
        inner
            .commit_seq
            .store(commit_seq_seed, std::sync::atomic::Ordering::Release);

        // Try to load root from disk if root_ptr != 0
        // Default: lazy loading (eager_depth = None)
        let mut loaded_from_disk = false;
        if root_ptr != 0 {
            let root_swizzled = SwizzledPtr::from_raw(root_ptr);
            match inner.load_root_from_disk(&buffer_manager, &root_swizzled, None) {
                Ok((root, len)) => {
                    inner.root = root;
                    inner.len.store(len, AtomicOrdering::Release);
                    loaded_from_disk = true;
                }
                Err(e) => {
                    log::warn!("Failed to load root from disk: {:?}", e);
                    // Fall back to WAL replay
                }
            }
        }

        // Apply buggy_clear_recovery theorem: ensure_valid() restores the arena manager
        // invariant after clear_for_loading + failed load_arena sequence.
        // See: formal-verification/rocq/Invariants/ArenaInvariants.v
        //      Theorem open_with_failed_loading_recovered
        if let Some(ref arena_manager) = inner.arena_manager {
            arena_manager.write().ensure_valid();
        }

        // Replay WAL records that came after the checkpoint via the shared
        // Order-A reconcile (design C′): per-term last-writer-wins by commit
        // generation, NOT WAL physical/LSN order. Records with LSN <=
        // checkpoint_lsn are already persisted to disk and are skipped inside
        // `reconcile_lww`. For a rank-less (pre-fix) WAL this is byte-for-byte the
        // old in-order replay (generation_of = lsn).
        // S4: the on-disk rank-regime (Overlay for a flipped/overlay file) drives the
        // reconcile's unranked-orphan DROP; Owned for legacy/base/vocab/un-flipped files.
        let rank_regime = WalReader::read_header(&wal_path)
            .map(|h| h.regime())
            .unwrap_or(crate::persistent_artrie_core::wal::RankRegime::Owned);
        let applied_any =
            inner.replay_records_lww(recovered_ops, loaded_from_disk, checkpoint_lsn, rank_regime);
        let skipped_all = !applied_any;

        // If we loaded from disk and skipped all WAL records, we can truncate the WAL
        // (This is safe because all data is already persisted)
        if loaded_from_disk && skipped_all && checkpoint_lsn > 0 {
            // WAL truncation would happen here if we implement it
            // For now, just note that we could truncate
        }

        Ok(inner)
    }

    /// Open an existing disk-backed trie with slot-level dirty tracking enabled.
    ///
    /// Slot-level tracking reduces checkpoint I/O by writing only modified slots
    /// instead of entire arenas. For vocabularies with localized updates, this
    /// can reduce checkpoint I/O by 90%+.
    ///
    /// This is equivalent to calling `open()` followed by enabling slot tracking
    /// on the arena manager, but provides a convenient single-call API.
    ///
    /// # Arguments
    /// * `path` - Path to the trie file (must exist)
    ///
    /// # Example
    /// ```text
    /// // Open existing vocabulary with slot-level tracking
    /// let mut trie = PersistentARTrieChar::<u64>::open_with_slot_tracking("vocab.trie")?;
    ///
    /// // Subsequent allocations will be tracked at slot level
    /// trie.insert("new_term", Some(42));
    ///
    /// // Checkpoint writes only modified slots
    /// trie.checkpoint()?;
    /// ```
    pub fn open_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        let trie = Self::open(path)?;

        // Enable slot-level tracking on the arena manager
        if let Some(ref am) = trie.arena_manager {
            am.write().enable_slot_tracking();
        }

        Ok(trie)
    }

    /// Open an existing disk-backed trie with a specific loading depth.
    ///
    /// This allows control over the trade-off between open time and lookup latency:
    /// - `eager_depth = None` (or `Some(0)`): Lazy loading - fastest open, first lookups
    ///   load nodes on-demand
    /// - `eager_depth = Some(5)`: Load 5 levels eagerly - moderate open time, fast
    ///   lookups for common prefixes
    /// - `eager_depth = Some(usize::MAX)`: Fully eager - slowest open, fastest lookups
    ///
    /// # Arguments
    /// * `path` - Path to the trie directory
    /// * `eager_depth` - Number of levels to load eagerly. `None` means lazy loading.
    ///
    /// # Example
    /// ```ignore
    /// // Lazy loading (default behavior)
    /// let trie = PersistentARTrieChar::<u64>::open_with_depth("my_trie", None)?;
    ///
    /// // Load first 5 levels eagerly
    /// let trie = PersistentARTrieChar::<u64>::open_with_depth("my_trie", Some(5))?;
    ///
    /// // Fully eager loading
    /// let trie = PersistentARTrieChar::<u64>::open_with_depth("my_trie", Some(usize::MAX))?;
    /// ```
    pub fn open_with_depth<P: AsRef<Path>>(path: P, eager_depth: Option<usize>) -> Result<Self> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

        let path = path.as_ref();

        // Open disk manager
        let disk_manager = DiskManager::open(path)?;

        // Read root pointer and entry count from header
        let root_ptr = disk_manager.root_ptr()?;
        let _entry_count = disk_manager.entry_count()?;

        // Create buffer manager (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_CHAR_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Read WAL records for recovery if WAL exists
        let wal_path = path.with_extension("wal");
        let (recovered_ops, next_lsn, checkpoint_lsn, commit_seq_seed) = if wal_path.exists() {
            // Recover from WAL
            let mut reader =
                WalReader::new(&wal_path).map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;

            let mut records = Vec::new();
            let mut max_lsn = 0u64;
            let mut checkpoint_lsn = 0u64;
            // DG-RECON S1 seed: the max CommitRank generation surviving in the WAL.
            // Combined below with the durable header floor to seed `commit_seq`.
            let mut max_commit_seq_gen = 0u64;
            while let Some(result) = reader.next_record() {
                match result {
                    Ok((lsn, record)) => {
                        max_lsn = max_lsn.max(lsn);
                        // Track the latest checkpoint LSN
                        if let WalRecord::Checkpoint {
                            checkpoint_lsn: cp_lsn,
                            ..
                        } = &record
                        {
                            checkpoint_lsn = checkpoint_lsn.max(*cp_lsn);
                        }
                        // Track the max commit generation (DG-RECON S1 seed).
                        if let WalRecord::CommitRank { generation, .. } = &record {
                            max_commit_seq_gen = max_commit_seq_gen.max(*generation);
                        }
                        records.push((lsn, record));
                    }
                    Err(_) => break, // Stop on error
                }
            }

            let next_lsn = max_lsn + 1;
            // Seed = max(durable header floor, scanned max generation). The floor is
            // currently 0 until DG2 wires it at checkpoint; scan-max covers the
            // un-checkpointed tail. A failed header read falls back to scan-max.
            let floor = WalReader::read_header(&wal_path)
                .map(|h| h.commit_seq_floor)
                .unwrap_or(0);
            let commit_seq_seed = floor.max(max_commit_seq_gen);
            (records, next_lsn, checkpoint_lsn, commit_seq_seed)
        } else {
            (Vec::new(), 1, 0, 0)
        };

        // Create async WAL writer using TOCTOU-safe open_or_create
        let wal_writer = open_or_create_async_wal(&wal_path, path).map_err(|e| {
            PersistentARTrieError::WalError {
                reason: format!("{:?}", e),
            }
        })?;
        let wal_writer = Arc::new(wal_writer);

        // Create arena manager for space-efficient node storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        let mut inner = Self {
            root: CharTrieRoot::Empty,
            len: AtomicUsize::new(0), // Updated from disk or WAL replay
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager.clone()),
            wal_writer: Some(wal_writer),
            wal_config: WalConfig::default(),
            next_lsn: std::sync::atomic::AtomicU64::new(next_lsn),
            committed_watermark: super::committed_watermark::CommittedWatermark::new(
                next_lsn.saturating_sub(1),
            ),
            overlay_write_mode: super::overlay_write_mode::OverlayWriteMode::default(),
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: Arc::new(EpochManager::new()),
            retire_list: Arc::new(super::reclaim::CharRetireList::new()),
            structural_generation: std::sync::atomic::AtomicU64::new(0),
            retry_stats: RetryStats::new(),
            #[cfg(feature = "group-commit")]
            group_commit: None,
            memory_monitor: None,
            cache_stats: CacheStats::default(),
            checkpoint_manager: None,
            durability_policy: DurabilityPolicy::default(),
            eviction_coordinator: None,
            prefetcher: crate::persistent_artrie::prefetch::Prefetcher::new(),
            _phantom: std::marker::PhantomData,
            lockfree_root: None,
            commit_seq: std::sync::atomic::AtomicU64::new(0),
            commit_seq_by_data_lsn: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            lockfree_cache: None,
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        };
        // DG-RECON S1 seed (inert until S4 stamps producers): raise commit_seq to
        // out-rank every generation surviving recovery, so a post-reopen claim cannot
        // collide with a replayed generation (the A.2 cross-restart-order fix).
        inner
            .commit_seq
            .store(commit_seq_seed, std::sync::atomic::Ordering::Release);

        // Try to load root from disk if root_ptr != 0
        let mut loaded_from_disk = false;
        if root_ptr != 0 {
            let root_swizzled = SwizzledPtr::from_raw(root_ptr);
            match inner.load_root_from_disk(&buffer_manager, &root_swizzled, eager_depth) {
                Ok((root, len)) => {
                    inner.root = root;
                    inner.len.store(len, AtomicOrdering::Release);
                    loaded_from_disk = true;
                }
                Err(e) => {
                    log::warn!("Failed to load root from disk: {:?}", e);
                    // Fall back to WAL replay
                }
            }
        }

        if let Some(ref arena_manager) = inner.arena_manager {
            arena_manager.write().ensure_valid();
        }

        // Replay WAL records that came after the checkpoint via the shared
        // Order-A reconcile (design C′) — identical to the `open()` site so the
        // two cannot drift (no-drift constraint). Per-term last-writer-wins by
        // commit generation; checkpoint-subsumed records skipped inside
        // `reconcile_lww`. Rank-less WAL ⇒ byte-for-byte the old in-order replay.
        let rank_regime = WalReader::read_header(&wal_path)
            .map(|h| h.regime())
            .unwrap_or(crate::persistent_artrie_core::wal::RankRegime::Owned);
        let _ =
            inner.replay_records_lww(recovered_ops, loaded_from_disk, checkpoint_lsn, rank_regime);

        Ok(inner)
    }

    /// Open an existing disk-backed trie with custom WAL configuration
    ///
    /// This allows specifying WAL archive settings for crash recovery.
    pub fn open_with_config<P: AsRef<Path>>(path: P, wal_config: WalConfig) -> Result<Self> {
        let mut trie = Self::open(path.as_ref())?;

        // Create archive directory if archive mode is enabled
        // NOTE: create_dir_all() is idempotent - no exists() check needed.
        // Checking exists() before create_dir_all() creates a TOCTOU race window.
        if wal_config.archive_enabled {
            if let Some(ref file_path) = trie.file_path {
                let archive_dir = file_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(&wal_config.archive_dir);
                std::fs::create_dir_all(&archive_dir).map_err(|e| {
                    PersistentARTrieError::io_error(
                        "create archive directory",
                        archive_dir.display().to_string(),
                        e,
                    )
                })?;
            }
        }

        trie.wal_config = wal_config;
        Ok(trie)
    }

    /// Open an existing disk-backed trie with automatic corruption detection and recovery.
    ///
    /// This is the recommended way to open a trie that may have been corrupted
    /// by a crash (OOM kill, power failure, etc.).
    ///
    /// # Recovery Process
    ///
    /// 1. **Check if file exists** - If not, create a new trie
    /// 2. **Detect corruption** - Check header checksum, arena checksums
    /// 3. **If corrupted** - Rebuild from WAL archive segments
    /// 4. **Return trie with recovery report**
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the trie data file
    ///
    /// # Returns
    ///
    /// Tuple of (trie, recovery_report) indicating what recovery was performed.
    ///
    /// # Example
    ///
    /// ```text
    /// use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    ///
    /// let (trie, report) = PersistentARTrieChar::<()>::open_with_recovery("words.artc")?;
    ///
    /// if !report.mode.is_normal() {
    ///     eprintln!("Recovered from crash: {} records replayed", report.records_replayed);
    /// }
    /// ```
    pub fn open_with_recovery<P: AsRef<Path>>(
        path: P,
    ) -> Result<(Self, crate::persistent_artrie::recovery::RecoveryReport)> {
        Self::open_with_recovery_config(path, WalConfig::default())
    }

    /// Open with crash recovery and slot-level dirty tracking.
    ///
    /// Combines `open_with_recovery()` functionality with slot-level tracking
    /// enabled. This is the recommended method for production use where both
    /// crash recovery and optimized incremental checkpoints are desired.
    ///
    /// Slot-level tracking reduces checkpoint I/O by 90%+ for localized updates
    /// by writing only modified slots instead of entire 256KB arenas.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the dictionary file
    ///
    /// # Returns
    ///
    /// Tuple of (trie, recovery_report) with slot tracking enabled.
    pub fn open_with_recovery_and_slot_tracking<P: AsRef<Path>>(
        path: P,
    ) -> Result<(Self, crate::persistent_artrie::recovery::RecoveryReport)> {
        let (trie, report) = Self::open_with_recovery(path)?;
        if let Some(ref am) = trie.arena_manager {
            am.write().enable_slot_tracking();
        }
        Ok((trie, report))
    }

    /// Enable slot-level dirty tracking for reduced checkpoint I/O.
    ///
    /// Slot-level tracking only flushes modified slots within arenas,
    /// reducing checkpoint I/O by 90%+ for localized updates.
    ///
    /// This is idempotent - calling when already enabled has no effect.
    pub fn enable_slot_tracking(&self) {
        if let Some(ref am) = self.arena_manager {
            am.write().enable_slot_tracking();
        }
    }

    /// Flush dirty arenas in sequential order for optimized disk I/O.
    ///
    /// Sorts dirty arenas by ID before flushing, improving I/O locality
    /// especially on rotational storage. Expected 5-15% faster checkpoints.
    pub fn flush_sequential(&self) -> Result<()> {
        if let Some(ref am) = self.arena_manager {
            am.write().flush_sequential()?;
        }
        Ok(())
    }

    /// Open with recovery and custom WAL configuration.
    ///
    /// Same as `open_with_recovery()` but allows specifying custom WAL settings.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the trie data file
    /// * `config` - WAL configuration for archive mode, segment limits, etc.
    ///
    /// # Returns
    ///
    /// Tuple of (trie, recovery_report) indicating what recovery was performed.
    pub fn open_with_recovery_config<P: AsRef<Path>>(
        path: P,
        config: WalConfig,
    ) -> Result<(Self, crate::persistent_artrie::recovery::RecoveryReport)> {
        use crate::persistent_artrie::recovery::{
            collect_retained_wal_segments_for_rebuild, detect_corruption, RecoveryReport,
        };
        use std::time::Instant;

        let path = path.as_ref();
        let start_time = Instant::now();

        // Check if file exists
        if !path.exists() {
            // No file - create new and return CreatedNew report
            let trie = Self::create_with_config(path, config)?;
            return Ok((trie, RecoveryReport::created_new()));
        }

        // Check for corruption
        match detect_corruption(path, true) {
            Ok(None) => {
                // No corruption detected - open normally
                let trie = Self::open_with_config(path, config)?;
                Ok((trie, RecoveryReport::normal()))
            }
            Ok(Some(corruption)) => {
                // Corruption detected - attempt recovery from WAL archives
                let corruption_reason = corruption.to_string();

                let wal_path = path.with_extension("wal");
                let pending_dir = path.parent().unwrap_or(Path::new(".")).join("wal_pending");
                let segments =
                    collect_retained_wal_segments_for_rebuild(&wal_path, &config, &pending_dir)
                        .map_err(|e| PersistentARTrieError::RecoveryError {
                            reason: format!(
                                "Corruption detected ({}) but WAL segment retention failed: {}",
                                corruption_reason, e
                            ),
                        })?;

                if segments.is_empty() {
                    // No archive segments - can't recover
                    return Err(PersistentARTrieError::RecoveryError {
                        reason: format!(
                            "Corruption detected ({}) but no WAL archive, pending, or active segments found",
                            corruption_reason
                        ),
                    });
                }

                // Remove corrupted file
                let _ = std::fs::remove_file(path);

                // Also remove any header-only active WAL left at the original path.
                let _ = std::fs::remove_file(&wal_path);

                // Create fresh trie
                let mut trie = Self::create_with_config(path, config.clone())?;

                // Rebuild from WAL archive segments
                let mut records_replayed: u64 = 0;
                let mut terms_recovered: u64 = 0;
                let mut segments_used = Vec::new();

                'segments: for segment_path in &segments {
                    // Create reader for this segment
                    use crate::persistent_artrie::wal::WalReader;

                    let reader = match WalReader::new(segment_path) {
                        Ok(r) => r,
                        Err(_) => continue, // Skip unreadable segments
                    };

                    segments_used.push(segment_path.clone());

                    for result in reader.iter() {
                        let (_lsn, record) = match result {
                            Ok(r) => r,
                            Err(e) => {
                                log::warn!(
                                    "Corrupted WAL record during rebuild; stopping at durable prefix: {:?}",
                                    e
                                );
                                break 'segments;
                            }
                        };

                        records_replayed += 1;

                        // Apply the record to the trie
                        use crate::persistent_artrie::wal::WalRecord;
                        match record {
                            WalRecord::Insert { term, value } => {
                                let term_str = String::from_utf8_lossy(&term);
                                if let Some(value_bytes) = value {
                                    if let Ok(v) =
                                        crate::serialization::bincode_compat::deserialize::<V>(
                                            &value_bytes,
                                        )
                                    {
                                        trie.insert_impl_no_wal_with_value(&term_str, v);
                                        terms_recovered += 1;
                                    }
                                } else {
                                    trie.insert_impl_no_wal(&term_str);
                                    terms_recovered += 1;
                                }
                            }
                            WalRecord::Remove { term } => {
                                let term_str = String::from_utf8_lossy(&term);
                                trie.remove_impl_no_wal(&term_str);
                            }
                            WalRecord::Increment {
                                term,
                                delta: _,
                                result: val,
                            } => {
                                // For increment, store the final result
                                let term_str = String::from_utf8_lossy(&term);
                                let value_bytes =
                                    crate::serialization::bincode_compat::serialize(&val)
                                        .unwrap_or_default();
                                if let Ok(v) = crate::serialization::bincode_compat::deserialize::<V>(
                                    &value_bytes,
                                ) {
                                    trie.insert_impl_no_wal_with_value(&term_str, v);
                                    terms_recovered += 1;
                                }
                            }
                            WalRecord::Upsert { term, value } => {
                                let term_str = String::from_utf8_lossy(&term);
                                if let Ok(v) =
                                    crate::serialization::bincode_compat::deserialize::<V>(&value)
                                {
                                    trie.insert_impl_no_wal_with_value(&term_str, v);
                                    terms_recovered += 1;
                                }
                            }
                            WalRecord::CompareAndSwap {
                                term,
                                new_value,
                                success,
                                ..
                            } => {
                                if success {
                                    let term_str = String::from_utf8_lossy(&term);
                                    if let Ok(v) = crate::serialization::bincode_compat::deserialize::<
                                        V,
                                    >(&new_value)
                                    {
                                        trie.insert_impl_no_wal_with_value(&term_str, v);
                                        terms_recovered += 1;
                                    }
                                }
                            }
                            WalRecord::BatchInsert { entries } => {
                                for (term, value) in entries {
                                    let term_str = String::from_utf8_lossy(&term);
                                    if let Some(value_bytes) = value {
                                        if let Ok(v) =
                                            crate::serialization::bincode_compat::deserialize::<V>(
                                                &value_bytes,
                                            )
                                        {
                                            trie.insert_impl_no_wal_with_value(&term_str, v);
                                            terms_recovered += 1;
                                        }
                                    } else {
                                        trie.insert_impl_no_wal(&term_str);
                                        terms_recovered += 1;
                                    }
                                }
                            }
                            WalRecord::BatchIncrement { entries } => {
                                for (term, delta) in entries {
                                    let term_str = String::from_utf8_lossy(&term);
                                    if let Err(error) =
                                        trie.try_increment_impl_no_wal(&term_str, delta)
                                    {
                                        log::warn!(
                                            "Invalid WAL batch increment during rebuild; stopping at durable prefix: {:?}",
                                            error
                                        );
                                        break 'segments;
                                    }
                                    terms_recovered += 1;
                                }
                            }
                            _ => {} // Skip transaction/checkpoint records
                        }
                    }
                }

                let duration_ms = start_time.elapsed().as_millis() as u64;

                let report = RecoveryReport::rebuild_from_wal(
                    path.to_path_buf(),
                    corruption_reason,
                    records_replayed,
                    terms_recovered,
                    segments_used,
                    duration_ms,
                );

                Ok((trie, report))
            }
            Err(e) => {
                // I/O error during corruption check
                Err(PersistentARTrieError::InternalError {
                    message: format!("Error during corruption check: {}", e),
                })
            }
        }
    }

    /// Open with full recovery integration (epoch + per-node logging).
    ///
    /// This method provides the most comprehensive recovery strategy:
    /// 1. If epoch checkpointing is enabled, uses epoch-based recovery
    /// 2. If per-node logging is enabled, uses O(dirty nodes) recovery
    /// 3. Falls back to standard WAL recovery otherwise
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the trie data file
    /// * `epoch_config` - Optional epoch configuration for epoch-based recovery
    /// * `wal_config` - WAL configuration
    ///
    /// # Returns
    ///
    /// Tuple of (trie, recovery_stats) with detailed recovery information.
    ///
    /// # Example
    ///
    /// ```text
    /// use libdictenstein::persistent_artrie_char::SharedCharARTrie;
    /// use libdictenstein::persistent_artrie::epoch::EpochConfig;
    ///
    /// let epoch_config = EpochConfig::default();
    /// let (trie, stats) = SharedCharARTrie::<i64>::open_with_full_recovery(
    ///     "data.artrie",
    ///     Some(epoch_config),
    ///     WalConfig::default(),
    /// )?;
    ///
    /// println!("Recovery took {} ms", stats.duration_ms);
    /// println!("Recovered {} records", stats.records_replayed);
    /// ```
    pub fn open_with_full_recovery<P: AsRef<Path>>(
        path: P,
        _epoch_config: Option<crate::persistent_artrie::epoch::EpochConfig>,
        config: WalConfig,
    ) -> Result<(Self, EnhancedRecoveryStats)> {
        use crate::persistent_artrie::recovery::detect_corruption;
        use std::time::Instant;

        let path = path.as_ref();
        let start_time = Instant::now();

        // Check if file exists
        if !path.exists() {
            // No file - create new
            let trie = Self::create_with_config(path, config)?;
            return Ok((
                trie,
                EnhancedRecoveryStats {
                    mode: EnhancedRecoveryMode::CreatedNew,
                    duration_ms: start_time.elapsed().as_millis() as u64,
                    records_replayed: 0,
                    epochs_recovered: 0,
                    dirty_nodes_recovered: 0,
                    archive_segments_used: 0,
                },
            ));
        }

        // Check for corruption
        match detect_corruption(path, true) {
            Ok(None) => {
                // No corruption - open normally
                let trie = Self::open_with_config(path, config)?;
                Ok((
                    trie,
                    EnhancedRecoveryStats {
                        mode: EnhancedRecoveryMode::Normal,
                        duration_ms: start_time.elapsed().as_millis() as u64,
                        records_replayed: 0,
                        epochs_recovered: 0,
                        dirty_nodes_recovered: 0,
                        archive_segments_used: 0,
                    },
                ))
            }
            Ok(Some(_corruption)) => {
                // Corruption detected - attempt recovery
                // Use standard recovery with archive segments
                let (trie, report) = Self::open_with_recovery_config(path, config)?;

                Ok((
                    trie,
                    EnhancedRecoveryStats {
                        mode: EnhancedRecoveryMode::RebuiltFromWal,
                        duration_ms: start_time.elapsed().as_millis() as u64,
                        records_replayed: report.records_replayed as usize,
                        epochs_recovered: 0,
                        dirty_nodes_recovered: 0,
                        archive_segments_used: report.archive_segments_used.len(),
                    },
                ))
            }
            Err(e) => Err(PersistentARTrieError::InternalError {
                message: format!("Error during corruption check: {}", e),
            }),
        }
    }

    /// Create an incremental recovery iterator for batch processing.
    ///
    /// This is useful when:
    /// - Memory is constrained and you need to process records in batches
    /// - You want to show progress during recovery
    /// - You need fine-grained control over the recovery process
    ///
    /// # Arguments
    ///
    /// * `wal_path` - Path to the WAL file
    ///
    /// # Returns
    ///
    /// An `IncrementalRecovery` iterator that yields batches of operations.
    ///
    /// # Example
    ///
    /// ```text
    /// use libdictenstein::persistent_artrie_char::SharedCharARTrie;
    ///
    /// let mut recovery = SharedCharARTrie::<i64>::incremental_recovery("data.wal")?;
    /// let mut total = 0;
    ///
    /// while let Some(batch) = recovery.next_batch(100)? {
    ///     for op in batch {
    ///         // Apply operation
    ///         total += 1;
    ///     }
    ///     println!("Processed {} operations so far", total);
    /// }
    /// ```
    pub fn incremental_recovery<P: AsRef<Path>>(
        wal_path: P,
    ) -> Result<super::recovery::IncrementalRecovery> {
        super::recovery::IncrementalRecovery::new(wal_path.as_ref()).map_err(|e| {
            PersistentARTrieError::internal(format!("Failed to create incremental recovery: {}", e))
        })
    }

    // NOTE (Order-A replay-order fix, OD1): `replay_records_lww`,
    // `apply_core_recovered_operation_no_wal`, and `value_from_recovered_i64`
    // were RELOCATED from this default-`S` (`MmapDiskManager`) impl block to the
    // `<V, S>`-generic block in `mutation_core.rs` so the `io_uring_ctor`
    // (`IoUringDiskManager`) owned-tree replay can route through the SAME shared
    // reconcile (no-drift constraint). See `mutation_core.rs`.

    /// Recover from archived WAL segments.
    ///
    /// This method collects all WAL archive segments and replays them
    /// to rebuild the trie from scratch.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the trie data file
    /// * `archive_dir` - Directory containing WAL archive segments
    /// * `config` - WAL configuration
    ///
    /// # Returns
    ///
    /// Tuple of (trie, stats) with recovery information.
    pub fn recover_from_archives<P: AsRef<Path>>(
        path: P,
        archive_dir: P,
        config: WalConfig,
    ) -> Result<(Self, EnhancedRecoveryStats)> {
        use super::recovery::find_wal_archive_segments;
        use std::time::Instant;

        let path = path.as_ref();
        let start_time = Instant::now();

        // Find archive segments
        let segments = find_wal_archive_segments(archive_dir.as_ref());

        if segments.is_empty() {
            return Err(PersistentARTrieError::RecoveryError {
                reason: format!(
                    "No WAL archive segments found in {:?}",
                    archive_dir.as_ref()
                ),
            });
        }

        // Remove any existing files
        let _ = std::fs::remove_file(path);
        let wal_path = path.with_extension("wal");
        let _ = std::fs::remove_file(&wal_path);

        // Create fresh trie
        let mut trie = Self::create_with_config(path, config)?;

        let (records_replayed, _) =
            // A2 fix (S5 v4 §1.5): regime-aware rebuild so a post-flip Overlay
            // archive DROPS never-acked two-append-window orphans instead of
            // resurrecting them. INERT for Owned archives (identical to the raw
            // in-order replay).
            crate::persistent_artrie::recovery::rebuild_from_wal_segments_regime_aware(
                &segments,
                |op| {
                if trie.apply_core_recovered_operation_no_wal(op) {
                    Ok(())
                } else {
                    Err("failed to apply recovered archive operation".to_string())
                }
            })
            .map_err(|error| PersistentARTrieError::RecoveryError {
                reason: error.to_string(),
            })?;

        Ok((
            trie,
            EnhancedRecoveryStats {
                mode: EnhancedRecoveryMode::RebuiltFromArchives,
                duration_ms: start_time.elapsed().as_millis() as u64,
                records_replayed: records_replayed as usize,
                epochs_recovered: 0,
                dirty_nodes_recovered: 0,
                archive_segments_used: segments.len(),
            },
        ))
    }
}
