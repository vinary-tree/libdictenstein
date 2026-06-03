//! Public mutation API for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~280-332, ~53 LOC)
//! as the twenty-third Phase-6 char sub-module. Methods covered:
//!
//! - `insert` — WAL-logged term-only insert
//! - `insert_with_value` — WAL-logged term+value insert
//! - `remove` — WAL-logged remove
//!
//! These wrap the `_no_wal` core helpers that stay in
//! `dict_impl_char.rs` and route every operation through
//! `append_to_wal` (which honors group commit when enabled).

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::WalRecord;
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// Insert a term with WAL logging.
    ///
    /// **Flip routing (design §2):** when `route_overlay()` (the kill-switch
    /// selects the lock-free overlay AND it is live), this membership insert
    /// routes to the proven Order-A [`insert_cas_durable`](Self::insert_cas_durable)
    /// (value-free, so it is safe for ALL `V`). Otherwise the verbatim owned-tree
    /// body runs (the one-release fallback — NO mutation logic duplicated).
    pub fn insert(&mut self, term: &str) -> Result<bool> {
        if self.route_overlay() {
            // Overlay path: Order-A WAL-then-CAS over the immutable overlay. The
            // primitive itself does the WAL append (chokepoint =
            // `invalidate_eviction_registry`), the visibility CAS, and the
            // committed-watermark advance — so the registry-invalidation contract
            // and `NoLostWriteUnderLockFreeCommit` hold by construction.
            return self.insert_cas_durable(term);
        }

        if !self.preflight_insert_no_wal(term)? {
            return Ok(false);
        }

        // Log to WAL first (routes through group commit if enabled)
        let record = WalRecord::Insert {
            term: term.as_bytes().to_vec(),
            value: None,
        };
        self.append_to_wal(record)?;

        // Mark version as being written (odd = in-progress)
        self.version.begin_write();
        let result = self.try_insert_impl_no_wal(term);
        // Mark version as stable (even = complete)
        self.version.end_write();

        result
    }

    /// Insert a term with an associated value and WAL logging.
    ///
    /// **Flip routing (design §2, per-monomorph guard):** the overlay value-write
    /// path is hardcoded `<u64>` (arbitrary `V` needs G1), so this routes to the
    /// overlay ONLY for the `V = u64` monomorph, dispatched via the SAFE `Any`
    /// downcast in [`super::lockfree_value_route::route_insert_with_value`] (zero
    /// unsafe). Arbitrary `V` stays on the proven owned tree (the design's
    /// "arbitrary V → forced OwnedTree" gap).
    pub fn insert_with_value(&mut self, term: &str, value: V) -> Result<bool> {
        if self.route_overlay() {
            if let Some(routed) =
                super::lockfree_value_route::route_insert_with_value(self, term, &value)
            {
                return routed;
            }
            // Arbitrary V under LockFreeOverlay: fall through to the owned body
            // (forced OwnedTree — the value write path cannot represent arbitrary V).
        }

        self.preflight_insert_with_value_no_wal(term)?;

        // Log to WAL first (routes through group commit if enabled)
        let value_bytes = crate::serialization::bincode_compat::serialize(&value).map_err(|e| {
            PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
        })?;
        let record = WalRecord::Insert {
            term: term.as_bytes().to_vec(),
            value: Some(value_bytes),
        };
        self.append_to_wal(record)?;

        // Mark version as being written (odd = in-progress)
        self.version.begin_write();
        let result = self.try_insert_impl_no_wal_with_value(term, value);
        // Mark version as stable (even = complete)
        self.version.end_write();

        result
    }

    /// Remove a term with WAL logging.
    ///
    /// **Flip routing (design RB6 / §2):** when `route_overlay()`, this routes to
    /// the PROVEN [`remove_cas_durable`](Self::remove_cas_durable) (R-B: Order-A
    /// WAL `Remove` → path-copy clearing the leaf's finality → root-CAS →
    /// mark_committed; loom/proptest/TLA-re-proven, committed). Value-free, so it
    /// is safe for ALL `V`. RB6 depends on fault-in being a production path (F0
    /// un-gated it), because remove-under-evicted-prefix needs fault-in.
    pub fn remove(&mut self, term: &str) -> Result<bool> {
        if self.route_overlay() {
            return self.remove_cas_durable(term);
        }

        if !self.preflight_remove_no_wal(term)? {
            return Ok(false);
        }

        // Log to WAL first (routes through group commit if enabled)
        let record = WalRecord::Remove {
            term: term.as_bytes().to_vec(),
        };
        self.append_to_wal(record)?;

        // Mark version as being written
        self.version.begin_write();
        let result = self.try_remove_impl_no_wal(term);
        self.version.end_write();

        result
    }
}
