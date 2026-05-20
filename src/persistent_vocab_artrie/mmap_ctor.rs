//! `MmapDiskManager`-specific constructors for `PersistentVocabARTrie`.
//!
//! Split out of vocab `dict_impl.rs` (lines ~221-597, ~377 LOC) as
//! a Phase-6 vocab sub-module. These constructors target the default
//! `MmapDiskManager` storage backend:
//!
//! - `new` (in-memory ctor)
//! - `create` / `create_with_start_index` / `create_with_config`
//! - `open` / `open_with_recovery`
//!
//! The `IoUringDiskManager` variants live in `super::io_uring_ctor`;
//! generic methods (any `BlockStorage` backend) stay in
//! `dict_impl.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use parking_lot::RwLock;
use xxhash_rust::xxh3::Xxh3DefaultBuilder;

use crate::persistent_artrie::buffer_manager::BufferManager;
use crate::persistent_artrie::dict_impl::DurabilityPolicy;
use crate::persistent_artrie::disk_manager::{DiskManager, MmapDiskManager};
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::recovery::RecoveryReport;
use crate::persistent_artrie::wal::{WalConfig, WalReader, WalRecord};
use crate::persistent_artrie::wal_managed::{create_async_wal, open_or_create_async_wal};
use crate::persistent_artrie_char::arena_manager::{ArenaManager, ArenaSlot};
use crate::bloom_filter::BloomFilter;

use super::dict_impl::PersistentVocabARTrie;
use super::reverse_cache::VocabReverseCache;
use super::reverse_index::VocabReverseIndex;
use super::types::{
    NodeRef, VocabTrieFileHeader, VocabTrieNode, VocabTrieRoot, DEFAULT_REVERSE_CACHE_SIZE,
    DEFAULT_VOCAB_BUFFER_POOL_SIZE,
};

impl PersistentVocabARTrie {
    /// Create a new vocabulary trie at the given path.
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::create_with_start_index(path, 0)
    }

    /// Create a new vocabulary trie with BloomFilter enabled.
    ///
    /// The BloomFilter provides O(1) fast-path for detecting new terms during
    /// bulk insert operations, skipping expensive O(k) trie lookups.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the vocabulary file
    /// * `bloom_capacity` - Expected number of vocabulary entries (for optimal bloom sizing)
    pub fn create_with_bloom<P: AsRef<Path>>(path: P, bloom_capacity: usize) -> Result<Self> {
        Self::create_with_start_index_and_bloom(path, 0, bloom_capacity)
    }

    /// Create a new vocabulary trie with a custom starting index and BloomFilter.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the vocabulary file
    /// * `start_index` - Starting vocabulary index (default is 0)
    /// * `bloom_capacity` - Expected number of vocabulary entries (for optimal bloom sizing)
    pub fn create_with_start_index_and_bloom<P: AsRef<Path>>(
        path: P,
        start_index: u64,
        bloom_capacity: usize,
    ) -> Result<Self> {
        let mut trie = Self::create_with_start_index(path, start_index)?;
        trie.bloom_filter = Some(BloomFilter::new(bloom_capacity));
        Ok(trie)
    }

    /// Create a new vocabulary trie with a custom starting index.
    pub fn create_with_start_index<P: AsRef<Path>>(path: P, start_index: u64) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        if path.exists() {
            return Err(PersistentARTrieError::CorruptedFile {
                reason: format!("File already exists: {}", path.display()),
            });
        }

        // Create disk manager for the main file
        let disk_manager = DiskManager::create(&path)?;

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
        let wal_writer = create_async_wal(&wal_path, &path)
            .map_err(|e| PersistentARTrieError::io_error(
                "create WAL",
                wal_path.to_string_lossy(),
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ))?;

        // Create root node
        let root_node = VocabTrieNode::new();
        let root_ref = NodeRef::new(0, 0);

        let mut node_map = HashMap::with_hasher(Xxh3DefaultBuilder);
        let root_ptr = Box::into_raw(Box::new(root_node));
        node_map.insert(root_ref, root_ptr as *const VocabTrieNode);

        // Reconstruct root from pointer
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
            next_lsn: AtomicU64::new(1), // Start at 1, 0 reserved for "no LSN"
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

    /// Open an existing vocabulary trie.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        if !path.exists() {
            return Err(PersistentARTrieError::io_error(
                "open vocab trie",
                path.to_string_lossy(),
                std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"),
            ));
        }

        // Open disk manager without validating standard PART header
        // (VocabTrie uses a different header format: VOCB)
        let disk_manager = DiskManager::open_without_validation(&path)?;

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
        // Blocks 1 to block_count-1 contain arena data
        if header.block_count > 1 {
            let mut am = arena_manager.write();
            am.clear_for_loading();

            for block_id in 1..header.block_count {
                am.load_arena(block_id)?;
            }

            // Set active arena to the last one
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
            let wal = open_or_create_async_wal(&wal_path, &path)
                .map_err(|e| PersistentARTrieError::io_error(
                    "open WAL",
                    wal_path.to_string_lossy(),
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                ))?;

            // Ensure WAL's starting LSN is at least checkpoint_lsn + 1 to avoid
            // writing records with LSN <= checkpoint_lsn after a truncate
            let min_lsn = header.checkpoint_lsn + 1;
            wal.set_min_lsn(min_lsn);

            let lsn = wal.current_lsn();
            (Some(Arc::new(wal)), lsn)
        };

        // Load root from disk if present
        let (root, node_map, next_slot) = if header.root_ptr != 0 {
            // Load the entire trie from disk
            let slot = ArenaSlot::from_u64(header.root_ptr);
            Self::load_trie_from_disk(&arena_manager, &buffer_manager, slot)?
        } else {
            // Create new empty root
            let root_node = VocabTrieNode::new();
            let root_ref = NodeRef::new(0, 0);

            let mut map = HashMap::with_hasher(Xxh3DefaultBuilder);
            let root_ptr = Box::into_raw(Box::new(root_node));
            map.insert(root_ref, root_ptr as *const VocabTrieNode);

            (VocabTrieRoot::Node(unsafe { Box::from_raw(root_ptr) }), map, 1)
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
        // This is necessary because load_trie_from_disk assigns new NodeRefs
        // that don't match the old NodeRefs stored in the serialized reverse_index
        if header.root_ptr != 0 {
            trie.rebuild_reverse_index()?;
        }

        // Load bloom filter from disk, or rebuild if missing
        match Self::load_bloom_filter(&trie.path) {
            Ok(Some(bloom)) => {
                trie.bloom_filter = Some(bloom);
            }
            Ok(None) => {
                // Bloom filter file doesn't exist - rebuild from vocabulary
                let count = trie.entry_count.load(Ordering::Acquire);
                if count > 0 {
                    trie.rebuild_bloom_filter(count);
                }
            }
            Err(_) => {
                // Bloom filter file corrupted - rebuild from vocabulary
                let count = trie.entry_count.load(Ordering::Acquire);
                if count > 0 {
                    trie.rebuild_bloom_filter(count);
                }
            }
        }

        Ok(trie)
    }

    /// Open with crash recovery.
    ///
    /// Replays WAL records if present to restore state after a crash.
    /// This handles three cases:
    /// 1. Clean shutdown (checkpoint followed by close) - data loaded from disk
    /// 2. Crash after checkpoint - data loaded from disk
    /// 3. Crash before checkpoint - data loaded from disk + WAL replay
    pub fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        let path = path.as_ref().to_path_buf();

        if !path.exists() {
            // Create new
            let trie = Self::create(&path)?;
            let report = RecoveryReport::created_new();
            return Ok((trie, report));
        }

        // Open existing (loads trie from disk if checkpointed)
        let mut trie = Self::open(&path)?;

        // Check for WAL file and replay records AFTER checkpoint_lsn
        let wal_path = path.with_extension("vocab.wal");
        let mut records_replayed = 0;
        let mut inserts_replayed = 0;
        let checkpoint_lsn = trie.synced_lsn.load(Ordering::Acquire);

        if wal_path.exists() {
            let reader = WalReader::new(&wal_path)?;
            for record_result in reader.iter() {
                let (lsn, record) = record_result?;

                // Skip records that were already applied before the checkpoint
                if lsn <= checkpoint_lsn {
                    continue;
                }

                records_replayed += 1;

                match record {
                    WalRecord::Insert { term, value } => {
                        // Replay insert
                        let term_str = String::from_utf8(term)
                            .map_err(|e| PersistentARTrieError::CorruptedFile {
                                reason: format!("Invalid UTF-8 in WAL term: {}", e),
                            })?;

                        // Extract index from value bytes
                        if let Some(value_bytes) = value {
                            if value_bytes.len() >= 8 {
                                let index = u64::from_le_bytes(
                                    value_bytes[..8].try_into().expect("checked length")
                                );
                                trie.replay_insert(&term_str, index)?;
                                inserts_replayed += 1;
                            }
                        }
                    }
                    WalRecord::BatchInsert { entries } => {
                        // Replay batch insert
                        for (term, value) in entries {
                            let term_str = String::from_utf8(term)
                                .map_err(|e| PersistentARTrieError::CorruptedFile {
                                    reason: format!("Invalid UTF-8 in WAL batch term: {}", e),
                                })?;

                            if let Some(value_bytes) = value {
                                if value_bytes.len() >= 8 {
                                    let index = u64::from_le_bytes(
                                        value_bytes[..8].try_into().expect("checked length")
                                    );
                                    trie.replay_insert(&term_str, index)?;
                                    inserts_replayed += 1;
                                }
                            }
                        }
                    }
                    WalRecord::Checkpoint { checkpoint_lsn: new_lsn, .. } => {
                        // Update synced LSN
                        trie.synced_lsn.store(new_lsn, Ordering::Release);
                    }
                    _ => {
                        // Other record types not used by vocabulary trie
                    }
                }

                // Update next LSN (monotonic high-water mark)
                trie.next_lsn.fetch_max(lsn + 1, Ordering::AcqRel);
            }
        }

        // If we replayed records, mark dirty and truncate WAL
        if records_replayed > 0 {
            if let Some(ref wal) = trie.wal_writer {
                let _ = wal.truncate();
            }
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
