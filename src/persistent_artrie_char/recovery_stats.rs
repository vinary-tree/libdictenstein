//! Enhanced-recovery mode + statistics for the char trie.
//!
//! Split out of char `dict_impl_char.rs` (lines ~83-175) as part of the
//! Phase-6 decomposition. These types describe the outcome of opening a
//! disk-backed char trie: which recovery path was taken (clean open, WAL
//! replay, archive rebuild, epoch-based, or per-node-log replay) and the
//! timing / record-count statistics that go with it.

/// Mode of enhanced recovery (with epoch/per-node logging integration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnhancedRecoveryMode {
    /// File was created new (didn't exist before)
    CreatedNew,
    /// Normal open, no recovery needed
    Normal,
    /// Recovered from WAL after last checkpoint
    WalReplay,
    /// Rebuilt from WAL archive segments
    RebuiltFromWal,
    /// Rebuilt from WAL archive files
    RebuiltFromArchives,
    /// Recovered using epoch-based checkpointing
    EpochRecovery,
    /// Recovered using per-node logging (O(dirty nodes))
    PerNodeRecovery,
}

impl EnhancedRecoveryMode {
    /// Returns true if recovery required rebuilding from WAL
    pub fn required_rebuild(&self) -> bool {
        matches!(
            self,
            EnhancedRecoveryMode::RebuiltFromWal | EnhancedRecoveryMode::RebuiltFromArchives
        )
    }

    /// Returns true if this was a normal open (no recovery)
    pub fn is_normal(&self) -> bool {
        matches!(
            self,
            EnhancedRecoveryMode::Normal | EnhancedRecoveryMode::CreatedNew
        )
    }
}

/// Statistics from enhanced recovery.
#[derive(Debug, Clone)]
pub struct EnhancedRecoveryStats {
    /// The recovery mode used
    pub mode: EnhancedRecoveryMode,
    /// Total time for recovery in milliseconds
    pub duration_ms: u64,
    /// Number of WAL records replayed
    pub records_replayed: usize,
    /// Number of epochs recovered (for epoch-based recovery)
    pub epochs_recovered: usize,
    /// Number of dirty nodes recovered (for per-node logging)
    pub dirty_nodes_recovered: usize,
    /// Number of archive segments used
    pub archive_segments_used: usize,
}

impl EnhancedRecoveryStats {
    /// Create stats for normal open (no recovery)
    pub fn normal() -> Self {
        Self {
            mode: EnhancedRecoveryMode::Normal,
            duration_ms: 0,
            records_replayed: 0,
            epochs_recovered: 0,
            dirty_nodes_recovered: 0,
            archive_segments_used: 0,
        }
    }

    /// Create stats for new file creation
    pub fn created_new() -> Self {
        Self {
            mode: EnhancedRecoveryMode::CreatedNew,
            duration_ms: 0,
            records_replayed: 0,
            epochs_recovered: 0,
            dirty_nodes_recovered: 0,
            archive_segments_used: 0,
        }
    }
}
