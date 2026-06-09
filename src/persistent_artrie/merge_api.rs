//! Merge API for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~2332-2549, ~218 LOC) as
//! the nineteenth Phase-5 byte sub-module. These public methods
//! merge another trie's contents into this one:
//!
//! - `merge_from` — single-pass arena-grouped merge with custom merge_fn
//! - `merge_replace` — convenience wrapper that overwrites on conflict
//! - `merge_from_batched` — memory-bounded batched merge
//! - `merge_from_batched_grouped` — batched merge sorted by arena ID
//!   for sequential I/O on disk-resident tries
//!
//! The private `merge_from_batched_with_options` shared by the two
//! batched paths lives here too. Since L3.3 the overlay is the sole
//! representation, so all of these route through the per-key CAS-retry
//! overlay merge funnel `merge_entries_overlay` (the owned arena-grouped
//! merge was deleted with the owned tree at L3.3b).

use crate::value::DictionaryValue;

use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;
use super::error::Result;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Overlay merge core (C2): apply pre-collected `(term, value)` byte entries into
    /// the lock-free overlay via a per-key read→merge→CAS-retry loop reusing the
    /// proven, phantom-safe `compare_and_swap_cas_durable`. Self is read via the overlay
    /// seam `value_read_faulting` (NOT `get_value_impl`, which reads the empty owned tree
    /// under the overlay — the original merge-into-overlay bug). An absent key INSERTS
    /// `other`'s value WITHOUT calling `merge_fn` (the owned merge contract). A lost CAS
    /// burns an UNRANKED record (dropped on Overlay reopen ⇒ phantom-safe) and retries.
    /// Per-key durable, NOT batch-atomic. Pre-F4 the Shared `RwLock` serializes writers
    /// so the CAS wins first try; the retry loop is forward-compatible with F4.
    pub(crate) fn merge_entries_overlay<F>(
        &self,
        entries: Vec<(Vec<u8>, V)>,
        merge_fn: F,
    ) -> Result<usize>
    where
        F: Fn(&V, &V) -> V,
        V: Clone,
    {
        use crate::persistent_artrie_core::key_encoding::ByteKey;
        use crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite;
        let mut processed = 0usize;
        for (term, other_value) in entries {
            let mut spins = 0u32;
            loop {
                let self_val =
                    <Self as DurableOverlayWrite<ByteKey, V, S>>::value_read_faulting(self, &term)?;
                let merged = match &self_val {
                    Some(s) => merge_fn(s, &other_value),
                    None => other_value.clone(),
                };
                if <Self as DurableOverlayWrite<ByteKey, V, S>>::compare_and_swap_cas_durable_default(
                    self, &term, self_val, merged,
                )? {
                    break;
                }
                // Ok(false): a concurrent writer changed the value between the read and
                // the publish CAS; the appended record was burned (unranked → dropped on
                // Overlay reopen). Re-read + re-merge + retry. Pre-F4 never fires.
                spins += 1;
                if spins < 32 {
                    std::hint::spin_loop();
                } else {
                    std::thread::yield_now();
                }
            }
            processed += 1;
        }
        Ok(processed)
    }

    /// Merge another trie into this one using a custom merge function.
    ///
    /// Uses arena-aware iteration for improved I/O locality. Groups terms by
    /// their disk arena before processing, processing arena groups in sorted
    /// order for sequential I/O patterns.
    ///
    /// **M3 reject (BROKEN-BY-DESIGN, audit §B #2 — covers `merge_replace`).** Under
    /// `route_overlay()` `self.get_value_impl` reads the EMPTY owned tree (None,
    /// defeating `merge_fn`) and the write `insert_impl` mutates the owned tree the
    /// overlay read/checkpoint path does NOT observe — a trie-to-trie merge would
    /// silently REPLACE/DROP the live overlay counts. The overlay IS the durable
    /// production state; merge-into-it is incoherent. Reject (mirroring
    /// `merge_lockfree_values_to_persistent`); overlay merge is an E1-iter-B follow-on.
    pub fn merge_from<F>(&mut self, other: &Self, merge_fn: F) -> Result<usize>
    where
        F: Fn(&V, &V) -> V,
        V: Clone,
    {
        // C2: under the overlay, route to the shared per-key CAS-retry merge funnel —
        // reads self via the overlay seam (NOT get_value_impl over the empty owned
        // tree), combines via merge_fn, publishes phantom-safely. Arena grouping below
        // is an owned-tree I/O-locality optimization, inert for the overlay.
        let entries: Vec<(Vec<u8>, V)> = match other.iter_prefix_with_values_and_arena(b"")? {
            Some(terms) => terms.into_iter().map(|t| (t.term, t.value)).collect(),
            None => return Ok(0),
        };
        self.merge_entries_overlay(entries, merge_fn)
    }

    /// Merge another trie into this one, replacing values on conflict.
    ///
    /// This is a convenience method equivalent to:
    /// `merge_from(other, |_, other_val| other_val.clone())`
    pub fn merge_replace(&mut self, other: &Self) -> Result<usize>
    where
        V: Clone,
    {
        self.merge_from(other, |_, other_val| other_val.clone())
    }

    /// Merge another trie into this one with memory-bounded batching.
    ///
    /// This method processes the source trie in batches to bound peak memory
    /// usage. Each batch is processed and then discarded before loading the
    /// next batch.
    pub fn merge_from_batched<F>(
        &mut self,
        other: &Self,
        merge_fn: F,
        batch_size: usize,
    ) -> Result<usize>
    where
        F: Fn(&V, &V) -> V,
        V: Clone,
    {
        self.merge_from_batched_with_options(other, merge_fn, batch_size, false)
    }

    /// Merge terms from another trie in batches, sorted by arena ID for sequential I/O.
    ///
    /// This is an optimized version of `merge_from_batched` that sorts each batch
    /// by arena ID before processing. This optimization improves I/O performance
    /// when merging disk-resident tries by ensuring sequential disk access patterns.
    pub fn merge_from_batched_grouped<F>(
        &mut self,
        other: &Self,
        merge_fn: F,
        batch_size: usize,
    ) -> Result<usize>
    where
        F: Fn(&V, &V) -> V,
        V: Clone,
    {
        self.merge_from_batched_with_options(other, merge_fn, batch_size, true)
    }

    /// Internal implementation of batched merge with optional arena grouping.
    ///
    /// **M3 reject (BROKEN-BY-DESIGN, audit §B #3 — covers `merge_from_batched` +
    /// `merge_from_batched_grouped`).** Same hazard as [`merge_from`](Self::merge_from):
    /// `get_value_impl` + `insert_impl` over the empty owned tree silently
    /// replaces/drops live overlay counts. Reject under `route_overlay()`.
    fn merge_from_batched_with_options<F>(
        &mut self,
        other: &Self,
        merge_fn: F,
        batch_size: usize,
        arena_grouped: bool,
    ) -> Result<usize>
    where
        F: Fn(&V, &V) -> V,
        V: Clone,
    {
        // C2: under the overlay, route to the shared per-key CAS-retry merge funnel
        // (batching/grouping are owned-tree memory/I/O optimizations, inert for the
        // overlay; collect flat — merge is bulk/rare). See `merge_from`.
        // `batch_size` / `arena_grouped` were owned-tree I/O-locality knobs, inert for
        // the overlay merge funnel (collect flat — merge is bulk/rare).
        let _ = (batch_size, arena_grouped);
        let entries: Vec<(Vec<u8>, V)> = match other.iter_prefix_with_values_and_arena(b"")? {
            Some(terms) => terms.into_iter().map(|t| (t.term, t.value)).collect(),
            None => return Ok(0),
        };
        self.merge_entries_overlay(entries, merge_fn)
    }
}
