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
//! batched paths lives here too. They call `get_value_impl` /
//! `insert_impl` / `iter_prefix_with_values_and_arena` /
//! `iter_prefix_from_cursor` on `PersistentARTrie`.

use std::collections::BTreeMap;

use crate::value::DictionaryValue;

use super::block_storage::BlockStorage;
use super::dict_impl::{PersistentARTrie, PrefixTermWithValueAndArena};
use super::error::Result;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Merge another trie into this one using a custom merge function.
    ///
    /// Uses arena-aware iteration for improved I/O locality. Groups terms by
    /// their disk arena before processing, processing arena groups in sorted
    /// order for sequential I/O patterns.
    pub fn merge_from<F>(&mut self, other: &Self, merge_fn: F) -> Result<usize>
    where
        F: Fn(&V, &V) -> V,
        V: Clone,
    {
        let other_terms = match other.iter_prefix_with_values_and_arena(b"")? {
            Some(terms) => terms,
            None => return Ok(0),
        };

        let mut by_arena: BTreeMap<Option<u32>, Vec<PrefixTermWithValueAndArena<V>>> =
            BTreeMap::new();
        for term_info in other_terms {
            by_arena.entry(term_info.arena_id).or_default().push(term_info);
        }

        let mut processed = 0;

        for (_arena_id, arena_terms) in by_arena {
            for term_info in arena_terms {
                processed += 1;

                let existing_value = self.get_value_impl(&term_info.term);
                let merged_value = if let Some(ref self_value) = existing_value {
                    merge_fn(self_value, &term_info.value)
                } else {
                    term_info.value
                };

                self.insert_impl(&term_info.term, Some(merged_value));
            }
        }

        Ok(processed)
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
        let mut total_processed = 0;
        let mut cursor: Option<Vec<u8>> = None;

        loop {
            let mut batch = other.iter_prefix_from_cursor(b"", cursor.as_deref(), batch_size)?;

            if batch.is_empty() {
                break;
            }

            let batch_len = batch.len();
            let last_term = batch.last().map(|t| t.term.clone());

            if arena_grouped {
                batch.sort_by(|a, b| match (a.arena_id, b.arena_id) {
                    (Some(a_id), Some(b_id)) => a_id.cmp(&b_id).then_with(|| a.term.cmp(&b.term)),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => a.term.cmp(&b.term),
                });
            }

            for term_info in batch {
                let existing_value = self.get_value_impl(&term_info.term);
                let merged_value = if let Some(ref self_value) = existing_value {
                    merge_fn(self_value, &term_info.value)
                } else {
                    term_info.value
                };

                self.insert_impl(&term_info.term, Some(merged_value));
                total_processed += 1;
            }

            if batch_len < batch_size {
                break;
            }

            cursor = last_term;
        }

        Ok(total_processed)
    }
}
