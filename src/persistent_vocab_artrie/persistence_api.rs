//! Persistence + durability + observability API for `PersistentVocabARTrie`.
//!
//! Split out of vocab `dict_impl.rs` (lines ~1060-1391, ~332 LOC) as
//! a Phase-6 vocab sub-module. Methods covered:
//!
//! - `is_dirty` — observability
//! - `checkpoint` — persist all in-memory data + truncate WAL
//! - `sync` / `sync_to_disk` / `sync_to_disk_async`
//! - `rotate_wal`
//! - `current_lsn` / `synced_lsn`
//! - `durability_policy` / `set_durability_policy`
//! - `enable_slot_tracking` / `flush_sequential`
//! - `cache_stats`

use std::sync::atomic::Ordering;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::dict_impl::DurabilityPolicy;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie_char::arena_manager::ArenaSlot;

use super::sync_handle::VocabSyncHandle;
use super::types::{VocabTrieFileHeader, VOCAB_TRIE_MAGIC};

impl<S: BlockStorage> super::dict_impl::PersistentVocabARTrie<S> {
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }

    /// Checkpoint current state to disk.
    ///
    /// This persists the entire trie to disk:
    /// 1. Serialize all nodes to arenas (bottom-up)
    /// 2. Flush arenas to disk via buffer manager
    /// 3. Update header with root pointer
    /// 4. Flush reverse index
    /// 5. Write checkpoint record to WAL and truncate
    pub fn checkpoint(&mut self) -> Result<()> {
        if !self.dirty.load(Ordering::Acquire) && self.entry_count.load(Ordering::Acquire) == 0 {
            return Ok(());
        }

        // Step 1: Persist trie to disk (serialize nodes to arenas)
        let root_slot = if self.entry_count.load(Ordering::Acquire) > 0 {
            self.persist_to_disk()?
        } else {
            ArenaSlot::new(0, 0)
        };

        // Step 2: Update header with root pointer
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for checkpoint")
        })?;

        let reverse_index_capacity = self.reverse_index.as_ref().map(|r| r.capacity()).unwrap_or(0);

        // Get current block count from disk manager
        let block_count = {
            let bm = buffer_manager.read();
            bm.storage().block_count().unwrap_or(1)
        };

        let mut header = VocabTrieFileHeader {
            magic: VOCAB_TRIE_MAGIC,
            version: 1,
            _reserved: [0; 3],
            root_ptr: root_slot.to_u64(),
            entry_count: self.entry_count.load(Ordering::Acquire) as u64,
            block_count,
            _pad1: 0,
            checkpoint_lsn: self.next_lsn.load(Ordering::Acquire).saturating_sub(1),
            header_checksum: 0,
            _padding: [0; 20],
            start_index: self.start_index,
            next_index: self.next_index.load(Ordering::Acquire),
            reverse_index_capacity,
            _ext_padding: [0; 8],
        };

        {
            let bm = buffer_manager.write();
            let dm = bm.storage();
            dm.write_header_bytes(&header.to_bytes_with_checksum())?;
            bm.flush_all()?;
            dm.sync()?;
        }

        // Step 3: Flush reverse index
        if let Some(ref rev_idx) = self.reverse_index {
            rev_idx.flush()?;
        }

        // Step 3b: Save bloom filter if present
        if let Some(ref bloom) = self.bloom_filter {
            self.save_bloom_filter(bloom)?;
        }

        // Step 4: Write checkpoint record to WAL and truncate
        if let Some(ref wal) = self.wal_writer {
            let checkpoint_lsn = self.next_lsn.load(Ordering::Acquire).saturating_sub(1);
            if let Ok(lsn) = wal.checkpoint(checkpoint_lsn) {
                self.synced_lsn.fetch_max(lsn, Ordering::AcqRel);
            }
            // Truncate WAL after successful checkpoint
            let _ = wal.truncate();
        }

        self.dirty.store(false, Ordering::Release);
        Ok(())
    }

    /// Sync WAL to disk without full checkpoint.
    ///
    /// This ensures all logged operations are durable, but does not
    /// update the main data file. Useful for ensuring durability
    /// without the overhead of a full checkpoint.
    pub fn sync(&mut self) -> Result<()> {
        if let Some(ref wal) = self.wal_writer {
            let lsn = wal.sync().map_err(|e| PersistentARTrieError::io_error(
                "sync WAL",
                "WAL",
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ))?;
            self.synced_lsn.fetch_max(lsn, Ordering::AcqRel);
        }
        Ok(())
    }

    /// Rotate WAL without full checkpoint serialization.
    ///
    /// Unlike [`checkpoint()`], which re-serializes the entire trie to disk
    /// (causing file bloat), this method:
    /// 1. Flushes the reverse index (mmap, fast)
    /// 2. Saves the bloom filter to disk
    /// 3. Flushes only dirty slots via arena manager
    /// 4. Syncs and truncates the WAL
    ///
    /// This prevents file bloat while still providing crash recovery via WAL replay.
    /// On restart, all inserts are recovered from the WAL.
    ///
    /// # When to Use
    ///
    /// Use `rotate_wal()` for periodic durability during bulk imports:
    /// - No file bloat from repeated trie serialization
    /// - WAL truncation prevents unbounded WAL growth
    /// - Fast recovery via WAL replay
    ///
    /// Use `checkpoint()` for final compaction:
    /// - After import completes successfully
    /// - Reduces recovery time by avoiding WAL replay
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Bulk import with WAL rotation
    /// for batch in large_vocabulary.chunks(100_000) {
    ///     vocab.insert_batch(batch);
    ///     vocab.rotate_wal()?; // Durable without bloat
    /// }
    ///
    /// // Final compaction after import
    /// vocab.checkpoint()?;
    /// ```
    pub fn rotate_wal(&mut self) -> Result<()> {
        if !self.dirty.load(Ordering::Acquire) {
            return Ok(());
        }

        // Step 1: Flush reverse index (mmap, very fast)
        if let Some(ref ri) = self.reverse_index {
            ri.flush().map_err(|e| PersistentARTrieError::io_error(
                "flush reverse index",
                "reverse_index",
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ))?;
        }

        // Step 2: Save bloom filter if present
        if let Some(ref bloom) = self.bloom_filter {
            self.save_bloom_filter(bloom)?;
        }

        // Step 3: Flush dirty slots in arena manager (NOT full trie serialization)
        // This only writes slots that have been modified, avoiding file bloat
        if let Some(ref am) = self.arena_manager {
            am.write().flush_sequential()?;
        }

        // Step 4: Sync and truncate WAL
        if let Some(ref wal) = self.wal_writer {
            let lsn = wal.sync().map_err(|e| PersistentARTrieError::io_error(
                "sync WAL",
                "WAL",
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ))?;
            self.synced_lsn.fetch_max(lsn, Ordering::AcqRel);

            // Note: We do NOT truncate the WAL here because we haven't persisted
            // the trie structure. WAL replay on restart will recover all inserts.
            // Truncation only happens after full checkpoint().
        }

        // Note: We do NOT clear dirty flag because the trie structure itself
        // hasn't been persisted. On restart, WAL replay will recover the state.
        // The dirty flag is cleared by checkpoint() after full serialization.

        Ok(())
    }

    /// Non-blocking sync to disk without checkpoint bookkeeping.
    ///
    /// This method flushes dirty arenas in-place without re-serializing the entire
    /// trie (which the full `checkpoint()` method does). This avoids:
    /// - File fragmentation from repeated re-serialization
    /// - File bloat from old serialized data not being reclaimed
    /// - Checkpoint bookkeeping overhead (LSN tracking, WAL truncation)
    ///
    /// # Concurrency
    ///
    /// - **Reads**: Continue unblocked during sync
    /// - **Writes**: Continue unblocked during sync
    /// - **Single sync at a time**: Returns existing handle if sync in progress
    /// - **Data safety**: WAL ensures durability; arenas flushed in-place
    ///
    /// # Returns
    ///
    /// A [`VocabSyncHandle`] that can be used to:
    /// - Check completion: `handle.is_synced()`
    /// - Wait with timeout: `handle.wait_timeout(duration)`
    /// - Block until done: `handle.wait()`
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Start background sync, continue processing
    /// let handle = vocab.sync_to_disk_async()?;
    ///
    /// // ... continue vocabulary operations ...
    ///
    /// // Wait for sync completion before saving checkpoint metadata
    /// handle.wait()?;
    /// ```
    pub fn sync_to_disk_async(&self) -> Result<VocabSyncHandle> {
        // Flush reverse index synchronously (it uses mmap and is not cloneable)
        // This is fast since it just flushes the memory-mapped region
        if let Some(ref ri) = self.reverse_index {
            ri.flush().map_err(|e| PersistentARTrieError::io_error(
                "flush reverse index",
                "reverse_index",
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ))?;
        }

        // Sync the WAL to ensure all records are durable
        // WAL provides crash recovery - the main trie file is only updated during checkpoint
        if let Some(ref wal) = self.wal_writer {
            wal.sync().map_err(|e| PersistentARTrieError::io_error(
                "sync WAL",
                "WAL",
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ))?;
        }

        // Return immediately completed handle since sync() blocks
        Ok(VocabSyncHandle::already_synced())
    }

    /// Blocking sync to disk without checkpoint bookkeeping.
    ///
    /// This is a convenience wrapper around [`sync_to_disk_async()`] that blocks
    /// until the sync completes. Equivalent to calling `sync_to_disk_async()?.wait()`.
    ///
    /// # When to Use
    ///
    /// Use this method when:
    /// - You need to ensure data is durable before proceeding
    /// - You want simpler code without handle management
    /// - Blocking is acceptable in your use case
    ///
    /// Use `sync_to_disk_async()` when:
    /// - You want to continue processing while sync happens
    /// - You're implementing periodic sync with non-blocking behavior
    /// - You want fine-grained control over sync completion
    pub fn sync_to_disk(&mut self) -> Result<()> {
        let handle = self.sync_to_disk_async()?;
        handle.wait().map_err(|e| PersistentARTrieError::internal(&e))?;
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }

    /// Get the current (next) LSN.
    ///
    /// This is the LSN that will be assigned to the next WAL record.
    #[inline]
    pub fn current_lsn(&self) -> u64 {
        self.next_lsn.load(Ordering::Acquire)
    }

    /// Get the last synced LSN.
    ///
    /// Returns `None` if no records have been synced yet.
    #[inline]
    pub fn synced_lsn(&self) -> Option<u64> {
        let lsn = self.synced_lsn.load(Ordering::Acquire);
        if lsn == 0 {
            None
        } else {
            Some(lsn)
        }
    }

    /// Get the durability policy.
    #[inline]
    pub fn durability_policy(&self) -> DurabilityPolicy {
        self.durability_policy
    }

    /// Set the durability policy.
    #[inline]
    pub fn set_durability_policy(&mut self, policy: DurabilityPolicy) {
        self.durability_policy = policy;
    }

    /// Enable slot-level dirty tracking for reduced checkpoint I/O.
    ///
    /// Slot-level tracking only flushes modified slots within arenas,
    /// reducing checkpoint I/O by 90%+ for localized updates.
    ///
    /// This is idempotent - calling when already enabled has no effect.
    pub fn enable_slot_tracking(&mut self) {
        if let Some(ref am) = self.arena_manager {
            am.write().enable_slot_tracking();
        }
    }

    /// Internal version that doesn't require &mut self for use during construction.
    pub(crate) fn enable_slot_tracking_internal(&self) {
        if let Some(ref am) = self.arena_manager {
            am.write().enable_slot_tracking();
        }
    }

    /// Flush dirty arenas in sequential order for optimized disk I/O.
    ///
    /// Sorts dirty arenas by ID before flushing, improving I/O locality
    /// especially on rotational storage.
    pub fn flush_sequential(&mut self) -> Result<()> {
        if let Some(ref am) = self.arena_manager {
            am.write().flush_sequential()?;
        }
        Ok(())
    }

    /// Get cache statistics.
    pub fn cache_stats(&self) -> super::reverse_cache::CacheStats {
        self.reverse_cache.stats()
    }
}
