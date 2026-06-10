//! `IoUringDiskManager`-specific constructors for `PersistentARTrieChar<V>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~1294-1538, ~245 LOC)
//! as a Phase-6 char sub-module, mirroring the byte and vocab
//! IoUring constructor splits. These constructors target the
//! `IoUringDiskManager` storage backend.

#![cfg(feature = "io-uring-backend")]

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Arc;

use crate::persistent_artrie::adaptive_pool::CacheStats;
use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::buffer_manager::BufferManager;
use crate::persistent_artrie::concurrency::{EpochManager, OptimisticVersion, RetryStats};
use crate::persistent_artrie::dict_impl::DurabilityPolicy;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::{WalConfig, WalReader, WalRecord};
use crate::persistent_artrie::wal_managed::{create_async_wal, open_or_create_async_wal};
use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;

use super::arena_manager::ArenaManager;
use super::DEFAULT_CHAR_BUFFER_POOL_SIZE;

impl<V: DictionaryValue>
    super::PersistentARTrieChar<V, crate::persistent_artrie::IoUringDiskManager>
{
    /// **io_uring twin of the mmap `install_overlay_on_create`.** A freshly created
    /// io_uring trie builds the lock-free overlay directly; the overlay is the SOLE
    /// representation for ALL `V`. The mmap `install_overlay_on_create` lives in the
    /// default-`S` (`MmapDiskManager`) impl block and is not visible here, so the
    /// `IoUringDiskManager` create path needs its own. The shared
    /// `install_overlay_on_create` / `install_overlay` defaults are on the
    /// `<V, S: BlockStorage>` `LockFreeOverlay` block (visible for any `S`). Fresh WAL
    /// ⇒ the Overlay stamp MUST take; a failure to engage ⇒ hard error (V-2).
    fn install_overlay_on_create(self) -> Result<Self> {
        <Self as crate::persistent_artrie_core::overlay::flip::LockFreeOverlay<
            crate::persistent_artrie_core::key_encoding::CharKey,
            _,
            _,
        >>::install_overlay_on_create(self)
    }

    /// Create a new disk-backed trie using io_uring + O_DIRECT.
    ///
    /// This uses `IoUringDiskManager` instead of `MmapDiskManager`, which:
    /// - Bypasses the kernel page cache (O_DIRECT) to eliminate double caching
    /// - Uses io_uring for async I/O with predictable latency
    /// - Supports batched block submissions for better throughput
    ///
    /// # Arguments
    /// * `path` - Path to the trie file (must not exist)
    pub fn create_with_io_uring<P: AsRef<Path>>(path: P) -> Result<Self> {
        use crate::persistent_artrie::IoUringDiskManager;

        let path = path.as_ref();

        // Create io_uring disk manager (creates new file with O_DIRECT)
        let disk_manager = IoUringDiskManager::create(path)?;

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
        Self::install_overlay_on_create(Self {
            len: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            wal_config: WalConfig::default(),
            next_lsn: std::sync::atomic::AtomicU64::new(1),
            committed_watermark: super::committed_watermark::CommittedWatermark::new(0),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
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

    /// Open an existing disk-backed trie using io_uring + O_DIRECT.
    ///
    /// This opens an existing trie and replays the WAL if needed,
    /// using `IoUringDiskManager` for block I/O.
    ///
    /// # Arguments
    /// * `path` - Path to the trie file (must exist)
    pub fn open_with_io_uring<P: AsRef<Path>>(path: P) -> Result<Self> {
        use crate::persistent_artrie::IoUringDiskManager;

        let path = path.as_ref();

        // Open io_uring disk manager (validates header)
        let disk_manager = IoUringDiskManager::open(path)?;

        // Read root pointer and entry count from header
        let root_ptr = disk_manager.root_ptr()?;
        let _entry_count = disk_manager.entry_count()?;

        // Create buffer manager (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_CHAR_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Read WAL records for recovery if WAL exists
        let wal_path = path.with_extension("wal");
        let (recovered_ops, next_lsn, checkpoint_lsn, commit_seq_seed) = if wal_path.exists() {
            let mut reader =
                WalReader::new(&wal_path).map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;

            let mut records = Vec::new();
            let mut max_lsn = 0u64;
            let mut checkpoint_lsn = 0u64;
            // DG-RECON S1 seed: max CommitRank generation surviving in the WAL.
            let mut max_commit_seq_gen = 0u64;
            while let Some(result) = reader.next_record() {
                match result {
                    Ok((lsn, record)) => {
                        max_lsn = max_lsn.max(lsn);
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
                    Err(_) => break,
                }
            }

            let next_lsn = max_lsn + 1;
            // Seed = max(durable header floor, scanned max generation).
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
        // post-conversion checkpoint. Computed BEFORE `wal_writer` is moved into the struct.
        let recovered_frontier = {
            let archive_config_for_base = WalConfig::default();
            let full_max = wal_writer
                .collect_wal_segments(&archive_config_for_base)
                .ok()
                .and_then(|segments| {
                    crate::persistent_artrie::wal::AsyncWalWriter::max_lsn_in_segments(&segments)
                });
            full_max
                .unwrap_or_else(|| next_lsn.saturating_sub(1))
                .max(next_lsn.saturating_sub(1))
        };

        let mut inner = Self {
            len: AtomicUsize::new(0),
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
        // out-rank every generation surviving recovery (the A.2 cross-restart fix).
        inner
            .commit_seq
            .store(commit_seq_seed, std::sync::atomic::Ordering::Release);

        // F5 trait methods resolve through the seam.
        use crate::persistent_artrie_core::key_encoding::CharKey;
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;

        // The on-disk rank-regime, read up-front so the F5 gate can decide BEFORE the
        // legacy owned dense-load (no-drift with the mmap ctors).
        let rank_regime = WalReader::read_header(&wal_path)
            .map(|h| h.regime())
            .unwrap_or(crate::persistent_artrie_core::wal::RankRegime::Owned);
        // F5 honors the SAME gate (no per-ctor drift). `IoUringDiskManager` is the `S`.
        let use_f5 = <Self as LockFreeOverlay<
            CharKey,
            V,
            crate::persistent_artrie::IoUringDiskManager,
        >>::USE_F5_REOPEN_LOADER
            && rank_regime == crate::persistent_artrie_core::wal::RankRegime::Overlay;
        // **F7 convert gate** (io_uring twin; const-keyed, no `force_f5`): an Owned-regime
        // eligible file converts into the overlay.
        let convert_owned = <Self as LockFreeOverlay<
            CharKey,
            V,
            crate::persistent_artrie::IoUringDiskManager,
        >>::USE_F5_REOPEN_LOADER
            && rank_regime == crate::persistent_artrie_core::wal::RankRegime::Owned;

        if convert_owned {
            // ===== F7 CONVERT PATH (Owned-regime eligible → overlay; io_uring twin) =====
            // Rotate-if-records-non-empty → stamp Overlay (+ fsync, OBL-1) → F5 build →
            // archive-aware drain (FIX B). OBL-2: image_checkpoint_lsn = the recovery
            // `checkpoint_lsn` (read PRE-rotate). A `?` aborts open with durable state intact.
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
            // ===== F5 PATH (Overlay-regime; direct dense→overlay) =====
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
        }

        Ok(inner)
    }
}
