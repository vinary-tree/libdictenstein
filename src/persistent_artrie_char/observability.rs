//! Observability + durability + group-commit + memory-pressure +
//! cache-stats API for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~344-594, ~251 LOC)
//! as a Phase-6 char sub-module. Methods covered:
//!
//! - `sync` — flush WAL to disk
//! - `current_lsn` / `synced_lsn` — LSN observability
//! - `enable_group_commit` / `disable_group_commit` /
//!   `is_group_commit_enabled` / `group_commit_stats`
//! - `enable_memory_monitor` (+ `_default`) / `disable_memory_monitor` /
//!   `has_memory_monitor` / `memory_stats` / `memory_pressure_level`
//! - `record_cache_hit` / `record_cache_miss` / `cache_hit_rate` /
//!   `cache_counts` / `cache_total_accesses` / `cache_stats_and_reset` /
//!   `get_cache_stats`

use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::Arc;

use crate::persistent_artrie::adaptive_pool::CacheStats;
use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
#[cfg(feature = "group-commit")]
use crate::persistent_artrie::group_commit::{GroupCommitConfig, GroupCommitCoordinator};
use crate::persistent_artrie::memory_monitor::{
    MemoryPressureConfig, MemoryPressureLevel, MemoryPressureMonitor, MemoryStats,
};
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// Sync changes to disk
    pub fn sync(&self) -> Result<()> {
        if let Some(ref wal_writer) = self.wal_writer {
            wal_writer
                .sync()
                .map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;
        }
        Ok(())
    }

    /// Returns the next LSN that will be assigned to a write operation.
    ///
    /// This value increases monotonically with each write (insert, remove, update).
    /// It can be used as a "version" or "sequence number" for the trie state.
    ///
    /// # Returns
    /// - The next LSN to be assigned (starts at 1 for persistent tries)
    ///
    /// # Example
    /// ```text
    /// let mut trie = PersistentARTrieChar::<i32>::create("test.part")?;
    /// let before = trie.current_lsn();
    /// trie.upsert("key", 42)?;
    /// let after = trie.current_lsn();
    /// assert!(after > before);
    /// ```
    #[inline]
    pub fn current_lsn(&self) -> u64 {
        // Use WAL's authoritative LSN if available, otherwise fall back to cached value
        self.wal_writer
            .as_ref()
            .map(|wal| wal.current_lsn())
            .unwrap_or_else(|| self.next_lsn.load(AtomicOrdering::Acquire))
    }

    /// Returns the highest LSN that has been durably synced to storage.
    ///
    /// Operations with LSN <= synced_lsn are guaranteed to survive crashes.
    /// Operations with LSN > synced_lsn may be lost if a crash occurs before
    /// the next sync or checkpoint.
    ///
    /// # Returns
    /// - `Some(lsn)` if WAL is enabled and has synced data
    /// - `None` if WAL is disabled (in-memory trie) or no data has been synced yet
    ///
    /// # Example
    /// ```text
    /// let mut trie = PersistentARTrieChar::<i32>::create("test.part")?;
    /// trie.upsert("key", 42)?;
    /// trie.sync()?;  // Force durability
    /// let synced = trie.synced_lsn();
    /// assert!(synced.is_some());
    /// ```
    pub fn synced_lsn(&self) -> Option<u64> {
        self.wal_writer.as_ref().map(|wal| wal.synced_lsn())
    }

    // ========================================================================
    // Group Commit Support
    // ========================================================================

    /// Enable group commit for WAL write batching.
    ///
    /// Group commit batches multiple WAL writes into a single fsync() operation,
    /// significantly improving write throughput at the cost of slightly increased
    /// latency for individual operations.
    ///
    /// # Arguments
    ///
    /// * `config` - Group commit configuration (batch size, delay, etc.)
    ///
    /// # Returns
    ///
    /// Returns an error if:
    /// - The trie is in in-memory mode (no WAL)
    /// - Group commit is already enabled
    ///
    /// # Example
    ///
    /// ```text
    /// use libdictenstein::persistent_artrie::group_commit::GroupCommitConfig;
    ///
    /// let mut trie = PersistentARTrieChar::<u64>::create("data.trie")?;
    ///
    /// // Enable with default config (balanced latency/throughput)
    /// trie.enable_group_commit(GroupCommitConfig::default())?;
    ///
    /// // Or use a throughput-optimized config
    /// trie.enable_group_commit(GroupCommitConfig::high_throughput())?;
    /// ```
    ///
    /// **F4:** `&self` (subsystem family). The coordinator builds OUTSIDE the field
    /// lock; install under a brief lock. Already-enabled ⇒ error (no old to join).
    #[cfg(feature = "group-commit")]
    pub fn enable_group_commit(&self, config: GroupCommitConfig) -> Result<()> {
        if self
            .group_commit
            .lock()
            .expect("group_commit mutex poisoned")
            .is_some()
        {
            return Err(PersistentARTrieError::InvalidOperation(
                "Group commit is already enabled".to_string(),
            ));
        }

        let wal_writer = self.wal_writer.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Cannot enable group commit on in-memory trie".to_string(),
            )
        })?;

        let coordinator = Arc::new(GroupCommitCoordinator::new(Arc::clone(wal_writer), config)?);
        let mut slot = self
            .group_commit
            .lock()
            .expect("group_commit mutex poisoned");
        if slot.is_some() {
            return Err(PersistentARTrieError::InvalidOperation(
                "Group commit is already enabled".to_string(),
            ));
        }
        *slot = Some(coordinator);
        Ok(())
    }

    /// Disable group commit, returning to direct WAL writes.
    ///
    /// This flushes any pending writes and shuts down the group commit coordinator.
    /// After this call, all WAL writes will be performed directly.
    ///
    /// **F4 drop-before-join (V11.3 site 5):** take the coordinator into a
    /// statement-temporary so the field-mutex guard DROPS before the old `Arc` is
    /// dropped (its `Drop` flushes + joins the coordinator thread — joining under
    /// the held field mutex could deadlock if the coordinator re-enters).
    #[cfg(feature = "group-commit")]
    pub fn disable_group_commit(&self) -> Result<()> {
        let old = self
            .group_commit
            .lock()
            .expect("group_commit mutex poisoned")
            .take();
        // Field-mutex guard dropped here; THEN drop the old Arc (flush + join).
        drop(old);
        Ok(())
    }

    /// Check if group commit is enabled.
    #[cfg(feature = "group-commit")]
    pub fn is_group_commit_enabled(&self) -> bool {
        self.group_commit
            .lock()
            .expect("group_commit mutex poisoned")
            .is_some()
    }

    /// Get group commit statistics.
    ///
    /// Returns None if group commit is not enabled.
    #[cfg(feature = "group-commit")]
    pub fn group_commit_stats(
        &self,
    ) -> Option<crate::persistent_artrie::group_commit::GroupCommitStats> {
        self.group_commit
            .lock()
            .expect("group_commit mutex poisoned")
            .as_ref()
            .map(|gc| gc.stats())
    }

    // ==================== Performance Infrastructure Methods ====================

    /// Enables memory pressure monitoring with the given configuration and callback.
    ///
    /// Memory monitoring tracks system memory usage and invokes the callback when
    /// pressure thresholds change, allowing the trie to adapt its memory usage
    /// (e.g., by evicting cached nodes or reducing buffer sizes).
    ///
    /// # Arguments
    /// * `config` - Configuration for memory pressure thresholds and polling interval
    /// * `callback` - Function to call when memory pressure level changes
    ///
    /// # Returns
    /// * `Ok(())` - Monitor enabled successfully
    /// * `Err(_)` - Failed to start monitor thread
    ///
    /// # Example
    /// ```text
    /// trie.enable_memory_monitor(
    ///     MemoryPressureConfig::default(),
    ///     |level, stats| {
    ///         log::info!("Memory pressure: {:?}, used: {} MB", level, stats.used_mb());
    ///     }
    /// )?;
    /// ```
    ///
    /// **F4:** `&self` (subsystem family). The monitor STARTS (spawns its thread)
    /// outside the field lock; the take-old-then-drop-guard-then-drop-old re-arm
    /// (V11.3 site 9) ensures a re-enable joins the OLD monitor thread WITHOUT
    /// holding the field mutex (its callback can re-enter the trie → force_eviction
    /// → OR/EC, so joining under the field mutex would deadlock).
    pub fn enable_memory_monitor<F>(
        &self,
        config: MemoryPressureConfig,
        callback: F,
    ) -> Result<()>
    where
        F: Fn(MemoryPressureLevel, &MemoryStats) + Send + Sync + 'static,
    {
        let monitor = Arc::new(MemoryPressureMonitor::start(config, callback)?);
        let old = {
            let mut slot = self
                .memory_monitor
                .lock()
                .expect("memory_monitor mutex poisoned");
            slot.replace(monitor)
        };
        // Field-mutex guard dropped; THEN drop the old monitor (joins its thread —
        // safe: not under the field mutex, so a re-entrant callback can't deadlock).
        drop(old);
        Ok(())
    }

    /// Enables memory pressure monitoring with default configuration and a no-op callback.
    ///
    /// Use this when you only want to query memory stats periodically
    /// without receiving pressure change notifications.
    pub fn enable_memory_monitor_default(&self) -> Result<()> {
        self.enable_memory_monitor(MemoryPressureConfig::default(), |_level, _stats| {})
    }

    /// Disables memory pressure monitoring.
    ///
    /// The monitor thread is stopped when the Arc is dropped.
    ///
    /// **F4 drop-before-join (V11.3 GAP 2):** take the monitor into a
    /// statement-temporary so the field-mutex guard DROPS before the old `Arc`'s
    /// `Drop` joins the monitor thread — its callback can re-enter the trie, so a
    /// join under the field mutex would deadlock.
    pub fn disable_memory_monitor(&self) {
        let old = self
            .memory_monitor
            .lock()
            .expect("memory_monitor mutex poisoned")
            .take();
        drop(old);
    }

    /// Returns whether memory monitoring is enabled.
    pub fn has_memory_monitor(&self) -> bool {
        self.memory_monitor
            .lock()
            .expect("memory_monitor mutex poisoned")
            .is_some()
    }

    /// Returns current memory statistics if monitoring is enabled.
    pub fn memory_stats(&self) -> Option<MemoryStats> {
        self.memory_monitor
            .lock()
            .expect("memory_monitor mutex poisoned")
            .as_ref()
            .map(|m| m.current_stats())
    }

    /// Returns current memory pressure level if monitoring is enabled.
    pub fn memory_pressure_level(&self) -> Option<MemoryPressureLevel> {
        self.memory_monitor
            .lock()
            .expect("memory_monitor mutex poisoned")
            .as_ref()
            .map(|m| m.current_level())
    }

    // -------------------- Cache Statistics --------------------

    /// Records a cache hit.
    ///
    /// Call this when a node lookup finds the node in cache.
    pub fn record_cache_hit(&self) {
        self.cache_stats.record_hit();
    }

    /// Records a cache miss.
    ///
    /// Call this when a node lookup requires loading from disk.
    pub fn record_cache_miss(&self) {
        self.cache_stats.record_miss();
    }

    /// Returns the current cache hit rate (0.0 to 1.0).
    ///
    /// Returns 1.0 if no cache accesses have been recorded.
    pub fn cache_hit_rate(&self) -> f64 {
        self.cache_stats.hit_rate()
    }

    /// Returns cache hit/miss counts.
    ///
    /// Returns `(hits, misses)`.
    pub fn cache_counts(&self) -> (u64, u64) {
        self.cache_stats.counts()
    }

    /// Returns the total number of cache accesses (hits + misses).
    pub fn cache_total_accesses(&self) -> u64 {
        self.cache_stats.total_accesses()
    }

    /// Gets cache statistics and resets the counters atomically.
    ///
    /// Returns `(hit_rate, hits, misses)`.
    ///
    /// Use this for periodic reporting where you want to measure
    /// hit rates over fixed time intervals.
    pub fn cache_stats_and_reset(&self) -> (f64, u64, u64) {
        self.cache_stats.get_and_reset()
    }

    /// Returns a reference to the underlying cache statistics.
    pub fn get_cache_stats(&self) -> &CacheStats {
        &self.cache_stats
    }
}
