//! Arena-aware prefix iteration for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~1156-1791, ~636 LOC) as
//! the twenty-second Phase-5 byte sub-module. These methods power the
//! arena-grouped iteration paths that the merge API relies on for
//! sequential disk I/O:
//!
//! - `navigate_to_prefix_with_arena` — descends the trie to a prefix,
//!   tracking the arena ID of the final node
//! - `collect_terms_with_arena` / `collect_terms_with_values_and_arena`
//!   — DFS collectors that emit `PrefixTermWithArena` records
//! - `iter_prefix_with_arena` / `iter_prefix_with_values_and_arena` —
//!   public entry points that drive the collectors
//!
//! The cursor-based iteration (`iter_prefix_from_cursor` +
//! `collect_terms_from_cursor` + `collect_terms_with_cursor_and_arena`)
//! stays in `dict_impl.rs` until it can be paired with its own
//! sibling module.

use crate::value::DictionaryValue;

use super::block_storage::BlockStorage;
use super::dict_impl::{PersistentARTrie, PrefixTermWithArena, PrefixTermWithValueAndArena, TrieRoot};
use super::error::Result;
use super::transitions::ChildNode;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    // =========================================================================
    // Arena-aware iteration and merge operations
    // =========================================================================

    /// Navigate to a prefix node, returning the child and its arena ID.
    ///
    /// This variant of prefix navigation also tracks the arena ID from the
    /// SwizzledPtr that points to the final node. This is used for page-aware
    /// batch operations.
    ///
    /// # Returns
    ///
    /// - `Ok(Some((child, arena_id)))` - The child at the prefix and its arena location
    /// - `Ok(None)` - The prefix path doesn't exist
    /// - `Err` - An I/O error occurred during lazy loading
    fn navigate_to_prefix_with_arena(
        &self,
        prefix: &[u8],
    ) -> Result<Option<(&ChildNode, Option<u32>)>> {
        if prefix.is_empty() {
            // Empty prefix means the root - root has no incoming pointer
            return match &self.root {
                TrieRoot::Bucket(_) => Ok(None), // Can't return ChildNode for root bucket
                TrieRoot::ArtNode { children: _, .. } => {
                    // For empty prefix on ART root, return first child if any
                    // This is a special case - we can't return ChildNode for root itself
                    Ok(None)
                }
            };
        }

        match &self.root {
            TrieRoot::Bucket(_) => {
                // Root bucket doesn't have individual prefix navigation
                Ok(None)
            }
            TrieRoot::ArtNode { children, .. } => {
                let first_byte = prefix[0];
                let remaining = &prefix[1..];

                // Find child for first byte
                let child_entry = children.iter().find(|(b, _)| *b == first_byte);
                let (child, mut current_arena) = match child_entry {
                    Some((_, child)) => {
                        let arena = match child {
                            ChildNode::DiskRef { ptr } => {
                                ptr.as_arena_slot().map(|s| s.arena_id)
                            }
                            _ => None,
                        };
                        (child, arena)
                    }
                    None => return Ok(None),
                };

                // Navigate through remaining bytes
                let mut current = child;
                for &byte in remaining {
                    match current {
                        ChildNode::Bucket(_) => {
                            // Can't navigate further into bucket
                            return Ok(None);
                        }
                        ChildNode::ArtNode { children, .. } => {
                            let next = children.iter().find(|(b, _)| *b == byte);
                            match next {
                                Some((_, next_child)) => {
                                    current_arena = match next_child {
                                        ChildNode::DiskRef { ptr } => {
                                            ptr.as_arena_slot().map(|s| s.arena_id)
                                        }
                                        _ => None,
                                    };
                                    current = next_child;
                                }
                                None => return Ok(None),
                            }
                        }
                        ChildNode::DiskRef { ptr: _ } => {
                            // Would need to load from disk - not yet implemented
                            // For now, return what we have
                            return Ok(Some((current, current_arena)));
                        }
                    }
                }

                Ok(Some((current, current_arena)))
            }
        }
    }

    /// Collect terms with arena information for page-aware batch operations.
    ///
    /// This method traverses the subtree and collects terms along with their
    /// disk arena location. This enables grouping operations by arena for
    /// improved I/O locality.
    ///
    /// # Arguments
    ///
    /// * `child` - The subtree root to collect from
    /// * `prefix` - The prefix bytes leading to this node
    /// * `current_arena` - Arena ID from the parent's SwizzledPtr to this node
    /// * `terms` - Output vector for collected terms with arena info
    /// * `limit` - Maximum number of terms to collect
    ///
    /// # Returns
    ///
    /// `Ok(true)` if the limit was reached, `Ok(false)` otherwise.
    fn collect_terms_with_arena(
        &self,
        child: &ChildNode,
        prefix: Vec<u8>,
        current_arena: Option<u32>,
        terms: &mut Vec<PrefixTermWithArena>,
        limit: usize,
    ) -> Result<bool> {
        if terms.len() >= limit {
            return Ok(true);
        }

        match child {
            ChildNode::Bucket(bucket) => {
                // Iterate through bucket entries
                for i in 0..bucket.len() {
                    if terms.len() >= limit {
                        return Ok(true);
                    }
                    if let Some(entry) = bucket.get_entry(i) {
                        let suffix = bucket.get_suffix(&entry);
                        let mut term = prefix.clone();
                        term.extend_from_slice(suffix);
                        terms.push(PrefixTermWithArena {
                            term,
                            arena_id: current_arena,
                        });
                    }
                }
            }
            ChildNode::ArtNode {
                is_final,
                children,
                ..
            } => {
                // If this node is final, record the term
                if *is_final {
                    terms.push(PrefixTermWithArena {
                        term: prefix.clone(),
                        arena_id: current_arena,
                    });
                    if terms.len() >= limit {
                        return Ok(true);
                    }
                }

                // Recurse into children
                for (edge, child) in children {
                    let mut child_prefix = prefix.clone();
                    child_prefix.push(*edge);

                    let child_arena = match child {
                        ChildNode::DiskRef { ptr } => {
                            ptr.as_arena_slot().map(|s| s.arena_id)
                        }
                        _ => None,
                    };

                    if self.collect_terms_with_arena(
                        child,
                        child_prefix,
                        child_arena,
                        terms,
                        limit,
                    )? {
                        return Ok(true);
                    }
                }
            }
            ChildNode::DiskRef { ptr } => {
                // Resolve the disk reference and recurse into it
                if let Some(disk_location) = ptr.disk_location() {
                    let child_arena = ptr.as_arena_slot().map(|s| s.arena_id);
                    if let Ok(resolved) = self.resolve_disk_ref(&disk_location) {
                        if self.collect_terms_with_arena(
                            &resolved,
                            prefix,
                            child_arena,
                            terms,
                            limit,
                        )? {
                            return Ok(true);
                        }
                    }
                }
            }
        }

        Ok(false)
    }

    /// Collect terms with their values and arena locations.
    ///
    /// This method performs a DFS traversal, recording each final node's term,
    /// value, and the arena where it resides. Used for page-locality optimized
    /// merge operations.
    fn collect_terms_with_values_and_arena(
        &self,
        child: &ChildNode,
        prefix: Vec<u8>,
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

        match child {
            ChildNode::Bucket(bucket) => {
                // Iterate through bucket entries
                for i in 0..bucket.len() {
                    if terms.len() >= limit {
                        return Ok(true);
                    }
                    if let Some(entry) = bucket.get_entry(i) {
                        let suffix = bucket.get_suffix(&entry);
                        let mut term = prefix.clone();
                        term.extend_from_slice(suffix);

                        // Deserialize value from bucket
                        if let Some(value_bytes) = bucket.get_value(&entry) {
                            if let Ok(value) = bincode::deserialize::<V>(value_bytes) {
                                terms.push(PrefixTermWithValueAndArena {
                                    term,
                                    value,
                                    arena_id: current_arena,
                                });
                            }
                        }
                    }
                }
            }
            ChildNode::ArtNode {
                is_final,
                value,
                children,
                ..
            } => {
                // If this node is final with a value, record it
                if *is_final {
                    if let Some(value_bytes) = value {
                        // Deserialize the value from bytes
                        if let Ok(v) = bincode::deserialize::<V>(value_bytes) {
                            terms.push(PrefixTermWithValueAndArena {
                                term: prefix.clone(),
                                value: v,
                                arena_id: current_arena,
                            });
                            if terms.len() >= limit {
                                return Ok(true);
                            }
                        }
                    }
                }

                // Recurse into children
                for (edge, child) in children {
                    let mut child_prefix = prefix.clone();
                    child_prefix.push(*edge);

                    let child_arena = match child {
                        ChildNode::DiskRef { ptr } => {
                            ptr.as_arena_slot().map(|s| s.arena_id)
                        }
                        _ => None,
                    };

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
            }
            ChildNode::DiskRef { ptr } => {
                // Resolve the disk reference and recurse into it
                if let Some(disk_location) = ptr.disk_location() {
                    let child_arena = ptr.as_arena_slot().map(|s| s.arena_id);
                    if let Ok(resolved) = self.resolve_disk_ref(&disk_location) {
                        if self.collect_terms_with_values_and_arena(
                            &resolved,
                            prefix.clone(),
                            child_arena,
                            terms,
                            limit,
                        )? {
                            return Ok(true);
                        }
                    }
                }
            }
        }

        Ok(false)
    }

    /// Iterate over all terms with the given prefix, including arena locations.
    ///
    /// Returns all terms matching the prefix along with their disk arena IDs.
    /// This enables page-aware batch operations by grouping terms by arena.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The byte prefix to search for
    ///
    /// # Returns
    ///
    /// - `Ok(Some(vec))` - Vector of terms with arena info
    /// - `Ok(None)` - The prefix path doesn't exist
    /// - `Err` - An I/O error occurred
    pub fn iter_prefix_with_arena(
        &self,
        prefix: &[u8],
    ) -> Result<Option<Vec<PrefixTermWithArena>>> {
        const DEFAULT_LIMIT: usize = 100_000;

        match &self.root {
            TrieRoot::Bucket(bucket) => {
                // For root bucket, collect matching entries
                let mut terms = Vec::new();
                for i in 0..bucket.len() {
                    if let Some(entry) = bucket.get_entry(i) {
                        let suffix = bucket.get_suffix(&entry);
                        if suffix.starts_with(prefix) {
                            terms.push(PrefixTermWithArena {
                                term: suffix.to_vec(),
                                arena_id: None, // Root bucket is in-memory
                            });
                        }
                    }
                }
                if terms.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(terms))
                }
            }
            TrieRoot::ArtNode {
                is_final,
                children,
                ..
            } => {
                let mut terms = Vec::new();

                if prefix.is_empty() {
                    // Empty prefix - collect all terms
                    if *is_final {
                        terms.push(PrefixTermWithArena {
                            term: Vec::new(),
                            arena_id: None,
                        });
                    }

                    for (edge, child) in children {
                        let child_arena = match child {
                            ChildNode::DiskRef { ptr } => {
                                ptr.as_arena_slot().map(|s| s.arena_id)
                            }
                            _ => None,
                        };

                        self.collect_terms_with_arena(
                            child,
                            vec![*edge],
                            child_arena,
                            &mut terms,
                            DEFAULT_LIMIT,
                        )?;
                    }
                } else {
                    // Navigate to prefix and collect from there
                    let first_byte = prefix[0];
                    let remaining = &prefix[1..];

                    let child_entry = children.iter().find(|(b, _)| *b == first_byte);
                    if let Some((_, child)) = child_entry {
                        let child_arena = match child {
                            ChildNode::DiskRef { ptr } => {
                                ptr.as_arena_slot().map(|s| s.arena_id)
                            }
                            _ => None,
                        };

                        // Navigate through remaining prefix
                        let mut current = child;
                        let mut current_arena = child_arena;
                        let mut path = vec![first_byte];

                        for &byte in remaining {
                            match current {
                                ChildNode::ArtNode { children, .. } => {
                                    let next = children.iter().find(|(b, _)| *b == byte);
                                    match next {
                                        Some((_, next_child)) => {
                                            current_arena = match next_child {
                                                ChildNode::DiskRef { ptr } => {
                                                    ptr.as_arena_slot().map(|s| s.arena_id)
                                                }
                                                _ => None,
                                            };
                                            current = next_child;
                                            path.push(byte);
                                        }
                                        None => return Ok(None),
                                    }
                                }
                                ChildNode::Bucket(bucket) => {
                                    // Check if remaining prefix exists in bucket
                                    let search_suffix = &prefix[path.len()..];
                                    for i in 0..bucket.len() {
                                        if let Some(entry) = bucket.get_entry(i) {
                                            let suffix = bucket.get_suffix(&entry);
                                            if suffix.starts_with(search_suffix) {
                                                let mut term = path.clone();
                                                term.extend_from_slice(suffix);
                                                terms.push(PrefixTermWithArena {
                                                    term,
                                                    arena_id: current_arena,
                                                });
                                            }
                                        }
                                    }
                                    return if terms.is_empty() {
                                        Ok(None)
                                    } else {
                                        Ok(Some(terms))
                                    };
                                }
                                ChildNode::DiskRef { .. } => {
                                    // Would need lazy loading - not yet implemented
                                    return Ok(None);
                                }
                            }
                        }

                        // Collect all terms under the prefix
                        self.collect_terms_with_arena(
                            current,
                            path,
                            current_arena,
                            &mut terms,
                            DEFAULT_LIMIT,
                        )?;
                    }
                }

                if terms.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(terms))
                }
            }
        }
    }

    /// Iterate over all terms with values and arena locations for the given prefix.
    ///
    /// Returns all (term, value, arena_id) tuples matching the prefix.
    /// This enables page-locality optimized merge operations.
    pub fn iter_prefix_with_values_and_arena(
        &self,
        prefix: &[u8],
    ) -> Result<Option<Vec<PrefixTermWithValueAndArena<V>>>>
    where
        V: Clone,
    {
        const DEFAULT_LIMIT: usize = 100_000;

        match &self.root {
            TrieRoot::Bucket(bucket) => {
                // For root bucket, collect matching entries with values
                let mut terms = Vec::new();
                for i in 0..bucket.len() {
                    if let Some(entry) = bucket.get_entry(i) {
                        let suffix = bucket.get_suffix(&entry);
                        if suffix.starts_with(prefix) {
                            if let Some(value_bytes) = bucket.get_value(&entry) {
                                if let Ok(value) = bincode::deserialize::<V>(value_bytes) {
                                    terms.push(PrefixTermWithValueAndArena {
                                        term: suffix.to_vec(),
                                        value,
                                        arena_id: None,
                                    });
                                }
                            }
                        }
                    }
                }
                if terms.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(terms))
                }
            }
            TrieRoot::ArtNode {
                is_final,
                value,
                children,
                ..
            } => {
                let mut terms = Vec::new();

                if prefix.is_empty() {
                    // Empty prefix - collect all terms
                    if *is_final {
                        if let Some(v) = value {
                            terms.push(PrefixTermWithValueAndArena {
                                term: Vec::new(),
                                value: v.clone(),
                                arena_id: None,
                            });
                        }
                    }

                    for (edge, child) in children {
                        let child_arena = match child {
                            ChildNode::DiskRef { ptr } => {
                                ptr.as_arena_slot().map(|s| s.arena_id)
                            }
                            _ => None,
                        };

                        self.collect_terms_with_values_and_arena(
                            child,
                            vec![*edge],
                            child_arena,
                            &mut terms,
                            DEFAULT_LIMIT,
                        )?;
                    }
                } else {
                    // Navigate to prefix and collect from there
                    let first_byte = prefix[0];
                    let remaining = &prefix[1..];

                    let child_entry = children.iter().find(|(b, _)| *b == first_byte);
                    if let Some((_, child)) = child_entry {
                        let child_arena = match child {
                            ChildNode::DiskRef { ptr } => {
                                ptr.as_arena_slot().map(|s| s.arena_id)
                            }
                            _ => None,
                        };

                        // Navigate through remaining prefix
                        let mut current = child;
                        let mut current_arena = child_arena;
                        let mut path = vec![first_byte];

                        for &byte in remaining {
                            match current {
                                ChildNode::ArtNode { children, .. } => {
                                    let next = children.iter().find(|(b, _)| *b == byte);
                                    match next {
                                        Some((_, next_child)) => {
                                            current_arena = match next_child {
                                                ChildNode::DiskRef { ptr } => {
                                                    ptr.as_arena_slot().map(|s| s.arena_id)
                                                }
                                                _ => None,
                                            };
                                            current = next_child;
                                            path.push(byte);
                                        }
                                        None => return Ok(None),
                                    }
                                }
                                ChildNode::Bucket(bucket) => {
                                    // Check if remaining prefix exists in bucket
                                    let search_suffix = &prefix[path.len()..];
                                    for i in 0..bucket.len() {
                                        if let Some(entry) = bucket.get_entry(i) {
                                            let suffix = bucket.get_suffix(&entry);
                                            if suffix.starts_with(search_suffix) {
                                                if let Some(value_bytes) = bucket.get_value(&entry) {
                                                    if let Ok(value) = bincode::deserialize::<V>(value_bytes) {
                                                        let mut term = path.clone();
                                                        term.extend_from_slice(suffix);
                                                        terms.push(PrefixTermWithValueAndArena {
                                                            term,
                                                            value,
                                                            arena_id: current_arena,
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    return if terms.is_empty() {
                                        Ok(None)
                                    } else {
                                        Ok(Some(terms))
                                    };
                                }
                                ChildNode::DiskRef { .. } => {
                                    // Would need lazy loading - not yet implemented
                                    return Ok(None);
                                }
                            }
                        }

                        // Collect all terms under the prefix
                        self.collect_terms_with_values_and_arena(
                            current,
                            path,
                            current_arena,
                            &mut terms,
                            DEFAULT_LIMIT,
                        )?;
                    }
                }

                if terms.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(terms))
                }
            }
        }
    }
}
