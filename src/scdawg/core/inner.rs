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
use smallvec::SmallVec;

use super::node::{ScdawgNode, NIL};
use crate::value::DictionaryValue;
use crate::{CharUnit, SnapshotTraversalCursor};

const INLINE_GRAPH_WORKLIST: usize = 32;

#[derive(Clone, Copy)]
struct LeftEdgeTraversalFrame {
    node: usize,
    next_left_edge: usize,
}

type LeftEdgeTraversal = SmallVec<[LeftEdgeTraversalFrame; INLINE_GRAPH_WORKLIST]>;

/// Inner mutable state of the SCDAWG, generic over the edge-label type.
///
/// Public byte/char wrappers publish cloned revisions of this state through an
/// atomic snapshot handle so readers can hold stable, wait-free traversals.
#[derive(Debug, Clone)]
pub struct ScdawgCoreInner<U: CharUnit, V: DictionaryValue> {
    /// All nodes. Index 0 is always root.
    pub nodes: Vec<ScdawgNode<U, V>>,
    /// Last created node (for online construction).
    pub last: usize,
    /// Number of terms inserted.
    pub term_count: usize,
    /// Stored terms for enumeration.
    pub terms: Vec<String>,
    /// Insertion-record indices ordered by Unicode scalar/UTF-8 lexicographic
    /// term order. This keeps entry-cursor creation O(1) while preserving the
    /// immutable record indices used by occurrence metadata.
    pub sorted_term_indices: Vec<usize>,
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
            sorted_term_indices: Vec::new(),
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
            sorted_term_indices: Vec::with_capacity(term_count),
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
        let sorted_position = self
            .sorted_term_indices
            .partition_point(|&index| self.terms[index].as_str() < term);
        self.sorted_term_indices.insert(sorted_position, term_idx);
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
            {
                let next = self.nodes[current].get_edge(unit)?;
                current = next
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
        let mut results = Vec::with_capacity(self.nodes[end_node].term_ends.len());
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
        self.visit_left_edge_subtree(node, |current| {
            for &(term_idx, end_pos) in &self.nodes[current].term_ends {
                if end_pos + 1 >= pattern_len {
                    let start_pos = end_pos + 1 - pattern_len;
                    if term_idx < self.terms.len() {
                        results.push((self.terms[term_idx].clone(), start_pos));
                    }
                }
            }
        });
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
        self.visit_left_edge_subtree(node, |current| {
            *count += self.nodes[current].term_ends.len();
        });
    }

    /// Visit a left-edge subtree in the same pre-order as the former
    /// recursive traversal while keeping traversal state off the native call
    /// stack.
    ///
    /// A continuation frame stores the next child of each active ancestor.
    /// Consequently, auxiliary space is proportional to graph depth rather
    /// than to the total node count or the maximum frontier width. Repeated
    /// paths are deliberately not de-duplicated: each path is visited exactly
    /// as it was by the recursive implementation.
    #[inline]
    fn visit_left_edge_subtree(&self, node: usize, mut visit: impl FnMut(usize)) {
        let mut traversal = LeftEdgeTraversal::new();
        traversal.push(LeftEdgeTraversalFrame {
            node,
            next_left_edge: 0,
        });

        while let Some(frame) = traversal.last() {
            let current = frame.node;
            let next_left_edge = frame.next_left_edge;

            if next_left_edge == 0 {
                visit(current);
            }

            if let Some(&(_, child)) = self.nodes[current].left_edges.get(next_left_edge) {
                // Advance the parent's continuation before descending so the
                // child can push without retaining a mutable borrow.
                traversal
                    .last_mut()
                    .expect("the current traversal frame exists")
                    .next_left_edge += 1;
                traversal.push(LeftEdgeTraversalFrame {
                    node: child,
                    next_left_edge: 0,
                });
            } else {
                traversal.pop();
            }
        }
    }

    /// Return the dense traversal cursor for one node in this immutable
    /// revision.
    #[inline]
    pub(crate) fn snapshot_cursor(&self, node: usize) -> Option<SnapshotTraversalCursor> {
        (node < self.nodes.len())
            .then(|| SnapshotTraversalCursor::from_index(node))
            .flatten()
    }

    /// Validate a dense cursor against this exact immutable revision.
    #[inline]
    pub(crate) fn contains_snapshot_cursor(&self, cursor: SnapshotTraversalCursor) -> bool {
        cursor.index() < self.nodes.len()
    }

    /// Project one cursor node's borrowed forward edges without constructing
    /// owned node handles for accepted children.
    #[inline]
    pub(crate) fn filter_map_snapshot_cursor_edges_and_finality<T, P, F>(
        &self,
        cursor: SnapshotTraversalCursor,
        mut project: P,
        mut visitor: F,
    ) -> Option<bool>
    where
        P: FnMut(U) -> Option<T>,
        F: FnMut(U, SnapshotTraversalCursor, T),
    {
        let node = self.nodes.get(cursor.index())?;
        for &(label, target) in &node.forward_edges {
            if let Some(projected) = project(label) {
                let target = self
                    .snapshot_cursor(target)
                    .expect("an SCDAWG edge targets this immutable revision");
                visitor(label, target, projected);
            }
        }
        Some(node.is_final)
    }

    /// Read finality through a validated dense cursor.
    #[inline]
    pub(crate) fn snapshot_cursor_is_final(&self, cursor: SnapshotTraversalCursor) -> Option<bool> {
        self.nodes.get(cursor.index()).map(|node| node.is_final)
    }

    /// Follow one forward edge through a validated dense cursor.
    #[inline]
    pub(crate) fn snapshot_cursor_transition(
        &self,
        cursor: SnapshotTraversalCursor,
        label: U,
    ) -> Option<Option<SnapshotTraversalCursor>> {
        let node = self.nodes.get(cursor.index())?;
        Some(node.get_edge(label).map(|target| {
            self.snapshot_cursor(target)
                .expect("an SCDAWG edge targets this immutable revision")
        }))
    }

    /// Visit one directly sliced page of borrowed forward edges.
    #[inline]
    pub(crate) fn visit_snapshot_cursor_edge_page<F>(
        &self,
        cursor: SnapshotTraversalCursor,
        start: usize,
        capacity: usize,
        mut visitor: F,
    ) -> Option<(bool, usize)>
    where
        F: FnMut(U, SnapshotTraversalCursor),
    {
        let node = self.nodes.get(cursor.index())?;
        let total = node.forward_edges.len();
        let start = start.min(total);
        let end = start.saturating_add(capacity).min(total);
        for &(label, target) in &node.forward_edges[start..end] {
            let target = self
                .snapshot_cursor(target)
                .expect("an SCDAWG edge targets this immutable revision");
            visitor(label, target);
        }
        Some((node.is_final, total))
    }

    /// Read the optional mapped value through a validated dense cursor.
    #[inline]
    pub(crate) fn snapshot_cursor_value(
        &self,
        cursor: SnapshotTraversalCursor,
    ) -> Option<Option<V>> {
        self.nodes.get(cursor.index()).map(|node| {
            if node.is_final {
                node.value.clone()
            } else {
                None
            }
        })
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

    #[test]
    fn one_hundred_thousand_deep_left_edge_walks_are_stack_safe() {
        const DEPTH: usize = 100_000;
        let mut inner: ScdawgCoreInner<u8, ()> = ScdawgCoreInner::new();
        for length in 1..=DEPTH {
            inner.alloc_node(length, length - 1, b'x');
        }
        inner.terms.push("x".to_owned());
        inner.nodes[DEPTH].term_ends.push((0, DEPTH - 1));
        inner.compute_left_edges();

        let mut positions = Vec::new();
        inner.collect_term_positions(0, 1, &mut positions);
        assert_eq!(positions, vec![("x".to_owned(), DEPTH - 1)]);

        let mut count = 0;
        inner.count_occurrences(0, &mut count);
        assert_eq!(count, 1);
    }

    #[test]
    fn iterative_left_edge_walk_preserves_recursive_order_and_path_multiplicity() {
        let mut inner: ScdawgCoreInner<u8, ()> = ScdawgCoreInner::new();
        let left = inner.alloc_node(1, 0, b'l');
        let right = inner.alloc_node(1, 0, b'r');
        let shared = inner.alloc_node(2, left, b's');
        inner.terms.push("left".to_owned());
        inner.terms.push("shared".to_owned());
        inner.nodes[left].term_ends.push((0, 0));
        inner.nodes[shared].term_ends.push((1, 1));
        inner.nodes[0].left_edges.push((b'l', left));
        inner.nodes[0].left_edges.push((b'r', right));
        inner.nodes[left].left_edges.push((b's', shared));
        inner.nodes[right].left_edges.push((b's', shared));

        let mut positions = Vec::new();
        inner.collect_term_positions(0, 1, &mut positions);
        assert_eq!(
            positions,
            vec![
                ("left".to_owned(), 0),
                ("shared".to_owned(), 1),
                ("shared".to_owned(), 1),
            ]
        );

        let mut count = 0;
        inner.count_occurrences(0, &mut count);
        assert_eq!(count, 3);
    }
}
