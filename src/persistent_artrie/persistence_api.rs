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

use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::Arc;

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
    pub fn mark_clean(&self) {
        self.dirty.store(false, AtomicOrdering::Release);
    }

    /// Flush all buffered data to disk for durability.
    ///
    /// This ensures all WAL records are synced to persistent storage.
    /// Call this after a batch of operations to ensure durability.
    /// Honors [`DurabilityPolicy`] for flush behavior.
    pub fn sync(&self) -> Result<()> {
        if let Some(ref wal_writer) = self.wal_writer {
            match self.durability_policy.load() {
                DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit => {
                    wal_writer.sync().map_err(|e| {
                        PersistentARTrieError::io_error(
                            "sync",
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
        self.durability_policy.load()
    }

    /// Set the durability policy for this trie.
    ///
    /// **F4:** `&self` — the field is an `AtomicEnumCell`. Configured pre-share in
    /// practice, but `&self` is harmless and keeps the collapse compiling.
    pub fn set_durability_policy(&self, policy: DurabilityPolicy) {
        self.durability_policy.store(policy);
    }

    /// Invalidate the published eviction registry on a durable in-place mutation
    /// (Phase 6 — the byte twin of char's `invalidate_eviction_registry`).
    ///
    /// A durable mutation diverges the in-memory trie from the last checkpoint's on-disk
    /// image, so any published eviction registry now references potentially-stale on-disk
    /// data. Marking the registry invalid makes the coordinator refuse to select any node
    /// for eviction (`force_eviction`/`select_for_eviction` gate on `is_valid()`) until
    /// the next checkpoint rebuilds and republishes a fresh, durable registry.
    ///
    /// This is the coarse early-out; the CORRECTNESS mechanism is the per-node M-2a
    /// `serial_disk_ptr` guard (invalidation alone can't catch a mid-eviction-list
    /// overwrite — the stamp guard does). No-op when eviction is disabled. Byte has no
    /// `structural_generation` (char-only — the owned `DictionaryNode` walk detector), so
    /// this only invalidates the coordinator's registry.
    pub(crate) fn invalidate_eviction_registry(&self) {
        if let Some(coordinator) = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
        {
            coordinator.invalidate_registry();
        }
    }

    pub(super) fn append_mutation_wal_record(
        &self,
        record: WalRecord,
        operation: &'static str,
    ) -> Result<Lsn> {
        // Phase 6 (byte invalidation, byte twin of char's `append_to_wal_inner` head):
        // a durable mutation is being logged → the in-memory trie diverges from the last
        // checkpoint's on-disk image, so invalidate any published eviction registry HERE
        // — the single chokepoint every byte overlay durable mutation funnels through —
        // BEFORE the WAL append/visibility, so eviction cannot unswizzle a freshly-
        // overwritten live node onto its STALE pre-write disk ptr. No-op when eviction is
        // disabled.
        self.invalidate_eviction_registry();

        let Some(ref wal_writer) = self.wal_writer else {
            return Ok(0);
        };

        let appended_lsn = wal_writer.append(record).map_err(|e| {
            PersistentARTrieError::io_error(
                operation,
                "WAL",
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            )
        })?;
        self.sync_wal_after_append(appended_lsn, operation)?;
        Ok(appended_lsn)
    }

    /// **M2b — Order-A step 1 (the LSN-returning durable append).** Append +
    /// sync `record` to the WAL **DURABLE** (per the durability policy) and return
    /// its assigned LSN. The byte twin of char's `append_to_wal_returning_lsn`:
    /// the returned LSN is durable-per-policy at return — BEFORE the caller
    /// performs the visibility-publishing root CAS (Order A) — because the shared
    /// [`Self::append_mutation_wal_record`] both appends AND `sync_wal_after_append`s
    /// (which fails loudly if `synced_lsn < appended_lsn` under a synchronous
    /// policy). Returns `0` when no WAL writer is installed (no durability is
    /// available — Order-A callers MUST treat a `0` return as "no WAL" and refuse to
    /// acknowledge durability). The op label `"order_a_durable"` is the durable
    /// overlay write path's chokepoint for error attribution.
    pub(super) fn append_to_wal_returning_lsn(&self, record: WalRecord) -> Result<Lsn> {
        self.append_mutation_wal_record(record, "order_a_durable")
    }

    /// **M2b — Order-A step 2.5 (the commit-rank bind).** Append + sync a
    /// [`WalRecord::CommitRank`] binding the durable data record at `data_lsn` to
    /// the commit `generation` its visibility CAS landed at, returning the rank
    /// record's own LSN. The byte twin of char's `append_commit_rank`. Called by
    /// the shared [`DurableOverlayWrite::commit_rank_and_mark`] AFTER the visibility
    /// CAS wins and BEFORE the op is acked, so it STRENGTHENS Order-A (an ack now
    /// also waits for the rank to be durable). Recovery's `reconcile_lww` consumes
    /// these to order same-term replay by commit generation rather than WAL
    /// physical/LSN order (the A.2 fix). `term` is the raw key bytes. Returns `0`
    /// when no WAL writer is installed (same convention as
    /// [`Self::append_to_wal_returning_lsn`]).
    pub(super) fn append_commit_rank(
        &self,
        data_lsn: Lsn,
        term: &[u8],
        generation: u64,
    ) -> Result<Lsn> {
        self.append_to_wal_returning_lsn(WalRecord::CommitRank {
            data_lsn,
            term: term.to_vec(),
            generation,
        })
    }

    pub(super) fn append_batch_mutation_wal_record(
        &self,
        entries: &[(Vec<u8>, Option<Vec<u8>>)],
        operation: &'static str,
    ) -> Result<Lsn> {
        let Some(ref wal_writer) = self.wal_writer else {
            return Ok(0);
        };

        let appended_lsn = wal_writer.append_batch(entries).map_err(|e| {
            PersistentARTrieError::io_error(
                operation,
                "WAL",
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            )
        })?;
        self.sync_wal_after_append(appended_lsn, operation)?;
        Ok(appended_lsn)
    }

    pub(super) fn sync_wal_after_append(
        &self,
        appended_lsn: Lsn,
        operation: &'static str,
    ) -> Result<()> {
        if appended_lsn == 0 {
            return Ok(());
        }

        match self.durability_policy.load() {
            DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit => {
                let Some(ref wal_writer) = self.wal_writer else {
                    return Ok(());
                };

                let synced_lsn = wal_writer.sync().map_err(|e| {
                    PersistentARTrieError::io_error(
                        operation,
                        "WAL",
                        std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                    )
                })?;
                if synced_lsn < appended_lsn {
                    return Err(PersistentARTrieError::Wal(format!(
                        "{operation} sync failed to cover appended LSN {appended_lsn}; synced {synced_lsn}"
                    )));
                }
            }
            DurabilityPolicy::Periodic | DurabilityPolicy::None => {}
        }

        Ok(())
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
    ///
    /// **M2b (overlay-durable-architecture.md, trait 3):** the RES-4 route-split
    /// DECISION (under the overlay write mode the OWNED tree is empty — the live data
    /// is in the immutable overlay; capturing the owned tree would checkpoint NOTHING
    /// and lose every term on reopen, so route to the overlay capture +
    /// watermark-bounded retaining publisher) + the total-loss-guard assert now live
    /// ONCE in the SHARED GENERIC
    /// [`OverlayCheckpoint::checkpoint_route_split`](crate::persistent_artrie_core::overlay::checkpoint::OverlayCheckpoint::checkpoint_route_split);
    /// this method is a thin wrapper calling it. The per-variant capture/publish seams
    /// (`overlay_checkpoint.rs`) delegate to byte's serialize path. INERT pre-flip:
    /// `route_overlay()` is false until M4 wires the production ctors, so the owned arm
    /// runs — byte-for-byte the prior `persist_to_disk` + WAL-truncate body (plus the
    /// `&mut`-only dirty-tracking clear below, which the prior `persist_to_disk`
    /// performed inline and which the `&self` owned-arm seam cannot).
    pub fn checkpoint(&self) -> Result<()> {
        // **F4:** `&self`. Concurrent checkpoints are serialized by the `Shared*`
        // trait `checkpoint()` (the CK `checkpoint_lock`); the owned arm takes OR-read
        // for capture inside `checkpoint_route_split` → `capture_owned_snapshot`.
        let routed_overlay = self.route_overlay();
        <Self as crate::persistent_artrie_core::overlay::checkpoint::OverlayCheckpoint<
            crate::persistent_artrie_core::key_encoding::ByteKey,
            V,
            S,
        >>::checkpoint_route_split(self)?;
        // The owned arm's `&self` publish seam cannot run the `&mut`-only
        // dirty-tracking clear that the prior `persist_to_disk` did inline; do it here
        // (owned arm only — the overlay arm has no owned dirty flags). Preserves the
        // exact prior `checkpoint()` post-state.
        if !routed_overlay {
            self.clear_dirty_tracking_state();
        }
        Ok(())
    }

    /// Get prefetch statistics for performance monitoring.
    pub fn prefetch_stats(&self) -> PrefetchStatsSnapshot {
        self.prefetcher.stats().snapshot()
    }
}
