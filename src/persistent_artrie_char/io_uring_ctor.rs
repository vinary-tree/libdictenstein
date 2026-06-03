//! `IoUringDiskManager`-specific constructors for `PersistentARTrieChar<V>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~1294-1538, ~245 LOC)
//! as a Phase-6 char sub-module, mirroring the byte and vocab
//! IoUring constructor splits. These constructors target the
//! `IoUringDiskManager` storage backend.

#![cfg(feature = "io-uring-backend")]

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

use crate::persistent_artrie::adaptive_pool::CacheStats;
use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::buffer_manager::BufferManager;
use crate::persistent_artrie::concurrency::{EpochManager, OptimisticVersion, RetryStats};
use crate::persistent_artrie::dict_impl::DurabilityPolicy;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::{WalConfig, WalReader, WalRecord};
use crate::persistent_artrie::wal_managed::{create_async_wal, open_or_create_async_wal};
use crate::persistent_artrie::IoUringDiskManager;
use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;

use super::arena_manager::ArenaManager;
use super::types::CharTrieRoot;
use super::DEFAULT_CHAR_BUFFER_POOL_SIZE;

impl<V: DictionaryValue>
    super::PersistentARTrieChar<V, crate::persistent_artrie::IoUringDiskManager>
{
    /// **S5-12 EDIT 1 (io_uring twin of the mmap `apply_create_flip`).** A freshly
    /// created io_uring trie flips to the lock-free overlay for `V ∈ {(), u64}`; a
    /// strict NO-OP for arbitrary `V`. The mmap `apply_create_flip` lives in the
    /// default-`S` (`MmapDiskManager`) impl block and is not visible here, so the
    /// `IoUringDiskManager` create path needs its own. `flip_to_overlay` /
    /// `overlay_eligible_v` are on the `<V, S: BlockStorage>` block (visible for any
    /// `S`). Fresh WAL ⇒ the Overlay stamp MUST take; `!took` ⇒ hard error (V-2).
    fn apply_create_flip(mut self) -> Result<Self> {
        if Self::overlay_eligible_v() && !self.flip_to_overlay() {
            return Err(PersistentARTrieError::internal(
                "S5-12 create-flip (io_uring): flip_to_overlay did not engage on a fresh trie",
            ));
        }
        Ok(self)
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
        Self::apply_create_flip(Self {
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

    /// Open an existing disk-backed trie using io_uring + O_DIRECT.
    ///
    /// This opens an existing trie and replays the WAL if needed,
    /// using `IoUringDiskManager` for block I/O.
    ///
    /// # Arguments
    /// * `path` - Path to the trie file (must exist)
    pub fn open_with_io_uring<P: AsRef<Path>>(path: P) -> Result<Self> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        use crate::persistent_artrie::IoUringDiskManager;

        let path = path.as_ref();

        // Open io_uring disk manager (validates header)
        let disk_manager = IoUringDiskManager::open(path)?;

        // Read root pointer and entry count from header
        let root_ptr = disk_manager.root_ptr()?;
        let entry_count = disk_manager.entry_count()?;

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

        let mut inner = Self {
            root: CharTrieRoot::Empty,
            len: AtomicUsize::new(0),
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
        // out-rank every generation surviving recovery (the A.2 cross-restart fix).
        inner
            .commit_seq
            .store(commit_seq_seed, std::sync::atomic::Ordering::Release);

        // Try to load root from disk if root_ptr != 0
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
                }
            }
        }

        // Ensure arena manager validity after loading
        if let Some(ref arena_manager) = inner.arena_manager {
            arena_manager.write().ensure_valid();
        }

        // Replay WAL records that came after the checkpoint via the SAME shared
        // Order-A reconcile (design C′) the mmap ctors use (no-drift constraint):
        // per-term last-writer-wins by commit generation, checkpoint-subsumed
        // records skipped inside `reconcile_lww`. Rank-less WAL ⇒ byte-for-byte
        // the old in-order replay.
        // S4: the on-disk rank-regime drives the reconcile's unranked-orphan DROP.
        let rank_regime = WalReader::read_header(&wal_path)
            .map(|h| h.regime())
            .unwrap_or(crate::persistent_artrie_core::wal::RankRegime::Owned);
        let applied_any =
            inner.replay_records_lww(recovered_ops, loaded_from_disk, checkpoint_lsn, rank_regime);
        let skipped_all = !applied_any;

        if loaded_from_disk && skipped_all {
            inner.dirty.store(false, AtomicOrdering::Release);
        }

        // S5-12 EDIT 2 (IRREVERSIBLE, io_uring twin): an already-Overlay file moves the
        // recovered owned tree into the lock-free overlay (eligible V) + selects
        // LockFreeOverlay; Owned-regime (incl. empty) STAYS owned. `IoUringDiskManager`
        // is `'static`, so `reestablish_overlay_dispatch`'s `S: 'static` bound holds.
        if rank_regime == crate::persistent_artrie_core::wal::RankRegime::Overlay
            && Self::overlay_eligible_v()
        {
            let took = inner.flip_to_overlay();
            debug_assert!(took, "Overlay-regime open must flip");
            inner.reestablish_overlay_dispatch()?;
        }

        Ok(inner)
    }
}
