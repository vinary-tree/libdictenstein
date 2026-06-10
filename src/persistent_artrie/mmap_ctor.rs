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

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

use log::warn;

use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;

use super::arena_manager::ArenaManager;
use super::dict_impl::{DurabilityPolicy, PersistentARTrie};
use super::disk_load::read_root_descriptor_arena_count;
use super::error::{PersistentARTrieError, Result};
use super::wal::{AsyncWalConfig, AsyncWalWriter, WalConfig};

impl<V: DictionaryValue> PersistentARTrie<V> {
    /// **A freshly-created byte trie builds the lock-free overlay directly. The
    /// overlay is the SOLE representation for ALL `V`.** The byte twin of char's
    /// `install_overlay_on_create` (persistent_artrie_char/mmap_ctor.rs).
    ///
    /// A `create*` ctor builds a FRESH WAL (`current_lsn() == 1`), so the shared
    /// `install_overlay_on_create` default — `install_overlay()` (which stamps the
    /// Overlay regime on the empty WAL) + the V-2 stamp re-check — MUST engage. A
    /// failure to engage the Overlay regime therefore means the stamp silently failed
    /// (a torn header / no WAL), surfaced as a hard error rather than leaving a
    /// write-broken or recovery-unsafe overlay. NB the byte counter monomorph is
    /// `u64` (char's is also `u64`).
    fn install_overlay_on_create(self) -> Result<Self> {
        <Self as crate::persistent_artrie_core::overlay::flip::LockFreeOverlay<
            crate::persistent_artrie_core::key_encoding::ByteKey,
            _,
            _,
        >>::install_overlay_on_create(self)
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
        let mut trie = Self {
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
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
            // M2b: fresh in-memory trie — no durable WAL frontier, no prior
            // generations, so the watermark base + commit_seq are both 0 (a WAL-less
            // in-memory trie has no durable writes to advance them).
            committed_watermark:
                crate::persistent_artrie_core::committed_watermark::CommittedWatermark::new(0),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            commit_seq: std::sync::atomic::AtomicU64::new(0),
        };
        // **L3.3:** an in-memory `::new()` trie installs an empty lock-free overlay (WAL-less —
        // `install_overlay`'s WAL stamp is a no-op without a `wal_writer`), so `route_overlay()`
        // is UNIVERSALLY true across every constructor (the owned tree is gone). Writes degrade to
        // a non-durable in-memory CAS (the durable path's WAL append returns LSN 0 under
        // `Immediate`; `mark_committed(0)` is a no-op); reads + the zipper walk the overlay.
        // `checkpoint()` still errors (no buffer manager). This calls `install_overlay()`
        // directly (the WAL-less primitive), NOT `install_overlay_on_create` (which
        // hard-requires a WAL Overlay regime).
        trie.install_overlay();
        trie
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
        Self::install_overlay_on_create(Self {
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
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
            // install_overlay_on_create above builds the overlay (the sole representation
            // for ALL V). M2b: fresh on-disk trie (empty WAL) — no durable frontier, no
            // prior generations ⇒ watermark base + commit_seq both 0 (advanced by durable
            // writes).
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
        Self::install_overlay_on_create(Self {
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
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
            // M2b: fresh on-disk trie (empty WAL) — no durable frontier, no prior
            // generations ⇒ watermark base + commit_seq both 0 (advanced by durable writes).
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
        //     on open, exactly as char does. Nothing claims/marks until a durable
        //     `*_cas_durable` write runs.
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
        let use_f5 =
            force_f5 && rank_regime == crate::persistent_artrie_core::wal::RankRegime::Overlay;
        // **F7 convert gate:** an OWNED-regime, overlay-eligible file opened on the
        // PRODUCTION path (`force_f5` — `open`/`open_with_f5_loader`) is CONVERTED into the
        // overlay via `convert_owned_to_overlay_on_reopen` (rotate-if-records-non-empty →
        // stamp Overlay → F5 build → archive-aware drain). `open_with_legacy_loader`
        // (`force_f5 == false`) keeps the legacy owned-loader stay-owned path (the pre-F7
        // owned-reopen ORACLE the correspondence test compares against). An ineligible V
        // can never overlay, so it stays owned regardless.
        let convert_owned =
            force_f5 && rank_regime == crate::persistent_artrie_core::wal::RankRegime::Owned;

        // Create the dictionary with storage layer.
        // L3.3c (BLOCKER#4): the overlay is built DIRECTLY from the dense image via the codec
        // `load_root_immutable` (it reads the arenas itself); there is NO eager owned pre-load,
        // and the owned `dict.root` is a vestigial EMPTY placeholder (deleted at L3.3c-C2). The
        // REAL codec `image_loaded` (with the in-loader Err→empty fallback) drives the WAL
        // drain-skip — NOT a separate eager probe that could disagree with the codec on a
        // valid-descriptor + corrupt-NODE image and brick the reopen (the BLOCKER#4 footgun).
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
            // M2b: seed the watermark base from the recovered durable WAL frontier
            // (replayed LSNs are already committed) and the commit_seq from
            // max(header floor, surviving CommitRank generation) — the A.2
            // cross-restart fix.
            committed_watermark:
                crate::persistent_artrie_core::committed_watermark::CommittedWatermark::new(
                    recovered_frontier,
                ),
            checkpoint_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            merge_lock: std::sync::Arc::new(parking_lot::Mutex::new(())),
            commit_seq: std::sync::atomic::AtomicU64::new(commit_seq_seed),
        };

        // #48: the loaded image self-describes its IMAGE-COVERAGE frontier (the max WAL LSN whose
        // effects are folded into it), durable ATOMICALLY with the image. Take max(WAL Checkpoint
        // record, image coverage) so a TORN WAL `Checkpoint` record (stale/absent after a crash in
        // the publisher's image-fsync ↔ record-fsync window) cannot poison the drain-skip — the
        // durable image's own coverage backstops it. 0 when not loaded-from-disk or for a v1 image
        // ⇒ max = the WAL record = today's behavior.
        let effective_checkpoint_lsn: Option<super::wal::Lsn> = {
            let image_cov = if root_ptr != 0 {
                dict.buffer_manager
                    .as_ref()
                    .and_then(|bm| bm.read().storage().image_checkpoint_lsn().ok())
                    .unwrap_or(0)
            } else {
                0
            };
            let eff = checkpoint_lsn.unwrap_or(0).max(image_cov);
            if eff == 0 {
                None
            } else {
                Some(eff)
            }
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
                root_ptr,
                /* was_loaded_from_disk */ root_ptr != 0,
                effective_checkpoint_lsn.unwrap_or(0),
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
            let (_lc, image_loaded) = dict.load_root_immutable(root_ptr)?;
            let effective_loaded = (root_ptr != 0) && image_loaded;

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
                /* loaded_from_disk */ effective_loaded,
                if effective_loaded {
                    effective_checkpoint_lsn.unwrap_or(0)
                } else {
                    0
                },
            )?;
            dict.dirty.store(false, AtomicOrdering::Release);
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
                let trie = Self::create(path)?;

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

                // M2d (A2 fix, mirrors char's corruption-rebuild gate): an Overlay-regime
                // archive must DROP never-acked two-append-window orphans (else a
                // corruption rebuild resurrects them) and reorder same-term ops by commit
                // generation. Route the Overlay case through the canonical SHARED
                // regime-aware reconcile; the all-`Owned` case keeps the existing inline
                // streaming replay UNCHANGED (byte-for-byte the old rebuild for every
                // legacy/rank-less archive).
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
                            // L1: replay DIRECTLY into the overlay (the create-flip installed an
                            // empty overlay before this loop, so `route_overlay()==true`), NOT into
                            // the owned tree — eliminating the owned applier + the
                            // `reestablish_overlay_from_owned` conversion below (deleted in the same
                            // commit, R2). The overlay applier returns the same bool, so the
                            // `max_applied_lsn` / `had_apply_failure` image-coverage bookkeeping is
                            // unchanged (L1.0 made the BatchIncrement-delta arm return `false` on the
                            // same stop conditions the owned applier did).
                            if <Self as LockFreeOverlay<
                                ByteKey,
                                V,
                                super::disk_manager::MmapDiskManager,
                            >>::apply_recovered_operation_overlay(
                                &trie, op
                            ) {
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
                                // L1: replay DIRECTLY into the overlay (see the Overlay arm above).
                                if <Self as LockFreeOverlay<
                                    ByteKey,
                                    V,
                                    super::disk_manager::MmapDiskManager,
                                >>::apply_recovered_operation_overlay(
                                    &trie, op
                                ) {
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

                // L1: the recovered ops were replayed DIRECTLY into the overlay (the apply sinks
                // above), so there is NO owned→overlay conversion — the former owned-reading
                // reestablish sink is DELETED. That deletion was ATOMIC with the applier-swap
                // (R2): an owned converter here would have rebuilt an EMPTY root and FORCE-REPLACED
                // the just-populated overlay root = 100% silent loss.

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
