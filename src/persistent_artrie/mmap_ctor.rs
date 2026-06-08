//! `MmapDiskManager`-specific constructors for `PersistentARTrie<V>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~385-1109, ~725 LOC) as
//! the sixteenth Phase-5 byte sub-module. These constructors target
//! the default `MmapDiskManager` storage backend:
//!
//! - `new` (deprecated in-memory ctor)
//! - `create` / `create_with_slot_tracking`
//! - `open` / `open_with_slot_tracking`
//! - `open_with_recovery` / `open_with_recovery_and_slot_tracking`
//! - `open_with_recovery_config`
//!
//! The `IoUringDiskManager` variants live in `super::io_uring_ctor`;
//! generic methods (any `BlockStorage` backend) stay in
//! `dict_impl.rs`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

use log::warn;

use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;

use super::arena_manager::ArenaManager;
use super::bucket::StringBucket;
use super::dict_impl::{DurabilityPolicy, PersistentARTrie, TrieRoot};
use super::disk_load::read_root_descriptor_arena_count;
use super::error::{PersistentARTrieError, Result};
use super::wal::{AsyncWalConfig, AsyncWalWriter, WalConfig};

impl<V: DictionaryValue> PersistentARTrie<V> {
    /// **M4b EDIT 1 (owner-GO, IRREVERSIBLE): a freshly-created byte trie flips to
    /// the lock-free overlay for `V ∈ {(), i64}`; a strict NO-OP for arbitrary `V`.**
    /// The byte twin of char's `apply_create_flip` (persistent_artrie_char/mmap_ctor.rs).
    ///
    /// A `create*` ctor builds a FRESH WAL (`current_lsn() == 1`), so
    /// `flip_to_overlay`'s shared default — `enable_lockfree()` (which stamps the
    /// Overlay regime on the empty WAL, M2d) + selects `LockFreeOverlay` + the V-2
    /// stamp re-check — MUST engage. `!flip_to_overlay()` therefore means the stamp
    /// silently failed (a torn header / no WAL), surfaced as a hard error rather than
    /// enabling a write-broken or recovery-unsafe overlay. For `V ∉ {(), i64}`
    /// `overlay_eligible_v()` is false, the gate short-circuits, the trie stays
    /// `OwnedTree`, and this is a pure no-op (the proven owned path runs unchanged —
    /// backward-compat for arbitrary V). NB the byte eligible counter monomorph is
    /// `i64` (char's is `u64`).
    fn apply_create_flip(mut self) -> Result<Self> {
        if Self::overlay_eligible_v() && !self.flip_to_overlay() {
            return Err(PersistentARTrieError::internal(
                "byte create-flip: flip did not engage on a fresh trie",
            ));
        }
        Ok(self)
    }

    /// Create a new empty in-memory dictionary.
    ///
    /// # Deprecated
    ///
    /// This method is deprecated because "Persistent" types are designed for
    /// disk-backed storage. Use `create()` or `open()` for disk persistence.
    /// For in-memory tries, use the optimized implementations instead:
    /// - [`DoubleArrayTrie`](crate::double_array_trie::DoubleArrayTrie) (fastest reads, insert-only)
    /// - [`DynamicDawg`](crate::dynamic_dawg::DynamicDawg) (insert + remove, SIMD optimized)
    #[deprecated(
        since = "0.2.0",
        note = "Use `create()` or `open()` for disk persistence. For in-memory tries, use DoubleArrayTrie or DynamicDawg instead."
    )]
    pub fn new() -> Self {
        Self {
            root: RwLock::new(TrieRoot::Bucket(StringBucket::with_values())),
            term_count: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: None,
            wal_writer: None,
            next_lsn: std::sync::atomic::AtomicU64::new(0),
            prefetcher: super::prefetch::Prefetcher::disabled(),
            arena_manager: None,
            durability_policy: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                DurabilityPolicy::default(),
            ),
            epoch_manager: Arc::new(super::concurrency::EpochManager::new()),
            stats: Arc::new(super::concurrency::TrieStats::new()),
            eviction_coordinator: std::sync::Mutex::new(None),
            dirty_prefixes: std::sync::Mutex::new(HashSet::new()),
            persisted_disk_locations: RwLock::new(HashMap::new()),
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
            // M2a INERT default (OwnedTree) — changes no byte behavior.
            overlay_write_mode: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                crate::persistent_artrie_core::overlay::write_mode::OverlayWriteMode::default(),
            ),
            // M2b: fresh in-memory trie — no durable WAL frontier, no prior
            // generations, so the watermark base + commit_seq are both 0 (INERT
            // pre-flip; only opt-in `*_cas_durable` writes advance them).
            committed_watermark:
                crate::persistent_artrie_core::committed_watermark::CommittedWatermark::new(0),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            commit_seq: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Create a new persistent dictionary at the given path.
    ///
    /// This creates a new dictionary file with WAL for crash recovery.
    /// If a file already exists at the path, this will return an error.
    ///
    /// # Arguments
    /// * `path` - Path to the dictionary file (will also create `.wal` file)
    ///
    /// # Example
    /// ```text
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let dict: PersistentARTrie<()> = PersistentARTrie::create("words.part")?;
    /// ```
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        use super::buffer_manager::BufferManager;
        use super::disk_manager::DiskManager;
        use super::DEFAULT_BUFFER_POOL_SIZE;

        let path = path.as_ref();

        // Fail if file already exists
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

        // Create disk manager (creates new file)
        let disk_manager = DiskManager::create(path)?;

        // Create buffer manager with default pool size (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create async WAL file alongside the main file
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

        // Create arena manager for space-efficient node storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        // M4b EDIT 1: flip a fresh eligible-V trie to the overlay (no-op for arbitrary V).
        Self::apply_create_flip(Self {
            root: RwLock::new(TrieRoot::Bucket(StringBucket::with_values())),
            term_count: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            next_lsn: std::sync::atomic::AtomicU64::new(1), // Start at 1, 0 reserved for "no LSN"
            prefetcher: super::prefetch::Prefetcher::new(),
            arena_manager: Some(arena_manager),
            durability_policy: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                DurabilityPolicy::default(),
            ),
            epoch_manager: Arc::new(super::concurrency::EpochManager::new()),
            stats: Arc::new(super::concurrency::TrieStats::new()),
            eviction_coordinator: std::sync::Mutex::new(None),
            dirty_prefixes: std::sync::Mutex::new(HashSet::new()),
            persisted_disk_locations: RwLock::new(HashMap::new()),
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
            // M2a INERT default (OwnedTree) — flipped to LockFreeOverlay by
            // apply_create_flip above for eligible V; arbitrary V stays owned.
            overlay_write_mode: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                crate::persistent_artrie_core::overlay::write_mode::OverlayWriteMode::default(),
            ),
            // M2b: fresh on-disk trie (empty WAL) — no durable frontier, no prior
            // generations ⇒ watermark base + commit_seq both 0 (INERT pre-flip).
            committed_watermark:
                crate::persistent_artrie_core::committed_watermark::CommittedWatermark::new(0),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            commit_seq: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Create a new persistent dictionary with slot-level dirty tracking.
    ///
    /// This enables incremental checkpoints that write only modified slots
    /// instead of entire 256KB arenas, reducing checkpoint I/O by 90%+ for
    /// localized updates.
    ///
    /// # Arguments
    /// * `path` - Path to the dictionary file (must not exist)
    ///
    /// # Example
    /// ```text
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let dict: PersistentARTrie<()> = PersistentARTrie::create_with_slot_tracking("words.part")?;
    /// ```
    pub fn create_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        use super::arena_manager::FlushConfig;
        use super::buffer_manager::BufferManager;
        use super::disk_manager::DiskManager;
        use super::DEFAULT_BUFFER_POOL_SIZE;

        let path = path.as_ref();

        // Fail if file already exists
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

        // Create disk manager (creates new file)
        let disk_manager = DiskManager::create(path)?;

        // Create buffer manager with default pool size (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create async WAL file alongside the main file
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

        // Create arena manager with slot-level tracking enabled
        let flush_config = FlushConfig::with_slot_tracking();
        let arena_manager =
            ArenaManager::with_buffer_manager_and_config(Arc::clone(&buffer_manager), flush_config);
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        // M4b EDIT 1: flip a fresh eligible-V trie to the overlay (no-op for arbitrary V).
        Self::apply_create_flip(Self {
            root: RwLock::new(TrieRoot::Bucket(StringBucket::with_values())),
            term_count: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            next_lsn: std::sync::atomic::AtomicU64::new(1), // Start at 1, 0 reserved for "no LSN"
            prefetcher: super::prefetch::Prefetcher::new(),
            arena_manager: Some(arena_manager),
            durability_policy: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                DurabilityPolicy::default(),
            ),
            epoch_manager: Arc::new(super::concurrency::EpochManager::new()),
            stats: Arc::new(super::concurrency::TrieStats::new()),
            eviction_coordinator: std::sync::Mutex::new(None),
            dirty_prefixes: std::sync::Mutex::new(HashSet::new()),
            persisted_disk_locations: RwLock::new(HashMap::new()),
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
            // M2a INERT default (OwnedTree) — changes no byte behavior.
            overlay_write_mode: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                crate::persistent_artrie_core::overlay::write_mode::OverlayWriteMode::default(),
            ),
            // M2b: fresh on-disk trie (empty WAL) — no durable frontier, no prior
            // generations ⇒ watermark base + commit_seq both 0 (INERT pre-flip).
            committed_watermark:
                crate::persistent_artrie_core::committed_watermark::CommittedWatermark::new(0),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            commit_seq: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Open an existing persistent dictionary from disk.
    ///
    /// This opens an existing dictionary file and replays the WAL if needed
    /// to recover from any crash.
    ///
    /// # Arguments
    /// * `path` - Path to the dictionary file
    ///
    /// # Example
    /// ```text
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let dict: PersistentARTrie<()> = PersistentARTrie::open("words.part")?;
    /// ```
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        use crate::persistent_artrie_core::key_encoding::ByteKey;
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
        // This impl block is the default-`S` (`MmapDiskManager`) block.
        let gate = <Self as LockFreeOverlay<ByteKey, V, super::disk_manager::MmapDiskManager>>::USE_F5_REOPEN_LOADER;
        Self::open_inner(path.as_ref(), gate)
    }

    /// **F5 (S2 test surface) — reopen via the DIRECT dense→overlay loader**,
    /// regardless of the [`Self::USE_F5_REOPEN_LOADER`] gate (byte twin of char's
    /// `open_with_f5_loader`). An Overlay-regime file is reopened through
    /// `load_root_immutable` + `replay_records_lww_overlay`; an Owned-regime file still
    /// uses the owned loader. Used by the F5 both-loaders correspondence proptest.
    pub fn open_with_f5_loader<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_inner(path.as_ref(), true)
    }

    /// **F5 (S2 test surface) — reopen via the LEGACY owned-loader→reestablish path**,
    /// regardless of the [`Self::USE_F5_REOPEN_LOADER`] gate (byte twin of char's
    /// `open_with_legacy_loader`). Keeps the both-loaders proptest a meaningful
    /// legacy-vs-F5 oracle whether the gate is ON or OFF.
    pub fn open_with_legacy_loader<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_inner(path.as_ref(), false)
    }

    /// Shared `open` body. `force_f5` selects the F5 dense→overlay loader for an
    /// Overlay-regime file (the gate value from `open`, or `true` from
    /// `open_with_f5_loader`); an Owned-regime file ignores it.
    fn open_inner(path: &Path, force_f5: bool) -> Result<Self> {
        use super::buffer_manager::BufferManager;
        use super::disk_manager::DiskManager;
        use super::recovery::RecoveryManager;
        use super::DEFAULT_BUFFER_POOL_SIZE;
        // F5 trait methods resolve through the seam.
        #[allow(unused_imports)]
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;

        // Fail if file doesn't exist
        if !path.exists() {
            return Err(PersistentARTrieError::io_error(
                "open",
                path.display().to_string(),
                std::io::Error::new(std::io::ErrorKind::NotFound, "Dictionary file not found"),
            ));
        }

        super::compaction_impl::recover_in_place_compaction_finalization(path)?;

        // Open disk manager
        let disk_manager = DiskManager::open(path)?;

        // Get root pointer to check if trie exists
        let root_ptr = disk_manager.root_ptr()?;
        let _entry_count = disk_manager.entry_count()?;

        // Read arena_count from the root descriptor. A corrupt descriptor must
        // fail closed into WAL replay instead of driving unbounded arena loads.
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

        // Create buffer manager (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create arena manager for space-efficient node storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        // Load arenas into ArenaManager using derived block IDs
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

        // Now load trie from disk using the arena manager
        let (loaded_root, loaded_term_count) = if root_ptr != 0 {
            match Self::load_root_from_disk_with_arena(&buffer_manager, &arena_manager, root_ptr) {
                Ok((root, count)) => (Some(root), count),
                Err(e) => {
                    warn!("Failed to load trie from disk: {:?}", e);
                    (None, 0)
                }
            }
        } else {
            (None, 0)
        };

        // Recover from WAL if it exists
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

        // Open async WAL writer using TOCTOU-safe pattern
        // Matches formal model's `open_or_create_safe` in FileSystem.v:
        // - Uses mkdir_all (idempotent) to ensure parent exists
        // - Uses atomic open/create operations to avoid races
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

        // M2b — Order-A durable-overlay recovery seeding (mirrors char mmap_ctor).
        //
        // (1) The committed-watermark BASE is the recovered durable WAL frontier.
        //     **F7 FIX C:** seed it from the max LSN over the FULL segment set (archive +
        //     active), NOT active-only (`next_lsn - 1`). A converted/under-load file's
        //     committed tail lives in an ARCHIVED segment (the rotated Owned tail), so an
        //     active-only base would be 0 post-rotate ⇒ the first post-conversion
        //     checkpoint would write `checkpoint_lsn = 0 < tail_max` ⇒ the archive re-drain
        //     skip `tail_lsn <= checkpoint_lsn` would be FALSE ⇒ a BatchIncrement delta
        //     DOUBLE-APPLIES. Seeding from `max_lsn_in_segments(all)` sets "all <= max
        //     committed" directly (image+archive+active are ALL durable), guaranteeing
        //     `watermark() >= tail_max` before the first checkpoint. For a normal Overlay
        //     file the archive is already checkpoint-subsumed, so this equals the
        //     active-only value (a no-op there). On a fresh/empty WAL ⇒ base 0.
        // (2) The `commit_seq` SEED is `max(durable header floor, max surviving
        //     CommitRank generation)` — the A.2 cross-restart fix so a post-reopen
        //     durable op out-ranks every pre-restart survivor (the seeded commit_seq
        //     is strictly above any generation folded into the recovered state). The
        //     header floor is currently 0 until the overlay checkpoint raises it; the
        //     WAL scan covers the un-checkpointed tail. Byte's `open` uses
        //     `RecoveryManager` (which expands `CommitRank` to nothing), so we do a
        //     lightweight extra `WalReader` pass for the max generation — one-time,
        //     on open, exactly as char does. INERT pre-flip: nothing claims/marks
        //     until an opt-in `*_cas_durable` write runs.
        // F7 FIX C: base = max LSN over ALL segments (archive + active), falling back to
        // the active-only frontier when no segments are enumerable (e.g. archiving off).
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
                            Err(_) => break, // stop at the durable prefix
                        }
                    }
                }
            }
            // Combine with the durable header floor (raise-only, carried across rotate).
            wal_writer.commit_seq_floor().max(max_commit_seq_gen)
        };

        // The on-disk rank-regime, read up-front so the F5 gate can decide BEFORE the
        // owned dense tree is installed into `dict.root` (F5 does NOT materialize it
        // into the owned tree — it builds the overlay directly). Unreadable ⇒ Owned
        // (keep, never drop). This is the SAME value that drives the reconcile below.
        let rank_regime = {
            use crate::persistent_artrie_core::wal::WalReader;
            WalReader::read_header(&wal_path)
                .map(|h| h.regime())
                .unwrap_or(crate::persistent_artrie_core::wal::RankRegime::Owned)
        };
        // F5 gate: a direct dense→overlay reopen runs ONLY for an Overlay-regime,
        // overlay-eligible file when F5 is selected. Everything else is LEGACY.
        let use_f5 = force_f5
            && rank_regime == crate::persistent_artrie_core::wal::RankRegime::Overlay
            && Self::overlay_eligible_v();
        // **F7 convert gate:** an OWNED-regime, overlay-eligible file opened on the
        // PRODUCTION path (`force_f5` — `open`/`open_with_f5_loader`) is CONVERTED into the
        // overlay via `convert_owned_to_overlay_on_reopen` (rotate-if-records-non-empty →
        // stamp Overlay → F5 build → archive-aware drain). `open_with_legacy_loader`
        // (`force_f5 == false`) keeps the legacy owned-loader stay-owned path (the pre-F7
        // owned-reopen ORACLE the correspondence test compares against). An ineligible V
        // can never overlay, so it stays owned regardless.
        let convert_owned = force_f5
            && rank_regime == crate::persistent_artrie_core::wal::RankRegime::Owned
            && Self::overlay_eligible_v();

        // Create the dictionary with storage layer.
        // LEGACY: install the loaded dense root into `dict.root`. F5 + CONVERT: start with
        // an EMPTY bucket — the overlay is built directly from the dense image
        // (`load_root_immutable` re-loads it as transient scratch + converts; the eager
        // `loaded_root` above is dropped). The `was_loaded_from_disk` flag stays the
        // dense-image-present signal for the WAL checkpoint-skip in ALL paths.
        let was_loaded_from_disk = loaded_root.is_some();
        // **F7 corrupt-image fallback.** The dense image (`root_ptr`) is loaded ONLY if the
        // eager pre-load above SUCCEEDED. A corrupt/unreadable descriptor makes the eager
        // load fail (`loaded_root == None`) even though `root_ptr != 0`; the legacy owned
        // path then FALLS BACK to WAL replay (it does not trust the bad image). The F5/
        // converter path must do the same: pass `root_ptr = 0` to the overlay builder so it
        // installs an EMPTY overlay and recovers everything from the WAL drain, rather than
        // re-attempting the failing image load and propagating the error.
        let effective_root_ptr = if was_loaded_from_disk { root_ptr } else { 0 };
        let (initial_root, initial_term_count) = match loaded_root {
            // F5 / CONVERT: DROP the loaded owned root; `dict.root` stays an empty bucket.
            Some(_) if use_f5 || convert_owned => {
                (TrieRoot::Bucket(StringBucket::with_values()), 0)
            }
            Some(root) => (root, loaded_term_count as usize),
            None => (TrieRoot::Bucket(StringBucket::with_values()), 0),
        };

        let mut dict = Self {
            root: RwLock::new(initial_root),
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
            dirty_prefixes: std::sync::Mutex::new(HashSet::new()),
            persisted_disk_locations: RwLock::new(HashMap::new()),
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
            // M2a INERT default (OwnedTree) — changes no byte behavior.
            overlay_write_mode: crate::persistent_artrie_core::shared_access::AtomicEnumCell::new(
                crate::persistent_artrie_core::overlay::write_mode::OverlayWriteMode::default(),
            ),
            // M2b: seed the watermark base from the recovered durable WAL frontier
            // (replayed LSNs are already committed) and the commit_seq from
            // max(header floor, surviving CommitRank generation) — the A.2
            // cross-restart fix. INERT pre-flip.
            committed_watermark:
                crate::persistent_artrie_core::committed_watermark::CommittedWatermark::new(
                    recovered_frontier,
                ),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            commit_seq: std::sync::atomic::AtomicU64::new(commit_seq_seed),
        };

        if convert_owned {
            // ===== F7 CONVERT PATH (Owned-regime eligible file → overlay) =====
            // Rotate-if-records-non-empty → stamp Overlay (+ fsync, OBL-1) → F5 build from
            // the dense image → archive-aware drain (FIX B) with the per-segment regime and
            // the REAL (loaded_from_disk, image checkpoint_lsn) (OBL-2). `checkpoint_lsn`
            // here is the recovery value read PRE-rotate from the Owned active WAL Checkpoint
            // record = the dense-image redo frontier. A `?` aborts open with the durable
            // state intact. `recovered_ops` is unused (the converter reconciles raw segment
            // records carrying CommitRank).
            let _ = recovered_ops;
            let archive_config = WalConfig::default();
            dict.convert_owned_to_overlay_on_reopen(
                effective_root_ptr,
                was_loaded_from_disk,
                checkpoint_lsn.unwrap_or(0),
                &archive_config,
            )?;
            dict.dirty.store(false, AtomicOrdering::Release);
        } else if use_f5 {
            // ===== F5 PATH (Overlay-regime; direct dense→overlay; owned tree NOT
            // installed) =====
            // (1) Build the overlay root DIRECTLY from the dense image (eager-load owned
            // as transient scratch → walk-convert → install pre-built root + select
            // LockFreeOverlay + verify Overlay regime). A `?` aborts open; `dict.root`
            // stays the empty bucket and the durable image is intact. A corrupt image
            // (eager pre-load failed ⇒ `effective_root_ptr == 0`) installs an EMPTY overlay
            // and recovers from the WAL drain below (the legacy fallback parity).
            dict.load_root_immutable(effective_root_ptr)?;

            // (2) **F7 FIX B:** drain ALL WAL segments (archive + active) INTO THE OVERLAY,
            // not just the active file. A normal Overlay file's archive is checkpoint-
            // subsumed (the FIX-C base-seed + the reconcile skip make the subsumed archive
            // a no-op), but an Overlay file whose tail was archived under load (or a
            // post-S2-crash converted file that reopened as Overlay) needs the archived
            // tail drained too (OBLIGATION-A). `loaded_from_disk = was_loaded_from_disk`;
            // `image_checkpoint_lsn = checkpoint_lsn` (the recovery value = the image redo
            // frontier; OBL-2). The per-segment regime drops Overlay orphans and keeps a
            // converted Owned tail. A `?` (RES-3 prefix gap, FIX E) aborts open loudly.
            let _ = recovered_ops;
            let archive_config = WalConfig::default();
            let _applied = dict.reconcile_and_drain_overlay(
                &archive_config,
                /* loaded_from_disk */ was_loaded_from_disk,
                checkpoint_lsn.unwrap_or(0),
            )?;
            dict.dirty.store(false, AtomicOrdering::Release);
        } else {
            // ===== LEGACY PATH (unchanged) =====
            // Replay recovered operations via the shared M2d regime-aware path
            // (`replay_records_lww`): for an `Owned` WAL this is byte-for-byte the
            // pre-M2d in-order replay of the transaction-filtered `RecoveryManager` ops;
            // for an `Overlay` WAL it threads the SHARED A2 `reconcile_lww` (same-term
            // ops by commit generation; unranked orphans DROPPED).
            // The reconcile needs the RAW WAL records (the `CommitRank` markers that
            // `RecoveryManager` strips). Collect them ONLY for the rare/cold `Overlay`
            // post-flip path so the `Owned` hot path keeps its old single scan.
            let raw_records: Vec<(super::wal::Lsn, super::wal::WalRecord)> = if rank_regime
                == crate::persistent_artrie_core::wal::RankRegime::Overlay
                && wal_path.exists()
            {
                use crate::persistent_artrie_core::wal::WalReader;
                let mut records = Vec::new();
                if let Ok(mut reader) = WalReader::new(&wal_path) {
                    while let Some(result) = reader.next_record() {
                        match result {
                            Ok((lsn, record)) => records.push((lsn, record)),
                            Err(_) => break, // stop at the durable prefix
                        }
                    }
                }
                records
            } else {
                Vec::new()
            };
            let replayed_count = dict.replay_records_lww(
                recovered_ops,
                raw_records,
                was_loaded_from_disk,
                checkpoint_lsn,
                rank_regime,
            );
            // Mark clean after recovery replay
            dict.dirty.store(false, AtomicOrdering::Release);

            // If we loaded from disk AND replayed no operations, we can truncate the WAL
            // (all operations were already persisted to disk before the checkpoint)
            if was_loaded_from_disk && replayed_count == 0 {
                if let Err(e) = wal_writer.truncate() {
                    warn!("Failed to truncate WAL after recovery: {:?}", e);
                } else if let Some(threshold) = checkpoint_lsn {
                    let next_lsn = threshold.saturating_add(1);
                    wal_writer.set_min_lsn(next_lsn);
                    dict.next_lsn.store(next_lsn, AtomicOrdering::Release);
                }
            }

            // **F7 — LEGACY-LOADER ORACLE Overlay branch (force_f5 == false ONLY).** This
            // arm is now reachable solely via `open_with_legacy_loader` on an Overlay file
            // (the production path routes Overlay → F5 and Owned → convert above). It keeps
            // the legacy reopen-into-overlay behavior so the both-loaders correspondence
            // test stays a meaningful F5-vs-legacy oracle. D1: the flip precedes the
            // structural reestablish, so `reestablish_overlay_from_owned` (the KEPT converter
            // that REPLACES the deleted per-term `reestablish_overlay_dispatch` — same
            // overlay, strictly more correct) reads the recovered OWNED tree via the UN-routed
            // `owned_*` seams, builds + installs the overlay root, then clears owned LAST
            // (RES-7) — a mid-stream `?` aborts `open` with the owned tree intact.
            if rank_regime == crate::persistent_artrie_core::wal::RankRegime::Overlay
                && Self::overlay_eligible_v()
            {
                use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
                let took = dict.flip_to_overlay();
                debug_assert!(took, "Overlay-regime open must flip");
                <Self as LockFreeOverlay<
                    crate::persistent_artrie_core::key_encoding::ByteKey,
                    V,
                    super::disk_manager::MmapDiskManager,
                >>::reestablish_overlay_from_owned(&mut dict)?;
            }
        }

        Ok(dict)
    }

    /// Open an existing persistent dictionary with slot-level dirty tracking enabled.
    ///
    /// Slot-level tracking reduces checkpoint I/O by writing only modified slots
    /// instead of entire arenas. For vocabularies with localized updates, this
    /// can reduce checkpoint I/O by 90%+.
    ///
    /// This is equivalent to calling `open()` followed by enabling slot tracking
    /// on the arena manager, but provides a convenient single-call API.
    ///
    /// # Arguments
    /// * `path` - Path to the dictionary file (must exist)
    ///
    /// # Example
    /// ```text
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// // Open existing vocabulary with slot-level tracking
    /// let mut dict = PersistentARTrie::<u64>::open_with_slot_tracking("vocab.part")?;
    ///
    /// // Subsequent allocations will be tracked at slot level
    /// dict.insert("new_term", Some(42));
    ///
    /// // Checkpoint writes only modified slots
    /// dict.checkpoint()?;
    /// ```
    pub fn open_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        let dict = Self::open(path)?;

        // Enable slot-level tracking on the arena manager
        if let Some(ref am) = dict.arena_manager {
            am.write().enable_slot_tracking();
        }

        Ok(dict)
    }

    /// Open with both recovery and slot tracking enabled.
    ///
    /// Combines `open_with_recovery()` and slot tracking enablement.
    /// Returns `(trie, recovery_report)` so callers can inspect recovery status.
    pub fn open_with_recovery_and_slot_tracking<P: AsRef<Path>>(
        path: P,
    ) -> Result<(Self, super::recovery::RecoveryReport)> {
        let (dict, report) = Self::open_with_recovery(path)?;
        if let Some(ref am) = dict.arena_manager {
            am.write().enable_slot_tracking();
        }
        Ok((dict, report))
    }

    /// Open an existing persistent dictionary with automatic corruption detection and recovery.
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
    /// * `path` - Path to the dictionary file
    ///
    /// # Returns
    ///
    /// Tuple of (trie, recovery_report) indicating what recovery was performed.
    ///
    /// # Example
    ///
    /// ```text
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let (dict, report) = PersistentARTrie::<i64>::open_with_recovery("data.part")?;
    ///
    /// if !report.mode.is_normal() {
    ///     eprintln!("Recovered from crash: {} records replayed", report.records_replayed);
    /// }
    /// ```
    pub fn open_with_recovery<P: AsRef<Path>>(
        path: P,
    ) -> Result<(Self, super::recovery::RecoveryReport)> {
        use super::wal::WalConfig;
        Self::open_with_recovery_config(path, WalConfig::default())
    }

    /// Open with recovery and custom WAL configuration.
    ///
    /// Same as `open_with_recovery()` but allows specifying custom WAL settings.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the dictionary file
    /// * `config` - WAL configuration for archive mode, segment limits, etc.
    ///
    /// # Returns
    ///
    /// Tuple of (trie, recovery_report) indicating what recovery was performed.
    pub fn open_with_recovery_config<P: AsRef<Path>>(
        path: P,
        config: super::wal::WalConfig,
    ) -> Result<(Self, super::recovery::RecoveryReport)> {
        use super::recovery::{
            collect_retained_wal_segments_for_rebuild, detect_corruption, RecoveryReport,
        };
        use super::wal::WalReader;
        use std::time::Instant;
        // F7-R1: the structural owned→overlay converter resolves through the seam.
        use crate::persistent_artrie_core::key_encoding::ByteKey;
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;

        let path = path.as_ref();
        let start_time = Instant::now();

        // Check if file exists
        if !path.exists() {
            // No file - create new and return CreatedNew report
            let trie = Self::create(path)?;
            return Ok((trie, RecoveryReport::created_new()));
        }

        // Check for corruption
        match detect_corruption(path, true) {
            Ok(None) => {
                // No corruption detected - open normally
                let trie = Self::open(path)?;
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
                let mut trie = Self::create(path)?;

                // Rebuild from WAL archive segments
                let mut records_replayed: u64 = 0;
                let mut terms_recovered: u64 = 0;
                let mut segments_used = Vec::new();
                // C2 (recovery double-apply fix): track the max LSN ACTUALLY applied + whether
                // any apply failed, to compute the image-coverage frontier safely (see the set
                // below). NEVER derived from `max_lsn_in_segments` (which reads past interior
                // corruption → over-claim → reopen would SKIP un-applied records = silent LOSS).
                let mut max_applied_lsn: u64 = 0;
                let mut had_apply_failure = false;

                // M2d (A2 fix, mirrors char's corruption-rebuild gate): a post-flip
                // Overlay archive must DROP never-acked two-append-window orphans
                // (else a corruption rebuild resurrects them) and reorder same-term
                // ops by commit generation. Route the Overlay case through the
                // canonical SHARED regime-aware reconcile; the all-`Owned` case keeps
                // the existing inline streaming replay UNCHANGED (INERT pre-flip —
                // byte-for-byte the old rebuild for every legacy/rank-less archive).
                let any_overlay = segments.iter().any(|seg| {
                    WalReader::read_header(seg)
                        .map(|h| {
                            h.regime() == crate::persistent_artrie_core::wal::RankRegime::Overlay
                        })
                        .unwrap_or(false)
                });
                if any_overlay {
                    let (rr, tr) =
                        super::recovery::rebuild_from_wal_segments_regime_aware(&segments, |op| {
                            let op_lsn = op.lsn();
                            if trie.apply_recovered_operation_no_wal(op) {
                                if op_lsn > max_applied_lsn {
                                    max_applied_lsn = op_lsn;
                                }
                                Ok(())
                            } else {
                                had_apply_failure = true;
                                Err("failed to apply recovered archive operation".to_string())
                            }
                        })
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
                        let reader = match WalReader::new(segment_path) {
                            Ok(r) => r,
                            Err(_) => continue, // Skip unreadable segments
                        };

                        segments_used.push(segment_path.clone());

                        for result in reader.iter() {
                            let (lsn, record) = match result {
                                Ok(r) => r,
                                Err(e) => {
                                    warn!(
                                        "Corrupted WAL record during rebuild; stopping at durable prefix: {:?}",
                                        e
                                    );
                                    break 'segments;
                                }
                            };

                            records_replayed += 1;

                            for op in super::recovery::recovered_operations_from_record(lsn, record)
                            {
                                if trie.apply_recovered_operation_no_wal(op) {
                                    terms_recovered += 1;
                                } else {
                                    had_apply_failure = true;
                                    warn!(
                                        "Recovered operation failed during rebuild; stopping at durable prefix"
                                    );
                                    break 'segments;
                                }
                            }
                            // C2: this record applied IN FULL (no `break` above) — advance the
                            // image-coverage frontier. Records stream in LSN order, so this is a
                            // safe prefix bound.
                            if lsn > max_applied_lsn {
                                max_applied_lsn = lsn;
                            }
                        }
                    }
                }

                // C2 (recovery double-apply fix): record the IMAGE-COVERAGE frontier = the max
                // LSN ACTUALLY applied (0 on any apply failure — conservative; an over-claim
                // would make the reopen drain-skip SKIP un-applied records = silent LOSS). The
                // first post-recovery `checkpoint()` folds this into the on-disk
                // `Checkpoint.checkpoint_lsn` WITHOUT inflating the durability watermark (the #41
                // capture assert is untouched). 0 ⇒ no override (the rare apply-failure path
                // keeps the prior re-drain behavior — a recoverable double-apply, not loss).
                trie.committed_watermark
                    .set_recovery_image_coverage(if had_apply_failure {
                        0
                    } else {
                        max_applied_lsn
                    });

                let duration_ms = start_time.elapsed().as_millis() as u64;

                let report = RecoveryReport::rebuild_from_wal(
                    path.to_path_buf(),
                    corruption_reason,
                    records_replayed,
                    terms_recovered,
                    segments_used,
                    duration_ms,
                );

                // M4b REESTABLISH SINK (D-SINK — the corruption-rebuild arm).
                // `trie` was built by `Self::create(path)` above, which create-flips a
                // fresh eligible-V trie (`route_overlay()==true`); the replay arms above
                // then wrote the recovered terms into the OWNED tree via
                // `apply_recovered_operation_no_wal` (the `*_impl_core` un-routed owned
                // writers). Without this sink the recovered owned data would NEVER reach
                // the overlay, and the first post-recovery `checkpoint()` would take the
                // overlay route and persist the EMPTY overlay = total irreversible loss.
                // Gate on `route_overlay()` (⟺ an eligible V was create-flipped), NOT on
                // `any_overlay`: it covers BOTH replay arms (the regime-aware Overlay
                // path AND the inline Owned-archive streaming path) and is a strict
                // no-op for arbitrary V (which `create` did not flip ⇒ !route_overlay).
                // D1/F7-R1: `reestablish_overlay_from_owned` reads the recovered owned
                // tree via the UN-routed `owned_*` seams (it runs with `route_overlay()`
                // already true), STRUCTURALLY builds the overlay root via
                // `build_overlay_root_from_owned`, FORCE-REPLACES the empty create-flip
                // overlay root (the F5 structural converter, equivalent to the legacy
                // per-term `reestablish_overlay_dispatch` but strictly more correct on a
                // term-only counter member), and clears owned LAST (RES-7); a mid-stream
                // `?` aborts with the owned tree intact.
                if trie.route_overlay() {
                    <Self as LockFreeOverlay<
                        ByteKey,
                        V,
                        super::disk_manager::MmapDiskManager,
                    >>::reestablish_overlay_from_owned(&mut trie)?;
                }

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
}
