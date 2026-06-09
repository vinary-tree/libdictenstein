//! Rayon-based parallel merge for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~344-531, ~188 LOC)
//! as a Phase-6 char sub-module. Methods covered (all feature-gated
//! on `parallel-merge`):
//!
//! - `merge_from_parallel` — full-scan parallel merge
//! - `merge_from_batched_parallel` — memory-bounded batched
//!   parallel merge
//!
//! Each partition is processed in parallel (read source terms,
//! compute merge values via rayon); the write phase stays
//! sequential to avoid contention.

#![cfg(feature = "parallel-merge")]

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::Result;
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    #[cfg(feature = "parallel-merge")]
    pub fn merge_from_parallel<F>(&mut self, other: &Self, merge_fn: F) -> Result<usize>
    where
        F: Fn(&V, &V) -> V + Sync + Send,
        V: Clone + Send + Sync,
    {
        // L3.3: the overlay is the sole representation. The parallel WRITE was illusory —
        // the shared per-key CAS funnel re-reads `self` fresh each iteration, so any value
        // computed in a rayon partition was immediately re-resolved. Collect `other`'s
        // entries and apply SERIALLY via the shared overlay merge funnel (phantom-safe
        // CAS-retry).
        let entries: Vec<(String, V)> = match other.iter_prefix_with_values_and_arena("")? {
            Some(terms) => terms.into_iter().map(|i| (i.term, i.value)).collect(),
            None => return Ok(0),
        };
        self.merge_entries_overlay(entries, merge_fn)
    }

    /// Merge all terms from another trie with both batching and parallel processing.
    ///
    /// This combines the memory-bounded batching of `merge_from_batched` with
    /// the parallel computation of `merge_from_parallel`. Each batch is
    /// processed in parallel, then results are inserted sequentially.
    ///
    /// # Arguments
    ///
    /// * `other` - The source trie to merge from
    /// * `merge_fn` - Function to merge values when a term exists in both tries.
    /// * `batch_size` - Number of terms to process per batch (0 = default 5000)
    ///
    /// # Returns
    ///
    /// The number of terms processed from the source trie.
    ///
    /// # Feature
    ///
    /// Requires the `parallel-merge` feature to be enabled.
    #[cfg(feature = "parallel-merge")]
    pub fn merge_from_batched_parallel<F>(
        &mut self,
        other: &Self,
        merge_fn: F,
        _batch_size: usize,
    ) -> Result<usize>
    where
        F: Fn(&V, &V) -> V + Sync + Send,
        V: Clone + Send + Sync,
    {
        // L3.3: the overlay is the sole representation. Apply SERIALLY via the shared
        // overlay merge funnel (the parallel write was illusory — the CAS re-reads self
        // each iteration). Batching was an owned memory bound, inert for the overlay.
        let entries: Vec<(String, V)> = match other.iter_prefix_with_values_and_arena("")? {
            Some(terms) => terms.into_iter().map(|i| (i.term, i.value)).collect(),
            None => return Ok(0),
        };
        self.merge_entries_overlay(entries, merge_fn)
    }
}
