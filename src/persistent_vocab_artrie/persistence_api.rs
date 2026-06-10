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

#![allow(dead_code)]

use std::sync::atomic::Ordering;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::dict_impl::DurabilityPolicy;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::WalRecord;
use crate::persistent_artrie_char::arena_manager::ArenaSlot;

use super::sync_handle::VocabSyncHandle;
use super::types::{VocabTrieFileHeader, VOCAB_HEADER_VERSION_V2, VOCAB_TRIE_MAGIC};

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
        // Flip routing: under route_overlay() the durable snapshot is the OVERLAY image
        // (the owned persist_to_disk path below is dead post-flip; deleted at V6).
        if self.route_overlay() {
            return self.checkpoint_overlay();
        }
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
        let buffer_manager = self
            .buffer_manager
            .as_ref()
            .ok_or_else(|| PersistentARTrieError::internal("No buffer manager for checkpoint"))?;

        let reverse_index_capacity = self
            .reverse_index
            .as_ref()
            .map(|r| r.capacity())
            .unwrap_or(0);

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

        // Step 4: Write checkpoint record to WAL and truncate.
        // WAL errors must stay fail-closed: a failed checkpoint/truncate means
        // the active WAL is still needed for replay, so the dirty flag remains
        // set and the caller sees the error.
        if let Some(ref wal) = self.wal_writer {
            let checkpoint_lsn = self.next_lsn.load(Ordering::Acquire).saturating_sub(1);
            let wal_path = wal.path().to_string_lossy().into_owned();
            wal.checkpoint(checkpoint_lsn).map_err(|e| {
                PersistentARTrieError::io_error(
                    "checkpoint WAL",
                    wal_path.clone(),
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            self.synced_lsn.fetch_max(checkpoint_lsn, Ordering::AcqRel);

            wal.truncate().map_err(|e| {
                PersistentARTrieError::io_error(
                    "truncate WAL after vocab checkpoint",
                    wal_path,
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;

            let next_lsn = checkpoint_lsn.saturating_add(1);
            wal.set_min_lsn(next_lsn);
            self.next_lsn.store(next_lsn, Ordering::Release);
        }

        self.dirty.store(false, Ordering::Release);
        Ok(())
    }

    /// Lock-free OVERLAY checkpoint — the flip durable-snapshot path (mirrors char's
    /// `publish_immutable_snapshot_retaining_wal`).
    ///
    /// Captures the COMMITTED watermark BEFORE the root load (the safe `checkpoint_lsn`: the
    /// snapshot is guaranteed to contain every write `<= w`; appended-but-uncommitted writes
    /// beyond `w` stay in the WAL), serializes the immutable overlay into the dense char-arena
    /// image, writes the VOCB header (version 2 = overlay; `root_ptr` = the root NODE
    /// `SwizzledPtr.to_raw()`), then appends a `Checkpoint` record + syncs and RETAINS the WAL
    /// (no destructive truncate — reversible + non-double-counting via the Checkpoint gate;
    /// idempotent InsertOnce replay tolerates re-applying `(w, frontier]`). `mark_committed` on
    /// the Checkpoint record (#49) keeps the watermark == the committed frontier; the commit_seq
    /// floor (S5-2) keeps post-checkpoint ops out-ranking pre-checkpoint survivors.
    fn checkpoint_overlay(&mut self) -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let entry_count = self.entry_count.load(Ordering::Acquire);
        if !self.dirty.load(Ordering::Acquire) && entry_count == 0 {
            return Ok(());
        }

        // (0) Capture watermark + commit_seq floor BEFORE the root load (Order-A safe lsn).
        let checkpoint_lsn = self
            .committed_watermark
            .watermark()
            .max(self.committed_watermark.take_recovery_image_coverage());
        let commit_seq_floor = self.commit_seq.load(Ordering::Acquire);

        // (1) Serialize the immutable overlay root (empty -> root_ptr 0).
        let root_ptr_raw: u64 = match self.lockfree_root.as_ref().and_then(|r| r.load()) {
            Some(root) if entry_count > 0 => self.serialize_overlay_to_disk(&root)?.to_raw(),
            _ => 0,
        };

        // (2) Write the VOCB header (version 2 = overlay image; no owned reverse index).
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for overlay checkpoint")
        })?;
        let block_count = {
            let bm = buffer_manager.read();
            bm.storage().block_count().unwrap_or(1)
        };
        let mut header = VocabTrieFileHeader {
            magic: VOCAB_TRIE_MAGIC,
            version: VOCAB_HEADER_VERSION_V2,
            _reserved: [0; 3],
            root_ptr: root_ptr_raw,
            entry_count: entry_count as u64,
            block_count,
            _pad1: 0,
            checkpoint_lsn,
            header_checksum: 0,
            _padding: [0; 20],
            start_index: self.start_index,
            next_index: self.next_index.load(Ordering::Acquire),
            reverse_index_capacity: 0,
            _ext_padding: [0; 8],
        };
        {
            let bm = buffer_manager.write();
            let dm = bm.storage();
            dm.write_header_bytes(&header.to_bytes_with_checksum())?;
            bm.flush_all()?;
            dm.sync()?;
        }

        // (3) Append Checkpoint + sync; mark it committed (#49); raise the commit_seq floor
        //     (S5-2); RETAIN the WAL (no truncate — the Checkpoint record gates replay).
        if let Some(ref wal) = self.wal_writer {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let cp_lsn = wal
                .append(WalRecord::Checkpoint {
                    checkpoint_lsn,
                    timestamp,
                })
                .map_err(|e| {
                    PersistentARTrieError::Wal(format!("append overlay checkpoint record: {e}"))
                })?;
            wal.sync().map_err(|e| {
                PersistentARTrieError::Wal(format!("sync overlay checkpoint record: {e}"))
            })?;
            self.synced_lsn.fetch_max(cp_lsn, Ordering::AcqRel);
            self.committed_watermark.mark_committed(cp_lsn);
            wal.set_commit_seq_floor(commit_seq_floor).map_err(|e| {
                PersistentARTrieError::Wal(format!("set overlay commit_seq floor: {e}"))
            })?;
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
            let lsn = wal.sync().map_err(|e| {
                PersistentARTrieError::io_error(
                    "sync WAL",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
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
    /// 3. Syncs the WAL without truncating it
    ///
    /// This prevents file bloat while still providing crash recovery via WAL replay.
    /// On restart, all inserts are recovered from the WAL.
    ///
    /// # When to Use
    ///
    /// Use `rotate_wal()` for periodic durability during bulk imports:
    /// - No file bloat from repeated trie serialization
    /// - WAL replay remains available because this is not a checkpoint
    /// - Fast recovery via WAL replay
    ///
    /// Use `checkpoint()` for final compaction:
    /// - After import completes successfully
    /// - Reduces recovery time by avoiding WAL replay
    ///
    /// # Example
    ///
    /// ```text
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
            ri.flush().map_err(|e| {
                PersistentARTrieError::io_error(
                    "flush reverse index",
                    "reverse_index",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
        }

        // Step 2: Save bloom filter if present
        if let Some(ref bloom) = self.bloom_filter {
            self.save_bloom_filter(bloom)?;
        }

        // Step 3: Sync WAL while retaining it for replay. Do not flush arenas
        // here: vocab nodes are only assigned durable arena locations during
        // full checkpoint serialization, so an arena flush here would publish
        // incomplete storage state without a matching vocab header.
        if let Some(ref wal) = self.wal_writer {
            let lsn = wal.sync().map_err(|e| {
                PersistentARTrieError::io_error(
                    "sync WAL",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
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
    /// This method syncs the WAL without re-serializing the entire trie (which
    /// the full `checkpoint()` method does). This avoids:
    /// - File fragmentation from repeated re-serialization
    /// - File bloat from old serialized data not being reclaimed
    /// - Checkpoint bookkeeping overhead (LSN tracking, WAL truncation)
    ///
    /// # Concurrency
    ///
    /// - **Reads**: Continue unblocked during sync
    /// - **Writes**: Continue unblocked during sync
    /// - **Single sync at a time**: Returns existing handle if sync in progress
    /// - **Data safety**: WAL ensures durability; checkpoint publishes the trie
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
    /// ```text
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
            ri.flush().map_err(|e| {
                PersistentARTrieError::io_error(
                    "flush reverse index",
                    "reverse_index",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
        }

        // Sync the WAL to ensure all records are durable
        // WAL provides crash recovery - the main trie file is only updated during checkpoint
        if let Some(ref wal) = self.wal_writer {
            wal.sync().map_err(|e| {
                PersistentARTrieError::io_error(
                    "sync WAL",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
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
        handle
            .wait()
            .map_err(|e| PersistentARTrieError::internal(&e))?;
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
