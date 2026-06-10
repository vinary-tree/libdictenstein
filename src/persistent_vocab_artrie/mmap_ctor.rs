//! `MmapDiskManager`-specific constructors for `PersistentVocabARTrie` — OVERLAY-ONLY (V6).
//!
//! - `create` / `create_with_start_index`
//! - `open` / `open_with_recovery` / `open_snapshot`
//!
//! Every ctor flips to the lock-free overlay at construction; the owned tree, the reverse-index
//! sidecar, and the bloom filter are deleted. The `IoUringDiskManager` variants live in
//! `super::io_uring_ctor`; generic methods (any `BlockStorage` backend) stay in `dict_impl.rs`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::persistent_artrie::buffer_manager::BufferManager;
use crate::persistent_artrie::dict_impl::DurabilityPolicy;
use crate::persistent_artrie::disk_manager::DiskManager;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::recovery::RecoveryReport;
use crate::persistent_artrie::wal::WalConfig;
use crate::persistent_artrie::wal_managed::{create_async_wal, open_or_create_async_wal};
use crate::persistent_artrie_char::arena_manager::ArenaManager;

use super::dict_impl::PersistentVocabARTrie;
use super::types::{VocabTrieFileHeader, DEFAULT_VOCAB_BUFFER_POOL_SIZE, VOCAB_HEADER_VERSION_V2};

impl PersistentVocabARTrie {
    /// Create a new vocabulary trie at the given path.
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::create_with_start_index(path, 0)
    }

    /// Create a new vocabulary trie with a custom starting index.
    pub fn create_with_start_index<P: AsRef<Path>>(path: P, start_index: u64) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        if path.exists() {
            return Err(PersistentARTrieError::CorruptedFile {
                reason: format!("File already exists: {}", path.display()),
            });
        }

        let disk_manager = DiskManager::create(&path)?;
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_VOCAB_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        // Write initial header (version 2 = overlay format from creation).
        {
            let bm = buffer_manager.write();
            let dm = bm.storage();
            let mut header = VocabTrieFileHeader::with_start_index(start_index);
            header.version = VOCAB_HEADER_VERSION_V2;
            dm.write_header_bytes(&header.to_bytes_with_checksum())?;
            dm.sync()?;
        }

        let wal_path = path.with_extension("vocab.wal");
        let wal_config = WalConfig::default();
        let wal_writer = create_async_wal(&wal_path, &path).map_err(|e| {
            PersistentARTrieError::io_error(
                "create WAL",
                wal_path.to_string_lossy(),
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            )
        })?;

        let mut trie = Self {
            path,
            entry_count: AtomicUsize::new(0),
            start_index,
            next_index: AtomicU64::new(start_index),
            dirty: AtomicBool::new(false),
            wal_writer: Some(Arc::new(wal_writer)),
            wal_config,
            next_lsn: AtomicU64::new(1), // Start at 1, 0 reserved for "no LSN"
            synced_lsn: AtomicU64::new(0),
            durability_policy: DurabilityPolicy::default(),
            arena_manager: Some(arena_manager),
            buffer_manager: Some(buffer_manager),
            eviction_coordinator: None,
            lockfree_root: None,
            lockfree_cache: None,
            cas_retries: AtomicU64::new(0),
            commit_seq: AtomicU64::new(0),
            committed_watermark:
                crate::persistent_artrie_core::committed_watermark::CommittedWatermark::new(0),
            epoch_manager: Arc::new(
                crate::persistent_artrie_core::concurrency::EpochManager::new(),
            ),
            reverse_term_map: None,
        };
        // FLIP: the overlay is the LIVE representation from construction (single lock-free impl —
        // no enable_lockfree toggle). flip_to_overlay installs the overlay + stamps the Overlay
        // regime so route_overlay() -> true.
        if !trie.flip_to_overlay() {
            return Err(PersistentARTrieError::internal(
                "vocab create: flip_to_overlay did not engage on a fresh trie",
            ));
        }
        Ok(trie)
    }

    /// Open an existing vocabulary trie, replaying WAL records if needed.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(PersistentARTrieError::io_error(
                "open vocab trie",
                path.to_string_lossy(),
                std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"),
            ));
        }

        let (trie, _) = Self::open_with_recovery(path)?;
        Ok(trie)
    }

    /// Open the checkpoint snapshot (the v2 overlay image) without replaying WAL records.
    fn open_snapshot<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        if !path.exists() {
            return Err(PersistentARTrieError::io_error(
                "open vocab trie",
                path.to_string_lossy(),
                std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"),
            ));
        }

        // Open disk manager without validating the standard PART header (vocab uses VOCB).
        let disk_manager = DiskManager::open_without_validation(&path)?;
        let header = crate::persistent_vocab_artrie::header::read_vocab_header(&disk_manager)?;
        header.validate()?;

        // Legacy v1 (owned) files are no longer loadable — the owned loader is deleted (R4).
        if header.version != VOCAB_HEADER_VERSION_V2 {
            return Err(PersistentARTrieError::CorruptedFile {
                reason: format!(
                    "legacy v1 owned vocabulary format (version {}) is no longer supported; \
                     rebuild the vocabulary with the overlay format",
                    header.version
                ),
            });
        }

        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_VOCAB_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        // Load arenas from disk (blocks 1..block_count hold the overlay image's arena data).
        if header.block_count > 1 {
            let mut am = arena_manager.write();
            am.clear_for_loading();
            for block_id in 1..header.block_count {
                am.load_arena(block_id)?;
            }
            let arena_count = am.arena_count();
            if arena_count > 0 {
                am.set_active_arena(arena_count - 1);
            }
        }

        let wal_path = path.with_extension("vocab.wal");
        let wal_config = WalConfig::default();
        let (wal_writer, next_lsn) = {
            let wal = open_or_create_async_wal(&wal_path, &path).map_err(|e| {
                PersistentARTrieError::io_error(
                    "open WAL",
                    wal_path.to_string_lossy(),
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            // Ensure WAL's starting LSN is at least checkpoint_lsn + 1.
            let min_lsn = header.checkpoint_lsn + 1;
            wal.set_min_lsn(min_lsn);
            let lsn = wal.current_lsn();
            (Some(Arc::new(wal)), lsn)
        };

        let mut trie = Self {
            path,
            entry_count: AtomicUsize::new(header.entry_count as usize),
            start_index: header.start_index,
            next_index: AtomicU64::new(header.next_index),
            dirty: AtomicBool::new(false),
            wal_writer,
            wal_config,
            next_lsn: AtomicU64::new(next_lsn),
            synced_lsn: AtomicU64::new(header.checkpoint_lsn),
            durability_policy: DurabilityPolicy::default(),
            arena_manager: Some(arena_manager),
            buffer_manager: Some(buffer_manager),
            eviction_coordinator: None,
            lockfree_root: None,
            lockfree_cache: None,
            cas_retries: AtomicU64::new(0),
            commit_seq: AtomicU64::new(0),
            committed_watermark:
                crate::persistent_artrie_core::committed_watermark::CommittedWatermark::new(0),
            epoch_manager: Arc::new(
                crate::persistent_artrie_core::concurrency::EpochManager::new(),
            ),
            reverse_term_map: None,
        };

        // Build the in-memory overlay from the dense v2 image (+ reverse map + stamp the Overlay
        // regime so route_overlay() -> true). No owned reverse_index/bloom rebuild — the overlay's
        // reverse_term_map is authoritative + lock-free reads skip the bloom.
        trie.reestablish_overlay_from_image(header.root_ptr)?;

        Ok(trie)
    }

    /// Open with crash recovery — reestablishes the overlay image, then replays the WAL tail.
    pub fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        let path = path.as_ref().to_path_buf();

        if !path.exists() {
            let trie = Self::create(&path)?;
            let report = RecoveryReport::created_new();
            return Ok((trie, report));
        }

        // Open the v2 overlay snapshot, then replay WAL records newer than checkpoint_lsn.
        let mut trie = Self::open_snapshot(&path)?;

        let wal_path = path.with_extension("vocab.wal");
        let mut records_replayed = 0;
        let mut inserts_replayed = 0;
        let checkpoint_lsn = trie.synced_lsn.load(Ordering::Acquire);

        if wal_path.exists() {
            // V3: rank-aware overlay replay — apply only ranked Inserts into the overlay (a torn
            // Insert without CommitRank is uncommitted -> dropped); restore id/seq floors.
            let (seen, applied) =
                trie.replay_wal_into_overlay_rank_aware(&wal_path, checkpoint_lsn)?;
            records_replayed = seen;
            inserts_replayed = applied;
        }

        // If we replayed records, keep the WAL until a full checkpoint publishes the snapshot.
        if records_replayed > 0 {
            trie.dirty.store(true, Ordering::Release);
        }

        let report = if records_replayed > 0 {
            RecoveryReport::rebuild_from_wal(
                path.clone(),
                "WAL replay for vocabulary trie".to_string(),
                records_replayed as u64,
                inserts_replayed as u64,
                Vec::new(),
                0, // duration_ms not tracked here
            )
        } else {
            RecoveryReport::normal()
        };

        Ok((trie, report))
    }
}
