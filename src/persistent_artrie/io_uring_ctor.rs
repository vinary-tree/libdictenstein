//! `IoUringDiskManager`-specific constructors for `PersistentARTrie<V>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~1113-1445, ~333 LOC) as
//! the twelfth Phase-5 byte sub-module. These constructors
//! (`create_with_io_uring`, `open_with_io_uring`) are feature-gated
//! on `io-uring-backend` and target the `IoUringDiskManager` storage
//! backend. The MmapDiskManager (default) constructors live in
//! `dict_impl.rs`.

#![cfg(feature = "io-uring-backend")]

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

use log::warn;

use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;

use super::arena_manager::ArenaManager;
use super::block_storage::BlockStorage;
use super::buffer_manager::BufferManager;
use super::dict_impl::{DurabilityPolicy, PersistentARTrie};
use super::disk_load::read_root_descriptor_arena_count;
use super::error::{PersistentARTrieError, Result};
use super::recovery::RecoveryManager;
use super::wal::{AsyncWalConfig, AsyncWalWriter, WalConfig};
use super::{IoUringDiskManager, DEFAULT_BUFFER_POOL_SIZE};

impl<V: DictionaryValue> PersistentARTrie<V, IoUringDiskManager> {
    /// **M4b EDIT 1 (io_uring twin of the mmap `apply_create_flip`).** A freshly
    /// created io_uring byte trie flips to the lock-free overlay for `V ∈ {(), i64}`;
    /// a strict NO-OP for arbitrary `V`. The mmap `apply_create_flip` lives in the
    /// default-`S` (`MmapDiskManager`) impl block and is not visible here, so the
    /// `IoUringDiskManager` create path needs its own. `flip_to_overlay` /
    /// `overlay_eligible_v` are on the `<V, S: BlockStorage>` block (visible for any
    /// `S`). Fresh WAL ⇒ the Overlay stamp MUST take; `!flip_to_overlay()` ⇒ hard
    /// error (V-2). NB byte's eligible counter monomorph is `i64` (char's is `u64`).
    fn apply_create_flip(mut self) -> Result<Self> {
        if Self::overlay_eligible_v() && !self.flip_to_overlay() {
            return Err(PersistentARTrieError::internal(
                "byte create-flip (io_uring): flip did not engage on a fresh trie",
            ));
        }
        Ok(self)
    }

    /// Create a new persistent dictionary backed by io_uring + O_DIRECT.
    ///
    /// This uses `IoUringDiskManager` instead of `MmapDiskManager`, which:
    /// - Bypasses the kernel page cache (O_DIRECT) to eliminate double caching
    /// - Uses io_uring for async I/O with predictable latency
    /// - Supports batched block submissions for better throughput
    pub fn create_with_io_uring<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        if path.exists() {
            return Err(PersistentARTrieError::io_error(
                "create",
                path.display().to_string(),
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "Dictionary file already exists",
                ),
            ));
        }

        let disk_manager = IoUringDiskManager::create(path)?;

        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        let wal_path = path.with_extension("wal");
        let async_config = AsyncWalConfig {
            pending_dir: path.parent().unwrap_or(Path::new(".")).join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig::default();
        let wal_writer =
            AsyncWalWriter::create(&wal_path, async_config, archive_config).map_err(|e| {
                PersistentARTrieError::io_error(
                    "create_wal",
                    wal_path.display().to_string(),
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
        let wal_writer = Arc::new(wal_writer);

        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        // M4b EDIT 1: flip a fresh eligible-V trie to the overlay (no-op for arbitrary V).
        Self::apply_create_flip(Self {
            term_count: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            next_lsn: std::sync::atomic::AtomicU64::new(1),
            prefetcher: super::prefetch::Prefetcher::new(),
            arena_manager: Some(arena_manager),
            durability_policy: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                DurabilityPolicy::default(),
            ),
            epoch_manager: Arc::new(super::concurrency::EpochManager::new()),
            stats: Arc::new(super::concurrency::TrieStats::new()),
            eviction_coordinator: std::sync::Mutex::new(None),
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
            // apply_create_flip above for eligible V; arbitrary V stays owned.
            // M2b: fresh on-disk trie (empty WAL) — watermark base + commit_seq 0.
            committed_watermark:
                crate::persistent_artrie_core::committed_watermark::CommittedWatermark::new(0),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            commit_seq: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Open an existing persistent dictionary from disk using io_uring + O_DIRECT.
    ///
    /// This opens an existing dictionary file and replays the WAL if needed
    /// to recover from any crash, using `IoUringDiskManager` for block I/O.
    pub fn open_with_io_uring<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        if !path.exists() {
            return Err(PersistentARTrieError::io_error(
                "open",
                path.display().to_string(),
                std::io::Error::new(std::io::ErrorKind::NotFound, "Dictionary file not found"),
            ));
        }

        super::compaction_impl::recover_in_place_compaction_finalization(path)?;

        let disk_manager = IoUringDiskManager::open(path)?;

        let root_ptr = disk_manager.root_ptr()?;
        let _entry_count = disk_manager.entry_count()?;

        let storage_block_count = disk_manager.block_count()?;
        let arena_count = if root_ptr != 0 {
            match read_root_descriptor_arena_count(&disk_manager, root_ptr) {
                Ok(count) if count <= storage_block_count.saturating_sub(1) => count,
                Ok(count) => {
                    warn!(
                        "Ignoring invalid root descriptor arena_count {} for {} storage blocks",
                        count, storage_block_count
                    );
                    0
                }
                Err(e) => {
                    warn!("Failed to read root descriptor arena_count: {:?}", e);
                    0
                }
            }
        } else {
            0
        };

        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        if arena_count > 0 {
            let mut am = arena_manager.write();
            am.clear_for_loading();
            let mut load_failed = false;
            for block_id in 1..=arena_count {
                if let Err(e) = am.load_arena(block_id) {
                    warn!("Failed to load arena block {}: {:?}", block_id, e);
                    am.clear_for_loading();
                    am.ensure_valid();
                    load_failed = true;
                    break;
                }
            }
            if !load_failed {
                let count = am.arena_count();
                am.set_active_arena(count.saturating_sub(1));
            }
        }

        let wal_path = path.with_extension("wal");
        let (recovered_ops, next_lsn, checkpoint_lsn) = if wal_path.exists() {
            let recovery_manager = RecoveryManager::new(&wal_path);
            match recovery_manager.recover() {
                Ok(state) => {
                    let lsn = state.next_lsn;
                    let cp_lsn = state.stats.checkpoint_lsn;
                    (state.into_operations(), lsn, cp_lsn)
                }
                Err(e) => {
                    warn!("WAL recovery error: {:?}", e);
                    (Vec::new(), 1, None)
                }
            }
        } else {
            (Vec::new(), 1, None)
        };

        let async_config = AsyncWalConfig {
            pending_dir: path.parent().unwrap_or(Path::new(".")).join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig::default();
        let wal_writer = AsyncWalWriter::open_or_create(&wal_path, async_config, archive_config)
            .map_err(|e| {
                PersistentARTrieError::io_error(
                    "open_wal",
                    wal_path.display().to_string(),
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
        let wal_writer = Arc::new(wal_writer);

        // M2b — Order-A durable-overlay recovery seeding (mirrors mmap `open`):
        // watermark base = recovered durable WAL frontier (`next_lsn - 1`),
        // commit_seq seed = max(durable header floor, surviving CommitRank
        // generation) (the A.2 cross-restart fix). One-time WAL scan on open; INERT
        // pre-flip. See the mmap `open` body for the full rationale.
        // F7 FIX C: watermark base = max LSN over ALL segments (archive + active), so a
        // converted/under-load file's archived committed tail is covered before the first
        // post-conversion checkpoint (else a BatchIncrement delta double-applies). Falls
        // back to the active-only frontier when no segments are enumerable. (io_uring twin
        // of the mmap ctor's FIX-C seed.)
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
        let commit_seq_seed = {
            let mut max_commit_seq_gen = 0u64;
            if wal_path.exists() {
                use crate::persistent_artrie_core::wal::{WalReader, WalRecord};
                if let Ok(mut reader) = WalReader::new(&wal_path) {
                    while let Some(result) = reader.next_record() {
                        match result {
                            Ok((_lsn, WalRecord::CommitRank { generation, .. })) => {
                                max_commit_seq_gen = max_commit_seq_gen.max(generation);
                            }
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                }
            }
            wal_writer.commit_seq_floor().max(max_commit_seq_gen)
        };

        // The on-disk rank-regime + the F5 gate (read up-front so F5 can avoid
        // installing the owned dense tree). No-drift with the byte mmap ctor.
        let rank_regime = {
            use crate::persistent_artrie_core::wal::WalReader;
            WalReader::read_header(&wal_path)
                .map(|h| h.regime())
                .unwrap_or(crate::persistent_artrie_core::wal::RankRegime::Owned)
        };
        let use_f5 = {
            use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
            <Self as LockFreeOverlay<
                crate::persistent_artrie_core::key_encoding::ByteKey,
                V,
                IoUringDiskManager,
            >>::USE_F5_REOPEN_LOADER
                && rank_regime == crate::persistent_artrie_core::wal::RankRegime::Overlay
                && Self::overlay_eligible_v()
        };
        // **F7 convert gate** (io_uring twin): an Owned-regime eligible file is CONVERTED
        // into the overlay (rotate-if-records-non-empty → stamp → F5 build → archive-aware
        // drain). io_uring has no legacy/f5 test ctors, so the convert is gated on the F5
        // const directly (always true). Ineligible V stays owned.
        let convert_owned = {
            use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
            <Self as LockFreeOverlay<
                crate::persistent_artrie_core::key_encoding::ByteKey,
                V,
                IoUringDiskManager,
            >>::USE_F5_REOPEN_LOADER
                && rank_regime == crate::persistent_artrie_core::wal::RankRegime::Owned
                && Self::overlay_eligible_v()
        };

        // L3.3c (BLOCKER#4, io_uring twin): no eager owned pre-load; the owned `dict.root` is a
        // vestigial EMPTY placeholder (deleted at L3.3c-C2). The REAL codec `image_loaded` (with
        // the in-loader Err→empty fallback) drives the WAL drain-skip — not a separate eager
        // probe that could disagree with the codec on a corrupt-NODE image and brick the reopen.
        // L3.3c: the owned root is gone; the overlay (built below via `load_root_immutable`)
        // is the sole representation. The legacy owned term counter starts at 0.
        let initial_term_count = 0usize;

        let mut dict = Self {
            term_count: AtomicUsize::new(initial_term_count),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(Arc::clone(&wal_writer)),
            next_lsn: std::sync::atomic::AtomicU64::new(next_lsn),
            prefetcher: super::prefetch::Prefetcher::new(),
            arena_manager: Some(arena_manager),
            durability_policy: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                DurabilityPolicy::default(),
            ),
            epoch_manager: Arc::new(super::concurrency::EpochManager::new()),
            stats: Arc::new(super::concurrency::TrieStats::new()),
            eviction_coordinator: std::sync::Mutex::new(None),
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
            // M2b: seed watermark base + commit_seq from recovery (INERT pre-flip).
            committed_watermark:
                crate::persistent_artrie_core::committed_watermark::CommittedWatermark::new(
                    recovered_frontier,
                ),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            commit_seq: std::sync::atomic::AtomicU64::new(commit_seq_seed),
        };

        // F5 trait methods resolve through the seam.
        #[allow(unused_imports)]
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;

        if convert_owned {
            // ===== F7 CONVERT PATH (Owned-regime eligible → overlay; io_uring twin) =====
            // Rotate-if-records-non-empty → stamp Overlay (+ fsync, OBL-1) → F5 build →
            // archive-aware drain (FIX B) with the REAL (loaded_from_disk, image
            // checkpoint_lsn) (OBL-2; `checkpoint_lsn` is read PRE-rotate = the image redo
            // frontier). A `?` aborts open with the durable state intact.
            let _ = recovered_ops;
            let archive_config = WalConfig::default();
            dict.convert_owned_to_overlay_on_reopen(
                root_ptr,
                /* was_loaded_from_disk */ root_ptr != 0,
                checkpoint_lsn.unwrap_or(0),
                &archive_config,
            )?;
            dict.dirty.store(false, AtomicOrdering::Release);
        } else if use_f5 {
            // ===== F5 PATH (Overlay-regime; direct dense→overlay) =====
            // A corrupt/absent image ⇒ `image_loaded = false` (in-loader fallback) ⇒ empty
            // overlay + full WAL drain (corrupt-descriptor parity).
            let (_lc, image_loaded) = dict.load_root_immutable(root_ptr)?;
            let effective_loaded = (root_ptr != 0) && image_loaded;
            // **F7 FIX B:** drain ALL segments (archive + active) into the overlay (not
            // active-only), so an Overlay tail archived under load (or a post-S2-crash
            // converted file reopened as Overlay) recovers its archived tail. OBL-2:
            // image_checkpoint_lsn = the recovery `checkpoint_lsn`. RES-3 fail-loud (FIX E).
            let _ = recovered_ops;
            let archive_config = WalConfig::default();
            let _applied = dict.reconcile_and_drain_overlay(
                &archive_config,
                /* loaded_from_disk */ effective_loaded,
                if effective_loaded {
                    checkpoint_lsn.unwrap_or(0)
                } else {
                    0
                },
            )?;
            dict.dirty.store(false, AtomicOrdering::Release);
        }

        Ok(dict)
    }
}
