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
use crate::persistent_artrie::error::Result;
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// Insert a term with WAL logging.
    ///
    /// **Flip routing (design §2):** when `route_overlay()` (the kill-switch
    /// selects the lock-free overlay AND it is live), this membership insert
    /// routes to the proven Order-A [`insert_cas_durable`](Self::insert_cas_durable)
    /// (value-free, so it is safe for ALL `V`). Otherwise the verbatim owned-tree
    /// body runs (the one-release fallback — NO mutation logic duplicated).
    pub fn insert(&self, term: &str) -> Result<bool> {
        // Overlay path: Order-A WAL-then-CAS over the immutable overlay. The primitive
        // itself does the WAL append (chokepoint = `invalidate_eviction_registry`), the
        // visibility CAS, and the committed-watermark advance — so the
        // registry-invalidation contract and `NoLostWriteUnderLockFreeCommit` hold by
        // construction.
        self.insert_cas_durable(term)
    }

    /// Insert a term with an associated value and WAL logging.
    ///
    /// **Semantics — UPSERT (overwrite on duplicate):** `insert_with_value` is the
    /// canonical map "insert or update" (see [`crate::MutableMappedDictionary`]); the
    /// owned body overwrites an existing term's value
    /// ([`super::mutation_core`]'s `try_insert_impl_no_wal_with_value` returns
    /// `Ok(false)` *after* writing the new value), matching every other backend and the
    /// dictionary map laws. Returns `Ok(true)` iff the term was newly inserted
    /// (`Ok(false)` = an existing term's value was overwritten).
    ///
    /// **Flip routing (design §2 + C0):** under `route_overlay()` this routes to the
    /// generic Order-A [`upsert_cas_durable_default`](crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite::upsert_cas_durable_default)
    /// for ANY `V` (overwrite = last-writer-wins root-CAS) — NEVER falling through to
    /// the owned tree (a fall-through owned write for arbitrary `V` post-flip would be
    /// unranked → dropped on Overlay reopen = data loss). Empty `""` flows through the
    /// value seam's RANKED depth-0 publish. (C0 fix: this previously routed to the
    /// insert-once `insert_cas_with_value_durable_default`, diverging from the owned
    /// overwrite semantics — a silent overlay↔owned mismatch on duplicate keys.)
    pub fn insert_with_value(&self, term: &str, value: V) -> Result<bool> {
        <Self as crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite<
            crate::persistent_artrie_core::key_encoding::CharKey,
            V,
            S,
        >>::upsert_cas_durable_default(self, term.as_bytes(), value)
    }

    /// Remove a term with WAL logging.
    ///
    /// **Flip routing (design RB6 / §2):** when `route_overlay()`, this routes to
    /// the PROVEN [`remove_cas_durable`](Self::remove_cas_durable) (R-B: Order-A
    /// WAL `Remove` → path-copy clearing the leaf's finality → root-CAS →
    /// mark_committed; loom/proptest/TLA-re-proven, committed). Value-free, so it
    /// is safe for ALL `V`. RB6 depends on fault-in being a production path (F0
    /// un-gated it), because remove-under-evicted-prefix needs fault-in.
    pub fn remove(&self, term: &str) -> Result<bool> {
        self.remove_cas_durable(term)
    }
}
