//! Read-path query implementation for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~784-1006, ~223 LOC) as
//! the twenty-fifth Phase-5 byte sub-module. These methods form the
//! `contains` / `get_value` query path:
//!
//! - `contains_impl` (pub(super); called from `shared_trait_impl` +
//!   `dictionary_traits` + `atomic_ops`)
//! - `get_value_impl` (pub(super); called from `shared_trait_impl` +
//!   `dictionary_traits` + `atomic_ops` + `document_tx` + `merge_api`)
//! - `get_value_in_child` / `get_value_in_child_with_depth` (private
//!   recursive helpers)
//! - `contains_in_child` / `contains_in_child_with_depth` (private
//!   recursive helpers)
//!
//! Depth-bounded prefetching is preserved across the move.

use crate::value::DictionaryValue;

use super::block_storage::BlockStorage;
use super::dict_impl::{PersistentARTrie, TrieRoot};
use super::transitions::ChildNode;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Check if a term is contained in the dictionary.
    ///
    /// This method handles:
    /// - Bucket root lookups
    /// - ART node traversal
    /// - Lazy loading of DiskRef children
    /// - Multi-level prefetching of sibling nodes for better I/O performance
    pub(super) fn contains_impl(&self, term: &[u8]) -> bool {
        match &self.root {
            TrieRoot::Bucket(bucket) => bucket.contains(term),
            TrieRoot::ArtNode {
                children,
                is_final,
                ..
            } => {
                if term.is_empty() {
                    return *is_final;
                }

                let first_byte = term[0];
                let remaining = &term[1..];

                // Prefetch DiskRef children at the root level (depth 0)
                self.prefetch_disk_refs_bounded(children, 0);

                // Find child with matching first byte
                for (b, child) in children {
                    if *b == first_byte {
                        return self.contains_in_child_with_depth(child, remaining, 1);
                    }
                }
                false
            }
        }
    }

    /// Get the value associated with a term.
    ///
    /// Returns `Some(value)` if the term exists and has an associated value,
    /// `None` if the term doesn't exist or has no value.
    ///
    /// Uses multi-level prefetching for better I/O performance on disk-resident tries.
    ///
    /// `pub(super)` for the parallel-merge extension trait (see also
    /// `insert_impl` above).
    pub(super) fn get_value_impl(&self, term: &[u8]) -> Option<V> {
        match &self.root {
            TrieRoot::Bucket(bucket) => {
                // Search for the term in the bucket
                match bucket.search(term) {
                    Ok(idx) => {
                        // Found the term, get its value
                        if let Some(entry) = bucket.get_entry(idx) {
                            if let Some(value_bytes) = bucket.get_value(&entry) {
                                // Deserialize the value
                                bincode::deserialize(value_bytes).ok()
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    Err(_) => None, // Term not found
                }
            }
            TrieRoot::ArtNode {
                children,
                is_final,
                value,
                ..
            } => {
                if term.is_empty() {
                    if *is_final {
                        return value.clone();
                    }
                    return None;
                }

                let first_byte = term[0];
                let remaining = &term[1..];

                // Prefetch DiskRef children at the root level (depth 0)
                self.prefetch_disk_refs_bounded(children, 0);

                // Find child with matching first byte
                for (b, child) in children {
                    if *b == first_byte {
                        return self.get_value_in_child_with_depth(child, remaining, 1);
                    }
                }
                None
            }
        }
    }

    /// Get value from a child node.
    fn get_value_in_child(&self, child: &ChildNode, remaining: &[u8]) -> Option<V> {
        self.get_value_in_child_with_depth(child, remaining, 0)
    }

    /// Get value from a child node with depth tracking for multi-level prefetching.
    ///
    /// # Arguments
    ///
    /// * `child` - The child node to search
    /// * `remaining` - The remaining term bytes to match
    /// * `depth` - Current traversal depth (increments with each level)
    fn get_value_in_child_with_depth(&self, child: &ChildNode, remaining: &[u8], depth: u16) -> Option<V> {
        match child {
            ChildNode::Bucket(bucket) => {
                match bucket.search(remaining) {
                    Ok(idx) => {
                        if let Some(entry) = bucket.get_entry(idx) {
                            if let Some(value_bytes) = bucket.get_value(&entry) {
                                bincode::deserialize(value_bytes).ok()
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    Err(_) => None,
                }
            }
            ChildNode::ArtNode {
                is_final,
                children,
                value,
                ..
            } => {
                if remaining.is_empty() {
                    if *is_final {
                        // Deserialize value from stored bytes
                        return value.as_ref().and_then(|bytes| {
                            bincode::deserialize(bytes).ok()
                        });
                    }
                    return None;
                }

                let first_byte = remaining[0];
                let rest = &remaining[1..];

                // Multi-level prefetch with depth bounds
                self.prefetch_disk_refs_bounded(children, depth);

                for (b, grandchild) in children {
                    if *b == first_byte {
                        return self.get_value_in_child_with_depth(grandchild, rest, depth + 1);
                    }
                }
                None
            }
            ChildNode::DiskRef { ptr } => {
                // Lazy load from disk and get value
                if let Some(disk_location) = ptr.disk_location() {
                    if let Ok(resolved) = self.resolve_disk_ref(&disk_location) {
                        return self.get_value_in_child_with_depth(&resolved, remaining, depth);
                    }
                }
                None
            }
        }
    }

    /// Check if remaining term is contained in a child node.
    ///
    /// Handles all child node types including lazy loading of DiskRef.
    /// Uses prefetcher to read-ahead sibling nodes for better I/O performance.
    fn contains_in_child(&self, child: &ChildNode, remaining: &[u8]) -> bool {
        self.contains_in_child_with_depth(child, remaining, 0)
    }

    /// Check if remaining term is contained in a child node with depth tracking.
    ///
    /// This internal method tracks traversal depth for multi-level prefetching.
    /// The depth parameter enables the prefetcher to limit prefetching at deep
    /// levels to avoid excessive I/O for very deep tries.
    ///
    /// # Arguments
    ///
    /// * `child` - The child node to search
    /// * `remaining` - The remaining term bytes to match
    /// * `depth` - Current traversal depth (increments with each level)
    fn contains_in_child_with_depth(&self, child: &ChildNode, remaining: &[u8], depth: u16) -> bool {
        match child {
            ChildNode::Bucket(bucket) => bucket.contains(remaining),
            ChildNode::ArtNode {
                is_final,
                children,
                ..
            } => {
                if remaining.is_empty() {
                    return *is_final;
                }

                let first_byte = remaining[0];
                let rest = &remaining[1..];

                // Multi-level prefetch with depth bounds
                self.prefetch_disk_refs_bounded(children, depth);

                // Recursively search in children with incremented depth
                for (b, child) in children {
                    if *b == first_byte {
                        return self.contains_in_child_with_depth(child, rest, depth + 1);
                    }
                }
                false
            }
            ChildNode::DiskRef { ptr } => {
                // Lazy load from disk
                if let Some(disk_location) = ptr.disk_location() {
                    if let Ok(resolved) = self.resolve_disk_ref(&disk_location) {
                        return self.contains_in_child_with_depth(&resolved, remaining, depth);
                    }
                }
                false
            }
        }
    }
}
