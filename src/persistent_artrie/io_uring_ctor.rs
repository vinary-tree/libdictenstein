//! `IoUringDiskManager`-specific constructors for `PersistentARTrie<V>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~1113-1445, ~333 LOC) as
//! the twelfth Phase-5 byte sub-module. These constructors
//! (`create_with_io_uring`, `open_with_io_uring`) are feature-gated
//! on `io-uring-backend` and target the `IoUringDiskManager` storage
//! backend. The MmapDiskManager (default) constructors live in
//! `dict_impl.rs`.

#![cfg(feature = "io-uring-backend")]

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

use log::warn;

use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;

use super::arena_manager::ArenaManager;
use super::block_storage::BlockStorage;
use super::bucket::StringBucket;
use super::buffer_manager::BufferManager;
use super::dict_impl::{DurabilityPolicy, PersistentARTrie, TrieRoot};
use super::disk_load::read_root_descriptor_arena_count;
use super::error::{PersistentARTrieError, Result};
use super::recovery::RecoveryManager;
use super::wal::{AsyncWalConfig, AsyncWalWriter, WalConfig};
use super::{IoUringDiskManager, DEFAULT_BUFFER_POOL_SIZE};

impl<V: DictionaryValue> PersistentARTrie<V, IoUringDiskManager> {
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

        Ok(Self {
            root: TrieRoot::Bucket(StringBucket::with_values()),
            term_count: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            next_lsn: std::sync::atomic::AtomicU64::new(1),
            prefetcher: super::prefetch::Prefetcher::new(),
            arena_manager: Some(arena_manager),
            durability_policy: DurabilityPolicy::default(),
            epoch_manager: super::concurrency::EpochManager::new(),
            stats: Arc::new(super::concurrency::TrieStats::new()),
            eviction_coordinator: None,
            dirty_prefixes: HashSet::new(),
            persisted_disk_locations: RwLock::new(HashMap::new()),
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
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

        let was_loaded_from_disk = loaded_root.is_some();
        let (initial_root, initial_term_count) = match loaded_root {
            Some(root) => (root, loaded_term_count as usize),
            None => (TrieRoot::Bucket(StringBucket::with_values()), 0),
        };

        let mut dict = Self {
            root: initial_root,
            term_count: AtomicUsize::new(initial_term_count),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(Arc::clone(&wal_writer)),
            next_lsn: std::sync::atomic::AtomicU64::new(next_lsn),
            prefetcher: super::prefetch::Prefetcher::new(),
            arena_manager: Some(arena_manager),
            durability_policy: DurabilityPolicy::default(),
            epoch_manager: super::concurrency::EpochManager::new(),
            stats: Arc::new(super::concurrency::TrieStats::new()),
            eviction_coordinator: None,
            dirty_prefixes: HashSet::new(),
            persisted_disk_locations: RwLock::new(HashMap::new()),
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        };

        let skip_threshold = if was_loaded_from_disk {
            checkpoint_lsn
        } else {
            None
        };

        let mut replayed_count = 0;
        for op in recovered_ops.into_iter() {
            if let Some(threshold) = skip_threshold {
                if op.lsn() <= threshold {
                    continue;
                }
            }
            if dict.apply_recovered_operation_no_wal(op) {
                replayed_count += 1;
            }
        }

        dict.dirty.store(false, AtomicOrdering::Release);

        if was_loaded_from_disk && replayed_count == 0 {
            if let Err(e) = wal_writer.truncate() {
                warn!("Failed to truncate WAL after recovery: {:?}", e);
            }
        }

        Ok(dict)
    }
}
