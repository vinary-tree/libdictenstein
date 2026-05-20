//! Persistence + durability + stats public API for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~2190-2470, ~280 LOC) as
//! the fifteenth Phase-5 byte sub-module. These public methods form
//! the durability/observability surface:
//!
//! - `is_dirty` / `mark_clean`
//! - `sync` / `flush_sequential` / `sync_async`
//! - `current_lsn` / `synced_lsn`
//! - `durability_policy` / `set_durability_policy`
//! - `stats` / `stats_tracker`
//! - `advance_epoch` / `current_epoch`
//! - `checkpoint`
//! - `prefetch_stats`
//!
//! The heavy lifting (`persist_to_disk`, the actual WAL flush, the
//! prefetcher itself) stays in `dict_impl.rs` / `super::wal` /
//! `super::prefetch`; this module just wraps those calls in the
//! `PersistentARTrie` API surface.

use std::sync::Arc;
use std::sync::atomic::Ordering as AtomicOrdering;

use crate::persistent_artrie_core::concurrency::{TrieStats, TrieStatsSnapshot};
use crate::persistent_artrie_core::durability::DurabilityPolicy;
use crate::persistent_artrie_core::prefetch::PrefetchStatsSnapshot;
use crate::value::DictionaryValue;

use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;
use super::error::{PersistentARTrieError, Result};
use super::wal::{Lsn, SyncHandle, WalRecord};

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Check if the dictionary is dirty (has uncommitted changes).
    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(AtomicOrdering::Acquire)
    }

    /// Mark the dictionary as clean (after flushing to disk).
    #[inline]
    pub fn mark_clean(&mut self) {
        self.dirty.store(false, AtomicOrdering::Release);
    }

    /// Flush all buffered data to disk for durability.
    ///
    /// This ensures all WAL records are synced to persistent storage.
    /// Call this after a batch of operations to ensure durability.
    /// Honors [`DurabilityPolicy`] for flush behavior.
    pub fn sync(&self) -> Result<()> {
        if let Some(ref wal_writer) = self.wal_writer {
            match self.durability_policy {
                DurabilityPolicy::Immediate => {
                    wal_writer.sync().map_err(|e| {
                        PersistentARTrieError::io_error(
                            "sync",
                            "WAL",
                            std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                        )
                    })?;
                }
                DurabilityPolicy::GroupCommit => {
                    let _handle = wal_writer.sync_async().map_err(|e| {
                        PersistentARTrieError::io_error(
                            "sync_async",
                            "WAL",
                            std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                        )
                    })?;
                }
                DurabilityPolicy::Periodic | DurabilityPolicy::None => {
                    // No immediate sync - background thread handles it
                }
            }
        }

        if let Some(ref buffer_manager) = self.buffer_manager {
            buffer_manager.read().flush_all()?;
        }

        Ok(())
    }

    /// Flush dirty arena slots using sequential I/O.
    ///
    /// This writes modified slots to disk without full re-serialization.
    /// Requires slot tracking to be enabled.
    pub fn flush_sequential(&self) -> Result<()> {
        if let Some(ref am) = self.arena_manager {
            am.write().flush_sequential()?;
        }
        Ok(())
    }

    /// Async sync — returns a handle to track durability.
    ///
    /// The returned [`SyncHandle`] can be used to wait for durability or
    /// check status without blocking.
    pub fn sync_async(&self) -> Result<Option<SyncHandle>> {
        if let Some(ref wal_writer) = self.wal_writer {
            let handle = wal_writer.sync_async().map_err(|e| {
                PersistentARTrieError::io_error(
                    "sync_async",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            Ok(Some(handle))
        } else {
            Ok(None)
        }
    }

    /// Returns the next LSN that will be assigned to a write operation.
    ///
    /// This value increases monotonically with each write (insert, remove, update).
    #[inline]
    pub fn current_lsn(&self) -> Lsn {
        self.wal_writer
            .as_ref()
            .map(|wal| wal.current_lsn())
            .unwrap_or_else(|| self.next_lsn.load(AtomicOrdering::Acquire))
    }

    /// Returns the highest LSN that has been durably synced to storage.
    ///
    /// Operations with LSN <= synced_lsn are guaranteed to survive crashes.
    pub fn synced_lsn(&self) -> Option<Lsn> {
        self.wal_writer.as_ref().map(|wal| wal.synced_lsn())
    }

    /// Get the current durability policy.
    pub fn durability_policy(&self) -> DurabilityPolicy {
        self.durability_policy
    }

    /// Set the durability policy for this trie.
    pub fn set_durability_policy(&mut self, policy: DurabilityPolicy) {
        self.durability_policy = policy;
    }

    /// Get a snapshot of the trie statistics.
    pub fn stats(&self) -> TrieStatsSnapshot {
        self.stats.snapshot()
    }

    /// Get a reference to the stats tracker for direct recording.
    pub fn stats_tracker(&self) -> Arc<TrieStats> {
        Arc::clone(&self.stats)
    }

    /// Advance the MVCC epoch.
    ///
    /// This should be called periodically by a background thread to
    /// enable garbage collection of old versions.
    pub fn advance_epoch(&self) -> u64 {
        self.epoch_manager.advance()
    }

    /// Get the current MVCC epoch.
    pub fn current_epoch(&self) -> u64 {
        self.epoch_manager.current_epoch()
    }

    /// Create a checkpoint to allow WAL truncation.
    ///
    /// A checkpoint persists all in-memory trie data to disk, then records
    /// the current LSN in the WAL. This allows older WAL records to be safely
    /// removed after recovery.
    pub fn checkpoint(&mut self) -> Result<()> {
        self.persist_to_disk()?;

        if let Some(ref wal_writer) = self.wal_writer {
            let checkpoint_lsn = self.next_lsn.load(AtomicOrdering::Acquire).saturating_sub(1);
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let record = WalRecord::Checkpoint {
                checkpoint_lsn,
                timestamp,
            };

            wal_writer.append(record).map_err(|e| {
                PersistentARTrieError::io_error(
                    "checkpoint_append",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            wal_writer.sync().map_err(|e| {
                PersistentARTrieError::io_error(
                    "checkpoint_sync",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;

            wal_writer.truncate().map_err(|e| {
                PersistentARTrieError::io_error(
                    "checkpoint_truncate",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
        }

        Ok(())
    }

    /// Get prefetch statistics for performance monitoring.
    pub fn prefetch_stats(&self) -> PrefetchStatsSnapshot {
        self.prefetcher.stats().snapshot()
    }
}
