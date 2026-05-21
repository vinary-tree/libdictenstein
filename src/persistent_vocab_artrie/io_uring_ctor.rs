//! `IoUringDiskManager`-specific constructors for `PersistentVocabARTrie`.
//!
//! Split out of vocab `dict_impl.rs` (lines ~598-838, ~241 LOC) as
//! a Phase-6 vocab sub-module, mirroring the byte
//! `super::io_uring_ctor` split. These constructors target the
//! `IoUringDiskManager` storage backend; the MmapDiskManager
//! (default) constructors stay in `dict_impl.rs` for now.

#![cfg(feature = "io-uring-backend")]

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use xxhash_rust::xxh3::Xxh3DefaultBuilder;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::buffer_manager::BufferManager;
use crate::persistent_artrie::dict_impl::DurabilityPolicy;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::WalConfig;
use crate::persistent_artrie::wal_managed::{create_async_wal, open_or_create_async_wal};
use crate::persistent_artrie::IoUringDiskManager;
use crate::persistent_artrie_char::arena_manager::{ArenaManager, ArenaSlot};

use super::dict_impl::PersistentVocabARTrie;
use super::reverse_cache::VocabReverseCache;
use super::reverse_index::VocabReverseIndex;
use super::types::{
    NodeRef, VocabTrieFileHeader, VocabTrieNode, VocabTrieRoot, DEFAULT_REVERSE_CACHE_SIZE,
    DEFAULT_VOCAB_BUFFER_POOL_SIZE,
};

// === io_uring convenience constructors (Linux-only, requires `io-uring-backend` feature) ===

impl PersistentVocabARTrie<crate::persistent_artrie::IoUringDiskManager> {
    /// Create a new vocabulary trie using io_uring + O_DIRECT.
    ///
    /// This uses `IoUringDiskManager` instead of `MmapDiskManager`, which:
    /// - Bypasses the kernel page cache (O_DIRECT) to eliminate double caching
    /// - Uses io_uring for async I/O with predictable latency
    /// - Supports batched block submissions for better throughput
    ///
    /// # Arguments
    /// * `path` - Path to the vocabulary file (must not exist)
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

        // Create io_uring disk manager (creates new file with O_DIRECT)
        let disk_manager = IoUringDiskManager::create(&path)?;

        // Create buffer manager (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_VOCAB_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create arena manager with buffer manager for disk-backed storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        // Write initial header
        {
            let bm = buffer_manager.write();
            let dm = bm.storage();
            let mut header = VocabTrieFileHeader::with_start_index(start_index);
            dm.write_header_bytes(&header.to_bytes_with_checksum())?;
            dm.sync()?;
        }

        // Create reverse index file
        let idx_path = path.with_extension("vocab.idx");
        let reverse_index = VocabReverseIndex::create(&idx_path, start_index, 1024)?;

        // Create WAL file using async writer
        let wal_path = path.with_extension("vocab.wal");
        let wal_config = WalConfig::default();
        let wal_writer = create_async_wal(&wal_path, &path).map_err(|e| {
            PersistentARTrieError::io_error(
                "create WAL",
                wal_path.to_string_lossy(),
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            )
        })?;

        // Create root node
        let root_node = VocabTrieNode::new();
        let root_ref = NodeRef::new(0, 0);

        let mut node_map = HashMap::with_hasher(Xxh3DefaultBuilder);
        let root_ptr = Box::into_raw(Box::new(root_node));
        node_map.insert(root_ref, root_ptr as *const VocabTrieNode);

        let root = VocabTrieRoot::Node(unsafe { Box::from_raw(root_ptr) });

        Ok(Self {
            path,
            root,
            entry_count: AtomicUsize::new(0),
            start_index,
            next_index: AtomicU64::new(start_index),
            dirty: AtomicBool::new(false),
            reverse_index: Some(reverse_index),
            reverse_cache: VocabReverseCache::new(DEFAULT_REVERSE_CACHE_SIZE),
            node_map,
            next_slot: 1,
            wal_writer: Some(Arc::new(wal_writer)),
            wal_config,
            next_lsn: AtomicU64::new(1),
            synced_lsn: AtomicU64::new(0),
            durability_policy: DurabilityPolicy::default(),
            arena_manager: Some(arena_manager),
            buffer_manager: Some(buffer_manager),
            eviction_coordinator: None,
            bloom_filter: None,
            lockfree_root: None,
            lockfree_cache: None,
            cas_retries: AtomicU64::new(0),
        })
    }

    /// Open an existing vocabulary trie using io_uring + O_DIRECT.
    ///
    /// # Arguments
    /// * `path` - Path to the vocabulary file (must exist)
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

        // Open io_uring disk manager without validating standard PART header
        // (VocabTrie uses a different header format: VOCB)
        let disk_manager = IoUringDiskManager::open_without_validation(&path)?;

        // Read and validate the vocab-specific header
        let header = crate::persistent_vocab_artrie::header::read_vocab_header(&disk_manager)?;
        header.validate()?;

        // Create buffer manager
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_VOCAB_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create arena manager with buffer manager
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        // Load arenas from disk if there are data blocks
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

        // Open reverse index
        let idx_path = path.with_extension("vocab.idx");
        let reverse_index = if idx_path.exists() {
            Some(VocabReverseIndex::open(&idx_path)?)
        } else {
            None
        };

        // Open WAL file using async writer
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

        // Load root from disk if present
        let (root, node_map, next_slot) = if header.root_ptr != 0 {
            let slot = ArenaSlot::from_u64(header.root_ptr);
            Self::load_trie_from_disk(&arena_manager, &buffer_manager, slot)?
        } else {
            let root_node = VocabTrieNode::new();
            let root_ref = NodeRef::new(0, 0);

            let mut map = HashMap::with_hasher(Xxh3DefaultBuilder);
            let root_ptr = Box::into_raw(Box::new(root_node));
            map.insert(root_ref, root_ptr as *const VocabTrieNode);

            (
                VocabTrieRoot::Node(unsafe { Box::from_raw(root_ptr) }),
                map,
                1,
            )
        };

        let mut trie = Self {
            path,
            root,
            entry_count: AtomicUsize::new(header.entry_count as usize),
            start_index: header.start_index,
            next_index: AtomicU64::new(header.next_index),
            dirty: AtomicBool::new(false),
            reverse_index,
            reverse_cache: VocabReverseCache::new(DEFAULT_REVERSE_CACHE_SIZE),
            node_map,
            next_slot,
            wal_writer,
            wal_config,
            next_lsn: AtomicU64::new(next_lsn),
            synced_lsn: AtomicU64::new(header.checkpoint_lsn),
            durability_policy: DurabilityPolicy::default(),
            arena_manager: Some(arena_manager),
            buffer_manager: Some(buffer_manager),
            eviction_coordinator: None,
            bloom_filter: None,
            lockfree_root: None,
            lockfree_cache: None,
            cas_retries: AtomicU64::new(0),
        };

        // Rebuild reverse_index with fresh NodeRefs after loading
        if header.root_ptr != 0 {
            trie.rebuild_reverse_index()?;
        }

        // Load bloom filter from disk, or rebuild if missing
        match Self::load_bloom_filter(&trie.path) {
            Ok(Some(bloom)) => {
                trie.bloom_filter = Some(bloom);
            }
            Ok(None) => {
                let count = trie.entry_count.load(Ordering::Acquire);
                if count > 0 {
                    trie.rebuild_bloom_filter(count);
                }
            }
            Err(_) => {
                let count = trie.entry_count.load(Ordering::Acquire);
                if count > 0 {
                    trie.rebuild_bloom_filter(count);
                }
            }
        }

        Ok(trie)
    }
}
