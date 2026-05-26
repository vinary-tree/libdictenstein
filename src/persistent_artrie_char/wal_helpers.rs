//! WAL + durability helpers for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~346-426, ~81 LOC)
//! as the twenty-second Phase-6 char sub-module. Methods covered:
//!
//! - `durability_policy` / `set_durability_policy`
//! - `append_to_wal` (pub(super); routes through group commit when enabled)
//! - `sync_wal` (pub(super); respects full durability policies)

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::dict_impl::DurabilityPolicy;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::WalRecord;
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// Get the current durability policy.
    ///
    /// The durability policy controls when fsync is called after WAL writes.
    /// See [`DurabilityPolicy`] for available options and their trade-offs.
    pub fn durability_policy(&self) -> DurabilityPolicy {
        self.durability_policy
    }

    /// Set the durability policy for this trie.
    ///
    /// The durability policy controls when fsync is called after WAL writes,
    /// providing a trade-off between durability and performance.
    ///
    /// # Arguments
    ///
    /// * `policy` - The new durability policy
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use libdictenstein::persistent_artrie_char::{PersistentARTrieChar, DurabilityPolicy};
    ///
    /// let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::create("words.trie")?;
    ///
    /// // Use periodic sync for better performance (accepts bounded data loss)
    /// trie.set_durability_policy(DurabilityPolicy::Periodic);
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_durability_policy(&mut self, policy: DurabilityPolicy) {
        self.durability_policy = policy;
    }

    // ==================== End Epoch-Based Checkpointing Methods ====================

    /// Internal helper: Append a record to the WAL, routing through group commit if enabled.
    ///
    /// When group commit is enabled, the record is submitted to the group commit
    /// coordinator which batches writes and reduces fsync overhead. Otherwise,
    /// the record is written directly to the WAL. After a successful append,
    /// epoch checkpointing records the exact WAL bytes written so epoch
    /// metadata stays aligned with public mutations.
    pub(super) fn append_to_wal(&self, record: WalRecord) -> Result<()> {
        let wal_bytes = record.serialized_size();

        // Check if group commit is enabled first
        #[cfg(feature = "group-commit")]
        if let Some(ref gc) = self.group_commit {
            let appended_lsn =
                gc.append_with_sync(record)
                    .map_err(|e| PersistentARTrieError::WalError {
                        reason: format!("{:?}", e),
                    })?;
            self.record_epoch_operation(wal_bytes);
            self.verify_full_policy_sync_coverage(appended_lsn)?;
            return Ok(());
        }

        // Fall back to direct WAL write
        if let Some(ref wal_writer) = self.wal_writer {
            let appended_lsn =
                wal_writer
                    .append(record)
                    .map_err(|e| PersistentARTrieError::WalError {
                        reason: format!("{:?}", e),
                    })?;
            self.record_epoch_operation(wal_bytes);
            self.sync_wal_after_append(appended_lsn)?;
        }
        Ok(())
    }

    /// Internal helper: Sync the WAL based on durability policy.
    ///
    /// Immediate and GroupCommit both expose full durability acknowledgement
    /// semantics. If a group commit coordinator is installed, the append path
    /// has already waited for the submitted LSN; otherwise this direct WAL
    /// fallback performs a blocking sync.
    pub(super) fn sync_wal(&self) -> Result<()> {
        match self.durability_policy {
            DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit => {}
            DurabilityPolicy::Periodic | DurabilityPolicy::None => return Ok(()),
        }

        // Group commit handles syncing internally via append_with_sync.
        #[cfg(feature = "group-commit")]
        if self.group_commit.is_some() {
            return Ok(());
        }

        // Direct WAL sync
        if let Some(ref wal_writer) = self.wal_writer {
            wal_writer
                .sync()
                .map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;
        }
        Ok(())
    }

    fn sync_wal_after_append(&self, appended_lsn: u64) -> Result<()> {
        match self.durability_policy {
            DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit => {}
            DurabilityPolicy::Periodic | DurabilityPolicy::None => return Ok(()),
        }

        // Group commit handles syncing internally via append_with_sync.
        #[cfg(feature = "group-commit")]
        if self.group_commit.is_some() {
            return self.verify_full_policy_sync_coverage(appended_lsn);
        }

        if let Some(ref wal_writer) = self.wal_writer {
            let synced_lsn = wal_writer
                .sync()
                .map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;
            if synced_lsn < appended_lsn {
                return Err(PersistentARTrieError::Wal(format!(
                    "char WAL sync failed to cover appended LSN {appended_lsn}; synced {synced_lsn}"
                )));
            }
        }
        Ok(())
    }

    #[cfg(feature = "group-commit")]
    fn verify_full_policy_sync_coverage(&self, appended_lsn: u64) -> Result<()> {
        match self.durability_policy {
            DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit => {}
            DurabilityPolicy::Periodic | DurabilityPolicy::None => return Ok(()),
        }

        if let Some(ref wal_writer) = self.wal_writer {
            let synced_lsn = wal_writer.synced_lsn();
            if synced_lsn < appended_lsn {
                return Err(PersistentARTrieError::Wal(format!(
                    "char WAL sync failed to cover appended LSN {appended_lsn}; synced {synced_lsn}"
                )));
            }
        }
        Ok(())
    }
}
