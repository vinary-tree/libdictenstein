//! `IoUringDiskManager`-specific constructors for `PersistentVocabARTrie` — OVERLAY-ONLY (V6).
//!
//! Mirror the mmap ctors for the io_uring backend: every ctor flips to the lock-free overlay at
//! construction; the owned tree, the reverse-index sidecar, and the bloom filter are deleted.

#![cfg(feature = "io-uring-backend")]

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::buffer_manager::BufferManager;
use crate::persistent_artrie::dict_impl::DurabilityPolicy;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::WalConfig;
use crate::persistent_artrie::wal_managed::{create_async_wal, open_or_create_async_wal};
use crate::persistent_artrie_char::arena_manager::ArenaManager;

use super::dict_impl::PersistentVocabARTrie;
use super::types::{VocabTrieFileHeader, DEFAULT_VOCAB_BUFFER_POOL_SIZE, VOCAB_HEADER_VERSION_V2};

// === io_uring convenience constructors (Linux-only, requires `io-uring-backend` feature) ===

impl PersistentVocabARTrie<crate::persistent_artrie::IoUringDiskManager> {
    /// Create a new vocabulary trie using io_uring + O_DIRECT.
    pub fn create_with_io_uring<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::create_with_io_uring_and_start_index(path, 0)
    }

    /// Create a new vocabulary trie with io_uring and a custom starting index.
    pub fn create_with_io_uring_and_start_index<P: AsRef<Path>>(
        path: P,
        start_index: u64,
    ) -> Result<Self> {
        use crate::persistent_artrie::IoUringDiskManager;

        let path = path.as_ref().to_path_buf();

        if path.exists() {
            return Err(PersistentARTrieError::CorruptedFile {
                reason: format!("File already exists: {}", path.display()),
            });
        }

        let disk_manager = IoUringDiskManager::create(&path)?;
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
            next_lsn: AtomicU64::new(1),
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
        // FLIP (io_uring): overlay is the LIVE rep from construction (mirror mmap).
        <Self as crate::persistent_artrie_core::overlay::flip::LockFreeOverlay<
            crate::persistent_artrie_core::key_encoding::CharKey,
            _,
            _,
        >>::install_overlay_on_create(trie)
    }

    /// Open an existing vocabulary trie using io_uring + O_DIRECT (the v2 overlay image).
    pub fn open_with_io_uring<P: AsRef<Path>>(path: P) -> Result<Self> {
        use crate::persistent_artrie::IoUringDiskManager;

        let path = path.as_ref().to_path_buf();

        if !path.exists() {
            return Err(PersistentARTrieError::io_error(
                "open vocab trie",
                path.to_string_lossy(),
                std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"),
            ));
        }

        let disk_manager = IoUringDiskManager::open_without_validation(&path)?;
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

        // Reestablish the overlay from the v2 image + drain the WAL tail rank-aware (crash-safe).
        trie.reestablish_overlay_from_image(header.root_ptr)?;
        let wal_path = trie.path.with_extension("vocab.wal");
        if wal_path.exists() {
            let checkpoint_lsn = trie.synced_lsn.load(Ordering::Acquire);
            trie.replay_wal_into_overlay_rank_aware(&wal_path, checkpoint_lsn)?;
        }

        Ok(trie)
    }
}
