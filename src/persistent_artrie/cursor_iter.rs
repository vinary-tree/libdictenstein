//! Cursor-based prefix iteration for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~1158-1413, ~256 LOC) as
//! the twenty-third Phase-5 byte sub-module. These methods power
//! memory-bounded batched iteration used by `merge_api`'s batched
//! merge paths:
//!
//! - `iter_prefix_from_cursor` (pub) — returns up to `limit` terms with
//!   their values + arena IDs, starting strictly after `cursor`
//! - `collect_terms_from_cursor` (private DFS collector)
//! - `collect_terms_with_cursor_and_arena` (recursive cursor walker)

use smallvec::SmallVec;

use crate::value::DictionaryValue;

use super::block_storage::BlockStorage;
use super::dict_impl::{
    bytes_gt, bytes_le, PersistentARTrie, PrefixTermWithValueAndArena, TrieRoot,
};
use super::error::Result;
use super::transitions::ChildNode;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Iterate terms with values starting from a cursor position.
    ///
    /// This method enables memory-bounded iteration by returning terms in batches.
    /// The cursor allows resuming iteration from where the previous batch ended.
    ///
    /// # Arguments
    ///
    /// * `prefix` - Only return terms starting with this prefix
    /// * `cursor` - If Some, skip terms <= cursor (exclusive lower bound)
    /// * `limit` - Maximum number of terms to return
    ///
    /// # Returns
    ///
    /// A vector of terms (sorted lexicographically) starting after the cursor,
    /// up to the specified limit.
    pub fn iter_prefix_from_cursor(
        &self,
        prefix: &[u8],
        cursor: Option<&[u8]>,
        limit: usize,
    ) -> Result<Vec<PrefixTermWithValueAndArena<V>>>
    where
        V: Clone,
    {
        // **M3 read-flip (C6).** This is the memory-bounded merge-read chokepoint
        // (the batched merges + the parallel merge funnel through it). Under
        // `route_overlay()` enumerate the prefix from the VALUE-CARRYING overlay
        // (non-faulting, resident-finals; `arena_id` None), then apply the same
        // cursor (exclusive `> cursor`) + limit the owned collector applies. The
        // value-carrying route satisfies the audit §C.2 rule (no owned value
        // re-read). The owned arm below is the verbatim pre-flip cursor walk.
        if self.route_overlay() {
            let mut entries: Vec<PrefixTermWithValueAndArena<V>> = self
                .overlay_iter_prefix_with_values(prefix)
                .unwrap_or_default()
                .into_iter()
                .filter(|(term, _)| match cursor {
                    Some(c) => bytes_gt(term.as_slice(), c),
                    None => true,
                })
                .map(|(term, value)| PrefixTermWithValueAndArena {
                    term,
                    value,
                    arena_id: None,
                })
                .collect();
            entries.sort_by(|a, b| a.term.cmp(&b.term));
            entries.truncate(limit);
            return Ok(entries);
        }

        let mut terms = Vec::with_capacity(limit);

        // Collect terms with the cursor filtering
        self.collect_terms_from_cursor(prefix, cursor, limit, &mut terms)?;

        Ok(terms)
    }

    /// Helper to collect terms from a cursor position.
    fn collect_terms_from_cursor(
        &self,
        prefix: &[u8],
        cursor: Option<&[u8]>,
        limit: usize,
        terms: &mut Vec<PrefixTermWithValueAndArena<V>>,
    ) -> Result<()>
    where
        V: Clone,
    {
        // F4 (OR read): owned read path takes the inner `root` RwLock for read.
        match &*self.root.read() {
            TrieRoot::Bucket(bucket) => {
                // For root bucket, collect matching entries
                let mut entries: Vec<_> = (0..bucket.len())
                    .filter_map(|i| bucket.get_entry(i))
                    .filter_map(|entry| {
                        let suffix = bucket.get_suffix(&entry);
                        if !suffix.starts_with(prefix) {
                            return None;
                        }
                        // Apply cursor filter using SIMD-accelerated comparison
                        if let Some(c) = cursor {
                            if bytes_le(suffix.as_ref(), c) {
                                return None;
                            }
                        }
                        bucket.get_value(&entry).and_then(|value_bytes| {
                            crate::serialization::bincode_compat::deserialize::<V>(value_bytes)
                                .ok()
                                .map(|value| PrefixTermWithValueAndArena {
                                    term: suffix.to_vec(),
                                    value,
                                    arena_id: None,
                                })
                        })
                    })
                    .collect();

                // Sort for consistent ordering
                entries.sort_by(|a, b| a.term.cmp(&b.term));
                terms.extend(entries.into_iter().take(limit));
            }
            TrieRoot::ArtNode {
                is_final,
                value,
                children,
                ..
            } => {
                // If prefix is empty and we're at root
                if prefix.is_empty() {
                    // Check root node itself
                    if *is_final {
                        if let Some(v) = value {
                            let empty_term = Vec::new();
                            // Apply cursor filter
                            let include = cursor.map_or(true, |c| empty_term.as_slice() > c);
                            if include && terms.len() < limit {
                                terms.push(PrefixTermWithValueAndArena {
                                    term: empty_term,
                                    value: v.clone(),
                                    arena_id: None,
                                });
                            }
                        }
                    }

                    // Collect from children in sorted order
                    let mut sorted_children: Vec<_> = children.iter().collect();
                    sorted_children.sort_by_key(|(b, _)| *b);

                    for (edge, child) in sorted_children {
                        if terms.len() >= limit {
                            break;
                        }

                        let child_arena = match child {
                            ChildNode::DiskRef { ptr } => ptr.as_arena_slot().map(|s| s.arena_id),
                            _ => None,
                        };

                        self.collect_terms_with_cursor_and_arena(
                            child,
                            vec![*edge],
                            cursor,
                            limit,
                            child_arena,
                            terms,
                        )?;
                    }
                } else {
                    // Navigate to prefix first, then collect
                    // This is a simplified version; full implementation would
                    // navigate to prefix and then collect
                    if let Some(all_terms) = self.iter_prefix_with_values_and_arena(prefix)? {
                        let filtered: Vec<_> = all_terms
                            .into_iter()
                            .filter(|t| cursor.map_or(true, |c| bytes_gt(t.term.as_slice(), c)))
                            .take(limit)
                            .collect();
                        terms.extend(filtered);
                    }
                }
            }
        }

        Ok(())
    }

    /// Collect terms from a child node with cursor filtering.
    fn collect_terms_with_cursor_and_arena(
        &self,
        child: &ChildNode,
        path: Vec<u8>,
        cursor: Option<&[u8]>,
        limit: usize,
        arena_id: Option<u32>,
        terms: &mut Vec<PrefixTermWithValueAndArena<V>>,
    ) -> Result<()>
    where
        V: Clone,
    {
        if terms.len() >= limit {
            return Ok(());
        }

        match child {
            ChildNode::Bucket(bucket) => {
                for i in 0..bucket.len() {
                    if terms.len() >= limit {
                        break;
                    }
                    if let Some(entry) = bucket.get_entry(i) {
                        let suffix = bucket.get_suffix(&entry);
                        // Use SmallVec to avoid heap allocation for short paths
                        let mut full_term: SmallVec<[u8; 64]> = SmallVec::from_slice(&path);
                        full_term.extend_from_slice(suffix);

                        // Apply cursor filter using SIMD-accelerated comparison
                        if let Some(c) = cursor {
                            if bytes_le(full_term.as_slice(), c) {
                                continue;
                            }
                        }

                        if let Some(value_bytes) = bucket.get_value(&entry) {
                            if let Ok(value) =
                                crate::serialization::bincode_compat::deserialize::<V>(value_bytes)
                            {
                                terms.push(PrefixTermWithValueAndArena {
                                    term: full_term.into_vec(),
                                    value,
                                    arena_id,
                                });
                            }
                        }
                    }
                }
                // Sort bucket terms
                terms.sort_by(|a, b| a.term.cmp(&b.term));
            }
            ChildNode::ArtNode {
                is_final,
                value,
                children,
                ..
            } => {
                // Check this node's finality
                if *is_final {
                    if let Some(value_bytes) = value {
                        // Deserialize the value from bytes
                        if let Ok(v) =
                            crate::serialization::bincode_compat::deserialize::<V>(value_bytes)
                        {
                            // Apply cursor filter using SIMD-accelerated comparison
                            if cursor.map_or(true, |c| bytes_gt(path.as_slice(), c))
                                && terms.len() < limit
                            {
                                terms.push(PrefixTermWithValueAndArena {
                                    term: path.clone(),
                                    value: v,
                                    arena_id,
                                });
                            }
                        }
                    }
                }

                // Recurse into children in sorted order
                let mut sorted_children: Vec<_> = children.iter().collect();
                sorted_children.sort_by_key(|(b, _)| *b);

                for (edge, child_node) in sorted_children {
                    if terms.len() >= limit {
                        break;
                    }
                    // Use SmallVec to avoid heap allocation for short paths
                    let mut child_path: SmallVec<[u8; 64]> = SmallVec::from_slice(&path);
                    child_path.push(*edge);

                    let child_arena = match child_node {
                        ChildNode::DiskRef { ptr } => ptr.as_arena_slot().map(|s| s.arena_id),
                        _ => arena_id,
                    };

                    self.collect_terms_with_cursor_and_arena(
                        child_node,
                        child_path.into_vec(),
                        cursor,
                        limit,
                        child_arena,
                        terms,
                    )?;
                }
            }
            ChildNode::DiskRef { .. } => {
                // DiskRef children are not loaded in this simple implementation
                // The parent method handles disk-backed nodes through the buffer manager
                // For streaming merge, we skip disk refs (they would be loaded via
                // iter_prefix_with_values_and_arena which handles this)
            }
        }

        Ok(())
    }
}
