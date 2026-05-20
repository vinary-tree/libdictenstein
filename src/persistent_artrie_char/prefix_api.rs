//! Public prefix-iteration + remove-prefix API for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~344-543, ~200 LOC)
//! as a Phase-6 char sub-module. Methods covered:
//!
//! - `iter_prefix` (Result<Option<Vec<String>>>)
//! - `iter_prefix_with_values` (Result<Option<Vec<(String, V)>>>)
//! - `iter_prefix_with_arena` (Result<Option<Vec<PrefixTermWithArena>>>)
//! - `iter_prefix_with_values_and_arena`
//! - `remove_prefix` / `remove_prefix_batched`
//!
//! These are thin wrappers over the pub(super) navigation +
//! collection helpers in `super::prefix_helpers`.

use std::collections::HashMap;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::Result;
use crate::value::DictionaryValue;

use super::prefix_term::{PrefixTermWithArena, PrefixTermWithValueAndArena};

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    pub fn iter_prefix(&self, prefix: &str) -> Result<Option<Vec<String>>> {
        let node = match self.navigate_to_prefix(prefix)? {
            Some(n) => n,
            None => return Ok(None),
        };

        let mut terms = Vec::new();
        self.collect_terms_under_node(node, prefix.to_string(), &mut terms)?;
        Ok(Some(terms))
    }

    /// Iterate over all (term, value) pairs with the given prefix.
    ///
    /// Returns `Ok(None)` if the prefix path doesn't exist in the trie.
    /// Returns `Ok(Some(vec))` with all (term, value) pairs for terms starting with the prefix.
    pub fn iter_prefix_with_values(&self, prefix: &str) -> Result<Option<Vec<(String, V)>>>
    where
        V: Clone,
    {
        let node = match self.navigate_to_prefix(prefix)? {
            Some(n) => n,
            None => return Ok(None),
        };

        let mut terms = Vec::new();
        self.collect_terms_with_values_under_node(node, prefix.to_string(), &mut terms)?;
        Ok(Some(terms))
    }

    /// Iterate over all terms with the given prefix, including arena information.
    ///
    /// Returns terms along with their disk arena location, enabling page-aware
    /// batch operations that group I/O by arena for improved cache locality.
    ///
    /// # Returns
    ///
    /// - `Ok(None)` - The prefix path doesn't exist in the trie
    /// - `Ok(Some(vec))` - Vector of `PrefixTermWithArena` for matching terms
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let trie = PersistentARTrieChar::open("data.artrie")?;
    /// if let Some(terms) = trie.iter_prefix_with_arena("app")? {
    ///     // Group by arena for I/O-efficient processing
    ///     let mut by_arena: HashMap<Option<u32>, Vec<String>> = HashMap::new();
    ///     for item in terms {
    ///         by_arena.entry(item.arena_id)
    ///             .or_default()
    ///             .push(item.term);
    ///     }
    /// }
    /// ```
    pub fn iter_prefix_with_arena(&self, prefix: &str) -> Result<Option<Vec<PrefixTermWithArena>>> {
        let (node, prefix_arena) = match self.navigate_to_prefix_with_arena(prefix)? {
            Some(pair) => pair,
            None => return Ok(None),
        };

        let mut terms = Vec::new();
        self.collect_terms_with_arena(node, prefix.to_string(), prefix_arena, &mut terms, usize::MAX)?;
        Ok(Some(terms))
    }

    /// Iterate over all terms with values and arena locations for the given prefix.
    ///
    /// Returns terms along with their values and disk arena location, enabling
    /// page-aware merge operations that group I/O by arena for improved cache locality.
    /// This is the same pattern used by `remove_prefix_batched()`.
    ///
    /// # Returns
    ///
    /// - `Ok(None)` - The prefix path doesn't exist in the trie
    /// - `Ok(Some(vec))` - Vector of `PrefixTermWithValueAndArena<V>` for matching terms
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let trie = PersistentARTrieChar::<i64>::open("data.artrie")?;
    /// if let Some(terms) = trie.iter_prefix_with_values_and_arena("")? {
    ///     // Group by arena for I/O-efficient merge processing
    ///     let mut by_arena: HashMap<Option<u32>, Vec<(String, i64)>> = HashMap::new();
    ///     for item in terms {
    ///         by_arena.entry(item.arena_id)
    ///             .or_default()
    ///             .push((item.term, item.value));
    ///     }
    ///     // Process each arena's terms together for page locality
    ///     for (arena_id, terms) in by_arena {
    ///         for (term, value) in terms {
    ///             // Merge logic here
    ///         }
    ///     }
    /// }
    /// ```
    pub fn iter_prefix_with_values_and_arena(
        &self,
        prefix: &str,
    ) -> Result<Option<Vec<PrefixTermWithValueAndArena<V>>>>
    where
        V: Clone,
    {
        let (node, prefix_arena) = match self.navigate_to_prefix_with_arena(prefix)? {
            Some(pair) => pair,
            None => return Ok(None),
        };

        let mut terms = Vec::new();
        self.collect_terms_with_values_and_arena(
            node,
            prefix.to_string(),
            prefix_arena,
            &mut terms,
            usize::MAX,
        )?;
        Ok(Some(terms))
    }

    /// Remove all terms with the given prefix.
    ///
    /// Uses a default batch size of 1024 to limit memory usage.
    /// Each removal is logged to WAL individually for crash recovery safety.
    ///
    /// # Returns
    ///
    /// The number of terms removed.
    pub fn remove_prefix(&mut self, prefix: &str) -> Result<usize> {
        self.remove_prefix_batched(prefix, 1024)
    }

    /// Remove all terms with the given prefix using page-aware batching.
    ///
    /// This method groups terms by their disk arena before removal, improving
    /// cache locality and reducing page faults for large prefix subtrees.
    /// Arenas are processed in sorted order for sequential I/O patterns.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The prefix to match
    /// * `batch_size` - Maximum terms to collect per batch
    ///
    /// # Returns
    ///
    /// The number of terms removed.
    pub fn remove_prefix_batched(&mut self, prefix: &str, batch_size: usize) -> Result<usize> {
        use std::collections::HashMap;

        let batch_size = batch_size.max(1);
        let mut total_removed = 0;

        loop {
            // Collect a batch of terms WITH arena information
            let batch: Vec<PrefixTermWithArena> = {
                let (node, prefix_arena) = match self.navigate_to_prefix_with_arena(prefix)? {
                    Some(pair) => pair,
                    None => break, // Prefix no longer exists
                };

                let mut terms = Vec::with_capacity(batch_size);
                self.collect_terms_with_arena(
                    node,
                    prefix.to_string(),
                    prefix_arena,
                    &mut terms,
                    batch_size,
                )?;
                terms
            };

            if batch.is_empty() {
                break;
            }

            // GROUP BY ARENA for cache locality
            let mut arena_groups: HashMap<Option<u32>, Vec<String>> = HashMap::new();
            for item in batch {
                arena_groups
                    .entry(item.arena_id)
                    .or_insert_with(Vec::new)
                    .push(item.term);
            }

            // Process each arena's terms together (cache-friendly order)
            // Sort by arena_id to process pages sequentially
            let mut arena_ids: Vec<_> = arena_groups.keys().copied().collect();
            arena_ids.sort();

            for arena_id in arena_ids {
                if let Some(terms) = arena_groups.remove(&arena_id) {
                    for term in terms {
                        if self.remove(&term)? {
                            total_removed += 1;
                        }
                    }
                }
            }
        }

        Ok(total_removed)
    }
}
