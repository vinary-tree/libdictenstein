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

        Ok(Self {
            root: CharTrieRoot::Empty,
            len: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            wal_config: WalConfig::default(),
            next_lsn: std::sync::atomic::AtomicU64::new(1),
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: EpochManager::new(),
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
        let (recovered_ops, next_lsn, checkpoint_lsn) = if wal_path.exists() {
            let mut reader =
                WalReader::new(&wal_path).map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;

            let mut records = Vec::new();
            let mut max_lsn = 0u64;
            let mut checkpoint_lsn = 0u64;
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
                        records.push((lsn, record));
                    }
                    Err(_) => break,
                }
            }

            let next_lsn = max_lsn + 1;
            (records, next_lsn, checkpoint_lsn)
        } else {
            (Vec::new(), 1, 0)
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
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: EpochManager::new(),
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
            lockfree_cache: None,
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        };

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
                    #[cfg(test)]
                    panic!("load_root_from_disk failed: {:?}", e);
                }
            }
        }

        // Ensure arena manager validity after loading
        if let Some(ref arena_manager) = inner.arena_manager {
            arena_manager.write().ensure_valid();
        }

        // Replay WAL records that came after the checkpoint
        let mut skipped_all = true;
        for (lsn, record) in recovered_ops {
            if loaded_from_disk && checkpoint_lsn > 0 && lsn <= checkpoint_lsn {
                continue;
            }
            skipped_all = false;

            match record {
                WalRecord::Insert { term, value } => {
                    let term_str = String::from_utf8_lossy(&term);
                    if let Some(value_bytes) = value {
                        if let Ok(v) =
                            crate::serialization::bincode_compat::deserialize::<V>(&value_bytes)
                        {
                            inner.insert_impl_no_wal_with_value(&term_str, v);
                        }
                    } else {
                        inner.insert_impl_no_wal(&term_str);
                    }
                }
                WalRecord::Remove { term } => {
                    let term_str = String::from_utf8_lossy(&term);
                    inner.remove_impl_no_wal(&term_str);
                }
                WalRecord::Checkpoint { .. } => {}
                WalRecord::BeginTx { .. }
                | WalRecord::CommitTx { .. }
                | WalRecord::AbortTx { .. } => {}
                WalRecord::Increment { term, result, .. } => {
                    let term_str = String::from_utf8_lossy(&term);
                    if let Ok(value_bytes) =
                        crate::serialization::bincode_compat::serialize(&result)
                    {
                        if let Ok(v) =
                            crate::serialization::bincode_compat::deserialize::<V>(&value_bytes)
                        {
                            inner.insert_impl_no_wal_with_value(&term_str, v);
                        }
                    }
                }
                WalRecord::Upsert { term, value } => {
                    let term_str = String::from_utf8_lossy(&term);
                    if let Ok(v) = crate::serialization::bincode_compat::deserialize::<V>(&value) {
                        inner.insert_impl_no_wal_with_value(&term_str, v);
                    }
                }
                WalRecord::CompareAndSwap {
                    term,
                    new_value,
                    success,
                    ..
                } => {
                    if success {
                        let term_str = String::from_utf8_lossy(&term);
                        if let Ok(v) =
                            crate::serialization::bincode_compat::deserialize::<V>(&new_value)
                        {
                            inner.insert_impl_no_wal_with_value(&term_str, v);
                        }
                    }
                }
                WalRecord::BatchInsert { entries } => {
                    for (term, value_opt) in entries {
                        let term_str = String::from_utf8_lossy(&term);
                        if let Some(value_bytes) = value_opt {
                            if let Ok(v) =
                                crate::serialization::bincode_compat::deserialize::<V>(&value_bytes)
                            {
                                inner.insert_impl_no_wal_with_value(&term_str, v);
                            }
                        } else {
                            inner.insert_impl_no_wal(&term_str);
                        }
                    }
                }
                WalRecord::BatchIncrement { entries } => {
                    for (term, delta) in entries {
                        let term_str = String::from_utf8_lossy(&term);
                        inner.increment_impl_no_wal(&term_str, delta);
                    }
                }
                WalRecord::VersionUpdate { .. }
                | WalRecord::VersionDurable { .. }
                | WalRecord::VersionGc { .. } => {}
            }
        }

        if loaded_from_disk && skipped_all {
            inner.dirty.store(false, AtomicOrdering::Release);
        }

        Ok(inner)
    }
}
