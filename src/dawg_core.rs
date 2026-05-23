//! Generic DAWG (Directed Acyclic Word Graph) implementation.
//!
//! This module provides `DawgCore<U, V>`, a generic implementation of a dynamic DAWG
//! that can operate on any character unit type (`u8`, `char`, or `u64`). It serves as
//! the shared foundation for [`DynamicDawg`](crate::dynamic_dawg::DynamicDawg) and
//! [`DynamicDawgChar`](crate::dynamic_dawg_char::DynamicDawgChar).
//!
//! # Design
//!
//! The generic implementation handles:
//! - Node storage with [`SmallVec`]-optimized edges
//! - Suffix caching for memory reduction (20-40% savings)
//! - Incremental minimization via [`NodeSignature`](crate::node_signature::NodeSignature)
//! - Optional Bloom filter for negative lookup rejection
//! - Thread-safe interior mutability via `Arc<RwLock<...>>`
//!
//! Concrete types like `DynamicDawg` are thin wrappers that provide type-specific
//! string conversion and iteration.

use crate::bloom_filter::BloomFilter;
use crate::node_signature::NodeSignature;
use crate::value::DictionaryValue;
use crate::CharUnit;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use std::collections::HashMap;

/// Generic DAWG node that can use any character unit type for edge labels.
///
/// Nodes use `SmallVec` for edges to avoid heap allocation for nodes with ≤4 edges,
/// which is the common case for natural language dictionaries.
#[derive(Clone, Debug)]
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(
    all(feature = "serialization", not(feature = "persistent-artrie")),
    serde(bound(serialize = "U: serde::Serialize, V: serde::Serialize")),
    serde(bound(deserialize = "U: serde::Deserialize<'de>, V: serde::Deserialize<'de>"))
)]
#[cfg_attr(
    all(feature = "serialization", feature = "persistent-artrie"),
    serde(bound(serialize = "U: serde::Serialize, V: serde::Serialize")),
    serde(bound(deserialize = "U: serde::de::DeserializeOwned, V: serde::de::DeserializeOwned"))
)]
pub struct DawgNode<U: CharUnit, V: DictionaryValue> {
    /// Edges to child nodes, sorted by label for binary search
    pub(crate) edges: SmallVec<[(U, usize); 4]>,
    /// Whether this node marks the end of a valid term
    pub(crate) is_final: bool,
    /// Reference count for dynamic deletion (not used for lock-free variant)
    pub(crate) ref_count: usize,
    /// Optional value associated with this node (only for final nodes)
    pub(crate) value: Option<V>,
}

impl<U: CharUnit, V: DictionaryValue> DawgNode<U, V> {
    /// Create a new non-final node with no edges.
    pub fn new(is_final: bool) -> Self {
        DawgNode {
            edges: SmallVec::new(),
            is_final,
            ref_count: 0,
            value: None,
        }
    }

    /// Create a new node with an optional value.
    pub fn new_with_value(is_final: bool, value: Option<V>) -> Self {
        DawgNode {
            edges: SmallVec::new(),
            is_final,
            ref_count: 0,
            value,
        }
    }
}

/// Core DAWG structure that is generic over character unit and value types.
///
/// This is the internal representation used by `DynamicDawg`, `DynamicDawgChar`,
/// and potentially other DAWG variants. It provides all the core DAWG operations
/// without string conversion logic.
#[derive(Debug)]
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(
    all(feature = "serialization", not(feature = "persistent-artrie")),
    serde(bound(serialize = "U: serde::Serialize, V: serde::Serialize")),
    serde(bound(deserialize = "U: serde::Deserialize<'de>, V: serde::Deserialize<'de>"))
)]
#[cfg_attr(
    all(feature = "serialization", feature = "persistent-artrie"),
    serde(bound(serialize = "U: serde::Serialize, V: serde::Serialize")),
    serde(bound(deserialize = "U: serde::de::DeserializeOwned, V: serde::de::DeserializeOwned"))
)]
pub struct DawgCore<U: CharUnit, V: DictionaryValue> {
    /// All nodes in the DAWG. Node 0 is always the root.
    pub(crate) nodes: Vec<DawgNode<U, V>>,
    /// Number of complete terms stored in the DAWG.
    pub(crate) term_count: usize,
    /// Whether compaction is recommended (set after deletions).
    pub(crate) needs_compaction: bool,
    /// Suffix sharing cache: hash of suffix -> node index
    #[cfg_attr(feature = "serialization", serde(skip))]
    pub(crate) suffix_cache: FxHashMap<u64, usize>,
    /// Last node count after minimization (for auto-minimize threshold).
    #[cfg_attr(feature = "serialization", serde(skip))]
    pub(crate) last_minimized_node_count: usize,
    /// Threshold ratio to trigger auto-minimization.
    #[cfg_attr(feature = "serialization", serde(skip))]
    pub(crate) auto_minimize_threshold: f32,
    /// Optional Bloom filter for fast negative lookup.
    #[cfg_attr(feature = "serialization", serde(skip))]
    pub(crate) bloom_filter: Option<BloomFilter>,
}

impl<U: CharUnit, V: DictionaryValue> DawgCore<U, V> {
    /// Create a new empty DAWG core with default settings.
    pub fn new() -> Self {
        Self::with_config(f32::INFINITY, None)
    }

    /// Create a new empty DAWG core with auto-minimize threshold.
    ///
    /// # Arguments
    ///
    /// * `threshold` - Ratio of node growth that triggers minimization.
    ///   Use `f32::INFINITY` to disable.
    pub fn with_auto_minimize_threshold(threshold: f32) -> Self {
        Self::with_config(threshold, None)
    }

    /// Create a new empty DAWG core with full configuration.
    ///
    /// # Arguments
    ///
    /// * `auto_minimize_threshold` - Ratio of node growth that triggers minimization.
    /// * `bloom_filter_capacity` - Optional expected term count for Bloom filter.
    pub fn with_config(auto_minimize_threshold: f32, bloom_filter_capacity: Option<usize>) -> Self {
        let nodes = vec![DawgNode::new(false)]; // Root at index 0
        let bloom_filter = bloom_filter_capacity.map(BloomFilter::new);

        DawgCore {
            nodes,
            term_count: 0,
            needs_compaction: false,
            suffix_cache: FxHashMap::default(),
            last_minimized_node_count: 1,
            auto_minimize_threshold,
            bloom_filter,
        }
    }

    /// Get the number of terms in the DAWG.
    #[inline]
    pub fn term_count(&self) -> usize {
        self.term_count
    }

    /// Get the number of nodes in the DAWG.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Check if compaction is recommended.
    #[inline]
    pub fn needs_compaction(&self) -> bool {
        self.needs_compaction
    }

    /// Insert a unit sequence into the DAWG.
    ///
    /// Returns `true` if the sequence was newly inserted, `false` if it already existed.
    pub fn insert_units(&mut self, units: &[U]) -> bool {
        // Navigate to insertion point, creating nodes as needed
        let mut node_idx = 0;
        let mut path_len = 0;

        for &unit in units {
            if let Some(&child_idx) = self.nodes[node_idx]
                .edges
                .iter()
                .find(|(u, _)| *u == unit)
                .map(|(_, idx)| idx)
            {
                // Edge exists, follow it
                node_idx = child_idx;
                path_len += 1;
            } else {
                // Need to create new suffix
                break;
            }
        }

        // Check if term already exists
        if path_len == units.len() && self.nodes[node_idx].is_final {
            return false; // Already exists
        }

        // Create nodes one by one
        for i in path_len..units.len() {
            let unit = units[i];
            let new_idx = self.nodes.len();
            let is_final = i == units.len() - 1;
            let mut new_node = DawgNode::new(is_final);
            new_node.ref_count = 1;

            self.nodes.push(new_node);
            self.insert_edge_sorted(node_idx, unit, new_idx);
            node_idx = new_idx;
        }

        // Mark as final if we followed existing path
        if path_len == units.len() {
            self.nodes[node_idx].is_final = true;
        }

        self.term_count += 1;
        self.check_and_auto_minimize();
        true
    }

    /// Insert a unit sequence with an associated value.
    ///
    /// Returns `true` if the sequence was newly inserted, `false` if it already existed
    /// (in which case the value is updated).
    pub fn insert_units_with_value(&mut self, units: &[U], value: V) -> bool {
        let mut node_idx = 0;
        let mut path_len = 0;

        for &unit in units {
            if let Some(&child_idx) = self.nodes[node_idx]
                .edges
                .iter()
                .find(|(u, _)| *u == unit)
                .map(|(_, idx)| idx)
            {
                node_idx = child_idx;
                path_len += 1;
            } else {
                break;
            }
        }

        // Check if term already exists
        if path_len == units.len() {
            if self.nodes[node_idx].is_final {
                // Term exists - update value
                self.nodes[node_idx].value = Some(value);
                return false;
            } else {
                // Mark as final and set value
                self.nodes[node_idx].is_final = true;
                self.nodes[node_idx].value = Some(value);
                self.term_count += 1;
                return true;
            }
        }

        // Build remaining suffix
        for i in path_len..units.len() {
            let unit = units[i];
            let new_idx = self.nodes.len();
            let is_final = i == units.len() - 1;

            let mut new_node = if is_final {
                DawgNode::new_with_value(true, Some(value.clone()))
            } else {
                DawgNode::new(false)
            };
            new_node.ref_count = 1;

            self.nodes.push(new_node);
            self.insert_edge_sorted(node_idx, unit, new_idx);
            node_idx = new_idx;
        }

        self.term_count += 1;
        self.check_and_auto_minimize();
        true
    }

    /// Check if a unit sequence exists in the DAWG.
    pub fn contains_units(&self, units: &[U]) -> bool {
        let mut node_idx = 0;

        for &unit in units {
            match self.nodes[node_idx]
                .edges
                .iter()
                .find(|(u, _)| *u == unit)
                .map(|(_, idx)| idx)
            {
                Some(&child_idx) => node_idx = child_idx,
                None => return false,
            }
        }

        self.nodes[node_idx].is_final
    }

    /// Get the value associated with a unit sequence.
    pub fn get_value_for_units(&self, units: &[U]) -> Option<V> {
        let mut node_idx = 0;

        for &unit in units {
            match self.nodes[node_idx]
                .edges
                .iter()
                .find(|(u, _)| *u == unit)
                .map(|(_, idx)| idx)
            {
                Some(&child_idx) => node_idx = child_idx,
                None => return None,
            }
        }

        if self.nodes[node_idx].is_final {
            self.nodes[node_idx].value.clone()
        } else {
            None
        }
    }

    /// Remove a unit sequence from the DAWG.
    ///
    /// Returns `true` if the sequence was present and removed, `false` otherwise.
    pub fn remove_units(&mut self, units: &[U]) -> bool {
        // Navigate to the term
        let mut node_idx = 0;
        let mut path: Vec<(usize, U, usize)> = Vec::new(); // (parent, label, child)

        for &unit in units {
            if let Some(&child_idx) = self.nodes[node_idx]
                .edges
                .iter()
                .find(|(u, _)| *u == unit)
                .map(|(_, idx)| idx)
            {
                path.push((node_idx, unit, child_idx));
                node_idx = child_idx;
            } else {
                return false; // Term doesn't exist
            }
        }

        // Check if it's a final node
        if !self.nodes[node_idx].is_final {
            return false;
        }

        // Unmark as final
        self.nodes[node_idx].is_final = false;
        self.nodes[node_idx].value = None;
        self.term_count -= 1;

        // Prune unreachable branches (nodes with no children and not final)
        for (parent_idx, label, child_idx) in path.iter().rev() {
            let child = &self.nodes[*child_idx];
            if !child.is_final && child.edges.is_empty() {
                // Remove edge from parent
                self.nodes[*parent_idx].edges.retain(|(u, _)| *u != *label);
            } else {
                break;
            }
        }

        self.suffix_cache.clear();
        self.needs_compaction = true;
        true
    }

    /// Update an existing term's value or insert with a default value.
    ///
    /// Returns `true` if a new term was inserted, `false` if existing was updated.
    pub fn update_or_insert_units<F>(&mut self, units: &[U], default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        let mut node_idx = 0;
        let mut path_len = 0;

        for &unit in units {
            if let Some(&child_idx) = self.nodes[node_idx]
                .edges
                .iter()
                .find(|(u, _)| *u == unit)
                .map(|(_, idx)| idx)
            {
                node_idx = child_idx;
                path_len += 1;
            } else {
                break;
            }
        }

        // Check if term already exists
        if path_len == units.len() {
            if self.nodes[node_idx].is_final {
                // Term exists - update its value
                if let Some(ref mut existing_value) = self.nodes[node_idx].value {
                    update_fn(existing_value);
                } else {
                    self.nodes[node_idx].value = Some(default_value);
                }
                return false;
            } else {
                // Node exists but wasn't final
                self.nodes[node_idx].is_final = true;
                self.nodes[node_idx].value = Some(default_value);
                self.term_count += 1;
                return true;
            }
        }

        // Build remaining path
        for i in path_len..units.len() {
            let unit = units[i];
            let new_idx = self.nodes.len();
            let is_final = i == units.len() - 1;

            let mut new_node = if is_final {
                DawgNode::new_with_value(true, Some(default_value.clone()))
            } else {
                DawgNode::new(false)
            };
            new_node.ref_count = 1;

            self.nodes.push(new_node);
            self.insert_edge_sorted(node_idx, unit, new_idx);
            node_idx = new_idx;
        }

        self.term_count += 1;
        self.check_and_auto_minimize();
        true
    }

    /// Add to Bloom filter if enabled.
    pub fn bloom_insert(&mut self, term: &str) {
        if let Some(ref mut bloom) = self.bloom_filter {
            bloom.insert(term);
        }
    }

    /// Check Bloom filter (returns true if not using bloom or might contain).
    #[inline]
    pub fn bloom_might_contain(&self, term: &str) -> bool {
        match &self.bloom_filter {
            Some(bloom) => bloom.might_contain(term),
            None => true,
        }
    }

    /// Compact the DAWG to restore perfect minimality.
    ///
    /// Returns the number of nodes removed.
    pub fn compact(&mut self) -> usize {
        // Extract all terms
        let terms = self.extract_all_terms();
        let old_node_count = self.nodes.len();

        // Preserve settings
        let auto_minimize_threshold = self.auto_minimize_threshold;
        let bloom_capacity = self.bloom_filter.as_ref().map(|b| b.capacity() / 10);

        // Rebuild from scratch
        self.nodes = vec![DawgNode::new(false)];
        self.term_count = 0;
        self.needs_compaction = false;
        self.suffix_cache.clear();
        self.last_minimized_node_count = 1;
        self.auto_minimize_threshold = auto_minimize_threshold;
        self.bloom_filter = bloom_capacity.map(BloomFilter::new);

        // Re-insert sorted terms for optimal prefix sharing
        let mut sorted_terms = terms;
        sorted_terms.sort();

        for term in &sorted_terms {
            self.insert_direct(term);
            if let Some(ref mut bloom) = self.bloom_filter {
                let term_str = U::to_string(term);
                bloom.insert(&term_str);
            }
        }

        // Now minimize to merge equivalent suffixes
        let minimized = self.minimize_incremental();
        old_node_count - self.nodes.len() + minimized
    }

    /// Incremental minimization using signature-based node merging.
    ///
    /// Returns the number of nodes merged.
    pub fn minimize_incremental(&mut self) -> usize {
        let initial_count = self.nodes.len();

        // Step 1: Compute node signatures
        let signatures = self.compute_signatures();

        // Step 2: Build equivalence classes
        let mut sig_to_canonical: HashMap<NodeSignature, Vec<usize>> = HashMap::new();
        let mut node_mapping: Vec<usize> = (0..self.nodes.len()).collect();

        // Process nodes in reverse order (leaves first)
        for node_idx in (0..self.nodes.len()).rev() {
            let sig = &signatures[node_idx];

            if let Some(canonical_candidates) = sig_to_canonical.get(sig) {
                let mut found_match = false;
                for &canonical_idx in canonical_candidates {
                    if node_mapping[canonical_idx] != canonical_idx {
                        continue;
                    }
                    if self.nodes_structurally_equal(node_idx, canonical_idx, &node_mapping) {
                        node_mapping[node_idx] = canonical_idx;
                        found_match = true;
                        break;
                    }
                }
                if !found_match {
                    sig_to_canonical
                        .get_mut(sig)
                        .expect("sig exists")
                        .push(node_idx);
                    node_mapping[node_idx] = node_idx;
                }
            } else {
                sig_to_canonical.insert(*sig, vec![node_idx]);
                node_mapping[node_idx] = node_idx;
            }
        }

        // Step 3: Redirect all edges to canonical nodes
        for node in &mut self.nodes {
            for (_, target_idx) in &mut node.edges {
                *target_idx = node_mapping[*target_idx];
            }
        }

        // Step 4: Remove unreachable nodes
        let reachable = self.find_reachable_nodes();
        if reachable.len() < self.nodes.len() {
            self.compact_with_reachable(&reachable);
        }

        self.suffix_cache.clear();
        self.needs_compaction = false;
        self.last_minimized_node_count = self.nodes.len();

        initial_count - self.nodes.len()
    }

    /// Insert an edge in sorted order using binary search.
    #[inline]
    pub(crate) fn insert_edge_sorted(&mut self, node_idx: usize, label: U, target_idx: usize) {
        let edges = &mut self.nodes[node_idx].edges;
        match edges.binary_search_by_key(&label, |(l, _)| *l) {
            Ok(pos) => {
                edges[pos] = (label, target_idx);
            }
            Err(pos) => {
                edges.insert(pos, (label, target_idx));
            }
        }
    }

    /// Check if auto-minimization should be triggered.
    pub(crate) fn check_and_auto_minimize(&mut self) {
        let current_nodes = self.nodes.len();
        let threshold_nodes =
            (self.last_minimized_node_count as f32 * self.auto_minimize_threshold) as usize;

        if current_nodes > threshold_nodes && !self.auto_minimize_threshold.is_infinite() {
            self.minimize_incremental();
        }
    }

    /// Check if two nodes are structurally equivalent.
    pub(crate) fn nodes_structurally_equal(
        &self,
        idx1: usize,
        idx2: usize,
        node_mapping: &[usize],
    ) -> bool {
        let node1 = &self.nodes[idx1];
        let node2 = &self.nodes[idx2];

        if node1.is_final != node2.is_final {
            return false;
        }

        if node1.edges.len() != node2.edges.len() {
            return false;
        }

        for i in 0..node1.edges.len() {
            let (label1, target1) = node1.edges[i];
            let (label2, target2) = node2.edges[i];

            if label1 != label2 {
                return false;
            }

            if node_mapping[target1] != node_mapping[target2] {
                return false;
            }
        }

        true
    }

    /// Compute signatures for all nodes.
    pub(crate) fn compute_signatures(&self) -> Vec<NodeSignature> {
        let mut signatures = vec![NodeSignature::zero(); self.nodes.len()];
        let mut visited = vec![false; self.nodes.len()];
        self.compute_signatures_dfs(0, &mut signatures, &mut visited);
        signatures
    }

    pub(crate) fn compute_signatures_dfs(
        &self,
        node_idx: usize,
        signatures: &mut [NodeSignature],
        visited: &mut [bool],
    ) {
        if visited[node_idx] {
            return;
        }
        visited[node_idx] = true;

        let node = &self.nodes[node_idx];

        // Visit all children first (post-order)
        for (_, child_idx) in &node.edges {
            self.compute_signatures_dfs(*child_idx, signatures, visited);
        }

        // Compute signature for this node
        let edge_iter = node
            .edges
            .iter()
            .map(|(label, child_idx)| (*label, signatures[*child_idx]));

        signatures[node_idx] = NodeSignature::compute(node.is_final, edge_iter);
    }

    /// Find all nodes reachable from root.
    pub(crate) fn find_reachable_nodes(&self) -> Vec<usize> {
        let mut reachable = Vec::new();
        let mut visited = vec![false; self.nodes.len()];
        self.find_reachable_dfs(0, &mut visited);

        for (idx, &is_reachable) in visited.iter().enumerate() {
            if is_reachable {
                reachable.push(idx);
            }
        }

        reachable
    }

    pub(crate) fn find_reachable_dfs(&self, node_idx: usize, visited: &mut [bool]) {
        if visited[node_idx] {
            return;
        }
        visited[node_idx] = true;

        for (_, child_idx) in &self.nodes[node_idx].edges {
            self.find_reachable_dfs(*child_idx, visited);
        }
    }

    /// Compact the node array to only contain reachable nodes.
    pub(crate) fn compact_with_reachable(&mut self, reachable: &[usize]) {
        let mut old_to_new = vec![usize::MAX; self.nodes.len()];
        for (new_idx, &old_idx) in reachable.iter().enumerate() {
            old_to_new[old_idx] = new_idx;
        }

        let new_nodes: Vec<DawgNode<U, V>> = reachable
            .iter()
            .map(|&old_idx| {
                let mut node = self.nodes[old_idx].clone();
                for (_, target) in &mut node.edges {
                    *target = old_to_new[*target];
                }
                node
            })
            .collect();

        self.nodes = new_nodes;
    }

    /// Extract all terms as unit vectors.
    pub(crate) fn extract_all_terms(&self) -> Vec<Vec<U>> {
        let mut terms = Vec::new();
        let mut current_term = Vec::new();
        self.dfs_collect(0, &mut current_term, &mut terms);
        terms
    }

    pub(crate) fn dfs_collect(
        &self,
        node_idx: usize,
        current_term: &mut Vec<U>,
        terms: &mut Vec<Vec<U>>,
    ) {
        let node = &self.nodes[node_idx];

        if node.is_final {
            terms.push(current_term.clone());
        }

        for (unit, child_idx) in &node.edges {
            current_term.push(*unit);
            self.dfs_collect(*child_idx, current_term, terms);
            current_term.pop();
        }
    }

    /// Direct insert without bloom filter or auto-minimize.
    pub(crate) fn insert_direct(&mut self, units: &[U]) {
        let mut node_idx = 0;

        for &unit in units {
            if let Some(&child_idx) = self.nodes[node_idx]
                .edges
                .iter()
                .find(|(u, _)| *u == unit)
                .map(|(_, idx)| idx)
            {
                node_idx = child_idx;
            } else {
                let new_idx = self.nodes.len();
                self.nodes.push(DawgNode::new(false));
                self.nodes[node_idx].edges.push((unit, new_idx));
                node_idx = new_idx;
            }
        }

        self.nodes[node_idx].is_final = true;
        self.term_count += 1;
    }
}

impl<U: CharUnit, V: DictionaryValue> Default for DawgCore<U, V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dawg_core_insert_bytes() {
        let mut core: DawgCore<u8, ()> = DawgCore::new();

        assert!(core.insert_units(b"hello"));
        assert!(core.insert_units(b"world"));
        assert!(!core.insert_units(b"hello")); // Duplicate

        assert_eq!(core.term_count(), 2);
        assert!(core.contains_units(b"hello"));
        assert!(core.contains_units(b"world"));
        assert!(!core.contains_units(b"foo"));
    }

    #[test]
    fn test_dawg_core_insert_chars() {
        let mut core: DawgCore<char, ()> = DawgCore::new();

        let hello: Vec<char> = "hello".chars().collect();
        let world: Vec<char> = "world".chars().collect();
        let cafe: Vec<char> = "café".chars().collect();

        assert!(core.insert_units(&hello));
        assert!(core.insert_units(&world));
        assert!(core.insert_units(&cafe));
        assert!(!core.insert_units(&hello)); // Duplicate

        assert_eq!(core.term_count(), 3);
        assert!(core.contains_units(&hello));
        assert!(core.contains_units(&cafe));
    }

    #[test]
    fn test_dawg_core_with_values() {
        let mut core: DawgCore<u8, u32> = DawgCore::new();

        assert!(core.insert_units_with_value(b"key1", 42));
        assert!(core.insert_units_with_value(b"key2", 100));
        assert!(!core.insert_units_with_value(b"key1", 999)); // Update

        assert_eq!(core.get_value_for_units(b"key1"), Some(999));
        assert_eq!(core.get_value_for_units(b"key2"), Some(100));
        assert_eq!(core.get_value_for_units(b"unknown"), None);
    }

    #[test]
    fn test_dawg_core_remove() {
        let mut core: DawgCore<u8, ()> = DawgCore::new();

        core.insert_units(b"test");
        core.insert_units(b"testing");
        core.insert_units(b"tested");

        assert!(core.remove_units(b"testing"));
        assert_eq!(core.term_count(), 2);
        assert!(!core.remove_units(b"testing")); // Already removed
        assert!(core.contains_units(b"test"));
        assert!(!core.contains_units(b"testing"));
    }

    #[test]
    fn test_dawg_core_minimize() {
        let mut core: DawgCore<u8, ()> = DawgCore::new();

        core.insert_units(b"zebra");
        core.insert_units(b"apple");
        core.insert_units(b"banana");
        core.insert_units(b"apricot");

        let nodes_before = core.node_count();
        let merged = core.minimize_incremental();
        let nodes_after = core.node_count();

        assert_eq!(nodes_after, nodes_before - merged);

        // All terms should still be present
        assert!(core.contains_units(b"zebra"));
        assert!(core.contains_units(b"apple"));
        assert!(core.contains_units(b"banana"));
        assert!(core.contains_units(b"apricot"));
    }

    #[test]
    fn test_dawg_core_compact() {
        let mut core: DawgCore<u8, ()> = DawgCore::new();

        core.insert_units(b"test");
        core.insert_units(b"testing");
        core.insert_units(b"tested");
        core.remove_units(b"testing");

        let _removed = core.compact();

        assert_eq!(core.term_count(), 2);
        assert!(core.contains_units(b"test"));
        assert!(core.contains_units(b"tested"));
        assert!(!core.contains_units(b"testing"));
    }
}
