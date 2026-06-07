//! Parallel-merge extension trait for `SharedARTrie<V>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~6266-6399) as part of the
//! Phase-5 decomposition. Feature-gated on `parallel-merge`; uses rayon
//! to parallelize the read-and-merge phase before serializing the write
//! phase under the trie's write lock. The trait sits in its own module
//! because (a) it is the only top-level `pub trait` introduced by
//! byte's dict_impl, and (b) it is the only Phase-5 surface that does
//! not require new methods on `PersistentARTrie<V, S>` — the
//! `insert_impl` / `get_value_impl` helpers it calls just had their
//! visibility widened to `pub(super)` to permit access from this
//! sibling module.

#![cfg(feature = "parallel-merge")]

use super::SharedARTrie;
use crate::persistent_artrie::error::Result;
use crate::value::DictionaryValue;

/// Extension trait for parallel merge operations on [`SharedARTrie`].
///
/// These methods require the `parallel-merge` feature and use rayon for
/// parallel processing. They are implemented as an extension trait because
/// `SharedARTrie` is a type alias for `Arc<RwLock<PersistentARTrie<V>>>`,
/// and Rust doesn't allow inherent `impl` blocks on type aliases that resolve
/// to external types.
///
/// # Usage
///
/// ```text
/// use libdictenstein::persistent_artrie::{SharedARTrie, SharedARTrieParallelExt};
///
/// let trie1: SharedARTrie<u32> = /* ... */;
/// let trie2: SharedARTrie<u32> = /* ... */;
///
/// // Import the trait to use the method
/// let count = trie1.merge_from_parallel(&trie2, |a, b| a + b)?;
/// ```
pub trait SharedARTrieParallelExt<V: DictionaryValue> {
    /// Merge all terms from another trie using parallel processing.
    ///
    /// This method uses rayon to parallelize the merge computation across multiple
    /// cores. The parallelization strategy:
    /// 1. Partition source terms by first byte (256 possible partitions)
    /// 2. Process partitions in parallel: read source terms, compute merge values
    /// 3. Batch-insert results sequentially (avoids write contention)
    ///
    /// # Performance
    ///
    /// Expected speedup: 4-6x on 8 cores for large merges (100K+ terms).
    /// The speedup is limited by the sequential write phase but the parallel
    /// read and merge computation phases scale well.
    ///
    /// # Arguments
    ///
    /// * `other` - The source trie to merge from
    /// * `merge_fn` - Function to merge values when a term exists in both tries.
    ///                Called as `merge_fn(self_value, other_value)`.
    ///
    /// # Returns
    ///
    /// The number of terms processed from the source trie.
    fn merge_from_parallel<F>(&self, other: &Self, merge_fn: F) -> Result<usize>
    where
        F: Fn(&V, &V) -> V + Sync + Send;
}

impl<V: DictionaryValue + Clone + Send + Sync> SharedARTrieParallelExt<V> for SharedARTrie<V> {
    fn merge_from_parallel<F>(&self, other: &Self, merge_fn: F) -> Result<usize>
    where
        F: Fn(&V, &V) -> V + Sync + Send,
    {
        use rayon::prelude::*;

        // C2: under the overlay, snapshot `other` (release its read lock) then apply
        // SERIALLY under `self.write()` via the shared overlay merge funnel. The
        // parallel WRITE is illusory under the overlay (the per-key CAS re-reads self
        // each iteration), and snapshot-then-write avoids the cross-instance AB/BA
        // deadlock the owned parallel phase risks (holding `other.read()` per partition
        // across a pending `self.write()` — red-team R4-1). The owned parallel arm below
        // is unchanged (its latent owned-mode deadlock is the cross-instance sweep,
        // task #35).
        if self.read().route_overlay() {
            let entries: Vec<(Vec<u8>, V)> = {
                let other_guard = other.read();
                match other_guard.iter_prefix_with_values_and_arena(b"") {
                    Ok(Some(terms)) => terms.into_iter().map(|t| (t.term, t.value)).collect(),
                    _ => Vec::new(),
                }
            };
            let guard = self.write();
            return guard.merge_entries_overlay(entries, merge_fn);
        }

        // Partition by first byte (0-255) for parallel processing.
        let partitions: Vec<Vec<(Vec<u8>, V)>> = (0u8..=255u8)
            .into_par_iter()
            .map(|prefix_byte| {
                let prefix = [prefix_byte];
                let other_guard = other.read();

                let mut partition_terms = Vec::new();
                let mut cursor: Option<Vec<u8>> = None;
                let batch_size = 10_000;

                loop {
                    let batch = match other_guard.iter_prefix_from_cursor(
                        &prefix,
                        cursor.as_deref(),
                        batch_size,
                    ) {
                        Ok(b) => b,
                        Err(_) => break,
                    };

                    if batch.is_empty() {
                        break;
                    }

                    let batch_len = batch.len();
                    let last_term = batch.last().map(|t| t.term.clone());

                    for term_info in batch {
                        let self_guard = self.read();
                        let existing_value = self_guard.get_value_impl(&term_info.term);
                        drop(self_guard);

                        let merged_value = if let Some(ref self_value) = existing_value {
                            merge_fn(self_value, &term_info.value)
                        } else {
                            term_info.value
                        };

                        partition_terms.push((term_info.term, merged_value));
                    }

                    if batch_len < batch_size {
                        break;
                    }

                    cursor = last_term;
                }

                partition_terms
            })
            .collect();

        // Sequential write phase — batch insert all partitions.
        let mut total_processed = 0;
        let mut guard = self.write();

        for partition in partitions {
            for (term, value) in partition {
                guard.insert_impl(&term, Some(value));
                total_processed += 1;
            }
        }

        Ok(total_processed)
    }
}
