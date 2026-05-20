//! Prefix navigation + term collection helpers for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~343-694, ~352 LOC)
//! as a Phase-6 char sub-module. Methods covered:
//!
//! - `navigate_to_prefix` — descend the trie to a string prefix
//! - `navigate_to_prefix_with_arena` — arena-tracking variant
//! - `collect_terms_under_node` / `collect_terms_under_node_limited`
//! - `collect_terms_with_values_under_node`
//! - `collect_terms_with_arena` / `collect_terms_with_values_and_arena`
//!
//! These are pub(super) so the iter_prefix* and merge_from* APIs
//! (which remain in dict_impl_char.rs) can drive them.

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::Result;
use crate::value::DictionaryValue;

use super::types::{CharTrieNodeInner, CharTrieRoot};
use super::prefix_term::{PrefixTermWithArena, PrefixTermWithValueAndArena};

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    pub(super) fn navigate_to_prefix(&self, prefix: &str) -> Result<Option<&CharTrieNodeInner<V>>> {
        let root = match &self.root {
            CharTrieRoot::Node(node) => node.as_ref(),
            CharTrieRoot::Empty => return Ok(None),
        };

        let mut current = root;
        let mut depth = 0u16;
        for c in prefix.chars() {
            // Prefetch siblings before descending (multi-level prefetch)
            self.prefetch_disk_refs_bounded(current.node.iter_children(), depth);

            match self.get_child_lazy(current, c)? {
                Some(child) => {
                    current = child;
                    depth = depth.saturating_add(1);
                }
                None => return Ok(None),
            }
        }

        Ok(Some(current))
    }

    /// Navigate to the node at a given prefix, also returning arena info.
    ///
    /// This variant of `navigate_to_prefix` also tracks the arena ID from the
    /// SwizzledPtr that points to the final node. This is used for page-aware
    /// batch operations.
    ///
    /// For disk-backed tries, prefetches children at each level for improved I/O performance.
    ///
    /// # Returns
    ///
    /// - `Ok(Some((node, arena_id)))` - The node at the prefix and its arena location
    /// - `Ok(None)` - The prefix path doesn't exist
    /// - `Err` - An I/O error occurred during lazy loading
    pub(super) fn navigate_to_prefix_with_arena(
        &self,
        prefix: &str,
    ) -> Result<Option<(&CharTrieNodeInner<V>, Option<u32>)>> {
        let root = match &self.root {
            CharTrieRoot::Node(node) => node.as_ref(),
            CharTrieRoot::Empty => return Ok(None),
        };

        let mut current = root;
        let mut current_arena: Option<u32> = None; // Root has no incoming pointer
        let mut depth = 0u16;

        for c in prefix.chars() {
            // Prefetch siblings before descending (multi-level prefetch)
            self.prefetch_disk_refs_bounded(current.node.iter_children(), depth);

            // Get the SwizzledPtr to extract arena info
            match current.node.find_child(c as u32) {
                Some(ptr) => {
                    if ptr.is_null() {
                        return Ok(None);
                    }
                    // Extract arena from the pointer leading to this child
                    current_arena = ptr.as_arena_slot().map(|slot| slot.arena_id);
                    // Resolve to get the actual node reference
                    current = self.resolve_swizzled_ptr(ptr)?;
                    depth = depth.saturating_add(1);
                }
                None => return Ok(None),
            }
        }

        Ok(Some((current, current_arena)))
    }

    /// Collect all terms under a node via DFS traversal.
    ///
    /// This method eagerly collects terms. For memory efficiency when dealing
    /// with large subtrees, use `iter_prefix` with batched processing instead.
    ///
    /// Note: This method properly resolves DiskRef children via `resolve_swizzled_ptr`,
    /// ensuring all terms are collected even after checkpoint.
    pub(super) fn collect_terms_under_node(
        &self,
        node: &CharTrieNodeInner<V>,
        prefix: String,
        terms: &mut Vec<String>,
    ) -> Result<()> {
        // If this node is a final state, add the current prefix as a term
        if node.is_final() {
            terms.push(prefix.clone());
        }

        // Recursively traverse children, resolving disk refs as needed
        for (key, child_ptr) in node.node.iter_children() {
            if child_ptr.is_null() {
                continue;
            }

            let c = char::from_u32(key).unwrap_or('\u{FFFD}');
            let mut child_prefix = prefix.clone();
            child_prefix.push(c);

            // Resolve the child node (handles both in-memory and disk-backed)
            let child = self.resolve_swizzled_ptr(child_ptr)?;
            self.collect_terms_under_node(child, child_prefix, terms)?;
        }

        Ok(())
    }

    /// Collect terms under a node with a limit for batched processing.
    ///
    /// Stops collecting after `limit` terms have been found.
    ///
    /// Note: This method properly resolves DiskRef children via `resolve_swizzled_ptr`,
    /// ensuring all terms are collected even after checkpoint.
    pub(super) fn collect_terms_under_node_limited(
        &self,
        node: &CharTrieNodeInner<V>,
        prefix: String,
        terms: &mut Vec<String>,
        limit: usize,
    ) -> Result<bool> {
        if terms.len() >= limit {
            return Ok(true); // Signal that we're full
        }

        // If this node is a final state, add the current prefix as a term
        if node.is_final() {
            terms.push(prefix.clone());
            if terms.len() >= limit {
                return Ok(true);
            }
        }

        // Recursively traverse children, resolving disk refs as needed
        for (key, child_ptr) in node.node.iter_children() {
            if child_ptr.is_null() {
                continue;
            }

            let c = char::from_u32(key).unwrap_or('\u{FFFD}');
            let mut child_prefix = prefix.clone();
            child_prefix.push(c);

            // Resolve the child node (handles both in-memory and disk-backed)
            let child = self.resolve_swizzled_ptr(child_ptr)?;
            if self.collect_terms_under_node_limited(child, child_prefix, terms, limit)? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Collect terms with values under a node.
    ///
    /// Note: This method properly resolves DiskRef children via `resolve_swizzled_ptr`,
    /// ensuring all terms are collected even after checkpoint.
    pub(super) fn collect_terms_with_values_under_node(
        &self,
        node: &CharTrieNodeInner<V>,
        prefix: String,
        terms: &mut Vec<(String, V)>,
    ) -> Result<()>
    where
        V: Clone,
    {
        // If this node is a final state with a value, add it
        if node.is_final() {
            if let Some(value) = &node.value {
                terms.push((prefix.clone(), value.clone()));
            }
        }

        // Recursively traverse children, resolving disk refs as needed
        for (key, child_ptr) in node.node.iter_children() {
            if child_ptr.is_null() {
                continue;
            }

            let c = char::from_u32(key).unwrap_or('\u{FFFD}');
            let mut child_prefix = prefix.clone();
            child_prefix.push(c);

            // Resolve the child node (handles both in-memory and disk-backed)
            let child = self.resolve_swizzled_ptr(child_ptr)?;
            self.collect_terms_with_values_under_node(child, child_prefix, terms)?;
        }

        Ok(())
    }

    /// Collect terms with arena information for page-aware batch operations.
    ///
    /// This method traverses the subtree and collects terms along with their
    /// disk arena location (extracted from parent SwizzledPtrs). This enables
    /// grouping removals by arena for improved I/O locality.
    ///
    /// # Arguments
    ///
    /// * `node` - The subtree root to collect from
    /// * `prefix` - The prefix string leading to this node
    /// * `current_arena` - Arena ID from the parent's SwizzledPtr to this node
    /// * `terms` - Output vector for collected terms with arena info
    /// * `limit` - Maximum number of terms to collect
    ///
    /// # Returns
    ///
    /// `Ok(true)` if the limit was reached, `Ok(false)` otherwise.
    pub(super) fn collect_terms_with_arena(
        &self,
        node: &CharTrieNodeInner<V>,
        prefix: String,
        current_arena: Option<u32>,
        terms: &mut Vec<PrefixTermWithArena>,
        limit: usize,
    ) -> Result<bool> {
        if terms.len() >= limit {
            return Ok(true);
        }

        // If this node is a final state, record the term with its arena location
        if node.is_final() {
            terms.push(PrefixTermWithArena {
                term: prefix.clone(),
                arena_id: current_arena,
            });
            if terms.len() >= limit {
                return Ok(true);
            }
        }

        // Traverse children, extracting arena from each child's SwizzledPtr
        for (key, child_ptr) in node.node.iter_children() {
            if child_ptr.is_null() {
                continue;
            }

            // Extract arena from the SwizzledPtr pointing to this child
            let child_arena = child_ptr.as_arena_slot().map(|slot| slot.arena_id);

            // Build the child prefix
            let mut child_prefix = prefix.clone();
            child_prefix.push(char::from_u32(key).unwrap_or('\u{FFFD}'));

            // Resolve the pointer to get the child node
            let child = self.resolve_swizzled_ptr(child_ptr)?;

            // Recurse with the child's arena info
            if self.collect_terms_with_arena(child, child_prefix, child_arena, terms, limit)? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Collect terms with their values and arena locations under the given node.
    ///
    /// This method performs a DFS traversal, recording each final node's term,
    /// value, and the arena where it resides. Used for page-locality optimized
    /// merge operations.
    ///
    /// # Arguments
    ///
    /// * `node` - The node to start collection from
    /// * `prefix` - The prefix string accumulated so far
    /// * `current_arena` - The arena ID where the current node resides
    /// * `terms` - Output vector to collect terms with values and arena info
    /// * `limit` - Maximum number of terms to collect
    ///
    /// # Returns
    ///
    /// `Ok(true)` if the limit was reached, `Ok(false)` otherwise.
    pub(super) fn collect_terms_with_values_and_arena(
        &self,
        node: &CharTrieNodeInner<V>,
        prefix: String,
        current_arena: Option<u32>,
        terms: &mut Vec<PrefixTermWithValueAndArena<V>>,
        limit: usize,
    ) -> Result<bool>
    where
        V: Clone,
    {
        if terms.len() >= limit {
            return Ok(true);
        }

        // If this node is a final state with a value, record it with arena location
        if node.is_final() {
            if let Some(value) = &node.value {
                terms.push(PrefixTermWithValueAndArena {
                    term: prefix.clone(),
                    value: value.clone(),
                    arena_id: current_arena,
                });
                if terms.len() >= limit {
                    return Ok(true);
                }
            }
        }

        // Traverse children, extracting arena from each child's SwizzledPtr
        for (key, child_ptr) in node.node.iter_children() {
            if child_ptr.is_null() {
                continue;
            }

            // Extract arena from the SwizzledPtr pointing to this child
            let child_arena = child_ptr.as_arena_slot().map(|slot| slot.arena_id);

            // Build the child prefix
            let mut child_prefix = prefix.clone();
            child_prefix.push(char::from_u32(key).unwrap_or('\u{FFFD}'));

            // Resolve the pointer to get the child node
            let child = self.resolve_swizzled_ptr(child_ptr)?;

            // Recurse with the child's arena info
            if self.collect_terms_with_values_and_arena(
                child,
                child_prefix,
                child_arena,
                terms,
                limit,
            )? {
                return Ok(true);
            }
        }

        Ok(false)
    }
}
