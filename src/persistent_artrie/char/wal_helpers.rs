//! WAL + durability helpers for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~346-426, ~81 LOC)
//! as the twenty-second Phase-6 char sub-module. Methods covered:
//!
//! - `durability_policy` / `set_durability_policy`
//! - `append_to_wal` (pub(super); routes through group commit when enabled)
//! - `sync_wal` (pub(super); respects full durability policies)

#[cfg(feature = "group-commit")]
use std::sync::Arc;

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
        self.durability_policy.load()
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
    /// use libdictenstein::persistent_artrie::char::{PersistentARTrieChar, DurabilityPolicy};
    ///
    /// let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::create("words.trie")?;
    ///
    /// // Use periodic sync for better performance (accepts bounded data loss)
    /// trie.set_durability_policy(DurabilityPolicy::Periodic);
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_durability_policy(&self, policy: DurabilityPolicy) {
        self.durability_policy.store(policy);
    }

    // ==================== End Epoch-Based Checkpointing Methods ====================

    /// Append a record to the WAL, returning the assigned WAL **LSN** (in the
    /// WAL-writer LSN domain — the same domain `WalRecord::Checkpoint` and
    /// recovery use). This is the foundation of the lock-free **Order-A** durable
    /// write path: the returned LSN is durable-per-policy at return (group-commit
    /// blocks on the batch fsync; the direct path verifies `synced_lsn >= lsn`),
    /// so the WAL record is durable BEFORE the caller performs the
    /// visibility-publishing root CAS. Returns `0` when no WAL writer is
    /// installed (no durability is available — Order-A callers MUST treat a `0`
    /// return as "no WAL" and refuse to acknowledge durability).
    pub(super) fn append_to_wal_returning_lsn(&self, record: WalRecord) -> Result<u64> {
        self.append_to_wal_inner(record)
    }

    /// **Order-A replay-order fix (design C′, step 2.5).** Append + sync a
    /// [`WalRecord::CommitRank`] binding the durable data record at `data_lsn` to
    /// the commit `generation` its visibility CAS landed at, returning the rank
    /// record's own LSN. Called AFTER the visibility CAS wins and BEFORE the op is
    /// acked, so it STRENGTHENS Order-A (an ack now also waits for the rank to be
    /// durable). Recovery's `reconcile_lww` consumes these to order same-term
    /// replay by commit generation instead of WAL physical/LSN order.
    ///
    /// Returns `0` when no WAL writer is installed (same convention as
    /// [`Self::append_to_wal_returning_lsn`]).
    pub(super) fn append_commit_rank(
        &self,
        data_lsn: u64,
        term: &[u8],
        generation: u64,
    ) -> Result<u64> {
        self.append_to_wal_returning_lsn(WalRecord::CommitRank {
            data_lsn,
            term: term.to_vec(),
            generation,
        })
    }

    /// Shared body for [`Self::append_to_wal`] / [`Self::append_to_wal_returning_lsn`].
    fn append_to_wal_inner(&self, record: WalRecord) -> Result<u64> {
        // A durable mutation is being logged: the in-memory trie is diverging
        // from the last checkpoint's on-disk image, so any published eviction
        // registry now references potentially-stale on-disk data. Invalidate it
        // here — the single chokepoint every public mutation passes through — so
        // eviction cannot unswizzle a live node onto a stale disk location until
        // the next checkpoint rebuilds a fresh registry. No-op when eviction is
        // disabled. See `invalidate_eviction_registry` for the full rationale.
        self.invalidate_eviction_registry();

        let wal_bytes = record.serialized_size();

        // Check if group commit is enabled first. F4: clone the coordinator Arc out
        // under a BRIEF lock then RELEASE it before `append_with_sync` (which may
        // block on fsync) — never hold the subsystem mutex across I/O.
        #[cfg(feature = "group-commit")]
        {
            let gc = self
                .group_commit
                .lock()
                .expect("group_commit mutex poisoned")
                .as_ref()
                .map(Arc::clone);
            if let Some(gc) = gc {
                let appended_lsn =
                    gc.append_with_sync(record)
                        .map_err(|e| PersistentARTrieError::WalError {
                            reason: format!("{:?}", e),
                        })?;
                self.record_epoch_operation(wal_bytes);
                self.verify_full_policy_sync_coverage(appended_lsn)?;
                return Ok(appended_lsn);
            }
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
            return Ok(appended_lsn);
        }
        Ok(0)
    }

    fn sync_wal_after_append(&self, appended_lsn: u64) -> Result<()> {
        match self.durability_policy.load() {
            DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit => {}
            DurabilityPolicy::Periodic | DurabilityPolicy::None => return Ok(()),
        }

        // Group commit handles syncing internally via append_with_sync.
        #[cfg(feature = "group-commit")]
        if self
            .group_commit
            .lock()
            .expect("group_commit mutex poisoned")
            .is_some()
        {
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
        match self.durability_policy.load() {
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
