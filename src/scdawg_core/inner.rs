//! Generic `ScdawgCoreInner<U, V>` shared by byte and char SCDAWG variants.
//!
//! Hosts the on-line SCDAWG construction (Blumer et al. 1987 sa_extend
//! algorithm), the post-construction `compute_left_edges` pass, and the
//! IS-features (find / freq / locations) — all generic over
//! `U: CharUnit` so the byte (`Unit = u8`) and char (`Unit = char`)
//! variants share a single implementation.
//!
//! ## Sources unified by this module
//!
//! Before this module, `src/scdawg.rs` and `src/scdawg_char.rs` each
//! carried ~340 LOC of mostly-identical `ScdawgInner<V>` impl methods,
//! differing only in (a) the edge-label type (`u8` vs `char`) and
//! (b) the unit-count measurement when computing string lengths
//! (`term.as_bytes()`/`pattern.len()` vs `term.chars()`/
//! `pattern.chars().count()`). Both reduce to `U::iter_str(term)` /
//! `U::from_str(pattern).len()` here, removing the duplication.

use rustc_hash::{FxHashMap, FxHashSet};

use super::node::{ScdawgNode, NIL};
use crate::value::DictionaryValue;
use crate::CharUnit;

/// Inner mutable state of the SCDAWG, generic over the edge-label type.
#[derive(Debug)]
pub struct ScdawgCoreInner<U: CharUnit, V: DictionaryValue> {
    /// All nodes. Index 0 is always root.
    pub nodes: Vec<ScdawgNode<U, V>>,
    /// Last created node (for online construction).
    pub last: usize,
    /// Number of terms inserted.
    pub term_count: usize,
    /// Stored terms for enumeration.
    pub terms: Vec<String>,
    /// Fast duplicate detection using hash set.
    pub term_set: FxHashSet<String>,
    /// Exact term-to-value table for public mapped-dictionary semantics.
    ///
    /// SCDAWG states represent substring equivalence classes and may be split
    /// or shared as later terms are inserted. Keeping exact values here avoids
    /// conflating the public complete-term map with the internal substring
    /// automaton topology.
    pub term_values: FxHashMap<String, V>,
    /// Whether left edges have been computed.
    pub left_edges_computed: bool,
}

impl<U: CharUnit, V: DictionaryValue> ScdawgCoreInner<U, V> {
    /// Create a new empty SCDAWG inner state with a root node.
    pub fn new() -> Self {
        Self {
            nodes: vec![ScdawgNode::root()],
            last: 0,
            term_count: 0,
            terms: Vec::new(),
            term_set: FxHashSet::default(),
            term_values: FxHashMap::default(),
            left_edges_computed: false,
        }
    }

    /// Create with pre-allocated capacity. The suffix automaton has at
    /// most 2*n nodes for n total characters.
    pub fn with_capacity(term_count: usize, total_chars: usize) -> Self {
        let estimated_nodes = total_chars.saturating_mul(2);
        let mut nodes = Vec::with_capacity(estimated_nodes);
        nodes.push(ScdawgNode::root());
        Self {
            nodes,
            last: 0,
            term_count: 0,
            terms: Vec::with_capacity(term_count),
            term_set: FxHashSet::with_capacity_and_hasher(term_count, Default::default()),
            term_values: FxHashMap::with_capacity_and_hasher(term_count, Default::default()),
            left_edges_computed: false,
        }
    }

    /// Allocate a new node and return its index.
    pub fn alloc_node(&mut self, length: usize, suffix_link: usize, first_char: U) -> usize {
        let idx = self.nodes.len();
        self.nodes
            .push(ScdawgNode::new(length, suffix_link, first_char));
        idx
    }

    /// Clone a node (used in equivalence-class split operations).
    pub fn clone_node(&mut self, src: usize) -> usize {
        let idx = self.nodes.len();
        self.nodes.push(self.nodes[src].clone());
        idx
    }

    /// Insert a single unit, extending the suffix automaton.
    ///
    /// This is the core of Blumer et al.'s on-line suffix automaton
    /// construction algorithm. Each call extends the automaton by one
    /// unit, adding at most one new state plus possibly one clone for
    /// equivalence-class splitting.
    pub fn sa_extend(&mut self, c: U, term_idx: usize, pos: usize) {
        // Compute first_char for the new node:
        // - If extending from root, first_char is c.
        // - Otherwise, inherit first_char from the current last node.
        let first_char = if self.nodes[self.last].length == 0 {
            c
        } else {
            self.nodes[self.last].first_char
        };

        // Create new state for the new longest suffix.
        let cur = self.alloc_node(self.nodes[self.last].length + 1, 0, first_char);

        // Set parent info for the new node.
        self.nodes[cur].parent = self.last;
        self.nodes[cur].parent_label = c;
        self.nodes[cur].depth = self.nodes[self.last].depth + 1;

        // Walk up suffix links, adding edges to the new state.
        let mut p = self.last;

        // Phase 1: Add edges from states that don't have edge labeled c.
        while p != NIL && self.nodes[p].get_edge(c).is_none() {
            self.nodes[p].set_edge(c, cur);
            p = self.nodes[p].suffix_link;
        }

        if p == NIL {
            // Case 1: reached the root without finding edge c.
            // New state's suffix link goes to root.
            self.nodes[cur].suffix_link = 0;
        } else {
            // Found a state p that has edge c.
            let q = self.nodes[p]
                .get_edge(c)
                .expect("invariant: p has edge c by Phase 1 break condition");

            if self.nodes[p].length + 1 == self.nodes[q].length {
                // Case 2: edge p→q is "solid" — no split needed.
                self.nodes[cur].suffix_link = q;
            } else {
                // Case 3: split state q.
                let clone = self.clone_node(q);
                self.nodes[clone].length = self.nodes[p].length + 1;

                // Compute first_char for clone:
                self.nodes[clone].first_char = if self.nodes[p].length == 0 {
                    c
                } else {
                    self.nodes[p].first_char
                };

                // Update suffix links.
                self.nodes[cur].suffix_link = clone;
                self.nodes[q].suffix_link = clone;

                // Update parent info for clone.
                self.nodes[clone].parent = p;
                self.nodes[clone].parent_label = c;
                self.nodes[clone].depth = self.nodes[p].depth + 1;

                // Clear term_ends from clone (not a real final state).
                self.nodes[clone].term_ends.clear();
                self.nodes[clone].is_final = false;
                self.nodes[clone].value = None;

                // Redirect edges from p and its suffix chain that point to q.
                while p != NIL && self.nodes[p].get_edge(c) == Some(q) {
                    self.nodes[p].set_edge(c, clone);
                    p = self.nodes[p].suffix_link;
                }
            }
        }

        // Record position in term.
        self.nodes[cur].term_ends.push((term_idx, pos));

        self.last = cur;
        self.left_edges_computed = false;
    }

    /// Insert a term into the SCDAWG. Returns false if duplicate.
    pub fn insert(&mut self, term: &str) -> bool {
        if self.term_set.contains(term) {
            return false;
        }

        let term_idx = self.term_count;
        self.last = 0;

        for (pos, unit) in U::iter_str(term).enumerate() {
            self.sa_extend(unit, term_idx, pos);
        }

        // Mark the final state.
        self.nodes[self.last].is_final = true;

        let term_string = term.to_string();
        self.term_set.insert(term_string.clone());
        self.terms.push(term_string);
        self.term_count += 1;

        true
    }

    /// Insert a term with an associated value.
    pub fn insert_with_value(&mut self, term: &str, value: V) -> bool {
        if self.term_set.contains(term) {
            self.term_values.insert(term.to_string(), value.clone());
            if let Some(node) = self.find_substring_fast(term) {
                if self.nodes[node].is_final {
                    self.nodes[node].value = Some(value);
                }
            }
            return false;
        }

        if self.insert(term) {
            self.nodes[self.last].value = Some(value.clone());
            self.term_values.insert(term.to_string(), value);
            true
        } else {
            false
        }
    }

    /// Compute left extension edges from suffix links.
    pub fn compute_left_edges(&mut self) {
        if self.left_edges_computed {
            return;
        }

        // Clear existing left edges.
        for node in &mut self.nodes {
            node.left_edges.clear();
        }

        // For each node with a suffix link, add left edge to suffix target.
        for node_idx in 1..self.nodes.len() {
            let suffix_target = self.nodes[node_idx].suffix_link;
            if suffix_target != NIL {
                let label = self.nodes[node_idx].first_char;
                self.nodes[suffix_target].left_edges.push((label, node_idx));
            }
        }

        self.left_edges_computed = true;
    }

    /// Find the node where `pattern` ends, via O(|pattern|) traversal.
    pub fn find_substring_fast(&self, pattern: &str) -> Option<usize> {
        if pattern.is_empty() {
            return Some(0);
        }

        let mut current = 0;
        for unit in U::iter_str(pattern) {
            match self.nodes[current].get_edge(unit) {
                Some(next) => current = next,
                None => return None,
            }
        }

        Some(current)
    }

    /// Check if pattern is a substring of any indexed term.
    pub fn contains_substring(&self, pattern: &str) -> bool {
        self.find_substring_fast(pattern).is_some()
    }

    /// Find all occurrences of a substring pattern. Returns (term, position) pairs.
    pub fn find_exact_substring(&self, pattern: &str) -> Vec<(String, usize)> {
        if pattern.is_empty() {
            return self.terms.iter().map(|t| (t.clone(), 0)).collect();
        }

        let end_node = match self.find_substring_fast(pattern) {
            Some(node) => node,
            None => return Vec::new(),
        };

        let pattern_len = U::from_str(pattern).len();
        let mut results = Vec::new();
        self.collect_term_positions(end_node, pattern_len, &mut results);
        results
    }

    /// Collect all term positions reachable from a node via left edges.
    pub fn collect_term_positions(
        &self,
        node: usize,
        pattern_len: usize,
        results: &mut Vec<(String, usize)>,
    ) {
        for &(term_idx, end_pos) in &self.nodes[node].term_ends {
            if end_pos + 1 >= pattern_len {
                let start_pos = end_pos + 1 - pattern_len;
                if term_idx < self.terms.len() {
                    results.push((self.terms[term_idx].clone(), start_pos));
                }
            }
        }

        for &(_, target) in &self.nodes[node].left_edges {
            self.collect_term_positions(target, pattern_len, results);
        }
    }

    /// Check if the SCDAWG contains a complete term.
    pub fn contains(&self, term: &str) -> bool {
        self.term_set.contains(term)
    }

    /// Get the number of terms.
    pub fn term_count(&self) -> usize {
        self.term_count
    }

    /// Iterate over all terms.
    pub fn iter_terms(&self) -> impl Iterator<Item = &String> {
        self.terms.iter()
    }

    /// Get the frequency (occurrence count) of a substring pattern.
    pub fn frequency(&self, pattern: &str) -> usize {
        if pattern.is_empty() {
            // Empty pattern matches at every position in every term.
            return self.terms.iter().map(|t| U::from_str(t).len() + 1).sum();
        }

        match self.find_substring_fast(pattern) {
            Some(node) => {
                let mut count = 0;
                self.count_occurrences(node, &mut count);
                count
            }
            None => 0,
        }
    }

    /// Count all occurrences reachable from a node via left edges.
    pub fn count_occurrences(&self, node: usize, count: &mut usize) {
        *count += self.nodes[node].term_ends.len();

        for &(_, target) in &self.nodes[node].left_edges {
            self.count_occurrences(target, count);
        }
    }
}

impl<U: CharUnit, V: DictionaryValue> Default for ScdawgCoreInner<U, V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scdawg_inner_byte_smoke() {
        let mut inner: ScdawgCoreInner<u8, ()> = ScdawgCoreInner::new();
        assert!(inner.insert("cat"));
        assert!(inner.insert("car"));
        assert!(!inner.insert("cat")); // duplicate
        assert_eq!(inner.term_count(), 2);
        assert!(inner.contains_substring("ca"));
        assert!(inner.contains_substring("at"));
        assert!(!inner.contains_substring("zz"));
        inner.compute_left_edges();
        assert_eq!(inner.frequency("ca"), 2);
        assert_eq!(inner.frequency("at"), 1);
    }

    #[test]
    fn scdawg_inner_char_smoke() {
        let mut inner: ScdawgCoreInner<char, ()> = ScdawgCoreInner::new();
        assert!(inner.insert("café"));
        assert!(!inner.insert("café")); // duplicate suppressed (returns false)
        assert_eq!(inner.term_count(), 1);
        assert!(inner.contains_substring("café"));
        assert!(inner.contains_substring("afé"));
        inner.compute_left_edges();
        assert_eq!(inner.frequency("café"), 1);
    }
}
