//! Suffix automaton dictionary for approximate substring matching.
//!
//! This module implements a suffix automaton, which enables efficient approximate
//! matching of substrings anywhere within indexed text (not just prefixes like
//! traditional dictionaries).
//!
//! # Overview
//!
//! A **suffix automaton** is a minimal deterministic finite automaton (DFA) that
//! recognizes all suffixes of indexed text. Key properties:
//!
//! - **Substring Recognition**: Any path from root represents a substring
//! - **Minimality**: Typically ≤ 2n-1 states for n characters
//! - **Online Construction**: O(1) amortized per character
//! - **Endpos Equivalence**: States group substrings by ending positions
//!
//! # Use Cases
//!
//! ## Code Search
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::suffix_automaton::SuffixAutomaton;
//!
//! let code = r#"
//! fn calculate_total(items: &[Item]) -> f64 {
//!     items.iter().map(|i| i.price).sum()
//! }
//! "#;
//!
//! let dict = SuffixAutomaton::<()>::from_text(code);
//!
//! // Exact substring containment via the automaton itself.
//! assert!(dict.contains("calculate_total"));
//! assert!(dict.contains("items.iter()"));
//! ```
//!
//! Approximate matching is provided by the downstream
//! [`liblevenshtein`](https://github.com/vinary-tree/liblevenshtein-rust)
//! crate's `Transducer`: wrap the `SuffixAutomaton` returned here and query
//! with a target distance. The transducer is intentionally upstream-owned
//! (same separation of concerns as `pathmap` in [`crate::pathmap`]).
//!
//! ## Document Search
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::suffix_automaton::SuffixAutomaton;
//!
//! let docs = vec![
//!     "Levenshtein automata for approximate matching",
//!     "Suffix trees and suffix arrays for pattern search",
//! ];
//!
//! let dict = SuffixAutomaton::<()>::from_texts(docs);
//!
//! // Substring lookup against the indexed text.
//! assert!(dict.contains("approximate matching"));
//! assert!(dict.contains("pattern search"));
//! ```
//!
//! For fuzzy queries (e.g. "algoritm" → "algorithm"), feed `dict` into the
//! `liblevenshtein` `Transducer` and call `match_positions` on the returned
//! candidates to recover the source document and offset.
//!
//! # Dynamic Updates
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::suffix_automaton::SuffixAutomaton;
//!
//! let dict = SuffixAutomaton::<()>::new();
//!
//! // Build index incrementally
//! dict.insert("testing the suffix automaton");
//! dict.insert("another test string");
//!
//! // Substring lookup
//! assert!(dict.contains("suffix"));
//! assert!(dict.contains("test"));
//!
//! // Update index
//! dict.remove("another test string");
//! assert!(dict.contains("testing the suffix automaton"));
//! dict.insert("added new testing content");
//!
//! // Compact periodically
//! if dict.needs_compaction() {
//!     dict.compact();
//! }
//! ```
//!
//! # Comparison with Prefix Dictionaries
//!
//! | Feature | PathMap/DAWG | SuffixAutomaton |
//! |---------|--------------|-----------------|
//! | **Matching** | Prefix (whole words) | Substring (anywhere) |
//! | **Use Case** | Spell check, completion | Full-text search |
//! | **Space** | O(n) | O(n) states + edges |
//! | **Construction** | O(n) | O(n) online |
//! | **Dynamic** | Yes (DynamicDawg) | Yes |
//! | **Example** | "test" → "testing" | "test" → "contest" |
//!
//! # Important: Removal Semantics
//!
//! Unlike prefix-based dictionaries (DynamicDawg, DoubleArrayTrie), the
//! `remove()` method in SuffixAutomaton only removes metadata tracking which
//! terms were explicitly indexed. It does **NOT** remove paths from the automaton
//! graph structure.
//!
//! This means `contains(term)` may still return `true` after `remove(term)` if:
//!
//! - The term shares paths with other indexed terms in the automaton
//! - The term's state nodes are still reachable via other indexed terms
//!
//! This behavior is intentional and stems from the fundamental design of suffix
//! automata, where states represent equivalence classes of substrings with the
//! same set of ending positions. Fully removing a term would require rebuilding
//! significant portions of the automaton.
//!
//! **Recommendation**: Use `iter()` to enumerate explicitly indexed terms, or
//! track indexed terms externally if precise removal semantics are required.
//!
//! # References
//!
//! - Blumer et al. (1985): "The smallest automaton recognizing the subwords of a text"
//! - Design document: `docs/SUFFIX_AUTOMATON_DESIGN.md`

use std::collections::HashMap;
use std::sync::Arc;

use super::lockfree::LockFreeSuffixAutomaton;
use super::zipper::SuffixAutomatonZipper;
use crate::iterator::{DictionaryIterator, DictionaryTermIterator};
use crate::value::DictionaryValue;
use crate::{Dictionary, DictionaryNode, SyncStrategy};

/// A state in the suffix automaton.
///
/// Each state represents an equivalence class of substrings that have the same
/// set of ending positions (endpos). This minimizes the number of states while
/// maintaining the ability to recognize all substrings.
// C3 step: byte-for-byte-identical local `SuffixNode<V>` struct + impl
// block replaced with a type alias to the generic
// `super::core::SuffixNode<u8, V>`. The generic version
// at `src/suffix_automaton/core/node.rs` carries an identical impl with
// `label: U` instead of `label: u8` — `U = u8` resolves the trait
// bounds the same way, so call-sites are unchanged.
#[allow(dead_code)]
pub(crate) type SuffixNode<V = ()> = super::core::SuffixNode<u8, V>;

/// Internal state of the suffix automaton.
///
/// This is published through an atomic snapshot handle in [`SuffixAutomaton`].
// C3 algorithmic dedup: byte-for-byte-identical local
// `SuffixAutomatonInner<V>` struct + 2-method impl block (`new`,
// `extend`) replaced with a type alias to the generic
// `super::core::SuffixAutomatonInner<u8, V>` (which
// carries the same fields and the same algorithmic `extend(unit: U)`
// method generic over `U: CharUnit`).
pub(crate) type SuffixAutomatonInner<V = ()> = super::core::SuffixAutomatonInner<u8, V>;

#[allow(dead_code)]
mod _legacy_extend_byte {
    // Original local impl preserved as a comment block (per CLAUDE.md's
    // never-disable-by-deleting). The methods now live on the canonical
    // generic `super::core::SuffixAutomatonInner<U, V>`.
    //
    // fn new() -> Self {
    //     Self {
    //         nodes: vec![SuffixNode::root()],
    //         last_state: 0,
    //         string_count: 0,
    //         source_texts: Vec::new(),
    //         positions: HashMap::new(),
    //         needs_compaction: false,
    //     }
    // }
    //
    // fn extend(&mut self, ch: u8) { /* … */ }
}

// The original `fn extend(&mut self, ch: u8) {...}` body (~60 LOC)
// lived here. It now lives on
// `super::core::SuffixAutomatonInner::extend(unit: U)`
// generic over CharUnit and is byte-for-byte equivalent for U=u8.

/// Suffix automaton for approximate substring matching.
///
/// This dictionary type enables finding approximate matches anywhere within
/// indexed text, not just at word boundaries like prefix-based dictionaries.
///
/// # Thread Safety
///
/// Uses atomic snapshot publication for dynamic updates. Readers traverse a
/// stable `Arc` snapshot without waiting; writers prepare a cloned graph and
/// publish it with CAS.
///
/// # Construction
///
/// - `new()` - Create empty automaton
/// - `from_text(s)` - Index single string
/// - `from_texts(iter)` - Index multiple strings
///
/// # Dynamic Operations
///
/// - `insert(text)` - Add a string
/// - `remove(text)` - Remove a string (may leave unreachable states)
/// - `compact()` - Garbage collect unreachable states
///
/// # Querying
///
/// Exact substring lookup is provided directly:
///
/// ```rust
/// use libdictenstein::prelude::*;
/// use libdictenstein::suffix_automaton::SuffixAutomaton;
///
/// let dict = SuffixAutomaton::<()>::from_text("example text");
/// assert!(dict.contains("example"));
/// assert!(dict.contains("xampl"));     // substring
/// assert!(!dict.contains("missing"));
/// ```
///
/// For approximate matching wrap the automaton in
/// [`liblevenshtein`](https://github.com/vinary-tree/liblevenshtein-rust)'s
/// `Transducer` (upstream-owned, not part of this crate). The `dict` value
/// returned here implements the traversal traits the transducer needs.
#[derive(Clone, Debug)]
pub struct SuffixAutomaton<V: DictionaryValue = ()> {
    pub(crate) inner: LockFreeSuffixAutomaton<u8, V>,
}

impl<V: DictionaryValue> SuffixAutomaton<V> {
    #[inline]
    fn from_inner(inner: SuffixAutomatonInner<V>) -> Self {
        Self {
            inner: LockFreeSuffixAutomaton::from_inner(inner),
        }
    }

    fn insert_text_into_inner(inner: &mut SuffixAutomatonInner<V>, text: &str, value: Option<V>) {
        inner.last_state = 0;
        let string_id = inner.source_texts.len();
        inner.source_texts.push(text.to_string());

        for byte in text.bytes() {
            inner.extend(byte);
        }

        let last_state = inner.last_state;
        if let Some(value) = value {
            inner.nodes[last_state].value = Some(value);
        }
        inner
            .positions
            .entry(last_state)
            .or_default()
            .push((string_id, text.len()));
        inner.string_count += 1;
        inner.last_state = 0;
    }

    fn find_term_state(inner: &SuffixAutomatonInner<V>, term: &str) -> Option<usize> {
        let mut state = 0;
        for &byte in term.as_bytes() {
            state = inner.nodes.get(state)?.find_edge(byte)?;
        }
        Some(state)
    }

    fn value_from_inner(inner: &SuffixAutomatonInner<V>, term: &str) -> Option<V> {
        let state = Self::find_term_state(inner, term)?;
        inner.nodes.get(state).and_then(|node| node.value.clone())
    }

    /// Create an empty suffix automaton.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let dict = SuffixAutomaton::<()>::new();
    /// dict.insert("hello");
    /// dict.insert("world");
    /// ```
    pub fn new() -> Self {
        Self::from_inner(SuffixAutomatonInner::new())
    }

    /// Get the number of states in the automaton (for debugging).
    pub fn state_count(&self) -> usize {
        self.inner.load().nodes.len()
    }

    /// Debug: print automaton structure (for development).
    #[allow(dead_code)]
    pub fn debug_print(&self) {
        let inner = self.inner.load();
        println!("Suffix Automaton with {} states:", inner.nodes.len());
        for (idx, node) in inner.nodes.iter().enumerate() {
            println!(
                "  State {}: is_final={}, max_len={}, edges={:?}, link={:?}",
                idx,
                node.is_final,
                node.max_length,
                node.edges
                    .iter()
                    .map(|(b, t)| (char::from(*b), t))
                    .collect::<Vec<_>>(),
                node.suffix_link
            );
        }
    }

    /// Build from a single text string.
    ///
    /// Indexes all suffixes of the input text, enabling substring search.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let code = "fn main() { println!(\"Hello\"); }";
    /// let dict = SuffixAutomaton::<()>::from_text(code);
    /// ```
    pub fn from_text(text: &str) -> Self {
        let mut inner = SuffixAutomatonInner::new();
        Self::insert_text_into_inner(&mut inner, text, None);
        Self::from_inner(inner)
    }

    /// Build from multiple texts.
    ///
    /// Creates a generalized suffix automaton indexing all input strings.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let docs = vec![
    ///     "First document text",
    ///     "Second document text",
    ///     "Third document text",
    /// ];
    /// let dict = SuffixAutomaton::<()>::from_texts(docs);
    /// ```
    pub fn from_texts<I, S>(texts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut inner = SuffixAutomatonInner::new();
        for text in texts {
            Self::insert_text_into_inner(&mut inner, text.as_ref(), None);
        }
        Self::from_inner(inner)
    }

    /// Insert a text string.
    ///
    /// Returns `true` if the operation succeeded (always true currently).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let dict = SuffixAutomaton::<()>::new();
    /// dict.insert("testing insertion");
    /// ```
    pub fn insert(&self, text: &str) -> bool {
        self.inner.mutate(|inner| {
            Self::insert_text_into_inner(inner, text, None);
            (true, true)
        })
    }

    /// Remove a text string.
    ///
    /// Returns `true` if removed, `false` if not found.
    ///
    /// **Note**: May leave unreachable states. Call `compact()` periodically
    /// to reclaim memory.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let dict = SuffixAutomaton::<()>::new();
    /// dict.insert("test string");
    /// assert!(dict.remove("test string"));
    /// assert!(!dict.remove("test string")); // Already removed
    /// ```
    pub fn remove(&self, text: &str) -> bool {
        self.inner.mutate(|inner| {
            // Remove one active source record matching this exact text. Source IDs
            // are stable `source_texts` indices, so duplicate texts are removed one
            // insertion at a time without renumbering later sources.
            let mut remove_location: Option<(usize, usize, usize)> = None;
            for (state_id, positions) in &inner.positions {
                for (position_index, (source_id, end)) in positions.iter().enumerate() {
                    if *end == text.len()
                        && inner
                            .source_texts
                            .get(*source_id)
                            .map(|source| source == text)
                            .unwrap_or(false)
                        && remove_location
                            .map(|(best_source_id, _, _)| *source_id < best_source_id)
                            .unwrap_or(true)
                    {
                        remove_location = Some((*source_id, *state_id, position_index));
                    }
                }
            }

            let removed_state = remove_location.map(|(_, state, _)| state);
            let removed = if let Some((_, state, index)) = remove_location {
                if let Some(positions) = inner.positions.get_mut(&state) {
                    positions.remove(index);
                    true
                } else {
                    false
                }
            } else {
                false
            };

            if removed {
                // Source text slots stay stable; position metadata is the active set.
                let should_remove = removed_state
                    .and_then(|state| inner.positions.get(&state).map(|v| (state, v.is_empty())));

                if let Some((state, true)) = should_remove {
                    // Note: We keep is_final=true because this state still represents
                    // a valid substring (possibly from other indexed strings).
                    // Only remove from positions map.
                    inner.positions.remove(&state);
                }

                inner.needs_compaction = true;
                inner.string_count = inner.string_count.saturating_sub(1);
            }

            (removed, removed)
        })
    }

    /// Clear all indexed text.
    ///
    /// Resets the automaton to empty state with only the root node.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let dict = SuffixAutomaton::<()>::new();
    /// dict.insert("test");
    /// dict.clear();
    /// assert_eq!(dict.string_count(), 0);
    /// ```
    pub fn clear(&self) {
        self.inner.mutate(|inner| {
            if inner.string_count == 0 && inner.nodes.len() == 1 {
                ((), false)
            } else {
                *inner = SuffixAutomatonInner::new();
                ((), true)
            }
        });
    }

    /// Compact internal structure (garbage collection).
    ///
    /// Removes unreachable states after deletions. Recommended after batch
    /// deletions or when `needs_compaction()` returns true.
    ///
    /// # Complexity
    ///
    /// - Time: O(states + edges)
    /// - Space: O(states) temporary
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let dict = SuffixAutomaton::<()>::new();
    /// dict.insert("test1");
    /// dict.insert("test2");
    /// dict.remove("test1");
    ///
    /// if dict.needs_compaction() {
    ///     dict.compact();
    /// }
    /// ```
    pub fn compact(&self) {
        self.inner.mutate(|inner| {
            if !inner.needs_compaction {
                return ((), false);
            }

            // Mark-and-sweep garbage collection
            let mut reachable = vec![false; inner.nodes.len()];
            let mut stack = vec![0]; // Start from root

            while let Some(state) = stack.pop() {
                if reachable[state] {
                    continue;
                }
                reachable[state] = true;

                for &(_, target) in &inner.nodes[state].edges {
                    stack.push(target);
                }
            }

            // Build new node vector with only reachable states
            let reachable_count = reachable
                .iter()
                .filter(|&&is_reachable| is_reachable)
                .count();
            let mut new_nodes = Vec::with_capacity(reachable_count);
            let mut old_to_new = vec![0; inner.nodes.len()];

            for (old_idx, node) in inner.nodes.iter().enumerate() {
                if reachable[old_idx] {
                    old_to_new[old_idx] = new_nodes.len();
                    new_nodes.push(node.clone());
                }
            }

            // Remap all state indices
            for node in &mut new_nodes {
                for edge in &mut node.edges {
                    edge.1 = old_to_new[edge.1];
                }
                if let Some(link) = node.suffix_link {
                    node.suffix_link = Some(old_to_new[link]);
                }
            }

            // Update positions map
            let mut new_positions = HashMap::with_capacity(inner.positions.len());
            for (old_state, positions) in inner.positions.drain() {
                if reachable[old_state] {
                    new_positions.insert(old_to_new[old_state], positions);
                }
            }

            inner.nodes = new_nodes;
            inner.positions = new_positions;
            inner.last_state = 0;
            inner.needs_compaction = false;
            ((), true)
        });
    }

    /// Get the number of indexed strings.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let dict = SuffixAutomaton::<()>::new();
    /// assert_eq!(dict.string_count(), 0);
    ///
    /// dict.insert("test");
    /// assert_eq!(dict.string_count(), 1);
    /// ```
    pub fn string_count(&self) -> usize {
        self.inner.load().string_count
    }

    /// Check if compaction is recommended.
    ///
    /// Returns `true` if strings have been removed and unreachable states
    /// may exist.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let dict = SuffixAutomaton::<()>::new();
    /// dict.insert("test");
    /// dict.remove("test");
    ///
    /// if dict.needs_compaction() {
    ///     dict.compact();
    /// }
    /// ```
    pub fn needs_compaction(&self) -> bool {
        self.inner.load().needs_compaction
    }

    /// Get match positions for a substring.
    ///
    /// Returns a list of (string_id, end_position) tuples indicating where
    /// the substring appears in the indexed texts.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let docs = vec!["testing", "test"];
    /// let dict = SuffixAutomaton::<()>::from_texts(docs);
    ///
    /// let positions = dict.match_positions("test");
    /// assert_eq!(positions, vec![(0, 4), (1, 4)]);
    /// ```
    pub fn match_positions(&self, substring: &str) -> Vec<(usize, usize)> {
        let inner = self.inner.load();

        if substring.is_empty() {
            return Vec::new();
        }

        // Navigate to the state for this substring
        let mut state = 0;
        for byte in substring.as_bytes() {
            match inner.nodes[state].find_edge(*byte) {
                Some(next) => state = next,
                None => return Vec::new(), // Substring not found
            }
        }

        let mut active_sources = vec![false; inner.source_texts.len()];
        for positions in inner.positions.values() {
            for (source_id, _) in positions {
                if let Some(active) = active_sources.get_mut(*source_id) {
                    *active = true;
                }
            }
        }

        let needle = substring.as_bytes();
        let mut result = Vec::new();
        for (source_id, source) in inner.source_texts.iter().enumerate() {
            if !active_sources.get(source_id).copied().unwrap_or(false)
                || needle.len() > source.len()
            {
                continue;
            }

            let bytes = source.as_bytes();
            for start in 0..=bytes.len() - needle.len() {
                if bytes[start..].starts_with(needle) {
                    result.push((source_id, start + needle.len()));
                }
            }
        }

        result.sort_unstable();
        result.dedup();
        result
    }

    /// Update an existing term's value in place, or insert a new term with a default value.
    ///
    /// This method is useful for accumulation patterns where you want to modify an existing
    /// value (e.g., add to a `HashSet`) or insert a new one if the term doesn't exist.
    ///
    /// Returns `true` if the term was newly inserted, `false` if it already existed.
    ///
    /// # Parameters
    ///
    /// - `term`: The term to update or insert
    /// - `default_value`: The value to use if the term doesn't exist
    /// - `update_fn`: Function to apply to the existing value if the term exists
    ///
    /// # Example
    ///
    /// ```text
    /// use std::collections::HashSet;
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let dict: SuffixAutomaton<HashSet<String>> = SuffixAutomaton::new();
    ///
    /// // First call - inserts new term with default value
    /// let was_new = dict.update_or_insert(
    ///     "key",
    ///     HashSet::from(["value1".to_string()]),
    ///     |set| { set.insert("value1".to_string()); }
    /// );
    /// assert!(was_new);
    ///
    /// // Second call - updates existing value
    /// let was_new = dict.update_or_insert(
    ///     "key",
    ///     HashSet::new(),
    ///     |set| { set.insert("value2".to_string()); }
    /// );
    /// assert!(!was_new);
    ///
    /// // Now "key" contains {"value1", "value2"}
    /// ```
    pub fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: Fn(&mut V),
    {
        self.inner.mutate(|inner| {
            let Some(state) = Self::find_term_state(inner, term) else {
                Self::insert_text_into_inner(inner, term, Some(default_value.clone()));
                return (true, true);
            };

            if inner.nodes[state].value.is_some() {
                update_fn(
                    inner.nodes[state]
                        .value
                        .as_mut()
                        .expect("value.is_some() checked one line above"),
                );
                (false, true)
            } else {
                inner.nodes[state].value = Some(default_value.clone());
                inner.nodes[state].is_final = true;
                if !inner.positions.get(&state).is_some_and(|positions| {
                    positions.iter().any(|(source_id, end)| {
                        *end == term.len()
                            && inner
                                .source_texts
                                .get(*source_id)
                                .map(|source| source == term)
                                .unwrap_or(false)
                    })
                }) {
                    let string_id = inner.source_texts.len();
                    inner.source_texts.push(term.to_string());
                    inner
                        .positions
                        .entry(state)
                        .or_default()
                        .push((string_id, term.len()));
                    inner.string_count += 1;
                }
                (true, true)
            }
        })
    }

    /// Internal helper for insert_with_value.
    fn insert_with_value_internal(&self, term: &str, value: V) -> bool {
        self.inner.mutate(|inner| {
            Self::insert_text_into_inner(inner, term, Some(value.clone()));
            (true, true)
        })
    }

    /// Get the original source texts used to build this automaton.
    ///
    /// Returns a vector of all texts that were indexed. This is useful
    /// for serialization, as the automaton can be reconstructed from
    /// these texts rather than extracting all possible substrings.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let texts = vec!["hello world", "test string"];
    /// let dict = SuffixAutomaton::<()>::from_texts(texts.clone());
    ///
    /// let sources = dict.source_texts();
    /// assert_eq!(sources.len(), 2);
    /// ```
    pub fn source_texts(&self) -> Vec<String> {
        let inner = self.inner.load();
        inner.source_texts.clone()
    }

    /// Iterate over all substrings as raw byte vectors (without values).
    ///
    /// Returns an iterator yielding `Vec<u8>` in depth-first order.
    /// Note: This yields all indexed substrings, not just complete terms.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let dict = SuffixAutomaton::<()>::from_text("hello");
    ///
    /// for bytes in dict.iter_terms() {
    ///     let substring = String::from_utf8(bytes).unwrap();
    ///     println!("Substring: {}", substring);
    /// }
    /// ```
    pub fn iter_terms(&self) -> DictionaryTermIterator<SuffixAutomatonZipper<V>> {
        let zipper = SuffixAutomatonZipper::new_from_dict(self);
        DictionaryTermIterator::new(zipper)
    }

    /// Iterate over all `(substring, value)` pairs as raw byte vectors.
    ///
    /// Returns an iterator yielding `(Vec<u8>, V)` tuples in depth-first order.
    /// Note: This yields all indexed substrings, not just complete terms.
    ///
    /// **Note**: This only works for dictionaries created with values.
    /// For dictionaries without values, use `iter_terms()` instead.
    ///
    /// # Examples
    ///
    /// ```text
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let mut dict = SuffixAutomaton::<u32>::new();
    /// dict.insert_with_value("hello", 42);
    ///
    /// for (bytes, value) in dict.iter_bytes() {
    ///     let substring = String::from_utf8(bytes).unwrap();
    ///     println!("{} -> {}", substring, value);
    /// }
    /// ```
    pub fn iter_bytes(&self) -> DictionaryIterator<SuffixAutomatonZipper<V>> {
        let zipper = SuffixAutomatonZipper::new_from_dict(self);
        DictionaryIterator::new(zipper)
    }

    /// Iterate over all `(substring, value)` pairs as UTF-8 strings.
    ///
    /// Returns an iterator yielding `(String, V)` tuples in depth-first order.
    /// Note: This yields all indexed substrings, not just complete terms.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::SuffixAutomaton;
    ///
    /// let dict = SuffixAutomaton::<()>::from_text("hello");
    ///
    /// for (substring, _) in dict.iter() {
    ///     println!("Substring: {}", substring);
    /// }
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (String, V)> + '_ {
        self.iter_bytes()
            .map(|(bytes, value)| (String::from_utf8_lossy(&bytes).into_owned(), value))
    }
}

impl<V: DictionaryValue> IntoIterator for &SuffixAutomaton<V> {
    type Item = (Vec<u8>, V);
    type IntoIter = DictionaryIterator<SuffixAutomatonZipper<V>>;

    /// Creates an iterator over all `(substring, value)` pairs as raw byte vectors.
    fn into_iter(self) -> Self::IntoIter {
        self.iter_bytes()
    }
}

impl<V: DictionaryValue> Default for SuffixAutomaton<V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "serialization")]
impl<V: DictionaryValue + serde::Serialize> serde::Serialize for SuffixAutomaton<V> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let inner = self.inner.load();
        inner.serialize(serializer)
    }
}

/// Deserialize implementation when only `serialization` feature is enabled (not `persistent-artrie`).
/// In this case, we need explicit `Deserialize` bounds.
#[cfg(all(feature = "serialization", not(feature = "persistent-artrie")))]
impl<'de, V: DictionaryValue + serde::Deserialize<'de>> serde::Deserialize<'de>
    for SuffixAutomaton<V>
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let inner = SuffixAutomatonInner::deserialize(deserializer)?;
        Ok(SuffixAutomaton {
            inner: LockFreeSuffixAutomaton::from_inner(inner),
        })
    }
}

/// Deserialize implementation when `persistent-artrie` feature is enabled.
/// `DictionaryValue` already includes `DeserializeOwned`, so no additional bounds needed.
#[cfg(all(feature = "serialization", feature = "persistent-artrie"))]
impl<'de, V: DictionaryValue> serde::Deserialize<'de> for SuffixAutomaton<V> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let inner = SuffixAutomatonInner::deserialize(deserializer)?;
        Ok(SuffixAutomaton {
            inner: LockFreeSuffixAutomaton::from_inner(inner),
        })
    }
}

/// Handle for traversing the suffix automaton.
///
/// Implements `DictionaryNode` trait for compatibility with existing
/// `Transducer` and query infrastructure.
#[derive(Clone, Debug)]
pub struct SuffixNodeHandle<V: DictionaryValue = ()> {
    /// Stable automaton snapshot for traversal.
    automaton: Arc<SuffixAutomatonInner<V>>,

    /// Current state index.
    state_id: usize,
}

impl<V: DictionaryValue> DictionaryNode for SuffixNodeHandle<V> {
    type Unit = u8;

    #[inline]
    fn snapshot_node_identity(&self) -> Option<crate::SnapshotNodeIdentity> {
        crate::SnapshotNodeIdentity::from_index(self.state_id)
    }

    fn is_final(&self) -> bool {
        self.automaton
            .nodes
            .get(self.state_id)
            .map(|node| node.is_final)
            .unwrap_or(false)
    }

    fn transition(&self, label: u8) -> Option<Self> {
        self.automaton
            .nodes
            .get(self.state_id)?
            .find_edge(label)
            .map(|target| Self {
                automaton: Arc::clone(&self.automaton),
                state_id: target,
            })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (u8, Self)> + '_> {
        let edges = self
            .automaton
            .nodes
            .get(self.state_id)
            .map(|node| node.edges.clone())
            .unwrap_or_default();

        Box::new(edges.into_iter().map(move |(label, target)| {
            (
                label,
                Self {
                    automaton: Arc::clone(&self.automaton),
                    state_id: target,
                },
            )
        }))
    }

    #[inline]
    fn for_each_edge<F>(&self, mut visitor: F)
    where
        F: FnMut(u8, Self),
    {
        let Some(node) = self.automaton.nodes.get(self.state_id) else {
            return;
        };
        for &(label, target) in &node.edges {
            visitor(
                label,
                Self {
                    automaton: Arc::clone(&self.automaton),
                    state_id: target,
                },
            );
        }
    }

    #[inline]
    fn filter_map_edges<T, P, F>(&self, mut project: P, mut visitor: F)
    where
        P: FnMut(u8) -> Option<T>,
        F: FnMut(u8, Self, T),
    {
        let Some(node) = self.automaton.nodes.get(self.state_id) else {
            return;
        };
        for &(label, target) in &node.edges {
            if let Some(projected) = project(label) {
                visitor(
                    label,
                    Self {
                        automaton: Arc::clone(&self.automaton),
                        state_id: target,
                    },
                    projected,
                );
            }
        }
    }

    fn has_edge(&self, label: u8) -> bool {
        self.automaton
            .nodes
            .get(self.state_id)
            .is_some_and(|node| node.find_edge(label).is_some())
    }

    fn edge_count(&self) -> Option<usize> {
        Some(
            self.automaton
                .nodes
                .get(self.state_id)
                .map(|node| node.edges.len())
                .unwrap_or(0),
        )
    }
}

impl<V: DictionaryValue> Dictionary for SuffixAutomaton<V> {
    type Node = SuffixNodeHandle<V>;

    fn root(&self) -> Self::Node {
        SuffixNodeHandle {
            automaton: self.inner.load(),
            state_id: 0,
        }
    }

    fn contains(&self, term: &str) -> bool {
        let mut node = self.root();
        for byte in term.as_bytes() {
            match node.transition(*byte) {
                Some(next) => node = next,
                None => return false,
            }
        }
        // For suffix automaton, we check substring existence, not finality
        // Any reachable state represents a valid substring
        true
    }

    fn len(&self) -> Option<usize> {
        Some(self.string_count())
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }

    fn is_suffix_based(&self) -> bool {
        true // Suffix automaton performs substring matching
    }
}

// NOTE: Serialization support (DictionaryFromTerms impl) is provided in liblevenshtein
// since the trait lives there. See liblevenshtein::serialization for the implementation.

// ============================================================================
// MappedDictionary Trait Implementation
// ============================================================================

use crate::{MappedDictionary, MappedDictionaryNode, MutableMappedDictionary};

impl<V: DictionaryValue> MappedDictionaryNode for SuffixNodeHandle<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        self.automaton
            .nodes
            .get(self.state_id)
            .and_then(|node| node.value.clone())
    }
}

impl<V: DictionaryValue> MappedDictionary for SuffixAutomaton<V> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        let inner = self.inner.load();
        Self::value_from_inner(&inner, term)
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

impl<V: DictionaryValue> MutableMappedDictionary for SuffixAutomaton<V> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        self.insert_with_value_internal(term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: Fn(&mut Self::Value),
    {
        SuffixAutomaton::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;

        // Iterate over the original source texts, not all suffixes
        // SuffixAutomaton stores values at ALL suffix positions, so iter_bytes()
        // would yield duplicates. We only want to merge the complete strings.
        for term in other.source_texts() {
            if term.is_empty() {
                continue; // Skip empty strings (removed entries)
            }

            if let Some(other_value) = other.get_value(&term) {
                processed += 1;
                // Compute the new value: merge if exists, otherwise use other_value
                let new_value = if let Some(self_value) = self.get_value(&term) {
                    merge_fn(&self_value, &other_value)
                } else {
                    other_value.clone()
                };
                // Use update_or_insert to ensure value is set correctly
                let new_value_clone = new_value.clone();
                self.update_or_insert(&term, new_value, move |v| *v = new_value_clone.clone());
            }
        }
        processed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_automaton() {
        let dict = SuffixAutomaton::<()>::new();
        assert_eq!(dict.string_count(), 0);
        assert!(!dict.needs_compaction());
    }

    #[test]
    fn test_single_character() {
        let dict = SuffixAutomaton::<()>::from_text("a");
        assert_eq!(dict.string_count(), 1);
        assert!(dict.contains("a"));
        assert!(!dict.contains("b"));
    }

    #[test]
    fn test_simple_string() {
        let dict = SuffixAutomaton::<()>::from_text("abc");
        assert_eq!(dict.string_count(), 1);

        // All suffixes should be present
        assert!(dict.contains("abc"));
        assert!(dict.contains("bc"));
        assert!(dict.contains("c"));

        // All substrings should be present (suffix automaton recognizes all substrings)
        assert!(dict.contains("ab"));
        assert!(dict.contains("b"));
        assert!(dict.contains("a"));

        // Non-substrings should not be present
        assert!(!dict.contains("d"));
        assert!(!dict.contains("abcd"));
    }

    #[test]
    fn test_repeated_characters() {
        let dict = SuffixAutomaton::<()>::from_text("aaa");
        assert_eq!(dict.string_count(), 1);

        assert!(dict.contains("aaa"));
        assert!(dict.contains("aa"));
        assert!(dict.contains("a"));
    }

    #[test]
    fn test_complex_string() {
        let dict = SuffixAutomaton::<()>::from_text("abcbc");
        assert_eq!(dict.string_count(), 1);

        // All suffixes
        assert!(dict.contains("abcbc"));
        assert!(dict.contains("bcbc"));
        assert!(dict.contains("cbc"));
        assert!(dict.contains("bc"));
        assert!(dict.contains("c"));

        // Some substrings that should be present
        assert!(dict.contains("abc"));
        assert!(dict.contains("bcb"));
    }

    #[test]
    fn test_multiple_strings() {
        let dict = SuffixAutomaton::<()>::from_texts(vec!["abc", "def"]);
        assert_eq!(dict.string_count(), 2);

        // Substrings from first text
        assert!(dict.contains("abc"));
        assert!(dict.contains("bc"));
        assert!(dict.contains("c"));

        // Substrings from second text
        assert!(dict.contains("def"));
        assert!(dict.contains("ef"));
        assert!(dict.contains("f"));
    }

    #[test]
    fn test_insert_and_remove() {
        let dict = SuffixAutomaton::<()>::new();

        assert!(dict.insert("test"));
        assert_eq!(dict.string_count(), 1);
        assert!(dict.contains("test"));

        assert!(dict.remove("test"));
        assert_eq!(dict.string_count(), 0);
        assert!(dict.needs_compaction());

        assert!(!dict.remove("test")); // Already removed
    }

    #[test]
    fn test_clear() {
        let dict = SuffixAutomaton::<()>::from_texts(vec!["abc", "def", "ghi"]);
        assert_eq!(dict.string_count(), 3);

        dict.clear();
        assert_eq!(dict.string_count(), 0);
        assert!(!dict.contains("abc"));
    }

    #[test]
    fn test_compaction() {
        let dict = SuffixAutomaton::<()>::new();

        dict.insert("test1");
        dict.insert("test2");
        dict.insert("test3");
        assert_eq!(dict.string_count(), 3);

        dict.remove("test2");
        assert_eq!(dict.string_count(), 2);
        assert!(dict.needs_compaction());

        dict.compact();
        assert!(!dict.needs_compaction());
        assert_eq!(dict.string_count(), 2);

        // Verify remaining strings are still accessible
        assert!(dict.contains("test1"));
        assert!(dict.contains("test3"));
    }

    #[test]
    fn test_match_positions() {
        let docs = vec!["banana", "bandana"];
        let dict = SuffixAutomaton::<()>::from_texts(docs);

        assert_eq!(dict.match_positions("ana"), vec![(0, 4), (0, 6), (1, 7)]);
        assert_eq!(dict.match_positions("band"), vec![(1, 4)]);
        assert_eq!(dict.match_positions("apple"), Vec::<(usize, usize)>::new());

        assert!(dict.remove("banana"));
        assert_eq!(dict.match_positions("ana"), vec![(1, 7)]);

        dict.compact();
        assert_eq!(dict.match_positions("ana"), vec![(1, 7)]);
    }

    #[test]
    fn test_match_positions_duplicate_sources_removed_one_at_a_time() {
        let dict = SuffixAutomaton::<()>::from_texts(["aba", "aba", "ababa"]);

        assert_eq!(
            dict.match_positions("aba"),
            vec![(0, 3), (1, 3), (2, 3), (2, 5)]
        );

        assert!(dict.remove("aba"));
        assert_eq!(dict.match_positions("aba"), vec![(1, 3), (2, 3), (2, 5)]);

        assert!(dict.remove("aba"));
        assert_eq!(dict.match_positions("aba"), vec![(2, 3), (2, 5)]);
        assert!(!dict.remove("aba"));
    }

    #[test]
    fn test_match_positions_for_valued_and_existing_substring_inserts() {
        let dict = SuffixAutomaton::<i32>::new();
        assert!(dict.insert_with_value("abracadabra", 11));
        assert_eq!(dict.match_positions("abra"), vec![(0, 4), (0, 11)]);
        assert!(dict.remove("abracadabra"));
        assert_eq!(dict.match_positions("abra"), Vec::<(usize, usize)>::new());

        assert!(dict.insert("banana"));
        assert!(dict.update_or_insert("nan", 7, |value| *value += 1));
        assert_eq!(dict.match_positions("nan"), vec![(1, 5), (2, 3)]);
    }

    #[test]
    fn test_dictionary_trait() {
        let dict = SuffixAutomaton::<()>::from_text("test");

        // Test Dictionary trait methods
        assert_eq!(dict.len(), Some(1));
        assert!(!dict.is_empty());
        assert_eq!(dict.sync_strategy(), SyncStrategy::InternalSync);

        // Test node traversal
        let root = dict.root();
        assert!(root.has_edge(b't'));

        let node_t = root.transition(b't').unwrap();
        assert!(node_t.has_edge(b'e'));
    }

    #[test]
    fn test_node_edges() {
        let dict = SuffixAutomaton::<()>::from_text("ab");
        let root = dict.root();

        let edges: Vec<_> = root.edges().collect();
        assert!(!edges.is_empty());

        // Should have edges for suffixes "ab" and "b"
        let labels: Vec<_> = edges.iter().map(|(l, _)| *l).collect();
        assert!(labels.contains(&b'a') || labels.contains(&b'b'));
    }

    #[test]
    fn test_mapped_dictionary_basic() {
        use crate::MappedDictionary;

        let dict: SuffixAutomaton<u32> = SuffixAutomaton::new();
        dict.insert_with_value("test", 42);
        dict.insert_with_value("hello", 100);

        assert_eq!(dict.get_value("test"), Some(42));
        assert_eq!(dict.get_value("hello"), Some(100));
        assert_eq!(dict.get_value("missing"), None);
    }

    #[test]
    fn test_mapped_dictionary_contains_with_value() {
        use crate::MappedDictionary;

        let dict: SuffixAutomaton<String> = SuffixAutomaton::new();
        dict.insert_with_value("test", "value1".to_string());
        dict.insert_with_value("hello", "value2".to_string());

        assert!(dict.contains_with_value("test", |v| v == "value1"));
        assert!(!dict.contains_with_value("test", |v| v == "wrong"));
        assert!(!dict.contains_with_value("missing", |v| v == "value1"));
    }

    #[test]
    fn test_mapped_dictionary_vec_values() {
        use crate::MappedDictionary;

        let dict: SuffixAutomaton<Vec<usize>> = SuffixAutomaton::new();
        dict.insert_with_value("scoped", vec![1, 2, 3]);
        dict.insert_with_value("global", vec![0]);

        assert_eq!(dict.get_value("scoped"), Some(vec![1, 2, 3]));
        assert!(dict.contains_with_value("scoped", |v| v.contains(&2)));
        assert!(!dict.contains_with_value("scoped", |v| v.contains(&999)));
    }

    #[test]
    fn test_mapped_node_value() {
        use crate::MappedDictionaryNode;

        let dict: SuffixAutomaton<u32> = SuffixAutomaton::new();
        dict.insert_with_value("test", 42);

        // Navigate to "test"
        let root = dict.root();
        let t = root.transition(b't').unwrap();
        let e = t.transition(b'e').unwrap();
        let s = e.transition(b's').unwrap();
        let t2 = s.transition(b't').unwrap();

        // The final node should have the value
        assert_eq!(t2.value(), Some(42));

        // Non-final nodes should not have values
        assert_eq!(t.value(), None);
    }

    #[test]
    fn test_union_with_both_empty() {
        let dict1: SuffixAutomaton<u32> = SuffixAutomaton::new();
        let dict2: SuffixAutomaton<u32> = SuffixAutomaton::new();

        let processed = dict1.union_with(&dict2, |a, b| a + b);
        assert_eq!(processed, 0);
        assert_eq!(dict1.string_count(), 0);
    }

    #[test]
    fn test_union_with_self_empty() {
        let dict1: SuffixAutomaton<u32> = SuffixAutomaton::new();
        let dict2: SuffixAutomaton<u32> = SuffixAutomaton::new();
        dict2.insert_with_value("hello", 10);
        dict2.insert_with_value("world", 20);

        let processed = dict1.union_with(&dict2, |a, b| a + b);
        assert!(processed > 0);
        assert_eq!(dict1.get_value("hello"), Some(10));
        assert_eq!(dict1.get_value("world"), Some(20));
    }

    #[test]
    fn test_union_with_other_empty() {
        let dict1: SuffixAutomaton<u32> = SuffixAutomaton::new();
        dict1.insert_with_value("hello", 10);
        let dict2: SuffixAutomaton<u32> = SuffixAutomaton::new();

        let processed = dict1.union_with(&dict2, |a, b| a + b);
        assert_eq!(processed, 0);
        assert_eq!(dict1.get_value("hello"), Some(10));
    }

    #[test]
    fn test_union_with_no_conflicts() {
        let dict1: SuffixAutomaton<u32> = SuffixAutomaton::new();
        dict1.insert_with_value("hello", 10);
        let dict2: SuffixAutomaton<u32> = SuffixAutomaton::new();
        dict2.insert_with_value("world", 20);

        let processed = dict1.union_with(&dict2, |a, b| a + b);
        assert!(processed > 0);
        assert_eq!(dict1.get_value("hello"), Some(10));
        assert_eq!(dict1.get_value("world"), Some(20));
    }

    #[test]
    fn test_union_with_conflicts_sum() {
        let dict1: SuffixAutomaton<u32> = SuffixAutomaton::new();
        dict1.insert_with_value("hello", 10);
        let dict2: SuffixAutomaton<u32> = SuffixAutomaton::new();
        dict2.insert_with_value("hello", 20);

        let processed = dict1.union_with(&dict2, |a, b| a + b);
        assert!(processed > 0);
        assert_eq!(dict1.get_value("hello"), Some(30));
    }

    #[test]
    fn test_union_with_conflicts_max() {
        let dict1: SuffixAutomaton<u32> = SuffixAutomaton::new();
        dict1.insert_with_value("hello", 10);
        let dict2: SuffixAutomaton<u32> = SuffixAutomaton::new();
        dict2.insert_with_value("hello", 20);

        let processed = dict1.union_with(&dict2, |a, b| *a.max(b));
        assert!(processed > 0);
        assert_eq!(dict1.get_value("hello"), Some(20));
    }

    #[test]
    fn test_union_with_partial_conflicts() {
        let dict1: SuffixAutomaton<u32> = SuffixAutomaton::new();
        dict1.insert_with_value("apple", 1);
        dict1.insert_with_value("banana", 2);
        let dict2: SuffixAutomaton<u32> = SuffixAutomaton::new();
        dict2.insert_with_value("banana", 3);
        dict2.insert_with_value("cherry", 4);

        let processed = dict1.union_with(&dict2, |a, b| a + b);
        assert!(processed > 0);
        assert_eq!(dict1.get_value("apple"), Some(1));
        assert_eq!(dict1.get_value("banana"), Some(5));
        assert_eq!(dict1.get_value("cherry"), Some(4));
    }
}
