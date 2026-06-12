//! Character-level SCDAWG implementation with Unicode support.
//!
//! This module implements an SCDAWG (Symmetric Compact Directed Acyclic Word Graph)
//! that operates on Unicode scalar values (`char`) instead of bytes (`u8`).
//!
//! # When to Use ScdawgChar
//!
//! Use `ScdawgChar` when:
//! - Working with non-ASCII text (accented characters, CJK, emoji, etc.)
//! - You need correct character-level Levenshtein distances
//! - Pattern pieces for WallBreaker should be character-aligned
//!
//! # Features
//!
//! - **O(|pattern|) substring search**: True suffix automaton indexing ALL substrings
//! - **Left extension edges**: Bidirectional traversal via sext links
//! - **IS features**: freq(), locations() operations from Blumer et al. (1987)
//! - **Unicode support**: Proper character-level semantics
//!
//! # Performance Trade-offs
//!
//! Compared to byte-level `Scdawg`:
//! - **Memory**: ~4x edge label storage (4 bytes per `char` vs 1 byte per `u8`)
//! - **Speed**: Slightly slower due to larger edge labels
//! - **Correctness**: Proper Unicode semantics (e.g., "café" has 4 characters, not 5 bytes)
//!
//! # Example
//!
//! ```rust
//! use libdictenstein::scdawg::char::ScdawgChar;
//! use libdictenstein::SubstringDictionary;
//!
//! // Create a Unicode-aware SCDAWG
//! let scdawg = ScdawgChar::<()>::from_terms(["café", "naïve", "中文"]);
//!
//! // O(|pattern|) substring search
//! assert!(scdawg.contains_substring("afé"));
//! assert!(scdawg.contains_substring("中"));
//!
//! // Find all occurrences
//! let matches = scdawg.find_exact_substring("afé");
//! assert_eq!(matches.len(), 1);
//! assert_eq!(matches[0].position, 1);  // Position 1 in characters, not bytes
//! ```

use std::sync::Arc;

use crate::substring::{BidirectionalDictionaryNode, SubstringDictionary, SubstringMatch};
use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;
use crate::{Dictionary, DictionaryNode};

/// Sentinel value for "no suffix link" or "no parent".
const NIL: usize = usize::MAX;

// ============================================================================
// True SCDAWG Char Node
// ============================================================================

// C4 step: byte-for-byte-identical local `ScdawgCharNode<V>` struct
// + 4-method impl block (root/new/get_edge/set_edge) replaced with a
// type alias to the generic `super::core::ScdawgNode<char, V>`.
// The canonical impl additionally provides `is_root()` which the char
// variant didn't previously have — harmless addition. Clone + Debug
// derives live on the generic struct, so the alias inherits them.
#[allow(dead_code)]
type ScdawgCharNode<V = ()> = super::core::ScdawgNode<char, V>;

// ============================================================================
// True SCDAWG Char Inner State
// ============================================================================

// C4c algorithmic dedup (char SCDAWG): byte-for-byte-identical local
// ScdawgCharInner<V> struct + ~300-LOC impl block replaced with a type
// alias to the generic super::core::ScdawgCoreInner<char, V>.
// Mirror of C4b for the char-keyed variant.
type ScdawgCharInner<V = ()> = super::core::ScdawgCoreInner<char, V>;

// C4c: the original ~300-LOC impl<V> ScdawgCharInner<V> block lived
// here. All algorithmic methods are now on the canonical generic
// super::core::ScdawgCoreInner<char, V>.

// ============================================================================
// Public ScdawgChar Type
// ============================================================================

/// Unicode-aware Symmetric Compact DAWG with O(|pattern|) substring search.
///
/// This is a proper suffix automaton implementation that indexes ALL substrings
/// of all terms, enabling efficient substring search and bidirectional extension.
/// Uses `char` for edge labels to support Unicode text.
#[derive(Clone, Debug)]
pub struct ScdawgChar<V: DictionaryValue = ()> {
    inner: Arc<RwLock<ScdawgCharInner<V>>>,
}

impl<V: DictionaryValue> Default for ScdawgChar<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> ScdawgChar<V> {
    /// Create a new empty Unicode-aware SCDAWG.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(ScdawgCharInner::new())),
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
        let total_chars: usize = terms_vec.iter().map(|s| s.as_ref().chars().count()).sum();

        let inner = ScdawgCharInner::with_capacity(term_count, total_chars);
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

    /// Create from an iterator of (term, value) pairs.
    pub fn from_terms_with_values<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        let scdawg = ScdawgChar::new();
        for (term, value) in terms {
            scdawg.insert_with_value(term.as_ref(), value);
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

    /// Get the number of nodes in the SCDAWG.
    pub fn node_count(&self) -> usize {
        self.inner.read().nodes.len()
    }

    /// Get the value associated with a term.
    pub fn get_value(&self, term: &str) -> Option<V>
    where
        V: Clone,
    {
        let inner = self.inner.read();
        if let Some(value) = inner.term_values.get(term) {
            return Some(value.clone());
        }

        let mut current = 0;
        for ch in term.chars() {
            match inner.nodes[current].get_edge(ch) {
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

    // ========================================================================
    // IS Features (Blumer et al. 1987)
    // ========================================================================

    /// Find a substring and return a handle to its SCDAWG state.
    ///
    /// This is the `find(x)` operation from Blumer et al. (1987).
    pub fn find(&self, pattern: &str) -> Option<ScdawgCharNodeHandle<V>> {
        let inner = self.inner.read();
        inner
            .find_substring_fast(pattern)
            .map(|node_idx| ScdawgCharNodeHandle {
                inner: Arc::clone(&self.inner),
                node_idx,
            })
    }

    /// Get the frequency (occurrence count) of a substring pattern.
    ///
    /// This is the `freq(x)` operation from Blumer et al. (1987).
    pub fn freq(&self, pattern: &str) -> usize {
        let inner = self.inner.read();
        inner.frequency(pattern)
    }

    /// Get the frequency at a specific SCDAWG node handle.
    pub fn freq_at(&self, handle: &ScdawgCharNodeHandle<V>) -> usize {
        let inner = self.inner.read();
        let mut count = 0;
        inner.count_occurrences(handle.node_idx, &mut count);
        count
    }

    /// Get all occurrence locations of a substring pattern.
    ///
    /// This is the `locations(x)` operation from Blumer et al. (1987).
    /// Returns (term, start_position) pairs where position is in characters.
    pub fn locations(&self, pattern: &str) -> Vec<(String, usize)> {
        let inner = self.inner.read();
        inner.find_exact_substring(pattern)
    }

    /// Get all occurrence locations from a specific SCDAWG node handle.
    pub fn locations_at(
        &self,
        handle: &ScdawgCharNodeHandle<V>,
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

impl<V: DictionaryValue> Dictionary for ScdawgChar<V> {
    type Node = ScdawgCharNodeHandle<V>;

    fn len(&self) -> Option<usize> {
        Some(self.inner.read().term_count())
    }

    fn contains(&self, term: &str) -> bool {
        self.inner.read().contains(term)
    }

    fn root(&self) -> Self::Node {
        ScdawgCharNodeHandle {
            inner: Arc::clone(&self.inner),
            node_idx: 0,
        }
    }

    fn sync_strategy(&self) -> crate::SyncStrategy {
        crate::SyncStrategy::ExternalSync
    }
}

impl<V: DictionaryValue> crate::MappedDictionary for ScdawgChar<V> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        Self::get_value(self, term)
    }
}

// ============================================================================
// Node Handle
// ============================================================================

/// Handle to a node in the Unicode-aware SCDAWG.
#[derive(Clone)]
pub struct ScdawgCharNodeHandle<V: DictionaryValue = ()> {
    inner: Arc<RwLock<ScdawgCharInner<V>>>,
    node_idx: usize,
}

impl<V: DictionaryValue> std::fmt::Debug for ScdawgCharNodeHandle<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScdawgCharNodeHandle")
            .field("node_idx", &self.node_idx)
            .finish()
    }
}

impl<V: DictionaryValue> DictionaryNode for ScdawgCharNodeHandle<V> {
    type Unit = char;

    fn is_final(&self) -> bool {
        let inner = self.inner.read();
        inner.nodes[self.node_idx].is_final
    }

    fn transition(&self, label: char) -> Option<Self> {
        let inner = self.inner.read();
        inner.nodes[self.node_idx]
            .get_edge(label)
            .map(|idx| ScdawgCharNodeHandle {
                inner: Arc::clone(&self.inner),
                node_idx: idx,
            })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (char, Self)> + '_> {
        let inner = self.inner.read();
        let edges: Vec<_> = inner.nodes[self.node_idx]
            .forward_edges
            .iter()
            .map(|&(label, idx)| {
                (
                    label,
                    ScdawgCharNodeHandle {
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

unsafe impl<V: DictionaryValue> Send for ScdawgCharNodeHandle<V> {}
unsafe impl<V: DictionaryValue> Sync for ScdawgCharNodeHandle<V> {}

// ============================================================================
// BidirectionalDictionaryNode Implementation
// ============================================================================

impl<V: DictionaryValue> BidirectionalDictionaryNode for ScdawgCharNodeHandle<V> {
    fn parent(&self) -> Option<Self> {
        let inner = self.inner.read();
        let node = &inner.nodes[self.node_idx];
        if node.parent == NIL {
            None
        } else {
            Some(ScdawgCharNodeHandle {
                inner: Arc::clone(&self.inner),
                node_idx: node.parent,
            })
        }
    }

    fn parent_label(&self) -> Option<char> {
        let inner = self.inner.read();
        let node = &inner.nodes[self.node_idx];
        if node.parent == NIL {
            None
        } else {
            Some(node.parent_label)
        }
    }

    fn reverse_edges(&self) -> Box<dyn Iterator<Item = (char, Self)> + '_> {
        let inner = self.inner.read();
        let edges: Vec<_> = inner.nodes[self.node_idx]
            .left_edges
            .iter()
            .map(|&(label, idx)| {
                (
                    label,
                    ScdawgCharNodeHandle {
                        inner: Arc::clone(&self.inner),
                        node_idx: idx,
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }

    fn reverse_transition(&self, label: char) -> Vec<Self> {
        let inner = self.inner.read();
        inner.nodes[self.node_idx]
            .left_edges
            .iter()
            .filter(|(l, _)| *l == label)
            .map(|(_, idx)| ScdawgCharNodeHandle {
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

impl<V: DictionaryValue> SubstringDictionary for ScdawgChar<V> {
    fn find_exact_substring(&self, pattern: &str) -> Vec<SubstringMatch<Self::Node>> {
        let inner = self.inner.read();
        let occurrences = inner.find_exact_substring(pattern);
        let pattern_len = pattern.chars().count();

        occurrences
            .into_iter()
            .map(|(term, position)| {
                // Find the node at the end of the pattern match
                let mut node_idx = 0;
                for ch in term.chars().take(position + pattern_len) {
                    if let Some(next) = inner.nodes[node_idx].get_edge(ch) {
                        node_idx = next;
                    }
                }

                SubstringMatch::new(
                    ScdawgCharNodeHandle {
                        inner: Arc::clone(&self.inner),
                        node_idx,
                    },
                    term,
                    position,
                    pattern_len,
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

    #[test]
    fn test_scdawg_char_empty() {
        let scdawg = ScdawgChar::<()>::new();
        assert_eq!(scdawg.term_count(), 0);
        assert!(!scdawg.contains("anything"));
    }

    #[test]
    fn test_scdawg_char_insert_single() {
        let scdawg = ScdawgChar::<()>::new();
        assert!(scdawg.insert("hello"));
        assert!(!scdawg.insert("hello")); // Duplicate
        assert_eq!(scdawg.term_count(), 1);
        assert!(scdawg.contains("hello"));
    }

    #[test]
    fn test_scdawg_char_unicode() {
        let scdawg = ScdawgChar::<()>::from_terms(vec!["café", "naïve", "中文"]);
        assert_eq!(scdawg.term_count(), 3);
        assert!(scdawg.contains("café"));
        assert!(scdawg.contains("naïve"));
        assert!(scdawg.contains("中文"));
        assert!(!scdawg.contains("cafe")); // Without accent
    }

    #[test]
    fn test_scdawg_char_substring_search() {
        let scdawg = ScdawgChar::<()>::from_terms(vec!["café"]);

        // Test O(|pattern|) substring search
        assert!(scdawg.contains_substring("afé"));
        assert!(scdawg.contains_substring("ca"));
        assert!(scdawg.contains_substring("fé"));
        assert!(!scdawg.contains_substring("xyz"));
    }

    #[test]
    fn test_scdawg_char_find_exact_substring() {
        let scdawg = ScdawgChar::<()>::from_terms(vec!["café"]);
        let matches = scdawg.find_exact_substring("afé");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].term, "café");
        assert_eq!(matches[0].position, 1); // Character position, not byte
        assert_eq!(matches[0].length, 3); // 3 characters
    }

    #[test]
    fn test_scdawg_char_cjk() {
        let scdawg = ScdawgChar::<()>::from_terms(vec!["中文字"]);

        assert!(scdawg.contains_substring("中"));
        assert!(scdawg.contains_substring("中文"));
        assert!(scdawg.contains_substring("文字"));
        assert!(scdawg.contains_substring("中文字"));

        let matches = scdawg.find_exact_substring("文");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].position, 1); // Position 1 in characters
    }

    #[test]
    fn test_scdawg_char_bidirectional() {
        let scdawg = ScdawgChar::<()>::from_terms(vec!["中文"]);

        let root = scdawg.root();
        let zhong = root.transition('中').unwrap();
        let wen = zhong.transition('文').unwrap();

        assert!(wen.is_final());
        assert_eq!(wen.depth(), 2);

        // Walk back
        let back = wen.parent().unwrap();
        assert_eq!(wen.parent_label(), Some('文'));
        assert_eq!(back.depth(), 1);

        let back_root = back.parent().unwrap();
        assert!(back_root.parent().is_none());
    }

    #[test]
    fn test_scdawg_char_path_string() {
        let scdawg = ScdawgChar::<()>::from_terms(vec!["café"]);

        let root = scdawg.root();
        let c = root.transition('c').unwrap();
        let a = c.transition('a').unwrap();
        let f = a.transition('f').unwrap();
        let e = f.transition('é').unwrap();

        assert_eq!(e.path_string(), "café");
        assert_eq!(a.path_string(), "ca");
    }

    #[test]
    fn test_scdawg_char_with_values() {
        let scdawg = ScdawgChar::<u32>::new();
        scdawg.insert_with_value("日本語", 42);

        assert_eq!(scdawg.get_value("日本語"), Some(42));
        assert_eq!(scdawg.get_value("日本"), None);
    }

    #[test]
    fn test_scdawg_char_emoji() {
        let scdawg = ScdawgChar::<()>::from_terms(vec!["hello🎉world"]);

        assert!(scdawg.contains("hello🎉world"));
        assert_eq!(scdawg.term_count(), 1);

        // Emoji is 1 character
        let matches = scdawg.find_exact_substring("🎉");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].position, 5); // After "hello"
    }

    #[test]
    fn test_scdawg_char_multiple_terms() {
        let scdawg = ScdawgChar::<()>::from_terms(vec!["abc", "bcd", "cde"]);

        // Each term should be found
        assert!(scdawg.contains("abc"));
        assert!(scdawg.contains("bcd"));
        assert!(scdawg.contains("cde"));

        // Common substrings
        assert!(scdawg.contains_substring("bc")); // In abc and bcd
        assert!(scdawg.contains_substring("cd")); // In bcd and cde
    }

    #[test]
    fn test_scdawg_char_is_freq() {
        let scdawg = ScdawgChar::<()>::from_terms(vec!["abab"]);

        // "ab" appears twice in "abab": at positions 0 and 2
        assert_eq!(scdawg.freq("ab"), 2);

        // "a" appears twice
        assert_eq!(scdawg.freq("a"), 2);

        // Non-existent pattern
        assert_eq!(scdawg.freq("xyz"), 0);
    }

    #[test]
    fn test_scdawg_char_is_locations() {
        let scdawg = ScdawgChar::<()>::from_terms(vec!["abab"]);

        let locs = scdawg.locations("ab");

        // Should find "ab" at positions 0 and 2 in "abab"
        assert_eq!(locs.len(), 2);

        let positions: std::collections::HashSet<_> = locs.iter().map(|(_, pos)| *pos).collect();
        assert!(positions.contains(&0));
        assert!(positions.contains(&2));
    }

    #[test]
    fn test_scdawg_char_left_extension_edges() {
        // "abc" and "dbc" share suffix "bc"
        let scdawg = ScdawgChar::<()>::from_terms(vec!["abc", "dbc"]);

        // Navigate to "bc" node
        let root = scdawg.root();
        let node_b = root.transition('b').expect("Should have edge 'b'");
        let node_bc = node_b.transition('c').expect("Should have edge 'c'");

        // "bc" should have left extensions for both 'a' and 'd'
        let left_edges: Vec<_> = node_bc.reverse_edges().collect();
        let labels: std::collections::HashSet<_> = left_edges.iter().map(|(l, _)| *l).collect();

        assert!(labels.contains(&'a'), "Should have left extension 'a'");
        assert!(labels.contains(&'d'), "Should have left extension 'd'");
    }
}
