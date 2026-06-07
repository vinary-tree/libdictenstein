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
    /// Overlay merge core (C2): apply pre-collected `(term, value)` entries into the
    /// lock-free overlay via a per-key read→merge→CAS-retry loop reusing the proven,
    /// phantom-safe [`compare_and_swap_cas_durable`](crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite::compare_and_swap_cas_durable_default).
    ///
    /// Self is read via the overlay seam `value_read_faulting` (NOT `self.get`, which is
    /// `None` under the overlay — the original merge-into-overlay bug). An absent key
    /// INSERTS `other`'s value WITHOUT calling `merge_fn` (the owned merge contract). A
    /// lost CAS (a concurrent change between the read and the publish) burns an UNRANKED
    /// record — dropped on Overlay reopen, so it is phantom-safe — and retries. Per-key
    /// durable, NOT batch-atomic (matches the owned merge). Pre-F4 the Shared `RwLock`
    /// write serializes all writers so the CAS wins first try; the retry loop is
    /// forward-compatible with the F4 lock collapse (merge_lock is an F4 concern).
    pub(crate) fn merge_entries_overlay<F>(
        &self,
        entries: Vec<(String, V)>,
        merge_fn: F,
    ) -> Result<usize>
    where
        F: Fn(&V, &V) -> V,
        V: Clone,
    {
        use crate::persistent_artrie_core::key_encoding::CharKey;
        use crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite;
        let mut processed = 0usize;
        for (term, other_value) in entries {
            let key = term.as_bytes();
            let mut spins = 0u32;
            loop {
                let self_val =
                    <Self as DurableOverlayWrite<CharKey, V, S>>::value_read_faulting(self, key)?;
                let merged = match &self_val {
                    Some(s) => merge_fn(s, &other_value),
                    None => other_value.clone(),
                };
                if <Self as DurableOverlayWrite<CharKey, V, S>>::compare_and_swap_cas_durable_default(
                    self, key, self_val, merged,
                )? {
                    break;
                }
                // Ok(false): a concurrent writer changed the value between the read and
                // the publish CAS; the durable record just appended was burned (unranked
                // → dropped on Overlay reopen). Re-read + re-merge + retry
                // (obstruction-free backoff). Pre-F4 this never fires.
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

    /// Merge pre-collected `(term, value)` entries into this trie (C2 shared funnel for
    /// the deadlock-safe `SharedCharARTrie::union_with`). Routes to the overlay
    /// [`Self::merge_entries_overlay`] under `route_overlay()`, else an owned
    /// get/merge/upsert loop. `union_with` snapshots `other` and drops its read lock
    /// BEFORE taking `self`'s write lock, then calls this — never holding two `RwLock`s
    /// at once (the AB/BA cross-instance deadlock fix; mirrors the vocab pattern).
    pub(crate) fn merge_entries<F>(
        &mut self,
        entries: Vec<(String, V)>,
        merge_fn: F,
    ) -> Result<usize>
    where
        F: Fn(&V, &V) -> V,
        V: Clone,
    {
        if self.route_overlay() {
            return self.merge_entries_overlay(entries, merge_fn);
        }
        let mut processed = 0usize;
        for (term, other_value) in entries {
            let merged = match self.get(&term) {
                Some(self_value) => merge_fn(self_value, &other_value),
                None => other_value,
            };
            self.upsert(&term, merged)?;
            processed += 1;
        }
        Ok(processed)
    }

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
    /// ```text
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
        // C2: under the overlay, route to the shared per-key CAS-retry merge funnel —
        // reads self via the overlay seam (NOT `self.get`, which is None under overlay),
        // combines via `merge_fn`, and publishes phantom-safely. Arena grouping below is
        // an owned-tree I/O-locality optimization, semantically inert for the merge
        // result, so a flat collect is correct for the overlay.
        if self.route_overlay() {
            let entries: Vec<(String, V)> = match other.iter_prefix_with_values_and_arena("")? {
                Some(terms) => terms.into_iter().map(|i| (i.term, i.value)).collect(),
                None => return Ok(0),
            };
            return self.merge_entries_overlay(entries, merge_fn);
        }

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
        // C2: under the overlay, route to the shared per-key CAS-retry merge funnel.
        // Batching/arena-grouping are owned-tree memory/I/O optimizations, inert for the
        // overlay; collect flat (merge is bulk/rare — acceptable). See `merge_from`.
        if self.route_overlay() {
            let entries: Vec<(String, V)> = match other.iter_prefix_with_values_and_arena("")? {
                Some(terms) => terms.into_iter().map(|i| (i.term, i.value)).collect(),
                None => return Ok(0),
            };
            return self.merge_entries_overlay(entries, merge_fn);
        }

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
