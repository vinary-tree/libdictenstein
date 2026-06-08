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
            root: parking_lot::RwLock::new(CharTrieRoot::Empty),
            len: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: None,
            wal_writer: None,
            wal_config: WalConfig::default(),
            next_lsn: std::sync::atomic::AtomicU64::new(1),
            committed_watermark: super::committed_watermark::CommittedWatermark::new(0),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            overlay_write_mode: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                super::overlay_write_mode::OverlayWriteMode::default(),
            ),
            file_path: None,
            arena_manager: None,
            version: OptimisticVersion::new(),
            epoch_manager: Arc::new(EpochManager::new()),
            structural_generation: std::sync::atomic::AtomicU64::new(0),
            retry_stats: RetryStats::new(),
            #[cfg(feature = "group-commit")]
            group_commit: std::sync::Mutex::new(None),
            memory_monitor: std::sync::Mutex::new(None),
            cache_stats: CacheStats::default(),
            checkpoint_manager: std::sync::Mutex::new(None),
            durability_policy: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                DurabilityPolicy::default(),
            ),
            eviction_coordinator: std::sync::Mutex::new(None),
            prefetcher: crate::persistent_artrie::prefetch::Prefetcher::disabled(),
            _phantom: std::marker::PhantomData,
            lockfree_root: None,
            commit_seq: std::sync::atomic::AtomicU64::new(0),
            commit_seq_by_data_lsn: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            lockfree_cache: None,
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// **S5-12 EDIT 1 (owner-GO, IRREVERSIBLE): a freshly-created trie flips to the
    /// lock-free overlay for `V ∈ {(), u64}`; a strict NO-OP for arbitrary `V`.**
    ///
    /// A `create*` ctor builds a FRESH WAL (`current_lsn() == 1`), so
    /// `flip_to_overlay`'s `enable_lockfree` stamps the Overlay regime and the V-2
    /// stamp re-check (`route_overlay() && rank_regime()==Overlay`) MUST succeed —
    /// `!took` therefore means the stamp silently failed (a torn header / no WAL),
    /// which we surface as a hard error rather than enabling a write-broken or
    /// recovery-unsafe overlay. For `V ∉ {(), u64}` `overlay_eligible_v()` is false,
    /// the gate short-circuits, the trie stays `OwnedTree`, and this is a pure no-op
    /// (the proven owned path runs, unchanged — backward-compat for arbitrary V).
    fn apply_create_flip(mut self) -> Result<Self> {
        if Self::overlay_eligible_v() && !self.flip_to_overlay() {
            return Err(PersistentARTrieError::internal(
                "S5-12 create-flip: flip_to_overlay did not engage on a fresh trie",
            ));
        }
        Ok(self)
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

        // S5-12 EDIT 1: flip a fresh eligible-V trie to the overlay (no-op for arbitrary V).
        Self::apply_create_flip(Self {
            root: parking_lot::RwLock::new(CharTrieRoot::Empty),
            len: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            wal_config: WalConfig::default(),
            next_lsn: std::sync::atomic::AtomicU64::new(1),
            committed_watermark: super::committed_watermark::CommittedWatermark::new(0),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            overlay_write_mode: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                super::overlay_write_mode::OverlayWriteMode::default(),
            ),
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: Arc::new(EpochManager::new()),
            structural_generation: std::sync::atomic::AtomicU64::new(0),
            retry_stats: RetryStats::new(),
            #[cfg(feature = "group-commit")]
            group_commit: std::sync::Mutex::new(None),
            memory_monitor: std::sync::Mutex::new(None),
            cache_stats: CacheStats::default(),
            checkpoint_manager: std::sync::Mutex::new(None),
            durability_policy: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                DurabilityPolicy::default(),
            ),
            eviction_coordinator: std::sync::Mutex::new(None),
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

        // S5-12 EDIT 1: flip a fresh eligible-V trie to the overlay (no-op for arbitrary V).
        Self::apply_create_flip(Self {
            root: parking_lot::RwLock::new(CharTrieRoot::Empty),
            len: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            wal_config: WalConfig::default(),
            next_lsn: std::sync::atomic::AtomicU64::new(1),
            committed_watermark: super::committed_watermark::CommittedWatermark::new(0),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            overlay_write_mode: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                super::overlay_write_mode::OverlayWriteMode::default(),
            ),
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: Arc::new(EpochManager::new()),
            structural_generation: std::sync::atomic::AtomicU64::new(0),
            retry_stats: RetryStats::new(),
            #[cfg(feature = "group-commit")]
            group_commit: std::sync::Mutex::new(None),
            memory_monitor: std::sync::Mutex::new(None),
            cache_stats: CacheStats::default(),
            checkpoint_manager: std::sync::Mutex::new(None),
            durability_policy: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                DurabilityPolicy::default(),
            ),
            eviction_coordinator: std::sync::Mutex::new(None),
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

        // S5-12 EDIT 1: flip a fresh eligible-V trie to the overlay (no-op for arbitrary V).
        Self::apply_create_flip(Self {
            root: parking_lot::RwLock::new(CharTrieRoot::Empty),
            len: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            wal_config,
            next_lsn: std::sync::atomic::AtomicU64::new(1),
            committed_watermark: super::committed_watermark::CommittedWatermark::new(0),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            overlay_write_mode: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                super::overlay_write_mode::OverlayWriteMode::default(),
            ),
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: Arc::new(EpochManager::new()),
            structural_generation: std::sync::atomic::AtomicU64::new(0),
            retry_stats: RetryStats::new(),
            #[cfg(feature = "group-commit")]
            group_commit: std::sync::Mutex::new(None),
            memory_monitor: std::sync::Mutex::new(None),
            cache_stats: CacheStats::default(),
            checkpoint_manager: std::sync::Mutex::new(None),
            durability_policy: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                DurabilityPolicy::default(),
            ),
            eviction_coordinator: std::sync::Mutex::new(None),
            prefetcher: crate::persistent_artrie::prefetch::Prefetcher::new(),
            _phantom: std::marker::PhantomData,
            lockfree_root: None,
            commit_seq: std::sync::atomic::AtomicU64::new(0),
            commit_seq_by_data_lsn: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            lockfree_cache: None,
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Open an existing disk-backed trie.
    ///
    /// Selects the reopen loader for an Overlay-regime file via the F5 gate
    /// [`crate::persistent_artrie_core::overlay::flip::LockFreeOverlay::USE_F5_REOPEN_LOADER`]
    /// (S1: dormant `false` ⇒ the legacy owned-loader→reestablish path; S3 flips it
    /// to the direct dense→overlay F5 loader). An Owned-regime file always uses the
    /// legacy owned loader.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        use crate::persistent_artrie_core::key_encoding::CharKey;
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
        // This impl block is the default-`S` (`MmapDiskManager` = `DiskManager`) block.
        let gate = <Self as LockFreeOverlay<CharKey, V, DiskManager>>::USE_F5_REOPEN_LOADER;
        Self::open_inner(path.as_ref(), gate)
    }

    /// **F5 (S2 test surface) — reopen via the DIRECT dense→overlay loader**,
    /// regardless of the [`Self::USE_F5_REOPEN_LOADER`] gate. Identical to [`Self::open`]
    /// except an Overlay-regime file is reopened through `load_root_immutable`
    /// (eager-load + walk-convert + install pre-built root) + `replay_records_lww_overlay`
    /// (WAL tail INTO the overlay) instead of the owned-loader→reestablish path. An
    /// Owned-regime file still uses the owned loader (F5 runs only for Overlay). Used by
    /// the F5 both-loaders correspondence proptest to compare against [`Self::open`]
    /// while the gate stays OFF.
    pub fn open_with_f5_loader<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_inner(path.as_ref(), true)
    }

    /// **F5 (S2 test surface) — reopen via the LEGACY owned-loader→reestablish path**,
    /// regardless of the [`Self::USE_F5_REOPEN_LOADER`] gate. The gate-independent
    /// counterpart of [`Self::open_with_f5_loader`], so the both-loaders correspondence
    /// proptest is a meaningful legacy-vs-F5 oracle whether the gate is ON or OFF.
    pub fn open_with_legacy_loader<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_inner(path.as_ref(), false)
    }

    /// Shared `open` body. `force_f5` selects the F5 dense→overlay loader for an
    /// Overlay-regime file (the gate value from `open`, or `true` from
    /// `open_with_f5_loader`); an Owned-regime file ignores it (always owned loader).
    fn open_inner(path: &Path, force_f5: bool) -> Result<Self> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        // F5 trait methods (`replay_records_lww_overlay`) resolve through the seam.
        #[allow(unused_imports)]
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;

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

        // **F7 FIX C:** watermark base = max LSN over ALL segments (archive + active), so a
        // converted/under-load file's archived committed tail is covered before the first
        // post-conversion checkpoint (else a BatchIncrement delta double-applies). Falls
        // back to the active-only frontier when no segments are enumerable. Computed BEFORE
        // `wal_writer` is moved into the struct.
        let recovered_frontier = {
            let archive_config_for_base = WalConfig::default();
            let full_max = wal_writer
                .collect_wal_segments(&archive_config_for_base)
                .ok()
                .and_then(|segments| AsyncWalWriter::max_lsn_in_segments(&segments));
            full_max
                .unwrap_or_else(|| next_lsn.saturating_sub(1))
                .max(next_lsn.saturating_sub(1))
        };

        let mut inner = Self {
            root: parking_lot::RwLock::new(CharTrieRoot::Empty),
            len: AtomicUsize::new(0), // Updated from disk or WAL replay
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager.clone()),
            wal_writer: Some(wal_writer),
            wal_config: WalConfig::default(),
            next_lsn: std::sync::atomic::AtomicU64::new(next_lsn),
            committed_watermark: super::committed_watermark::CommittedWatermark::new(
                recovered_frontier,
            ),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            overlay_write_mode: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                super::overlay_write_mode::OverlayWriteMode::default(),
            ),
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: Arc::new(EpochManager::new()),
            structural_generation: std::sync::atomic::AtomicU64::new(0),
            retry_stats: RetryStats::new(),
            #[cfg(feature = "group-commit")]
            group_commit: std::sync::Mutex::new(None),
            memory_monitor: std::sync::Mutex::new(None),
            cache_stats: CacheStats::default(),
            checkpoint_manager: std::sync::Mutex::new(None),
            durability_policy: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                DurabilityPolicy::default(),
            ),
            eviction_coordinator: std::sync::Mutex::new(None),
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

        // The on-disk rank-regime (Overlay for a flipped/overlay file; Owned for
        // legacy/base/vocab/un-flipped files). Read up-front so the F5 gate can choose
        // the loader BEFORE the legacy owned dense-load runs (F5 skips the owned
        // intermediate entirely). An unreadable header fails safe to `Owned` (keep,
        // never drop). This is the SAME value that drives the reconcile's
        // unranked-orphan DROP below.
        let rank_regime = WalReader::read_header(&wal_path)
            .map(|h| h.regime())
            .unwrap_or(crate::persistent_artrie_core::wal::RankRegime::Owned);

        // F5 gate: a direct dense→overlay reopen runs ONLY for an Overlay-regime,
        // overlay-eligible file when F5 is selected (the gate, or the test ctor's
        // `force_f5`). Everything else takes the proven LEGACY path.
        let use_f5 = force_f5
            && rank_regime == crate::persistent_artrie_core::wal::RankRegime::Overlay
            && Self::overlay_eligible_v();
        // **F7 convert gate:** an OWNED-regime, overlay-eligible file on the PRODUCTION path
        // (`force_f5` — `open`/`open_with_f5_loader`) is CONVERTED into the overlay.
        // `open_with_legacy_loader` (`force_f5 == false`) keeps the legacy owned-loader
        // stay-owned path (the pre-F7 owned-reopen ORACLE).
        let convert_owned = force_f5
            && rank_regime == crate::persistent_artrie_core::wal::RankRegime::Owned
            && Self::overlay_eligible_v();

        if convert_owned {
            // ===== F7 CONVERT PATH (Owned-regime eligible → overlay) =====
            // Rotate-if-records-non-empty → stamp Overlay (+ fsync, OBL-1) → F5 build from
            // the dense image → archive-aware drain (FIX B) with the REAL (loaded_from_disk,
            // image checkpoint_lsn) (OBL-2; `checkpoint_lsn` is the recovery value read
            // PRE-rotate = the image redo frontier). A `?` aborts open with the durable
            // state intact. The converter's seam `load_root_immutable_seam` reaches the
            // buffer manager via `self`.
            let _ = recovered_ops;
            let archive_config = WalConfig::default();
            inner.convert_owned_to_overlay_on_reopen(
                root_ptr,
                /* was_loaded_from_disk */ root_ptr != 0,
                checkpoint_lsn,
                &archive_config,
            )?;
            if let Some(ref arena_manager) = inner.arena_manager {
                arena_manager.write().ensure_valid();
            }
        } else if use_f5 {
            // ===== F5 PATH (Overlay-regime; direct dense→overlay; owned tree NOT
            // materialized into `inner.root`) =====
            // (1) Build the overlay root DIRECTLY from the dense image (eager-load owned
            // as transient scratch → walk-convert → install pre-built root + select
            // LockFreeOverlay + verify Overlay regime). A `?` aborts open; `inner.root`
            // stays `Empty` (untouched) and the durable image is intact. `image_loaded` is
            // `false` if the image was absent/corrupt (fell back to empty) — then the drain
            // must NOT skip records the absent image fails to cover (fallback parity).
            let (_lc, image_loaded) = inner.load_root_immutable(&buffer_manager, root_ptr)?;

            // ensure_valid() restores the arena manager invariant after the eager load
            // (the buggy_clear_recovery theorem — same as the legacy path).
            if let Some(ref arena_manager) = inner.arena_manager {
                arena_manager.write().ensure_valid();
            }

            // (2) **F7 FIX B:** drain ALL WAL segments (archive + active) INTO THE OVERLAY,
            // not just the active file (OBLIGATION-A) — so an Overlay tail archived under
            // load (or a post-S2-crash converted file reopened as Overlay) recovers its
            // archived tail. OBL-2: `image_checkpoint_lsn = checkpoint_lsn` (the recovery
            // value = the image redo frontier) WHEN a valid image loaded; 0 + not-loaded on
            // a corrupt/absent image so the drain replays every WAL record. The per-segment
            // regime drops Overlay orphans and keeps a converted Owned tail. A `?` (RES-3
            // prefix gap, FIX E) aborts open loudly.
            let _ = recovered_ops;
            let archive_config = WalConfig::default();
            let effective_loaded = (root_ptr != 0) && image_loaded;
            let _applied = inner.reconcile_and_drain_overlay(
                &archive_config,
                /* loaded_from_disk */ effective_loaded,
                if effective_loaded { checkpoint_lsn } else { 0 },
            )?;
        } else {
            // ===== LEGACY PATH (ineligible V stays owned; OR the legacy-loader ORACLE) ====
            // Try to load root from disk if root_ptr != 0 (default: lazy loading).
            let mut loaded_from_disk = false;
            if root_ptr != 0 {
                let root_swizzled = SwizzledPtr::from_raw(root_ptr);
                match inner.load_root_from_disk(&buffer_manager, &root_swizzled, None) {
                    Ok((root, len)) => {
                        *inner.root.get_mut() = root;
                        inner.len.store(len, AtomicOrdering::Release);
                        loaded_from_disk = true;
                    }
                    Err(e) => {
                        log::warn!("Failed to load root from disk: {:?}", e);
                        // Fall back to WAL replay
                    }
                }
            }

            // Apply buggy_clear_recovery theorem: ensure_valid() restores the arena
            // manager invariant after clear_for_loading + failed load_arena sequence.
            // See: formal-verification/rocq/Invariants/ArenaInvariants.v
            //      Theorem open_with_failed_loading_recovered
            if let Some(ref arena_manager) = inner.arena_manager {
                arena_manager.write().ensure_valid();
            }

            // Replay WAL records that came after the checkpoint via the shared Order-A
            // reconcile (design C′): per-term last-writer-wins by commit generation,
            // NOT WAL physical/LSN order. Records with LSN <= checkpoint_lsn are already
            // persisted to disk and are skipped inside `reconcile_lww`. For a rank-less
            // (pre-fix) WAL this is byte-for-byte the old in-order replay.
            let applied_any = inner.replay_records_lww(
                recovered_ops,
                loaded_from_disk,
                checkpoint_lsn,
                rank_regime,
            );
            let skipped_all = !applied_any;

            // If we loaded from disk and skipped all WAL records, we can truncate the
            // WAL (This is safe because all data is already persisted)
            if loaded_from_disk && skipped_all && checkpoint_lsn > 0 {
                // WAL truncation would happen here if we implement it
                // For now, just note that we could truncate
            }

            // **F7 — LEGACY-LOADER ORACLE Overlay branch (force_f5 == false ONLY).**
            // Reachable solely via `open_with_legacy_loader` on an Overlay file (production
            // routes Overlay → F5 and Owned → convert above). Keeps the legacy
            // reopen-into-overlay behavior so the both-loaders correspondence test stays a
            // meaningful F5-vs-legacy oracle. `reestablish_overlay_from_owned` (the KEPT
            // structural converter that REPLACES the deleted per-term
            // `reestablish_overlay_dispatch` — same overlay, strictly more correct) reads
            // the recovered OWNED tree via the UN-routed `owned_*` seams, builds + installs
            // the overlay root, then clears owned LAST (RES-7).
            if rank_regime == crate::persistent_artrie_core::wal::RankRegime::Overlay
                && Self::overlay_eligible_v()
            {
                let took = inner.flip_to_overlay();
                debug_assert!(took, "Overlay-regime open must flip");
                <Self as crate::persistent_artrie_core::overlay::flip::LockFreeOverlay<
                    crate::persistent_artrie_core::key_encoding::CharKey,
                    V,
                    DiskManager,
                >>::reestablish_overlay_from_owned(&mut inner)?;
            }
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

        // **F7 FIX C:** watermark base = max LSN over ALL segments (archive + active), so a
        // converted/under-load file's archived committed tail is covered before the first
        // post-conversion checkpoint (else a BatchIncrement delta double-applies). Falls
        // back to the active-only frontier when no segments are enumerable. Computed BEFORE
        // `wal_writer` is moved into the struct.
        let recovered_frontier = {
            let archive_config_for_base = WalConfig::default();
            let full_max = wal_writer
                .collect_wal_segments(&archive_config_for_base)
                .ok()
                .and_then(|segments| AsyncWalWriter::max_lsn_in_segments(&segments));
            full_max
                .unwrap_or_else(|| next_lsn.saturating_sub(1))
                .max(next_lsn.saturating_sub(1))
        };

        let mut inner = Self {
            root: parking_lot::RwLock::new(CharTrieRoot::Empty),
            len: AtomicUsize::new(0), // Updated from disk or WAL replay
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager.clone()),
            wal_writer: Some(wal_writer),
            wal_config: WalConfig::default(),
            next_lsn: std::sync::atomic::AtomicU64::new(next_lsn),
            committed_watermark: super::committed_watermark::CommittedWatermark::new(
                recovered_frontier,
            ),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            overlay_write_mode: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                super::overlay_write_mode::OverlayWriteMode::default(),
            ),
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: Arc::new(EpochManager::new()),
            structural_generation: std::sync::atomic::AtomicU64::new(0),
            retry_stats: RetryStats::new(),
            #[cfg(feature = "group-commit")]
            group_commit: std::sync::Mutex::new(None),
            memory_monitor: std::sync::Mutex::new(None),
            cache_stats: CacheStats::default(),
            checkpoint_manager: std::sync::Mutex::new(None),
            durability_policy: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                DurabilityPolicy::default(),
            ),
            eviction_coordinator: std::sync::Mutex::new(None),
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

        // F5 trait methods resolve through the seam.
        #[allow(unused_imports)]
        use crate::persistent_artrie_core::key_encoding::CharKey;
        #[allow(unused_imports)]
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;

        // Read the regime up-front so the F5 gate can decide BEFORE the legacy owned
        // dense-load. (No-drift with `open_inner`.)
        let rank_regime = WalReader::read_header(&wal_path)
            .map(|h| h.regime())
            .unwrap_or(crate::persistent_artrie_core::wal::RankRegime::Owned);
        // F5 honors the SAME gate as `open` (no per-ctor drift). For F5 the user's
        // `eager_depth` hint is moot — the dense→overlay converter always materializes
        // the whole tree.
        let use_f5 = <Self as LockFreeOverlay<CharKey, V, DiskManager>>::USE_F5_REOPEN_LOADER
            && rank_regime == crate::persistent_artrie_core::wal::RankRegime::Overlay
            && Self::overlay_eligible_v();
        // **F7 convert gate** (open_with_depth has no `force_f5` test ctor, so it is gated
        // on the F5 const directly — always true): an Owned-regime eligible file converts.
        let convert_owned = <Self as LockFreeOverlay<CharKey, V, DiskManager>>::USE_F5_REOPEN_LOADER
            && rank_regime == crate::persistent_artrie_core::wal::RankRegime::Owned
            && Self::overlay_eligible_v();

        if convert_owned {
            // ===== F7 CONVERT PATH (Owned-regime eligible → overlay) =====
            // The `eager_depth` hint is moot for the converter (the dense→overlay build
            // always materializes the whole tree). Rotate-if-records-non-empty → stamp
            // Overlay (+ fsync, OBL-1) → F5 build → archive-aware drain (FIX B). OBL-2:
            // image_checkpoint_lsn = the recovery `checkpoint_lsn` (read PRE-rotate).
            let _ = recovered_ops;
            let _ = eager_depth;
            let archive_config = WalConfig::default();
            inner.convert_owned_to_overlay_on_reopen(
                root_ptr,
                /* was_loaded_from_disk */ root_ptr != 0,
                checkpoint_lsn,
                &archive_config,
            )?;
            if let Some(ref arena_manager) = inner.arena_manager {
                arena_manager.write().ensure_valid();
            }
        } else if use_f5 {
            // ===== F5 PATH (Overlay-regime; direct dense→overlay; owned tree NOT
            // materialized) =====
            let (_lc, image_loaded) = inner.load_root_immutable(&buffer_manager, root_ptr)?;
            if let Some(ref arena_manager) = inner.arena_manager {
                arena_manager.write().ensure_valid();
            }
            // **F7 FIX B:** drain ALL segments (archive + active) into the overlay (not
            // active-only — OBLIGATION-A). OBL-2: image_checkpoint_lsn = checkpoint_lsn when
            // a valid image loaded; 0 + not-loaded on a corrupt/absent image (fallback
            // parity). RES-3 fail-loud (FIX E).
            let _ = recovered_ops;
            let archive_config = WalConfig::default();
            let effective_loaded = (root_ptr != 0) && image_loaded;
            let _applied = inner.reconcile_and_drain_overlay(
                &archive_config,
                /* loaded_from_disk */ effective_loaded,
                if effective_loaded { checkpoint_lsn } else { 0 },
            )?;
        } else {
            // ===== LEGACY PATH (ineligible V stays owned; honors `eager_depth`) =====
            let mut loaded_from_disk = false;
            if root_ptr != 0 {
                let root_swizzled = SwizzledPtr::from_raw(root_ptr);
                match inner.load_root_from_disk(&buffer_manager, &root_swizzled, eager_depth) {
                    Ok((root, len)) => {
                        *inner.root.get_mut() = root;
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

            // Replay WAL records that came after the checkpoint via the shared Order-A
            // reconcile (design C′) — identical to the `open()` site so the two cannot
            // drift. Per-term last-writer-wins by commit generation; checkpoint-subsumed
            // records skipped inside `reconcile_lww`.
            let _ = inner.replay_records_lww(
                recovered_ops,
                loaded_from_disk,
                checkpoint_lsn,
                rank_regime,
            );
            // Ineligible V cannot overlay → stays owned. (An eligible Owned file converts
            // and an eligible Overlay file takes F5 above; this arm is the ineligible-V
            // owned reopen.)
        }

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
        // F7-R1: the structural owned→overlay converter resolves through the seam.
        use crate::persistent_artrie_core::key_encoding::CharKey;
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;

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

                // A2 fix (S5 v4 §1.3): an Overlay archive must DROP never-acked
                // two-append-window orphans (else a post-flip corruption rebuild
                // resurrects them) and reorder same-term ops by commit generation.
                // Route the Overlay case through the canonical regime-aware reconcile
                // (DRY with `recover_from_archives`); the all-Owned case keeps the
                // existing inline streaming replay UNCHANGED (INERT pre-flip).
                let any_overlay = segments.iter().any(|seg| {
                    crate::persistent_artrie::wal::WalReader::read_header(seg)
                        .map(|h| h.regime() == crate::persistent_artrie::wal::RankRegime::Overlay)
                        .unwrap_or(false)
                });
                if any_overlay {
                    let (rr, tr) =
                        crate::persistent_artrie::recovery::rebuild_from_wal_segments_regime_aware(
                            &segments,
                            |op| {
                                if trie.apply_core_recovered_operation_no_wal(op) {
                                    Ok(())
                                } else {
                                    Err("failed to apply recovered archive operation".to_string())
                                }
                            },
                        )
                        .map_err(|error| {
                            PersistentARTrieError::RecoveryError {
                                reason: error.to_string(),
                            }
                        })?;
                    records_replayed = rr;
                    terms_recovered = tr;
                    segments_used = segments.clone();
                } else {
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
                                    // For increment, store the final (absolute) result.
                                    // `val` is the i64 BIT-PATTERN of the count
                                    // (`counter_return_i64` on write) — NEGATIVE for a
                                    // `u64` count > i64::MAX. Decode via the LEAF BYTES
                                    // through the shared `counter_codec` helper (the
                                    // bit-pattern-faithful path the owned/overlay
                                    // appliers use) so a u64 count round-trips correctly
                                    // and the v6 gate holds (no raw counter-leaf bincode
                                    // outside `counter_codec`). A non-counter `V` yields
                                    // `None` (skip), matching the prior deserialize-fail.
                                    let term_str = String::from_utf8_lossy(&term);
                                    if let Some(v) =
                                        crate::persistent_artrie_core::counter_codec::counter_leaf_to_i128::<V>(
                                            &val.to_le_bytes(),
                                        )
                                        .and_then(
                                            crate::persistent_artrie_core::counter_codec::i128_to_counter_value::<V>,
                                        )
                                    {
                                        trie.insert_impl_no_wal_with_value(&term_str, v);
                                        terms_recovered += 1;
                                    }
                                }
                                WalRecord::Upsert { term, value } => {
                                    let term_str = String::from_utf8_lossy(&term);
                                    if let Ok(v) = crate::serialization::bincode_compat::deserialize::<
                                        V,
                                    >(&value)
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
                                        if let Ok(v) =
                                            crate::serialization::bincode_compat::deserialize::<V>(
                                                &new_value,
                                            )
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
                }

                // S5-12 EDIT 3 (IRREVERSIBLE): the rebuild above repopulated the OWNED
                // tree (the no-WAL recovery path is owned-targeted). But after EDIT 1 the
                // fresh `trie` from `create_with_config` is ALREADY in LockFreeOverlay
                // mode for eligible V (with an empty overlay), so returning it as-is would
                // make the next checkpoint route-split capture the EMPTY overlay and lose
                // every rebuilt term (total loss). Move the rebuilt owned tree into the
                // overlay whenever the trie is overlay-routed (⟺ eligible V was create-
                // flipped). Gate on `route_overlay()`, NOT `any_overlay`: the orphan-drop
                // is the reconcile's job (regime-aware vs inline above), whereas the owned
                // →overlay move is required for the OVERLAY-MODE trie regardless of the
                // archives' regime. A `?` aborts with the owned tree intact (RES-7); a
                // pure no-op for arbitrary V (which create did not flip ⇒ !route_overlay).
                // F7-R1: the STRUCTURAL converter `reestablish_overlay_from_owned`
                // (build_overlay_root_from_owned + FORCE-REPLACE the empty create-flip
                // root + clear owned LAST) replaces the legacy per-term
                // `reestablish_overlay_dispatch` — same overlay (term-set + values, incl.
                // u64 > i64::MAX + ""), strictly more correct on a term-only counter.
                if trie.route_overlay() {
                    <Self as LockFreeOverlay<CharKey, V, DiskManager>>::reestablish_overlay_from_owned(
                        &mut trie,
                    )?;
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
        // F7-R1: the structural owned→overlay converter resolves through the seam.
        use crate::persistent_artrie_core::key_encoding::CharKey;
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;

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

        // S5-12 EDIT 3 (IRREVERSIBLE, recover_from_archives twin): the regime-aware
        // rebuild repopulated the OWNED tree. After EDIT 1 the fresh `trie` from
        // `create_with_config` is ALREADY overlay-routed for eligible V (empty overlay),
        // so without this move the next checkpoint would capture the empty overlay and
        // lose every rebuilt term. Gate on `route_overlay()` (⟺ eligible V was create-
        // flipped) rather than the archives' regime — the owned→overlay move is required
        // for the overlay-mode trie regardless of archive regime; the orphan-drop is the
        // reconcile's responsibility above. A `?` aborts with the owned tree intact
        // (RES-7); a pure no-op for arbitrary V (create did not flip ⇒ !route_overlay).
        // F7-R1: the STRUCTURAL converter `reestablish_overlay_from_owned`
        // (build_overlay_root_from_owned + FORCE-REPLACE the empty create-flip root +
        // clear owned LAST) replaces the legacy per-term `reestablish_overlay_dispatch`
        // — same overlay (term-set + values, incl. u64 > i64::MAX + ""), strictly more
        // correct on a term-only counter.
        if trie.route_overlay() {
            <Self as LockFreeOverlay<CharKey, V, DiskManager>>::reestablish_overlay_from_owned(
                &mut trie,
            )?;
        }

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

#[cfg(test)]
mod s5_12_flip_ctor_gate {
    //! **S5-12 production-flip ctor-wiring gate (EDIT 1/2/3).** The irreversible,
    //! data-loss-critical create-flip + open-3-cases + corruption-rebuild wiring.
    //! Scratch is REAL disk (`target/test-tmp`), never `/tmp` (tmpfs on this host).
    //!
    //! Read-path note: an Overlay reopen (EDIT 2) moves the recovered owned tree INTO
    //! the lock-free overlay and clears the owned tree, so post-reopen membership is
    //! read via `contains_lockfree` and values via `get_lockfree` (the owned-tree
    //! `Dictionary::contains` is intentionally empty after the move).

    use super::*;
    use crate::persistent_artrie::wal::{WalHeader, WalReader};
    use crate::persistent_artrie_char::PersistentARTrieChar;
    use crate::{Dictionary, MappedDictionary};

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp")
    }

    /// Read the on-disk WAL header for a trie data path (its sibling `.wal`).
    fn wal_header(data_path: &Path) -> WalHeader {
        let wal_path = data_path.with_extension("wal");
        WalReader::read_header(&wal_path).expect("read WAL header")
    }

    // ───────────────────────── Gate 1: create-flip TypeId gate ─────────────────────────

    /// `create<u64>`, `create<()>`, and `create<String>` (arbitrary V) all flip to the
    /// overlay (`route_overlay()==true`) and stamp the WAL header `MAGIC_OVERLAY`, and a
    /// subsequent overlay insert works — arbitrary-V overlay routing is the default.
    #[test]
    fn s5_12_create_flip_eligible_v_overlay_all_v() {
        // V = u64: flipped + Overlay magic.
        {
            let dir = scratch("s5-12-create-u64");
            let path = dir.path().join("t.artc");
            let trie = PersistentARTrieChar::<u64>::create(&path).expect("create<u64>");
            assert!(
                trie.route_overlay(),
                "create<u64> must flip to the overlay (route_overlay true)"
            );
            assert_eq!(
                wal_header(&path).magic,
                WalHeader::MAGIC_OVERLAY,
                "create<u64> WAL header must be stamped MAGIC_OVERLAY"
            );
        }
        // V = (): flipped + Overlay magic.
        {
            let dir = scratch("s5-12-create-unit");
            let path = dir.path().join("t.artc");
            let trie = PersistentARTrieChar::<()>::create(&path).expect("create<()>");
            assert!(
                trie.route_overlay(),
                "create<()> must flip to the overlay (route_overlay true)"
            );
            assert_eq!(
                wal_header(&path).magic,
                WalHeader::MAGIC_OVERLAY,
                "create<()> WAL header must be stamped MAGIC_OVERLAY"
            );
        }
        // V = String (arbitrary): arbitrary-V overlay routing is the default, so
        // String is eligible — create-flips + stamps MAGIC_OVERLAY and the overlay
        // value path works.
        {
            let dir = scratch("s5-12-create-string");
            let path = dir.path().join("t.artc");
            let trie = PersistentARTrieChar::<String>::create(&path).expect("create<String>");
            assert!(
                trie.route_overlay(),
                "create<String> flips to the overlay (arbitrary V is the default)"
            );
            assert_eq!(
                wal_header(&path).magic,
                WalHeader::MAGIC_OVERLAY,
                "create<String> WAL header is stamped MAGIC_OVERLAY when arbitrary V is eligible"
            );
            // The overlay value path works for arbitrary V.
            trie.insert_with_value("hello", "world".to_string())
                .expect("overlay insert");
            assert_eq!(
                MappedDictionary::get_value(&trie, "hello"),
                Some("world".to_string()),
                "overlay insert_with_value must work for arbitrary V"
            );
        }
    }

    // ───────────────────── Gate 2: create → durable write → reopen ─────────────────────

    /// create→durable-write→checkpoint→reopen with NO data loss and NO double-count,
    /// for both `()` (membership) and `u64` (counters). After the Overlay reopen the
    /// data lives in the overlay (read via `contains_lockfree` / `get_lockfree`).
    #[test]
    fn s5_12_create_write_reopen_no_loss_unit_and_u64() {
        // Membership (V = ()).
        {
            let dir = scratch("s5-12-rw-unit");
            let path = dir.path().join("t.artc");
            let terms: Vec<String> = (0..40u32).map(|i| format!("term{i:03}")).collect();
            {
                let trie = PersistentARTrieChar::<()>::create(&path).expect("create<()>");
                // create-flip already ran enable_lockfree + LockFreeOverlay; default
                // durability is Immediate (set it explicitly to match S5 conventions).
                trie.set_durability_policy(
                    crate::persistent_artrie_core::durability::DurabilityPolicy::Immediate,
                );
                assert!(trie.route_overlay(), "fresh create<()> is overlay-routed");
                for t in &terms {
                    assert!(
                        trie.insert_cas_durable(t).expect("durable overlay insert"),
                        "first durable insert of {t:?} must be newly-inserted"
                    );
                }
                trie.checkpoint().expect("overlay checkpoint (route-split)");
            }
            let recovered = PersistentARTrieChar::<()>::open(&path).expect("reopen<()>");
            assert!(
                recovered.route_overlay(),
                "an Overlay file must reopen overlay-routed (EDIT 2 CASE-a)"
            );
            for t in &terms {
                assert!(
                    recovered.contains_lockfree(t),
                    "membership lost {t:?} across create→write→checkpoint→reopen"
                );
            }
        }
        // Counters (V = u64): exact summed counts, no double, no loss.
        {
            let dir = scratch("s5-12-rw-u64");
            let path = dir.path().join("t.artc");
            // Distinct deltas so a double-count or drop is detectable per key.
            let entries: Vec<(String, u64)> = (0..40u32)
                .map(|i| (format!("k{i:03}"), (i as u64) + 1))
                .collect();
            {
                let trie = PersistentARTrieChar::<u64>::create(&path).expect("create<u64>");
                trie.set_durability_policy(
                    crate::persistent_artrie_core::durability::DurabilityPolicy::Immediate,
                );
                assert!(trie.route_overlay(), "fresh create<u64> is overlay-routed");
                for (k, d) in &entries {
                    let v = trie
                        .try_increment_cas_durable(k, *d)
                        .expect("durable increment");
                    assert_eq!(v, *d, "first increment of {k:?} must equal its delta");
                }
                trie.checkpoint().expect("overlay checkpoint (route-split)");
            }
            let recovered = PersistentARTrieChar::<u64>::open(&path).expect("reopen<u64>");
            assert!(
                recovered.route_overlay(),
                "u64 Overlay file reopens overlay-routed"
            );
            for (k, d) in &entries {
                assert_eq!(
                    recovered.get_lockfree(k),
                    Some(*d),
                    "counter {k:?} wrong after reopen (loss or double-count)"
                );
            }
        }
    }

    // ──────────────────── Gate 3: old-Owned file stays Owned on reopen ────────────────────

    /// An OWNED-regime file must stay Owned on reopen: `route_overlay()==false`, data
    /// intact via the OWNED read path, header still standard `MAGIC`. (Backward-compat:
    /// an Owned file never silently flips.) Arbitrary-V overlay routing is the default,
    /// so a fresh `create::<String>()` create-flips; kill-switch it to the Owned regime
    /// to produce the Owned-regime file and exercise the "an Owned-regime file stays
    /// owned on reopen" path.
    #[test]
    fn s5_12_old_owned_file_stays_owned_on_reopen() {
        let dir = scratch("s5-12-owned-stays");
        let path = dir.path().join("t.artc");
        let entries: Vec<(String, String)> = (0..30u32)
            .map(|i| (format!("w{i:03}"), format!("v{i:03}")))
            .collect();
        {
            let trie = PersistentARTrieChar::<String>::create(&path).expect("create<String>");
            trie.kill_switch_to_owned();
            assert!(!trie.route_overlay(), "String trie is on the owned path");
            for (k, v) in &entries {
                trie.insert_with_value(k, v.clone());
            }
            trie.checkpoint().expect("owned checkpoint");
        }
        // **F7:** production `open` CONVERTS an Owned-regime eligible file INTO the overlay
        // (Owned→Overlay conversion-on-reopen). The data must survive (`route_overlay()`
        // true after `open`); the on-disk WAL is re-stamped Overlay (the durable conversion
        // commit). The pre-F7 stay-Owned behavior is preserved by the legacy-loader oracle.
        let recovered = PersistentARTrieChar::<String>::open(&path).expect("reopen<String>");
        assert!(
            recovered.route_overlay(),
            "F7: an Owned-regime eligible file CONVERTS to the overlay on reopen"
        );
        assert_eq!(
            wal_header(&path).magic,
            WalHeader::MAGIC_OVERLAY,
            "F7: a converted file's WAL header is re-stamped to the Overlay MAGIC"
        );
        for (k, v) in &entries {
            assert_eq!(
                MappedDictionary::get_value(&recovered, k),
                Some(v.clone()),
                "converted data lost for {k:?} across reopen"
            );
            assert!(
                Dictionary::contains(&recovered, k),
                "converted membership lost for {k:?}"
            );
        }
        // (The pre-F7 stay-Owned oracle is covered by the dedicated
        // `persistent_owned_to_overlay_correspondence` suite, which builds a SEPARATE
        // fixture per loader since the converting `open` mutates the file to Overlay.)
    }

    // ─────────────────── Gate 4: mixed-monomorph reopen (V-4, no panic) ───────────────────

    /// A flipped `<u64>` file reopened as `<()>` must NOT panic and must NOT corrupt the
    /// file (it stays openable as `<u64>`). The cross-monomorph value-loss is a DOCUMENTED
    /// operational invariant (V-4: reopen with the same V); this gate only asserts the
    /// no-panic / no-corruption boundary.
    #[test]
    fn s5_12_mixed_monomorph_reopen_no_panic_no_corruption() {
        let dir = scratch("s5-12-mixed-mono");
        let path = dir.path().join("t.artc");
        let entries: Vec<(String, u64)> = vec![("alpha", 7u64), ("beta", 11), ("gamma", 13)]
            .into_iter()
            .map(|(t, v)| (t.to_string(), v))
            .collect();
        {
            let trie = PersistentARTrieChar::<u64>::create(&path).expect("create<u64>");
            trie.set_durability_policy(
                crate::persistent_artrie_core::durability::DurabilityPolicy::Immediate,
            );
            for (k, d) in &entries {
                trie.try_increment_cas_durable(k, *d)
                    .expect("durable increment");
            }
            trie.checkpoint().expect("overlay checkpoint");
        }
        // Reopen as <()> — the WRONG monomorph. V-4: bincode trailing-byte tolerance
        // means the u64 value bytes are dropped rather than panicking. The reestablish
        // for V=() routes through the MEMBERSHIP twin (no value decode), so this must
        // complete without panic. We tolerate either Ok (membership recovered) or a
        // clean Err — the contract is NO PANIC and NO file corruption.
        let reopened_as_unit = PersistentARTrieChar::<()>::open(&path);
        match reopened_as_unit {
            Ok(t) => {
                // Membership may be recovered into the overlay; value semantics are lost
                // by construction (documented V-4), which is fine for V=().
                let _ = t.contains_lockfree("alpha");
            }
            Err(_e) => {
                // A clean error is also acceptable — the point is no panic, no corruption.
            }
        }
        // CRITICAL: the file is NOT corrupted — it still opens as the correct <u64>
        // monomorph with the original counters intact.
        let recovered = PersistentARTrieChar::<u64>::open(&path)
            .expect("file must still open as <u64> after a wrong-monomorph reopen attempt");
        for (k, d) in &entries {
            assert_eq!(
                recovered.get_lockfree(k),
                Some(*d),
                "counter {k:?} corrupted by a wrong-monomorph reopen (must be intact)"
            );
        }
    }

    // ─────────────────── Gate 5: old-binary fail-closed on MAGIC_OVERLAY ───────────────────

    /// A flipped trie's WAL header carries `MAGIC_OVERLAY`. An OLD binary that only
    /// accepts the standard `MAGIC` (the Owned-only parse predicate `magic == MAGIC`)
    /// must FAIL-CLOSE on it, while THIS (new) binary's `from_bytes` accepts it with the
    /// Overlay regime — the D8-2 dual-magic tripwire, end-to-end on a real on-disk file.
    #[test]
    fn s5_12_old_binary_fail_closed_on_overlay_magic() {
        use std::io::Read;

        let dir = scratch("s5-12-fail-closed");
        let path = dir.path().join("t.artc");
        // A fresh create<()> stamps MAGIC_OVERLAY on the WAL header.
        let _trie = PersistentARTrieChar::<()>::create(&path).expect("create<()>");
        let wal_path = path.with_extension("wal");

        // Read the raw 64-byte header off disk.
        let mut buf = [0u8; WalHeader::SIZE];
        {
            let mut f = std::fs::File::open(&wal_path).expect("open .wal");
            f.read_exact(&mut buf).expect("read 64-byte header");
        }
        let magic: [u8; 8] = buf[0..8].try_into().unwrap();

        // Sanity: the on-disk magic IS the Overlay magic (create-flip stamped it).
        assert_eq!(
            magic,
            WalHeader::MAGIC_OVERLAY,
            "the flipped file must carry MAGIC_OVERLAY on disk"
        );

        // OLD-BINARY predicate (accepts ONLY the standard MAGIC) ⇒ fail-closed.
        assert_ne!(
            magic,
            WalHeader::MAGIC,
            "an Owned-only (MAGIC-only) parser must FAIL-CLOSE on the Overlay magic"
        );

        // NEW-BINARY `from_bytes` accepts it with the Overlay regime (dual-magic).
        let h = WalHeader::from_bytes(&buf).expect("new binary parses MAGIC_OVERLAY");
        assert_eq!(
            h.regime(),
            crate::persistent_artrie_core::wal::RankRegime::Overlay,
            "the dual-magic header must decode the Overlay regime"
        );
    }
}
