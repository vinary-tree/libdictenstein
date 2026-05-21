//! Symmetric Compact DAWG (SCDAWG) implementation.
//!
//! This module implements an SCDAWG (Symmetric Compact Directed Acyclic Word Graph)
//! following the algorithms described in:
//! - Blumer et al. (1987): "Complete Inverted Files for Efficient Text Retrieval and Analysis"
//! - Inenaga et al. (2005): "On-line construction of compact directed acyclic word graphs"
//!
//! # Features
//!
//! - **O(|pattern|) substring search**: True suffix automaton indexing ALL substrings
//! - **Left extension edges**: Bidirectional traversal via sext links
//! - **IS features**: freq(), locations() operations from Blumer et al.
//! - **WallBreaker compatible**: Supports requirements (1a), (1b), (1c)
//!
//! # Algorithm Overview
//!
//! For each term, we build a suffix automaton that indexes all substrings.
//! For multi-string support, each term is processed independently with shared structure.
//!
//! # Data Structure
//!
//! Each node represents an equivalence class of substrings with the same end-position set.
//! - `forward_edges`: Standard CDAWG edges (appending characters)
//! - `suffix_link`: Points to the longest proper suffix in a different equivalence class
//! - `left_edges`: Left extension edges (prepending characters) - derived from suffix links
//! - `length`: Maximum length of strings in this equivalence class
//!
//! # Example
//!
//! ```rust
//! use libdictenstein::scdawg::Scdawg;
//! use libdictenstein::SubstringDictionary;
//!
//! // Create an SCDAWG from terms
//! let scdawg = Scdawg::<()>::from_terms(["cathedral", "category", "catering"]);
//!
//! // O(|pattern|) substring search
//! assert!(scdawg.contains_substring("cat"));
//! assert!(scdawg.contains_substring("thedr"));
//!
//! // Find all occurrences
//! let matches = scdawg.find_exact_substring("cat");
//! assert_eq!(matches.len(), 3);  // Found in all three terms
//! ```

use std::sync::Arc;

use rustc_hash::FxHashSet;
use smallvec::SmallVec;

use crate::substring::{BidirectionalDictionaryNode, SubstringDictionary, SubstringMatch};
use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;
use crate::{Dictionary, DictionaryNode};

/// Sentinel value for "no suffix link" or "no parent".
const NIL: usize = usize::MAX;

/// End marker base for multi-string support.
/// Each term gets a unique end marker: END_MARKER_BASE + term_index.
/// Reserved for future use with generalized suffix automaton (Option 2).
#[allow(dead_code)]
const END_MARKER_BASE: u8 = 0x01; // Use low bytes as end markers

// ============================================================================
// True SCDAWG Node
// ============================================================================

// C4 step: byte-for-byte-identical local `ScdawgNode<V>` struct +
// 5-method impl block (root/new/get_edge/set_edge/is_root) replaced
// with a type alias to the generic `crate::scdawg_core::ScdawgNode<u8, V>`.
// The canonical impl carries the same methods with `label: U` instead
// of `label: u8`; for `U = u8` they resolve identically.
//
// Clone + Debug derives are already on the generic struct, so the
// alias inherits them automatically.
type ScdawgNode<V = ()> = crate::scdawg_core::ScdawgNode<u8, V>;

// ============================================================================
// True SCDAWG Inner State
// ============================================================================

// C4b algorithmic dedup: byte-for-byte-identical local ScdawgInner<V>
// struct + ~340-LOC impl block replaced with a type alias to the
// generic crate::scdawg_core::ScdawgCoreInner<u8, V>. Every algorithmic
// method (sa_extend, insert, compute_left_edges, find_substring_fast,
// contains_substring, find_exact_substring, frequency,
// count_occurrences, term_count, contains, iter_terms) now lives on the
// canonical generic core.
type ScdawgInner<V = ()> = crate::scdawg_core::ScdawgCoreInner<u8, V>;

// C4b: the original ~340-LOC impl<V> ScdawgInner<V> block lived
// here. All algorithmic methods (sa_extend, insert,
// compute_left_edges, find_substring_fast, contains_substring,
// find_exact_substring, collect_term_positions, frequency,
// count_occurrences, term_count, contains, iter_terms) now live on
// the canonical generic crate::scdawg_core::ScdawgCoreInner<U, V>.
// Original code preserved in git history.

// ============================================================================
// Public True SCDAWG Type
// ============================================================================

/// True Symmetric Compact DAWG with O(|pattern|) substring search.
///
/// This is a proper suffix automaton implementation that indexes ALL substrings
/// of all terms, enabling efficient substring search and bidirectional extension.
#[derive(Clone, Debug)]
pub struct Scdawg<V: DictionaryValue = ()> {
    inner: Arc<RwLock<ScdawgInner<V>>>,
}

impl<V: DictionaryValue> Default for Scdawg<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> Scdawg<V> {
    /// Create a new empty true SCDAWG.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(ScdawgInner::new())),
        }
    }

    /// Create from an iterator of terms.
    pub fn from_terms<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // Collect terms to enable pre-allocation
        let terms_vec: Vec<S> = terms.into_iter().collect();
        let term_count = terms_vec.len();
        let total_chars: usize = terms_vec.iter().map(|s| s.as_ref().len()).sum();

        let inner = ScdawgInner::with_capacity(term_count, total_chars);
        let scdawg = Self {
            inner: Arc::new(RwLock::new(inner)),
        };
        {
            let mut inner = scdawg.inner.write();
            for term in terms_vec {
                inner.insert(term.as_ref());
            }
            inner.compute_left_edges();
        }
        scdawg
    }

    /// Create from an iterator of `(term, value)` pairs.
    ///
    /// Matches `ScdawgChar::from_terms_with_values` (B3 parity backfill).
    pub fn from_terms_with_values<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        let pairs: Vec<(String, V)> = entries
            .into_iter()
            .map(|(s, v)| (s.as_ref().to_string(), v))
            .collect();
        let total_chars: usize = pairs.iter().map(|(s, _)| s.len()).sum();

        let inner = ScdawgInner::with_capacity(pairs.len(), total_chars);
        let scdawg = Self {
            inner: Arc::new(RwLock::new(inner)),
        };
        {
            let mut inner = scdawg.inner.write();
            for (term, value) in pairs {
                inner.insert_with_value(&term, value);
            }
            inner.compute_left_edges();
        }
        scdawg
    }

    /// Insert a term.
    pub fn insert(&self, term: &str) -> bool {
        let mut inner = self.inner.write();
        let result = inner.insert(term);
        if result {
            inner.compute_left_edges();
        }
        result
    }

    /// Insert a term with a value.
    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        let mut inner = self.inner.write();
        let result = inner.insert_with_value(term, value);
        if result {
            inner.compute_left_edges();
        }
        result
    }

    /// Get the value associated with a term.
    ///
    /// Matches `ScdawgChar::get_value` (B3 parity backfill).
    pub fn get_value(&self, term: &str) -> Option<V>
    where
        V: Clone,
    {
        let inner = self.inner.read();
        let mut current = 0;
        for byte in term.bytes() {
            match inner.nodes[current].get_edge(byte) {
                Some(next) => current = next,
                None => return None,
            }
        }
        if inner.nodes[current].is_final {
            inner.nodes[current].value.clone()
        } else {
            None
        }
    }

    /// Check if a substring exists in any term.
    pub fn contains_substring(&self, pattern: &str) -> bool {
        let inner = self.inner.read();
        inner.contains_substring(pattern)
    }

    /// Iterate over all terms.
    pub fn iter(&self) -> impl Iterator<Item = String> {
        let inner = self.inner.read();
        inner.terms.clone().into_iter()
    }

    /// Get the number of terms in the SCDAWG.
    pub fn term_count(&self) -> usize {
        self.inner.read().term_count()
    }

    // ========================================================================
    // IS Features (Blumer et al. 1987)
    // ========================================================================

    /// Find a substring and return a handle to its SCDAWG state.
    ///
    /// This is the `find(x)` operation from Blumer et al. (1987).
    /// Returns `None` if the pattern is not a substring of any term.
    ///
    /// # Time Complexity
    ///
    /// O(|pattern|) - linear in pattern length.
    ///
    /// # Example
    ///
    /// ```text
    /// let scdawg = Scdawg::<()>::from_terms(["cathedral", "category"]);
    /// if let Some(handle) = scdawg.find("cat") {
    ///     println!("Pattern 'cat' found, frequency: {}", scdawg.freq_at(&handle));
    /// }
    /// ```
    pub fn find(&self, pattern: &str) -> Option<ScdawgNodeHandle<V>> {
        let inner = self.inner.read();
        inner
            .find_substring_fast(pattern)
            .map(|node_idx| ScdawgNodeHandle {
                inner: Arc::clone(&self.inner),
                node_idx,
            })
    }

    /// Get the frequency (occurrence count) of a substring pattern.
    ///
    /// This is the `freq(x)` operation from Blumer et al. (1987).
    /// Returns the total number of occurrences across all terms.
    ///
    /// # Time Complexity
    ///
    /// O(|pattern| + k) where k is the number of occurrences.
    ///
    /// # Example
    ///
    /// ```text
    /// let scdawg = Scdawg::<()>::from_terms(["abab", "bab"]);
    /// assert_eq!(scdawg.freq("ab"), 3); // 2 in "abab" + 1 in "bab"
    /// ```
    pub fn freq(&self, pattern: &str) -> usize {
        let inner = self.inner.read();
        inner.frequency(pattern)
    }

    /// Get the frequency at a specific SCDAWG node handle.
    ///
    /// Use this with `find()` for efficient repeated frequency queries.
    pub fn freq_at(&self, handle: &ScdawgNodeHandle<V>) -> usize {
        let inner = self.inner.read();
        let mut count = 0;
        inner.count_occurrences(handle.node_idx, &mut count);
        count
    }

    /// Get all occurrence locations of a substring pattern.
    ///
    /// This is the `locations(x)` operation from Blumer et al. (1987).
    /// Returns (term, start_position) pairs for every occurrence.
    ///
    /// # Time Complexity
    ///
    /// O(|pattern| + k) where k is the number of occurrences.
    ///
    /// # Example
    ///
    /// ```text
    /// let scdawg = Scdawg::<()>::from_terms(["abab"]);
    /// let locs = scdawg.locations("ab");
    /// // Returns: [("abab", 0), ("abab", 2)]
    /// ```
    pub fn locations(&self, pattern: &str) -> Vec<(String, usize)> {
        let inner = self.inner.read();
        inner.find_exact_substring(pattern)
    }

    /// Get all occurrence locations from a specific SCDAWG node handle.
    ///
    /// Use this with `find()` for efficient repeated location queries.
    pub fn locations_at(
        &self,
        handle: &ScdawgNodeHandle<V>,
        pattern_len: usize,
    ) -> Vec<(String, usize)> {
        let inner = self.inner.read();
        let mut results = Vec::new();
        inner.collect_term_positions(handle.node_idx, pattern_len, &mut results);
        results
    }
}

// ============================================================================
// Dictionary Trait Implementation
// ============================================================================

impl<V: DictionaryValue> Dictionary for Scdawg<V> {
    type Node = ScdawgNodeHandle<V>;

    fn len(&self) -> Option<usize> {
        Some(self.inner.read().term_count())
    }

    fn contains(&self, term: &str) -> bool {
        self.inner.read().contains(term)
    }

    fn root(&self) -> Self::Node {
        ScdawgNodeHandle {
            inner: Arc::clone(&self.inner),
            node_idx: 0,
        }
    }

    fn sync_strategy(&self) -> crate::SyncStrategy {
        crate::SyncStrategy::ExternalSync
    }
}

impl<V: DictionaryValue> crate::MappedDictionary for Scdawg<V> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        // Delegate to the inherent method.
        Self::get_value(self, term)
    }
}

// ============================================================================
// Node Handle
// ============================================================================

/// Handle to a node in the true SCDAWG.
#[derive(Clone)]
pub struct ScdawgNodeHandle<V: DictionaryValue = ()> {
    inner: Arc<RwLock<ScdawgInner<V>>>,
    node_idx: usize,
}

impl<V: DictionaryValue> std::fmt::Debug for ScdawgNodeHandle<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScdawgNodeHandle")
            .field("node_idx", &self.node_idx)
            .finish()
    }
}

impl<V: DictionaryValue> DictionaryNode for ScdawgNodeHandle<V> {
    type Unit = u8;

    fn is_final(&self) -> bool {
        let inner = self.inner.read();
        inner.nodes[self.node_idx].is_final
    }

    fn transition(&self, label: u8) -> Option<Self> {
        let inner = self.inner.read();
        inner.nodes[self.node_idx]
            .get_edge(label)
            .map(|idx| ScdawgNodeHandle {
                inner: Arc::clone(&self.inner),
                node_idx: idx,
            })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (u8, Self)> + '_> {
        let inner = self.inner.read();
        let edges: Vec<_> = inner.nodes[self.node_idx]
            .forward_edges
            .iter()
            .map(|&(label, idx)| {
                (
                    label,
                    ScdawgNodeHandle {
                        inner: Arc::clone(&self.inner),
                        node_idx: idx,
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }

    fn edge_count(&self) -> Option<usize> {
        let inner = self.inner.read();
        Some(inner.nodes[self.node_idx].forward_edges.len())
    }
}

unsafe impl<V: DictionaryValue> Send for ScdawgNodeHandle<V> {}
unsafe impl<V: DictionaryValue> Sync for ScdawgNodeHandle<V> {}

// ============================================================================
// BidirectionalDictionaryNode Implementation
// ============================================================================

impl<V: DictionaryValue> BidirectionalDictionaryNode for ScdawgNodeHandle<V> {
    fn parent(&self) -> Option<Self> {
        let inner = self.inner.read();
        let node = &inner.nodes[self.node_idx];
        if node.parent == NIL {
            None
        } else {
            Some(ScdawgNodeHandle {
                inner: Arc::clone(&self.inner),
                node_idx: node.parent,
            })
        }
    }

    fn parent_label(&self) -> Option<u8> {
        let inner = self.inner.read();
        let node = &inner.nodes[self.node_idx];
        if node.parent == NIL {
            None
        } else {
            Some(node.parent_label)
        }
    }

    fn reverse_edges(&self) -> Box<dyn Iterator<Item = (u8, Self)> + '_> {
        let inner = self.inner.read();
        let edges: Vec<_> = inner.nodes[self.node_idx]
            .left_edges
            .iter()
            .map(|&(label, idx)| {
                (
                    label,
                    ScdawgNodeHandle {
                        inner: Arc::clone(&self.inner),
                        node_idx: idx,
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }

    fn reverse_transition(&self, label: u8) -> Vec<Self> {
        let inner = self.inner.read();
        inner.nodes[self.node_idx]
            .left_edges
            .iter()
            .filter(|(l, _)| *l == label)
            .map(|(_, idx)| ScdawgNodeHandle {
                inner: Arc::clone(&self.inner),
                node_idx: *idx,
            })
            .collect()
    }

    fn depth(&self) -> usize {
        let inner = self.inner.read();
        inner.nodes[self.node_idx].depth
    }
}

// ============================================================================
// SubstringDictionary Implementation
// ============================================================================

impl<V: DictionaryValue> SubstringDictionary for Scdawg<V> {
    fn find_exact_substring(&self, pattern: &str) -> Vec<SubstringMatch<Self::Node>> {
        let inner = self.inner.read();
        let occurrences = inner.find_exact_substring(pattern);

        occurrences
            .into_iter()
            .map(|(term, position)| {
                // Find the node at the end of the pattern match
                let mut node_idx = 0;
                for &byte in term.as_bytes().iter().take(position + pattern.len()) {
                    if let Some(next) = inner.nodes[node_idx].get_edge(byte) {
                        node_idx = next;
                    }
                }

                SubstringMatch::new(
                    ScdawgNodeHandle {
                        inner: Arc::clone(&self.inner),
                        node_idx,
                    },
                    term,
                    position,
                    pattern.len(),
                )
            })
            .collect()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use log::debug;

    #[test]
    fn test_scdawg_empty() {
        let scdawg = Scdawg::<()>::new();
        assert_eq!(scdawg.term_count(), 0);
        assert!(!scdawg.contains("anything"));
    }

    #[test]
    fn test_scdawg_insert_single() {
        let scdawg = Scdawg::<()>::new();
        assert!(scdawg.insert("hello"));
        assert!(!scdawg.insert("hello")); // Duplicate
        assert_eq!(scdawg.term_count(), 1);
        assert!(scdawg.contains("hello"));
    }

    #[test]
    fn test_scdawg_substring_search() {
        let scdawg = Scdawg::<()>::from_terms(vec!["cathedral", "category", "catering"]);

        // Test substring existence
        assert!(scdawg.contains_substring("cat"));
        assert!(scdawg.contains_substring("the"));
        assert!(scdawg.contains_substring("edral"));
        assert!(scdawg.contains_substring("gory"));
        assert!(!scdawg.contains_substring("xyz"));
    }

    #[test]
    fn test_scdawg_find_exact_substring() {
        let scdawg = Scdawg::<()>::from_terms(vec!["hello", "world"]);

        let matches = scdawg.find_exact_substring("hello");
        assert!(!matches.is_empty());
        assert!(matches.iter().any(|m| m.term == "hello" && m.position == 0));
    }

    #[test]
    fn test_scdawg_internal_substring() {
        let scdawg = Scdawg::<()>::from_terms(vec!["cathedral"]);

        // Test internal substrings
        assert!(scdawg.contains_substring("thedr"));
        assert!(scdawg.contains_substring("hedr"));
        assert!(scdawg.contains_substring("edra"));
    }

    #[test]
    fn test_scdawg_multiple_terms() {
        let scdawg = Scdawg::<()>::from_terms(vec!["abc", "bcd", "cde"]);

        // Each term should be found
        assert!(scdawg.contains("abc"));
        assert!(scdawg.contains("bcd"));
        assert!(scdawg.contains("cde"));

        // Common substrings
        assert!(scdawg.contains_substring("bc")); // In abc and bcd
        assert!(scdawg.contains_substring("cd")); // In bcd and cde
    }

    #[test]
    fn test_scdawg_iter() {
        let terms = vec!["apple", "banana", "cherry"];
        let scdawg = Scdawg::<()>::from_terms(terms.clone());

        let collected: Vec<_> = scdawg.iter().collect();
        assert_eq!(collected.len(), 3);
        for term in terms {
            assert!(collected.contains(&term.to_string()));
        }
    }

    /// Test that left extension edges are computed from suffix links.
    ///
    /// Left extension edges are derived from suffix links: if node A has a suffix link
    /// to node B, then B gets a left extension edge pointing to A with label = A's first_char.
    ///
    /// For a single term "abc", all suffix states collapse into equivalence classes,
    /// so no intermediate nodes have suffix links pointing to them. Left extension
    /// edges only appear when multiple terms share suffixes.
    #[test]
    fn test_left_extension_edges() {
        use crate::substring::BidirectionalDictionaryNode;
        use crate::Dictionary;

        // For left extension edges to exist, we need multiple terms sharing suffixes.
        // "abc" and "dbc" both end in "bc", so the node representing "bc" should have
        // left extension edges for both 'a' (to "abc") and 'd' (to "dbc").
        let scdawg = Scdawg::<()>::from_terms(vec!["abc", "dbc"]);

        // Navigate to the node representing "bc" via root -> 'b' -> 'c'
        let root = scdawg.root();
        let node_b = root
            .transition(b'b')
            .expect("Should have edge 'b' from root");
        let node_bc = node_b
            .transition(b'c')
            .expect("Should have edge 'c' from 'b'");

        // The left extension edges from "bc" should have labels 'a' and 'd'
        let left_edges: Vec<_> = node_bc.reverse_edges().collect();
        let labels: std::collections::HashSet<_> = left_edges.iter().map(|(l, _)| *l).collect();

        // Check for left extension edge with label 'a' (from "abc" suffix linking to "bc")
        assert!(
            labels.contains(&b'a'),
            "Node 'bc' should have left extension edge with label 'a'. \
             Found edges: {:?}",
            left_edges
                .iter()
                .map(|(l, _)| *l as char)
                .collect::<Vec<_>>()
        );

        // Check for left extension edge with label 'd' (from "dbc" suffix linking to "bc")
        assert!(
            labels.contains(&b'd'),
            "Node 'bc' should have left extension edge with label 'd'. \
             Found edges: {:?}",
            left_edges
                .iter()
                .map(|(l, _)| *l as char)
                .collect::<Vec<_>>()
        );
    }

    // =========================================================================
    // IS Features Tests (Blumer et al. 1987)
    // =========================================================================

    #[test]
    fn debug_abab_structure() {
        let scdawg = Scdawg::<()>::from_terms(vec!["abab"]);
        let inner = scdawg.inner.read();

        // Print all nodes with term_ends
        debug!("Node structure for 'abab':");
        for (i, node) in inner.nodes.iter().enumerate() {
            debug!(
                "Node {}: length={}, term_ends={:?}, edges={:?}",
                i,
                node.length,
                node.term_ends,
                node.forward_edges
                    .iter()
                    .map(|(l, t)| (*l as char, *t))
                    .collect::<Vec<_>>()
            );
        }

        // Navigate to "ab" and check what we find
        let ab_node = inner.find_substring_fast("ab").unwrap();
        debug!("Node for 'ab': {}", ab_node);
        debug!("term_ends at 'ab': {:?}", inner.nodes[ab_node].term_ends);
        debug!("children of 'ab': {:?}", inner.nodes[ab_node].forward_edges);

        // Try counting manually
        let mut results = Vec::new();
        inner.collect_term_positions(ab_node, 2, &mut results);
        debug!("Collected positions: {:?}", results);
    }

    #[test]
    fn test_is_find() {
        let scdawg = Scdawg::<()>::from_terms(vec!["cathedral", "category"]);

        // Should find common prefix
        assert!(scdawg.find("cat").is_some());

        // Should find internal substring
        assert!(scdawg.find("the").is_some());

        // Should not find non-existent pattern
        assert!(scdawg.find("xyz").is_none());
    }

    #[test]
    fn test_is_freq_single_term() {
        let scdawg = Scdawg::<()>::from_terms(vec!["abab"]);

        // "ab" appears twice in "abab": at positions 0 and 2
        assert_eq!(
            scdawg.freq("ab"),
            2,
            "Pattern 'ab' should appear twice in 'abab'"
        );

        // "a" appears twice in "abab": at positions 0 and 2
        assert_eq!(
            scdawg.freq("a"),
            2,
            "Pattern 'a' should appear twice in 'abab'"
        );

        // "b" appears twice in "abab": at positions 1 and 3
        assert_eq!(
            scdawg.freq("b"),
            2,
            "Pattern 'b' should appear twice in 'abab'"
        );

        // "abab" appears once
        assert_eq!(scdawg.freq("abab"), 1, "Pattern 'abab' should appear once");

        // Non-existent pattern
        assert_eq!(
            scdawg.freq("xyz"),
            0,
            "Non-existent pattern should have freq 0"
        );
    }

    #[test]
    fn test_is_freq_multiple_terms() {
        let scdawg = Scdawg::<()>::from_terms(vec!["abc", "bcd", "cde"]);

        // "bc" appears in "abc" (pos 1) and "bcd" (pos 0) = 2 occurrences
        assert_eq!(scdawg.freq("bc"), 2, "Pattern 'bc' should appear twice");

        // "cd" appears in "bcd" (pos 1) and "cde" (pos 0) = 2 occurrences
        assert_eq!(scdawg.freq("cd"), 2, "Pattern 'cd' should appear twice");

        // "c" appears in all three terms
        assert_eq!(scdawg.freq("c"), 3, "Pattern 'c' should appear three times");
    }

    #[test]
    fn test_is_locations() {
        let scdawg = Scdawg::<()>::from_terms(vec!["abab"]);

        let locs = scdawg.locations("ab");

        // Should find "ab" at positions 0 and 2 in "abab"
        assert_eq!(locs.len(), 2, "Should find 2 occurrences of 'ab'");

        let positions: std::collections::HashSet<_> = locs.iter().map(|(_, pos)| *pos).collect();
        assert!(positions.contains(&0), "Should find 'ab' at position 0");
        assert!(positions.contains(&2), "Should find 'ab' at position 2");
    }

    #[test]
    fn test_is_locations_multiple_terms() {
        let scdawg = Scdawg::<()>::from_terms(vec!["cat", "cathedral", "scatter"]);

        let locs = scdawg.locations("cat");

        // Debug: print what we found
        debug!("Locations of 'cat': {:?}", locs);

        // "cat" appears at:
        // - "cat" position 0
        // - "cathedral" position 0
        // - "scatter" position 2
        let term_positions: std::collections::HashSet<_> = locs
            .iter()
            .map(|(term, pos)| (term.as_str(), *pos))
            .collect();

        assert!(
            term_positions.contains(&("cat", 0)),
            "Should find 'cat' at position 0 in 'cat'"
        );
        assert!(
            term_positions.contains(&("cathedral", 0)),
            "Should find 'cat' at position 0 in 'cathedral'"
        );

        // Note: "scatter" contains "cat" starting at position 2 (s-c-a-t-t-e-r, indices 2,3,4)
        // Wait, let me verify: "scatter" = s(0) c(1) a(2) t(3) t(4) e(5) r(6)
        // So "cat" would be at positions... c(1) a(2) t(3), starting at index 1, not 2!
        // Let me fix the test
        assert!(
            term_positions.contains(&("scatter", 1)),
            "Should find 'cat' at position 1 in 'scatter'. Found: {:?}",
            term_positions
        );
    }

    #[test]
    fn test_is_freq_at_and_locations_at() {
        let scdawg = Scdawg::<()>::from_terms(vec!["abab", "bab"]);

        // First find the pattern
        let handle = scdawg.find("ab").expect("Should find 'ab'");

        // Then get frequency at that handle
        let freq = scdawg.freq_at(&handle);
        assert!(freq >= 2, "Should have at least 2 occurrences of 'ab'");

        // And locations at that handle
        let locs = scdawg.locations_at(&handle, 2);
        assert!(!locs.is_empty(), "Should have locations for 'ab'");
    }

    /// Test left extensions with multiple terms sharing suffixes
    #[test]
    fn test_left_extension_multiple_terms() {
        use crate::substring::BidirectionalDictionaryNode;
        use crate::Dictionary;

        // "abc" and "xbc" share suffix "bc"
        let scdawg = Scdawg::<()>::from_terms(vec!["abc", "xbc"]);

        // Navigate to "bc" node
        let root = scdawg.root();
        let node_b = root.transition(b'b').expect("Should have edge 'b'");
        let node_bc = node_b.transition(b'c').expect("Should have edge 'c'");

        // "bc" should have left extensions for both 'a' (-> "abc") and 'x' (-> "xbc")
        let left_edges: Vec<_> = node_bc.reverse_edges().collect();
        let labels: std::collections::HashSet<_> = left_edges.iter().map(|(l, _)| *l).collect();

        assert!(
            labels.contains(&b'a'),
            "Node 'bc' should have left extension 'a' -> 'abc'"
        );
        assert!(
            labels.contains(&b'x'),
            "Node 'bc' should have left extension 'x' -> 'xbc'"
        );
    }
}
