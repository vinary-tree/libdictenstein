//! Persistence + durability + observability API for `PersistentVocabARTrie` — OVERLAY-ONLY (V6).
//!
//! - `is_dirty` — observability
//! - `checkpoint` — publish the lock-free overlay image (retaining the WAL)
//! - `sync` / `sync_to_disk` / `sync_to_disk_async` / `rotate_wal` — WAL durability
//! - `current_lsn` / `synced_lsn`
//! - `durability_policy` / `set_durability_policy`
//! - `enable_slot_tracking` / `flush_sequential`

#![allow(dead_code)]

use std::sync::atomic::Ordering;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::dict_impl::DurabilityPolicy;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::WalRecord;

use super::sync_handle::VocabSyncHandle;
use super::types::{VocabTrieFileHeader, VOCAB_HEADER_VERSION_V2, VOCAB_TRIE_MAGIC};

impl<S: BlockStorage> super::dict_impl::PersistentVocabARTrie<S> {
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }

    /// Checkpoint current state to disk — publishes the lock-free overlay image (retaining the
    /// WAL). The owned tree is deleted; this is a thin wrapper over `checkpoint_overlay`.
    pub fn checkpoint(&self) -> Result<()> {
        self.checkpoint_overlay()
    }

    /// Lock-free OVERLAY checkpoint — the durable-snapshot path (mirrors char's
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
    fn checkpoint_overlay(&self) -> Result<()> {
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

        // (1) Serialize the immutable overlay root (empty -> root_ptr 0). CX-universal: the
        // PATH-COMPRESSED serializer (proven no-truncation; the loader expands prefixes on reopen,
        // which vocab inherits from the char loader). Uncompressed prefix_len=0 images still load.
        let root_ptr_raw: u64 = match self.lockfree_root.as_ref().and_then(|r| r.load()) {
            Some(root) if entry_count > 0 => {
                self.serialize_overlay_snapshot_compressed(&root)?.to_raw()
            }
            _ => 0,
        };
        // Flush the arenas to disk so block_count reflects the serialized overlay nodes (mirrors
        // the owned persist_to_disk's arena flush; without it block_count stays at the create-time
        // value and reopen skips arena loading -> "arena has 0 nodes").
        if let Some(ref arena_manager) = self.arena_manager {
            arena_manager.write().flush()?;
        }

        // (2) Write the VOCB header (version 2 = overlay image; no owned reverse-index sidecar).
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
    pub fn sync(&self) -> Result<()> {
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

    /// Rotate WAL without full checkpoint serialization — syncs the WAL while retaining it for
    /// replay (no overlay image written, no truncate). Crash recovery replays the WAL tail.
    pub fn rotate_wal(&self) -> Result<()> {
        if !self.dirty.load(Ordering::Acquire) {
            return Ok(());
        }

        // Sync the WAL while RETAINING it (the overlay image is only published by checkpoint();
        // a rotate publishes no header, so it must not truncate). WAL replay recovers on restart.
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

        // Do NOT clear the dirty flag: the overlay image hasn't been published. checkpoint()
        // clears it after publishing the image.
        Ok(())
    }

    /// Non-blocking sync to disk without checkpoint bookkeeping — syncs the WAL (the overlay
    /// image is only published by `checkpoint()`). Returns an already-completed handle.
    pub fn sync_to_disk_async(&self) -> Result<VocabSyncHandle> {
        if let Some(ref wal) = self.wal_writer {
            wal.sync().map_err(|e| {
                PersistentARTrieError::io_error(
                    "sync WAL",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
        }
        Ok(VocabSyncHandle::already_synced())
    }

    /// Blocking sync to disk without checkpoint bookkeeping — `sync_to_disk_async()?.wait()`.
    pub fn sync_to_disk(&self) -> Result<()> {
        let handle = self.sync_to_disk_async()?;
        handle
            .wait()
            .map_err(|e| PersistentARTrieError::internal(&e))?;
        Ok(())
    }

    /// Get the current (next) LSN.
    #[inline]
    pub fn current_lsn(&self) -> u64 {
        self.next_lsn.load(Ordering::Acquire)
    }

    /// Get the last synced LSN. `None` if nothing has been synced yet.
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

    /// Enable slot-level dirty tracking for reduced checkpoint I/O. Idempotent.
    pub fn enable_slot_tracking(&self) {
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
    pub fn flush_sequential(&self) -> Result<()> {
        if let Some(ref am) = self.arena_manager {
            am.write().flush_sequential()?;
        }
        Ok(())
    }
}
