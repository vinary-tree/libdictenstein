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

use rayon::prelude::*;

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
        // C2: under the overlay the parallel WRITE is illusory — the shared per-key CAS
        // funnel re-reads `self` fresh each iteration, so any value computed in a rayon
        // partition is immediately re-resolved. Collect `other`'s entries and apply
        // SERIALLY via the shared overlay merge funnel (phantom-safe CAS-retry). The
        // owned arm below keeps the parallel write phase.
        if self.route_overlay() {
            let entries: Vec<(String, V)> = match other.iter_prefix_with_values_and_arena("")? {
                Some(terms) => terms.into_iter().map(|i| (i.term, i.value)).collect(),
                None => return Ok(0),
            };
            return self.merge_entries_overlay(entries, merge_fn);
        }

        use rayon::prelude::*;
        use std::collections::HashMap;

        // Collect all terms with values from source
        let terms_with_values = match other.iter_prefix_with_values_and_arena("")? {
            Some(terms) => terms,
            None => return Ok(0),
        };

        if terms_with_values.is_empty() {
            return Ok(0);
        }

        // Group by first character for parallel processing
        let mut char_groups: HashMap<Option<char>, Vec<(String, V)>> = HashMap::new();
        for item in terms_with_values {
            let first_char = item.term.chars().next();
            char_groups
                .entry(first_char)
                .or_insert_with(Vec::new)
                .push((item.term, item.value));
        }

        // Parallel phase: compute merged values
        // Each partition computes what values need to be inserted
        let partitions: Vec<Vec<(String, V)>> = char_groups
            .into_par_iter()
            .map(|(_, terms)| {
                let mut results = Vec::with_capacity(terms.len());
                for (term, other_value) in terms {
                    // Note: Reading from self is a concurrent read - safe because we're not mutating
                    let merged_value = if let Some(self_value) = self.get(&term) {
                        merge_fn(&self_value, &other_value)
                    } else {
                        other_value
                    };
                    results.push((term, merged_value));
                }
                results
            })
            .collect();

        // Sequential phase: insert all results
        let mut total_processed = 0;
        for partition in partitions {
            for (term, value) in partition {
                self.upsert(&term, value)?;
                total_processed += 1;
            }
        }

        Ok(total_processed)
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
        batch_size: usize,
    ) -> Result<usize>
    where
        F: Fn(&V, &V) -> V + Sync + Send,
        V: Clone + Send + Sync,
    {
        // C2: under the overlay, apply SERIALLY via the shared overlay merge funnel
        // (the parallel write is illusory — the CAS re-reads self each iteration). The
        // owned arm below keeps batched parallel writes.
        if self.route_overlay() {
            let entries: Vec<(String, V)> = match other.iter_prefix_with_values_and_arena("")? {
                Some(terms) => terms.into_iter().map(|i| (i.term, i.value)).collect(),
                None => return Ok(0),
            };
            return self.merge_entries_overlay(entries, merge_fn);
        }

        use rayon::prelude::*;

        let batch_size = if batch_size == 0 { 5_000 } else { batch_size };

        // Collect all terms with values from source
        let terms_with_values = match other.iter_prefix_with_values_and_arena("")? {
            Some(terms) => terms,
            None => return Ok(0),
        };

        let mut total_processed = 0;

        // Process in batches
        for batch in terms_with_values.chunks(batch_size) {
            // Parallel phase: compute merged values for this batch
            let results: Vec<(String, V)> = batch
                .par_iter()
                .map(|item| {
                    let merged_value = if let Some(self_value) = self.get(&item.term) {
                        merge_fn(&self_value, &item.value)
                    } else {
                        item.value.clone()
                    };
                    (item.term.clone(), merged_value)
                })
                .collect();

            // Sequential phase: insert results for this batch
            for (term, value) in results {
                self.upsert(&term, value)?;
                total_processed += 1;
            }
        }

        Ok(total_processed)
    }
}
