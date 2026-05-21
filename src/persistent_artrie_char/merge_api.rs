//! Merge API for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~345-609, ~265 LOC)
//! as a Phase-6 char sub-module. Methods covered:
//!
//! - `merge_from` (arena-grouped single-pass merge)
//! - `merge_replace` (convenience wrapper)
//! - `merge_from_batched` / `merge_from_batched_grouped`
//! - `merge_from_batched_with_options` (private, shared)

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::Result;
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// Merge another trie into this one using a custom merge function.
    ///
    /// This method iterates over all terms in `other` and merges them into `self`:
    /// - If a term exists in both tries, applies `merge_fn` to combine values
    /// - If a term only exists in `other`, it's inserted with its value
    ///
    /// Uses page-locality optimization: terms from `other` are grouped by their
    /// disk arena location before processing, minimizing page faults when reading
    /// from the source trie. This follows the same pattern as `remove_prefix_batched()`.
    ///
    /// # Arguments
    ///
    /// * `other` - The source trie to merge from
    /// * `merge_fn` - Function to combine values when a term exists in both tries
    ///
    /// # Returns
    ///
    /// The number of terms processed from `other`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Merge n-gram counts from worker trie into main trie
    /// let processed = main_trie.merge_from(&worker_trie, |self_count, other_count| {
    ///     self_count + other_count  // Sum the counts
    /// })?;
    /// ```
    pub fn merge_from<F>(&mut self, other: &Self, merge_fn: F) -> Result<usize>
    where
        F: Fn(&V, &V) -> V,
        V: Clone,
    {
        use std::collections::HashMap;

        let mut processed = 0;

        // Collect all terms with arena info for page-locality optimization
        let terms_with_arena = match other.iter_prefix_with_values_and_arena("")? {
            Some(terms) => terms,
            None => return Ok(0), // Empty trie
        };

        // GROUP BY ARENA for read cache locality on the source trie
        let mut arena_groups: HashMap<Option<u32>, Vec<(String, V)>> = HashMap::new();
        for item in terms_with_arena {
            arena_groups
                .entry(item.arena_id)
                .or_insert_with(Vec::new)
                .push((item.term, item.value));
        }

        // Sort arena IDs for sequential I/O (None = in-memory first)
        let mut arena_ids: Vec<_> = arena_groups.keys().copied().collect();
        arena_ids.sort();

        // Process each arena's terms together (page-locality aware)
        for arena_id in arena_ids {
            if let Some(terms) = arena_groups.remove(&arena_id) {
                for (term, other_value) in terms {
                    processed += 1;

                    // Check if term exists in self and merge values
                    let merged_value = if let Some(self_value) = self.get(&term) {
                        merge_fn(self_value, &other_value)
                    } else {
                        other_value
                    };

                    // Upsert the merged value
                    self.upsert(&term, merged_value)?;
                }
            }
        }

        Ok(processed)
    }

    /// Merge another trie into this one, replacing existing values.
    ///
    /// This is equivalent to `merge_from(other, |_, other_val| other_val.clone())`.
    /// Terms from `other` overwrite terms in `self` if they exist.
    ///
    /// Uses page-locality optimization for efficient I/O.
    ///
    /// # Returns
    ///
    /// The number of terms processed from `other`.
    pub fn merge_replace(&mut self, other: &Self) -> Result<usize>
    where
        V: Clone,
    {
        self.merge_from(other, |_, other_val| other_val.clone())
    }

    /// Merge all terms from another trie with memory-bounded batching.
    ///
    /// This method processes terms in batches to avoid loading all terms
    /// into memory at once. Each batch is processed sequentially, with
    /// periodic WAL syncs for durability.
    ///
    /// # Arguments
    ///
    /// * `other` - The source trie to merge from
    /// * `merge_fn` - Function to combine values when a term exists in both tries.
    ///                Called as `merge_fn(self_value, other_value)`.
    /// * `batch_size` - Number of terms to process per batch (0 = default 5000)
    ///
    /// # Returns
    ///
    /// The number of terms processed from the source trie.
    ///
    /// # Memory Usage
    ///
    /// Memory usage is O(batch_size) for the term buffer, plus O(n) for reading
    /// from the source trie (where n is the number of terms in the source).
    /// For truly memory-bounded operation with very large source tries, consider
    /// using cursor-based iteration (not yet implemented for char tries).
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
    ///
    /// # Performance
    ///
    /// Expected improvement: 10-20% faster merge for disk-resident tries due to
    /// sequential I/O patterns. For in-memory tries, there is no significant difference.
    ///
    /// # Arguments
    ///
    /// * `other` - The source trie to merge from
    /// * `merge_fn` - Function to merge values when a term exists in both tries
    /// * `batch_size` - Number of terms to process per batch (0 uses default 5,000)
    ///
    /// # Returns
    ///
    /// The total number of terms processed from `other`.
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
    /// # Arguments
    ///
    /// * `other` - The source trie to merge from
    /// * `merge_fn` - Function to merge values when a term exists in both tries
    /// * `batch_size` - Number of terms to process per batch (0 uses default 5,000)
    /// * `arena_grouped` - If true, sort each batch by arena_id for sequential I/O
    ///
    /// # Returns
    ///
    /// The total number of terms processed from `other`.
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
        let batch_size = if batch_size == 0 { 5_000 } else { batch_size };

        // Collect all terms with arena info for page-locality optimization
        let terms_with_arena = match other.iter_prefix_with_values_and_arena("")? {
            Some(terms) => terms,
            None => return Ok(0), // Empty trie
        };

        let mut total_processed = 0;

        // Process in batches
        for chunk in terms_with_arena.chunks(batch_size) {
            // Sort batch by arena_id for sequential I/O if requested
            let batch: Vec<_> = if arena_grouped {
                let mut sorted_batch: Vec<_> = chunk.to_vec();
                sorted_batch.sort_by(|a, b| match (a.arena_id, b.arena_id) {
                    (Some(a_id), Some(b_id)) => a_id.cmp(&b_id).then_with(|| a.term.cmp(&b.term)),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => a.term.cmp(&b.term),
                });
                sorted_batch
            } else {
                chunk.to_vec()
            };

            for item in batch {
                // Check if term exists in self and merge values
                let merged_value = if let Some(self_value) = self.get(&item.term) {
                    merge_fn(self_value, &item.value)
                } else {
                    item.value.clone()
                };

                // Upsert the merged value
                self.upsert(&item.term, merged_value)?;
                total_processed += 1;
            }

            // Optional: sync after each batch for durability
            // self.sync()?;
        }

        Ok(total_processed)
    }
}
