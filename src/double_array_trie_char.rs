//! Character-level Double-Array Trie implementation for proper Unicode support.
//!
//! This module provides a character-based variant of Double-Array Trie that operates
//! at the Unicode character level rather than byte level. This ensures correct edit
//! distance semantics for multi-byte UTF-8 sequences.
//!
//! ## Differences from DoubleArrayTrie
//!
//! - Edge labels are `char` (4 bytes) instead of `u8` (1 byte)
//! - Distance calculations count characters, not bytes
//! - Correct semantics: "" → "¡" is distance 1, not 2
//!
//! ## Performance Trade-offs
//!
//! - **Memory**: 4x larger edge labels (char vs u8)
//! - **Speed**: Slightly slower (~10-15%) due to larger data
//! - **Correctness**: Proper Unicode semantics
//!
//! ## Use Cases
//!
//! Use `DoubleArrayTrieChar` when:
//! - Dictionary contains non-ASCII Unicode characters
//! - Edit distance must be measured in characters, not bytes
//! - Correctness is more important than maximum performance

use crate::double_array_trie_char_zipper::DoubleArrayTrieCharZipper;
use crate::iterator::DictionaryIterator;
use crate::value::DictionaryValue;
use crate::{Dictionary, DictionaryNode, MappedDictionary, MappedDictionaryNode};
use std::sync::Arc;

// serde helpers for Arc<Vec<T>> / Arc<Vec<Vec<T>>> round-tripping moved to
// `crate::serialization::serde_helpers` (C2 dedup). See sibling
// `double_array_trie.rs` for the same import pattern.
#[cfg(feature = "serialization")]
#[allow(unused_imports)]
use crate::serialization::serde_helpers::{
    deserialize_arc_vec, deserialize_arc_vec_vec, serialize_arc_vec, serialize_arc_vec_vec,
};

// C5 step 3: char DAT's local `DATSharedChar<V>` struct is now a type
// alias for the generic `crate::dat_core::DATCoreShared<char, V>`. The
// fields are byte-for-byte identical (same Arc<Vec<i32>>,
// Arc<Vec<bool>>, Arc<Vec<Vec<char>>>, Arc<Vec<Option<V>>>) and the
// serde plumbing flows through `crate::serialization::serde_helpers`.
//
// Call-sites throughout this file continue to reference `DATSharedChar<V>`
// unchanged.
pub(crate) type DATSharedChar<V = ()> = crate::dat_core::DATCoreShared<char, V>;

/// Character-level Double-Array Trie for proper Unicode support.
///
/// This variant operates at the Unicode character level, ensuring correct
/// edit distance calculations for multi-byte UTF-8 sequences.
///
/// # Type Parameters
///
/// * `V` - The type of values associated with dictionary terms (default: `()`)
///
/// # Example
///
/// ```
/// use libdictenstein::double_array_trie_char::DoubleArrayTrieChar;
/// use libdictenstein::Dictionary;
///
/// let terms = vec!["café", "中文", "🎉"];
/// let dict = DoubleArrayTrieChar::from_terms(terms);
///
/// assert!(dict.contains("café"));
/// assert!(dict.contains("中文"));
/// assert!(dict.contains("🎉"));
/// ```
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(
    all(feature = "serialization", not(feature = "persistent-artrie")),
    serde(bound(serialize = "V: serde::Serialize")),
    serde(bound(deserialize = "V: serde::Deserialize<'de>"))
)]
#[cfg_attr(
    all(feature = "serialization", feature = "persistent-artrie"),
    serde(bound = "")
)]
#[derive(Clone, Debug)]
pub struct DoubleArrayTrieChar<V: DictionaryValue = ()> {
    /// Shared data referenced by all nodes
    pub(crate) shared: DATSharedChar<V>,

    /// Free list for deleted/unused states.
    ///
    /// # Reserved for future dynamic operations
    ///
    /// Mirrors `DoubleArrayTrie::free_list`. Read by no code path today.
    /// Preserved here (and serialized) because the field is part of the
    /// on-disk format. See plan item B5.
    #[allow(dead_code)]
    #[cfg_attr(
        feature = "serialization",
        serde(
            serialize_with = "serialize_arc_vec",
            deserialize_with = "deserialize_arc_vec"
        )
    )]
    free_list: Arc<Vec<usize>>,

    /// Number of terms in the dictionary
    num_terms: usize,
}

impl DoubleArrayTrieChar<()> {
    /// Create a new character-level Double-Array Trie from an iterator of terms.
    ///
    /// # Example
    ///
    /// ```
    /// use libdictenstein::double_array_trie_char::DoubleArrayTrieChar;
    ///
    /// let dict = DoubleArrayTrieChar::from_terms(vec!["hello", "world", "café"]);
    /// ```
    pub fn from_terms<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut terms: Vec<Vec<char>> = terms
            .into_iter()
            .map(|s| s.as_ref().chars().collect())
            .collect();

        terms.sort_unstable();
        terms.dedup();

        let num_terms = terms.len();

        if terms.is_empty() {
            return Self::empty();
        }

        let mut builder = DATBuilderChar::new();
        for term in &terms {
            builder.insert(term, None);
        }

        let (base, check, is_final, edges, values) = builder.build();

        Self {
            shared: DATSharedChar {
                base: Arc::new(base),
                check: Arc::new(check),
                is_final: Arc::new(is_final),
                edges: Arc::new(edges),
                values: Arc::new(values),
            },
            free_list: Arc::new(Vec::new()),
            num_terms,
        }
    }

    /// Create an empty character-level Double-Array Trie.
    pub fn empty() -> Self {
        Self {
            shared: DATSharedChar {
                base: Arc::new(vec![0]),
                check: Arc::new(vec![0]),
                is_final: Arc::new(vec![false]),
                edges: Arc::new(vec![vec![]]),
                values: Arc::new(vec![None]),
            },
            free_list: Arc::new(Vec::new()),
            num_terms: 0,
        }
    }
}

impl<V: DictionaryValue> DoubleArrayTrieChar<V> {
    /// Create a character-level DAT from an iterator of (term, value) pairs.
    ///
    /// # Example
    ///
    /// ```
    /// use libdictenstein::double_array_trie_char::DoubleArrayTrieChar;
    ///
    /// let dict = DoubleArrayTrieChar::from_terms_with_values(vec![
    ///     ("café", 1),
    ///     ("中文", 2),
    /// ]);
    /// ```
    pub fn from_terms_with_values<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        let mut term_value_pairs: Vec<(Vec<char>, V)> = terms
            .into_iter()
            .map(|(s, v)| (s.as_ref().chars().collect(), v))
            .collect();

        term_value_pairs.sort_by(|a, b| a.0.cmp(&b.0));

        // Remove duplicates, keeping last value
        term_value_pairs.dedup_by(|a, b| {
            if a.0 == b.0 {
                b.1 = a.1.clone();
                true
            } else {
                false
            }
        });

        let num_terms = term_value_pairs.len();

        if term_value_pairs.is_empty() {
            return Self {
                shared: DATSharedChar {
                    base: Arc::new(vec![0]),
                    check: Arc::new(vec![0]),
                    is_final: Arc::new(vec![false]),
                    edges: Arc::new(vec![vec![]]),
                    values: Arc::new(vec![None]),
                },
                free_list: Arc::new(Vec::new()),
                num_terms: 0,
            };
        }

        let mut builder = DATBuilderChar::new();
        for (term, value) in term_value_pairs {
            builder.insert(&term, Some(value));
        }

        let (base, check, is_final, edges, values) = builder.build();

        Self {
            shared: DATSharedChar {
                base: Arc::new(base),
                check: Arc::new(check),
                is_final: Arc::new(is_final),
                edges: Arc::new(edges),
                values: Arc::new(values),
            },
            free_list: Arc::new(Vec::new()),
            num_terms,
        }
    }

    /// Create a character-level DAT from a pre-sorted iterator of (term, value) pairs.
    ///
    /// This is an optimized constructor that bypasses the O(n log n) sorting step,
    /// making it O(n * d) where n = number of terms and d = average term length.
    ///
    /// # Preconditions
    ///
    /// The input **must** be in lexicographic order. If duplicates are present,
    /// the last value wins (consistent with `from_terms_with_values`).
    ///
    /// # Performance
    ///
    /// This method streams directly into the DAT builder without collecting all
    /// terms into an intermediate vector, reducing memory usage from O(n * d)
    /// to O(d + DAT).
    ///
    /// # Example
    ///
    /// ```
    /// use libdictenstein::double_array_trie_char::DoubleArrayTrieChar;
    ///
    /// // Terms must be pre-sorted lexicographically
    /// let sorted_terms = vec![
    ///     ("apple", 1),
    ///     ("banana", 2),
    ///     ("café", 3),
    /// ];
    /// let dict = DoubleArrayTrieChar::from_sorted_terms_with_values(sorted_terms);
    /// ```
    pub fn from_sorted_terms_with_values<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        let mut builder = DATBuilderChar::new();
        let mut num_terms = 0usize;
        let mut prev_chars: Option<Vec<char>> = None;

        for (s, v) in terms {
            let chars: Vec<char> = s.as_ref().chars().collect();
            let is_duplicate = prev_chars.as_ref().is_some_and(|prev| prev == &chars);

            builder.insert(&chars, Some(v));

            if !is_duplicate {
                num_terms += 1;
            }
            prev_chars = Some(chars);
        }

        if num_terms == 0 {
            return Self {
                shared: DATSharedChar {
                    base: Arc::new(vec![0]),
                    check: Arc::new(vec![0]),
                    is_final: Arc::new(vec![false]),
                    edges: Arc::new(vec![vec![]]),
                    values: Arc::new(vec![None]),
                },
                free_list: Arc::new(Vec::new()),
                num_terms: 0,
            };
        }

        let (base, check, is_final, edges, values) = builder.build();

        Self {
            shared: DATSharedChar {
                base: Arc::new(base),
                check: Arc::new(check),
                is_final: Arc::new(is_final),
                edges: Arc::new(edges),
                values: Arc::new(values),
            },
            free_list: Arc::new(Vec::new()),
            num_terms,
        }
    }

    /// Get the value associated with a term.
    ///
    /// Returns `None` if the term doesn't exist or has no value.
    pub fn get_value(&self, term: &str) -> Option<V> {
        // Delegate to the generic `DATCoreShared::term_value_from` with
        // the char-DAT's `root_state = 0` convention (C5 algorithmic
        // dedup — char variant). The byte variant uses
        // `term_value()`/`contains_term()` which default to
        // `root_state = 1`.
        if let Some(v) = self.shared.term_value_from(term, 0) {
            return Some(v);
        }
        // Fall-through for the original "is_final but no value" case
        // — preserves the previous behavior of returning None when the
        // term reached a final state but no value was attached.
        let mut state = 0;
        for c in term.chars() {
            if state >= self.shared.base.len() {
                return None;
            }

            let base = self.shared.base[state];
            if base < 0 {
                return None;
            }

            let char_code = c as u32;
            let next = (base as u32).wrapping_add(char_code) as usize;

            if next >= self.shared.check.len() || self.shared.check[next] != state as i32 {
                return None;
            }

            state = next;
        }

        // Check if final and return value
        if state < self.shared.is_final.len() && self.shared.is_final[state] {
            self.shared.values.get(state).and_then(|v| v.clone())
        } else {
            None
        }
    }

    /// Iterate over all `(term, value)` pairs as character vectors.
    ///
    /// Returns an iterator yielding `(Vec<char>, V)` tuples in depth-first order.
    /// This is more efficient than `iter()` as it avoids String allocation.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::double_array_trie_char::DoubleArrayTrieChar;
    ///
    /// let dict = DoubleArrayTrieChar::from_terms_with_values(vec![
    ///     ("café", 1), ("naïve", 2)
    /// ]);
    ///
    /// for (chars, value) in dict.iter_chars() {
    ///     let term: String = chars.iter().collect();
    ///     println!("{} -> {}", term, value);
    /// }
    /// ```
    pub fn iter_chars(&self) -> DictionaryIterator<DoubleArrayTrieCharZipper<V>> {
        let zipper = DoubleArrayTrieCharZipper::new_from_dict(self);
        DictionaryIterator::new(zipper)
    }

    /// Iterate over all `(term, value)` pairs as UTF-8 strings.
    ///
    /// Returns an iterator yielding `(String, V)` tuples in depth-first order.
    /// For better performance with raw characters, use `iter_chars()` instead.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::double_array_trie_char::DoubleArrayTrieChar;
    ///
    /// let dict = DoubleArrayTrieChar::from_terms_with_values(vec![
    ///     ("café", 1), ("naïve", 2)
    /// ]);
    ///
    /// for (term, value) in dict.iter() {
    ///     println!("{} -> {}", term, value);
    /// }
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (String, V)> + '_ {
        self.iter_chars()
            .map(|(chars, value)| (chars.into_iter().collect::<String>(), value))
    }
}

impl<V: DictionaryValue> IntoIterator for &DoubleArrayTrieChar<V> {
    type Item = (Vec<char>, V);
    type IntoIter = DictionaryIterator<DoubleArrayTrieCharZipper<V>>;

    /// Creates an iterator over all `(term, value)` pairs as character vectors.
    fn into_iter(self) -> Self::IntoIter {
        self.iter_chars()
    }
}

impl<V: DictionaryValue> Dictionary for DoubleArrayTrieChar<V> {
    type Node = DoubleArrayTrieCharNode<V>;

    fn root(&self) -> Self::Node {
        DoubleArrayTrieCharNode {
            state: 0,
            shared: self.shared.clone(),
        }
    }

    fn len(&self) -> Option<usize> {
        Some(self.num_terms)
    }
}

/// Node reference for character-level Dictionary trait implementation.
#[derive(Clone)]
pub struct DoubleArrayTrieCharNode<V: DictionaryValue = ()> {
    /// Current state index
    state: usize,

    /// Shared data
    shared: DATSharedChar<V>,
}

impl<V: DictionaryValue> DictionaryNode for DoubleArrayTrieCharNode<V> {
    type Unit = char;

    fn is_final(&self) -> bool {
        self.state < self.shared.is_final.len() && self.shared.is_final[self.state]
    }

    fn transition(&self, label: char) -> Option<Self> {
        if self.state >= self.shared.base.len() {
            return None;
        }

        let base = self.shared.base[self.state];
        if base < 0 {
            return None;
        }

        // For char, we need to map to array index
        // Use char as u32 for computation
        let char_code = label as u32;
        let next = (base as u32).wrapping_add(char_code) as usize;

        if next < self.shared.check.len() && self.shared.check[next] == self.state as i32 {
            Some(DoubleArrayTrieCharNode {
                state: next,
                shared: self.shared.clone(),
            })
        } else {
            None
        }
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (char, Self)> + '_> {
        let state = self.state;

        if state >= self.shared.edges.len() {
            return Box::new(std::iter::empty());
        }

        let base = self.shared.base[state];
        if base < 0 {
            return Box::new(std::iter::empty());
        }

        let edges = self.shared.edges[state].clone();
        let shared = self.shared.clone();

        Box::new(edges.into_iter().filter_map(move |c| {
            let char_code = c as u32;
            let next = (base as u32).wrapping_add(char_code) as usize;

            if next < shared.check.len() && shared.check[next] == state as i32 {
                Some((
                    c,
                    DoubleArrayTrieCharNode {
                        state: next,
                        shared: shared.clone(),
                    },
                ))
            } else {
                None
            }
        }))
    }

    fn edge_count(&self) -> Option<usize> {
        if self.state < self.shared.edges.len() {
            Some(self.shared.edges[self.state].len())
        } else {
            Some(0)
        }
    }
}

/// Builder for character-level Double-Array Trie.
struct DATBuilderChar<V: DictionaryValue = ()> {
    base: Vec<i32>,
    check: Vec<i32>,
    is_final: Vec<bool>,
    edges: Vec<Vec<char>>,
    values: Vec<Option<V>>,
    used: Vec<bool>,
}

/// Type alias for the built Double-Array Trie components.
type DATCharComponents<V> = (
    Vec<i32>,       // base
    Vec<i32>,       // check
    Vec<bool>,      // is_final
    Vec<Vec<char>>, // edges
    Vec<Option<V>>, // values
);

impl<V: DictionaryValue> DATBuilderChar<V> {
    fn new() -> Self {
        Self {
            base: vec![0],
            check: vec![0],
            is_final: vec![false],
            edges: vec![vec![]],
            values: vec![None],
            used: vec![false],
        }
    }

    fn insert(&mut self, term: &[char], value: Option<V>) {
        let mut state = 0;

        for &c in term {
            state = self.get_or_create_transition(state, c);
        }

        if state < self.is_final.len() {
            self.is_final[state] = true;
        }

        // Store value
        while state >= self.values.len() {
            self.values.push(None);
        }
        self.values[state] = value;
    }

    fn get_or_create_transition(&mut self, from_state: usize, label: char) -> usize {
        // Ensure arrays are large enough
        while from_state >= self.base.len() {
            self.base.push(0);
            self.check.push(0);
            self.is_final.push(false);
            self.edges.push(vec![]);
            self.values.push(None);
            self.used.push(false);
        }

        let mut base = self.base[from_state];

        // If base is not set, find a suitable base
        if base == 0 {
            base = self.find_base(&[label]);
            self.base[from_state] = base;
        }

        let char_code = label as u32;
        let mut next_state = (base as u32).wrapping_add(char_code) as usize;

        // Ensure next_state exists
        while next_state >= self.base.len() {
            self.base.push(0);
            self.check.push(0);
            self.is_final.push(false);
            self.edges.push(vec![]);
            self.values.push(None);
            self.used.push(false);
        }

        // CRITICAL: Check for conflict before overwriting
        if self.used[next_state] && self.check[next_state] != from_state as i32 {
            // Conflict! This slot is already used by a different parent.
            // We need to relocate ALL existing children of from_state to a new base.

            // Collect all existing children
            let existing_labels = self.edges[from_state].clone();
            let mut all_labels = existing_labels.clone();

            // Add the new label if not already present
            if !all_labels.contains(&label) {
                all_labels.push(label);
            }

            // Find a new base that works for ALL labels
            let new_base = self.find_base(&all_labels);

            // Relocate existing transitions
            for &existing_label in &existing_labels {
                let old_char_code = existing_label as u32;
                let old_next = (base as u32).wrapping_add(old_char_code) as usize;
                let new_next = (new_base as u32).wrapping_add(old_char_code) as usize;

                // Ensure space for new location
                while new_next >= self.base.len() {
                    self.base.push(0);
                    self.check.push(0);
                    self.is_final.push(false);
                    self.edges.push(vec![]);
                    self.values.push(None);
                    self.used.push(false);
                }

                // Move the child's data
                self.base[new_next] = self.base[old_next];
                self.check[new_next] = from_state as i32;
                self.is_final[new_next] = self.is_final[old_next];
                self.edges[new_next] = self.edges[old_next].clone();
                self.values[new_next] = self.values[old_next].clone();
                self.used[new_next] = true;

                // Clear old location (only if it belongs to us)
                if self.check[old_next] == from_state as i32 {
                    self.check[old_next] = -1;
                    self.used[old_next] = false;
                    self.base[old_next] = 0;
                    self.is_final[old_next] = false;
                    self.edges[old_next].clear();
                    self.values[old_next] = None;
                }

                // Update any children of this relocated node
                for &child_label in &self.edges[new_next] {
                    let child_char_code = child_label as u32;
                    let child_base = self.base[new_next];
                    if child_base > 0 {
                        let child_next = (child_base as u32).wrapping_add(child_char_code) as usize;
                        if child_next < self.check.len() {
                            self.check[child_next] = new_next as i32;
                        }
                    }
                }
            }

            // Update the parent's base
            self.base[from_state] = new_base;
            base = new_base;

            // Recalculate next_state with new base
            next_state = (base as u32).wrapping_add(char_code) as usize;

            // Ensure next_state exists
            while next_state >= self.base.len() {
                self.base.push(0);
                self.check.push(0);
                self.is_final.push(false);
                self.edges.push(vec![]);
                self.values.push(None);
                self.used.push(false);
            }
        }

        // Set CHECK and mark as used
        self.check[next_state] = from_state as i32;
        self.used[next_state] = true;

        // Add edge to edge list
        if !self.edges[from_state].contains(&label) {
            self.edges[from_state].push(label);
        }

        next_state
    }

    fn find_base(&mut self, labels: &[char]) -> i32 {
        // Find a base value such that base + label is unused for all labels
        'base_search: for base in 1u32..100000 {
            for &label in labels {
                let char_code = label as u32;
                let next = base.wrapping_add(char_code) as usize;

                // Ensure we have space
                while next >= self.used.len() {
                    self.used.push(false);
                }

                if self.used[next] {
                    continue 'base_search;
                }
            }
            return base as i32;
        }

        // Fallback
        1
    }

    fn build(self) -> DATCharComponents<V> {
        (
            self.base,
            self.check,
            self.is_final,
            self.edges,
            self.values,
        )
    }
}

// MappedDictionary trait implementations
impl<V: DictionaryValue> MappedDictionaryNode for DoubleArrayTrieCharNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        if self.state < self.shared.values.len() {
            self.shared.values[self.state].clone()
        } else {
            None
        }
    }
}

impl<V: DictionaryValue> MappedDictionary for DoubleArrayTrieChar<V> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        Self::get_value(self, term)
    }

    fn contains_with_value<F>(&self, term: &str, predicate: F) -> bool
    where
        F: Fn(&Self::Value) -> bool,
    {
        match self.get_value(term) {
            Some(ref value) => predicate(value),
            None => false,
        }
    }
}

// =============================================================================
// Conversion from PersistentARTrieChar
// =============================================================================

#[cfg(feature = "persistent-artrie")]
use crate::persistent_artrie_char::PersistentARTrieChar;

/// Convert a `PersistentARTrieChar` reference to a `DoubleArrayTrieChar`.
///
/// This conversion leverages the fact that `PersistentARTrieChar::iter_with_values()`
/// yields terms in lexicographic order (due to `BTreeMap` children), allowing us
/// to bypass the O(n log n) sorting step.
///
/// # Performance
///
/// - Time: O(n * d) where n = number of terms, d = average term length
/// - Space: O(d + DAT size) - streams terms without collecting all into memory
///
/// This is more efficient than calling `from_terms_with_values()` which would
/// collect all terms, sort them, then build.
///
/// # Example
///
/// ```ignore
/// use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
/// use libdictenstein::double_array_trie_char::DoubleArrayTrieChar;
///
/// let pat: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
/// pat.insert_with_value("apple", 1);
/// pat.insert_with_value("banana", 2);
///
/// // Efficient conversion without sorting
/// let dat: DoubleArrayTrieChar<i32> = DoubleArrayTrieChar::from(&pat);
/// ```
#[cfg(feature = "persistent-artrie")]
impl<V: DictionaryValue> From<&PersistentARTrieChar<V>> for DoubleArrayTrieChar<V> {
    fn from(source: &PersistentARTrieChar<V>) -> Self {
        DoubleArrayTrieChar::from_sorted_terms_with_values(source.iter_with_values())
    }
}

/// Convert a `PersistentARTrieChar` (by value) to a `DoubleArrayTrieChar`.
///
/// This is a convenience wrapper that delegates to the reference implementation.
/// See [`From<&PersistentARTrieChar<V>>`] for performance details.
#[cfg(feature = "persistent-artrie")]
impl<V: DictionaryValue> From<PersistentARTrieChar<V>> for DoubleArrayTrieChar<V> {
    fn from(source: PersistentARTrieChar<V>) -> Self {
        DoubleArrayTrieChar::from(&source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_dict() {
        let dict = DoubleArrayTrieChar::empty();
        assert_eq!(dict.len(), Some(0));
        assert!(!dict.contains("test"));
    }

    #[test]
    fn test_basic_terms() {
        let dict = DoubleArrayTrieChar::from_terms(vec!["hello", "world"]);
        assert!(dict.contains("hello"));
        assert!(dict.contains("world"));
        assert!(!dict.contains("test"));
    }

    #[test]
    fn test_unicode_terms() {
        let dict = DoubleArrayTrieChar::from_terms(vec!["café", "naïve", "résumé"]);
        assert!(dict.contains("café"));
        assert!(dict.contains("naïve"));
        assert!(dict.contains("résumé"));
        assert!(!dict.contains("cafe")); // Different without accent
    }

    #[test]
    fn test_cjk_characters() {
        let dict = DoubleArrayTrieChar::from_terms(vec!["中文", "日本語", "한국어"]);
        assert!(dict.contains("中文"));
        assert!(dict.contains("日本語"));
        assert!(dict.contains("한국어"));
    }

    #[test]
    fn test_emoji() {
        let dict = DoubleArrayTrieChar::from_terms(vec!["hello🎉", "world🌍", "test✨"]);
        assert!(dict.contains("hello🎉"));
        assert!(dict.contains("world🌍"));
        assert!(dict.contains("test✨"));
    }

    #[test]
    fn test_mixed_unicode() {
        let dict = DoubleArrayTrieChar::from_terms(vec!["hello", "café", "中文", "🎉", "test123"]);

        assert!(dict.contains("hello"));
        assert!(dict.contains("café"));
        assert!(dict.contains("中文"));
        assert!(dict.contains("🎉"));
        assert!(dict.contains("test123"));
        assert!(!dict.contains("missing"));
    }

    #[test]
    fn test_node_traversal() {
        let dict = DoubleArrayTrieChar::from_terms(vec!["test"]);
        let root = dict.root();

        let t_node = root.transition('t').expect("Should have 't' edge");
        let e_node = t_node.transition('e').expect("Should have 'e' edge");
        let s_node = e_node.transition('s').expect("Should have 's' edge");
        let t2_node = s_node.transition('t').expect("Should have second 't' edge");

        assert!(t2_node.is_final());
    }

    #[test]
    fn test_edges_iterator() {
        let dict = DoubleArrayTrieChar::from_terms(vec!["cat", "car", "cart"]);
        let root = dict.root();

        let c_node = root.transition('c').unwrap();
        let a_node = c_node.transition('a').unwrap();

        let edges: Vec<char> = a_node.edges().map(|(c, _)| c).collect();
        assert!(edges.contains(&'t'));
        assert!(edges.contains(&'r'));
    }

    // MappedDictionary tests with UTF-8
    #[test]
    fn test_mapped_dictionary_with_unicode_values() {
        let terms = vec![("café", 1), ("中文", 2), ("🎉", 3), ("naïve", 4)];

        let dict = DoubleArrayTrieChar::from_terms_with_values(terms);

        assert_eq!(dict.get_value("café"), Some(1));
        assert_eq!(dict.get_value("中文"), Some(2));
        assert_eq!(dict.get_value("🎉"), Some(3));
        assert_eq!(dict.get_value("naïve"), Some(4));
        assert_eq!(dict.get_value("missing"), None);
    }

    #[test]
    fn test_mapped_dictionary_contains_with_value() {
        let dict = DoubleArrayTrieChar::from_terms_with_values(vec![("café", 42), ("résumé", 100)]);

        assert!(dict.contains_with_value("café", |v| *v == 42));
        assert!(dict.contains_with_value("résumé", |v| *v > 50));
        assert!(!dict.contains_with_value("café", |v| *v > 50));
        assert!(!dict.contains_with_value("missing", |v| *v == 42));
    }

    #[test]
    fn test_mapped_dictionary_node_value() {
        use crate::{Dictionary, MappedDictionaryNode};

        let dict = DoubleArrayTrieChar::from_terms_with_values(vec![("test", 123)]);

        let root = dict.root();
        let t_node = root.transition('t').unwrap();
        let e_node = t_node.transition('e').unwrap();
        let s_node = e_node.transition('s').unwrap();
        let final_node = s_node.transition('t').unwrap();

        assert!(final_node.is_final());
        assert_eq!(final_node.value(), Some(123));
        assert_eq!(s_node.value(), None); // Not final
    }

    #[test]
    fn test_backward_compatibility() {
        // Default type parameter should be ()
        let dict: DoubleArrayTrieChar = DoubleArrayTrieChar::from_terms(vec!["café", "中文"]);

        assert!(dict.contains("café"));
        assert!(dict.contains("中文"));
        assert_eq!(dict.len(), Some(2));
    }

    #[test]
    fn test_empty_string_with_value() {
        let dict = DoubleArrayTrieChar::from_terms_with_values(vec![("", 1), ("test", 2)]);

        assert_eq!(dict.get_value(""), Some(1));
        assert_eq!(dict.get_value("test"), Some(2));
    }

    #[test]
    fn test_duplicate_update_value() {
        // When duplicates exist, keep the last value
        let dict = DoubleArrayTrieChar::from_terms_with_values(vec![
            ("café", 1),
            ("café", 2), // Should override
        ]);

        assert_eq!(dict.get_value("café"), Some(2));
        assert_eq!(dict.len(), Some(1)); // Only one term
    }

    #[test]
    fn test_string_values() {
        let dict = DoubleArrayTrieChar::from_terms_with_values(vec![
            ("café", "coffee".to_string()),
            ("中文", "Chinese".to_string()),
            ("🎉", "party".to_string()),
        ]);

        assert_eq!(dict.get_value("café"), Some("coffee".to_string()));
        assert_eq!(dict.get_value("中文"), Some("Chinese".to_string()));
        assert_eq!(dict.get_value("🎉"), Some("party".to_string()));
    }

    // =========================================================================
    // Tests for from_sorted_terms_with_values
    // =========================================================================

    #[test]
    fn test_from_sorted_empty() {
        let terms: Vec<(&str, i32)> = vec![];
        let dict = DoubleArrayTrieChar::from_sorted_terms_with_values(terms);
        assert_eq!(dict.len(), Some(0));
        assert!(!dict.contains("anything"));
    }

    #[test]
    fn test_from_sorted_basic_terms() {
        // Terms must be in lexicographic order
        let sorted_terms = vec![("apple", 1), ("banana", 2), ("cherry", 3)];
        let dict = DoubleArrayTrieChar::from_sorted_terms_with_values(sorted_terms);

        assert_eq!(dict.len(), Some(3));
        assert_eq!(dict.get_value("apple"), Some(1));
        assert_eq!(dict.get_value("banana"), Some(2));
        assert_eq!(dict.get_value("cherry"), Some(3));
        assert_eq!(dict.get_value("missing"), None);
    }

    #[test]
    fn test_from_sorted_unicode() {
        // Unicode terms in lexicographic order
        // Note: lexicographic order for Unicode is by code points
        let sorted_terms = vec![("café", 10), ("naïve", 20), ("résumé", 30)];
        let dict = DoubleArrayTrieChar::from_sorted_terms_with_values(sorted_terms);

        assert_eq!(dict.len(), Some(3));
        assert_eq!(dict.get_value("café"), Some(10));
        assert_eq!(dict.get_value("naïve"), Some(20));
        assert_eq!(dict.get_value("résumé"), Some(30));
    }

    #[test]
    fn test_from_sorted_cjk() {
        // CJK characters
        let sorted_terms = vec![("中文", 100), ("日本語", 200), ("한국어", 300)];
        let dict = DoubleArrayTrieChar::from_sorted_terms_with_values(sorted_terms);

        assert_eq!(dict.len(), Some(3));
        assert_eq!(dict.get_value("中文"), Some(100));
        assert_eq!(dict.get_value("日本語"), Some(200));
        assert_eq!(dict.get_value("한국어"), Some(300));
    }

    #[test]
    fn test_from_sorted_duplicates_last_wins() {
        // When duplicates exist in sorted input, last value should win
        let sorted_terms = vec![
            ("apple", 1),
            ("apple", 2), // duplicate - should override
            ("banana", 3),
        ];
        let dict = DoubleArrayTrieChar::from_sorted_terms_with_values(sorted_terms);

        assert_eq!(dict.len(), Some(2)); // Only 2 unique terms
        assert_eq!(dict.get_value("apple"), Some(2)); // Last value wins
        assert_eq!(dict.get_value("banana"), Some(3));
    }

    #[test]
    fn test_from_sorted_single_term() {
        let sorted_terms = vec![("singleton", 42)];
        let dict = DoubleArrayTrieChar::from_sorted_terms_with_values(sorted_terms);

        assert_eq!(dict.len(), Some(1));
        assert_eq!(dict.get_value("singleton"), Some(42));
    }

    #[test]
    fn test_from_sorted_empty_string() {
        // Empty string should be handled correctly
        let sorted_terms = vec![("", 0), ("a", 1), ("ab", 2)];
        let dict = DoubleArrayTrieChar::from_sorted_terms_with_values(sorted_terms);

        assert_eq!(dict.len(), Some(3));
        assert_eq!(dict.get_value(""), Some(0));
        assert_eq!(dict.get_value("a"), Some(1));
        assert_eq!(dict.get_value("ab"), Some(2));
    }

    #[test]
    fn test_from_sorted_prefix_terms() {
        // Terms where some are prefixes of others
        let sorted_terms = vec![("a", 1), ("ab", 2), ("abc", 3), ("abd", 4), ("b", 5)];
        let dict = DoubleArrayTrieChar::from_sorted_terms_with_values(sorted_terms);

        assert_eq!(dict.len(), Some(5));
        assert_eq!(dict.get_value("a"), Some(1));
        assert_eq!(dict.get_value("ab"), Some(2));
        assert_eq!(dict.get_value("abc"), Some(3));
        assert_eq!(dict.get_value("abd"), Some(4));
        assert_eq!(dict.get_value("b"), Some(5));
    }

    #[test]
    fn test_from_sorted_matches_from_terms() {
        // Verify that from_sorted_terms_with_values produces the same result
        // as from_terms_with_values when given pre-sorted input
        let unsorted_terms = vec![("cherry", 3), ("apple", 1), ("banana", 2), ("date", 4)];
        let sorted_terms = vec![("apple", 1), ("banana", 2), ("cherry", 3), ("date", 4)];

        let dict1 = DoubleArrayTrieChar::from_terms_with_values(unsorted_terms);
        let dict2 = DoubleArrayTrieChar::from_sorted_terms_with_values(sorted_terms);

        // Both should have the same terms and values
        assert_eq!(dict1.len(), dict2.len());
        for term in ["apple", "banana", "cherry", "date"] {
            assert_eq!(dict1.get_value(term), dict2.get_value(term));
        }
    }
}

// =============================================================================
// Feature-gated tests for PersistentARTrieChar conversion
// =============================================================================

#[cfg(all(test, feature = "persistent-artrie"))]
mod persistent_artrie_conversion_tests {
    use super::*;
    use crate::persistent_artrie_char::PersistentARTrieChar;

    #[test]
    fn test_from_persistent_artrie_empty() {
        let pat: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
        let dat: DoubleArrayTrieChar<i32> = DoubleArrayTrieChar::from(&pat);

        assert_eq!(dat.len(), Some(0));
    }

    #[test]
    fn test_from_persistent_artrie_basic() {
        let pat: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
        pat.insert_with_value("apple", 1);
        pat.insert_with_value("banana", 2);
        pat.insert_with_value("cherry", 3);

        let dat: DoubleArrayTrieChar<i32> = DoubleArrayTrieChar::from(&pat);

        assert_eq!(dat.len(), Some(3));
        assert_eq!(dat.get_value("apple"), Some(1));
        assert_eq!(dat.get_value("banana"), Some(2));
        assert_eq!(dat.get_value("cherry"), Some(3));
        assert_eq!(dat.get_value("missing"), None);
    }

    #[test]
    fn test_from_persistent_artrie_unicode() {
        let pat: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
        pat.insert_with_value("café", 10);
        pat.insert_with_value("日本語", 20);
        pat.insert_with_value("🎉", 30);

        let dat: DoubleArrayTrieChar<i32> = DoubleArrayTrieChar::from(&pat);

        assert_eq!(dat.len(), Some(3));
        assert_eq!(dat.get_value("café"), Some(10));
        assert_eq!(dat.get_value("日本語"), Some(20));
        assert_eq!(dat.get_value("🎉"), Some(30));
    }

    #[test]
    fn test_from_persistent_artrie_by_value() {
        let pat: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
        pat.insert_with_value("test", 42);

        // Test conversion by value (not reference)
        let dat: DoubleArrayTrieChar<i32> = DoubleArrayTrieChar::from(pat);

        assert_eq!(dat.len(), Some(1));
        assert_eq!(dat.get_value("test"), Some(42));
    }

    #[test]
    fn test_from_persistent_artrie_roundtrip_values() {
        // Verify all values survive the conversion
        let pat: PersistentARTrieChar<String> = PersistentARTrieChar::new();
        let terms = vec![
            ("alpha", "A"),
            ("beta", "B"),
            ("gamma", "G"),
            ("delta", "D"),
            ("epsilon", "E"),
        ];
        for (term, value) in &terms {
            pat.insert_with_value(term, value.to_string());
        }

        let dat: DoubleArrayTrieChar<String> = DoubleArrayTrieChar::from(&pat);

        assert_eq!(dat.len(), Some(terms.len()));
        for (term, value) in &terms {
            assert_eq!(dat.get_value(term), Some(value.to_string()));
        }
    }

    #[test]
    fn test_from_persistent_artrie_iteration_order() {
        // Verify the DAT can be iterated and contains all original terms
        let pat: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
        pat.insert_with_value("cat", 1);
        pat.insert_with_value("car", 2);
        pat.insert_with_value("cart", 3);
        pat.insert_with_value("card", 4);

        let dat: DoubleArrayTrieChar<i32> = DoubleArrayTrieChar::from(&pat);

        let dat_terms: std::collections::HashSet<_> = dat.iter().map(|(s, _)| s).collect();
        assert_eq!(dat_terms.len(), 4);
        assert!(dat_terms.contains("cat"));
        assert!(dat_terms.contains("car"));
        assert!(dat_terms.contains("cart"));
        assert!(dat_terms.contains("card"));
    }
}
