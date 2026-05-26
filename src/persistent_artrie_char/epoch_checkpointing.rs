//! Epoch-based checkpoint tracking for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~422-552, ~131 LOC)
//! as the eighteenth Phase-6 char sub-module. Methods covered:
//!
//! - `enable_epoch_checkpointing` (+ `_default`, `_high_throughput`,
//!   `_low_latency` convenience variants)
//! - `disable_epoch_checkpointing`
//! - `has_epoch_checkpointing`
//! - `record_epoch_operation`
//! - `current_epoch_id`
//! - `force_epoch_checkpoint`
//! - `last_durable_epoch`
//! - `epoch_stats` / `epoch_metadata` / `epoch_config`

use std::sync::Arc;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::epoch::{
    CheckpointManager, EpochConfig, EpochId, EpochMetadata, EpochStats,
};
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    // ==================== Epoch-Based Checkpointing Methods ====================

    /// Enables epoch-based checkpoint tracking.
    ///
    /// The checkpoint manager tracks successful WAL appends and advances epoch
    /// metadata based on configurable thresholds:
    /// - Operation count per epoch
    /// - WAL size limit
    /// - Time-based epoch duration
    ///
    /// Public mutation APIs record epoch operations automatically after their
    /// WAL append succeeds. Use [`Self::force_epoch_checkpoint`] to publish the
    /// current trie checkpoint and then mark the previous epoch durable.
    ///
    /// **Important:** The checkpoint manager creates its own WAL in a subdirectory.
    /// [`Self::record_epoch_operation`] remains available for external/manual WAL
    /// records, but ordinary trie mutations should not call it directly.
    ///
    /// # Arguments
    /// * `config` - Configuration for epoch thresholds and behavior
    ///
    /// # Returns
    /// * `Ok(())` - Checkpoint manager enabled successfully
    /// * `Err(_)` - Failed to initialize (e.g., directory creation failed)
    ///
    /// # Example
    /// ```text
    /// // Enable with custom thresholds
    /// let config = EpochConfig {
    ///     epoch_duration: Duration::from_millis(500),
    ///     max_ops_per_epoch: 5000,
    ///     max_wal_size_bytes: 32 * 1024 * 1024, // 32MB
    ///     ..EpochConfig::default()
    /// };
    /// trie.enable_epoch_checkpointing(config)?;
    /// ```
    pub fn enable_epoch_checkpointing(&mut self, config: EpochConfig) -> Result<()> {
        // Create epoch subdirectory based on the trie's file path
        let epoch_dir = if let Some(ref path) = self.file_path {
            path.with_extension("epoch")
        } else {
            return Err(PersistentARTrieError::internal(
                "Cannot enable epoch checkpointing without a file path",
            ));
        };

        let manager = CheckpointManager::new(&epoch_dir, config)?;
        self.checkpoint_manager = Some(Arc::new(manager));
        Ok(())
    }

    /// Enables epoch-based checkpoint tracking with default configuration.
    pub fn enable_epoch_checkpointing_default(&mut self) -> Result<()> {
        self.enable_epoch_checkpointing(EpochConfig::default())
    }

    /// Enables epoch-based checkpoint tracking with high-throughput configuration.
    ///
    /// Uses longer epochs and higher operation limits, suitable for
    /// batch processing workloads.
    pub fn enable_epoch_checkpointing_high_throughput(&mut self) -> Result<()> {
        self.enable_epoch_checkpointing(EpochConfig::high_throughput())
    }

    /// Enables epoch-based checkpoint tracking with low-latency configuration.
    ///
    /// Uses shorter epochs for lower-latency epoch rotation, suitable for
    /// real-time tracking.
    pub fn enable_epoch_checkpointing_low_latency(&mut self) -> Result<()> {
        self.enable_epoch_checkpointing(EpochConfig::low_latency())
    }

    /// Disables epoch-based checkpointing.
    ///
    /// The checkpoint manager is stopped and dropped. Any pending
    /// checkpoint operations complete before this returns.
    pub fn disable_epoch_checkpointing(&mut self) {
        self.checkpoint_manager = None;
    }

    /// Returns whether epoch-based checkpointing is enabled.
    pub fn has_epoch_checkpointing(&self) -> bool {
        self.checkpoint_manager.is_some()
    }

    /// Records an externally managed operation in the current epoch.
    ///
    /// Public trie mutation APIs call this automatically after successful WAL
    /// appends. Call this manually only for WAL records written outside the
    /// trie's normal mutation path. The `wal_bytes` parameter should be the size
    /// of the WAL record written.
    ///
    /// # Returns
    /// The current epoch ID, or None if checkpointing is not enabled.
    pub fn record_epoch_operation(&self, wal_bytes: usize) -> Option<EpochId> {
        self.checkpoint_manager
            .as_ref()
            .map(|cm| cm.record_operation(wal_bytes))
    }

    /// Returns the current epoch ID.
    pub fn current_epoch_id(&self) -> Option<EpochId> {
        self.checkpoint_manager
            .as_ref()
            .map(|cm| cm.current_epoch_id())
    }

    /// Forces an immediate trie checkpoint and epoch metadata publication.
    ///
    /// This first persists and verifies the trie checkpoint through
    /// [`Self::checkpoint`], then advances to a new epoch and marks the previous
    /// epoch durable. The ordering is intentional: epoch metadata is published
    /// only after the trie state itself is durable.
    ///
    /// # Returns
    /// * `Some(Ok(epoch_id))` - The new current epoch ID after checkpoint publication
    /// * `Some(Err(_))` - The trie checkpoint or epoch metadata publication failed
    /// * `None` - Checkpoint manager not enabled
    pub fn force_epoch_checkpoint(&mut self) -> Option<Result<EpochId>> {
        let checkpoint_manager = self.checkpoint_manager.as_ref().cloned()?;
        Some((|| {
            self.checkpoint()?;
            checkpoint_manager.force_checkpoint()
        })())
    }

    /// Returns the last durable (fully checkpointed) epoch ID.
    pub fn last_durable_epoch(&self) -> Option<EpochId> {
        self.checkpoint_manager
            .as_ref()
            .and_then(|cm| cm.last_durable_epoch())
    }

    /// Returns epoch statistics.
    pub fn epoch_stats(&self) -> Option<EpochStats> {
        self.checkpoint_manager.as_ref().map(|cm| cm.stats())
    }

    /// Returns metadata for recent epochs.
    pub fn epoch_metadata(&self) -> Option<Vec<EpochMetadata>> {
        self.checkpoint_manager
            .as_ref()
            .map(|cm| cm.epoch_metadata())
    }

    /// Returns the configuration for epoch checkpointing.
    pub fn epoch_config(&self) -> Option<&EpochConfig> {
        self.checkpoint_manager.as_ref().map(|cm| cm.config())
    }
}
