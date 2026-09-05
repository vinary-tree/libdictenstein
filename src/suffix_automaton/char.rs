//! Character-level suffix automaton dictionary for Unicode substring matching.
//!
//! This module implements a character-level suffix automaton, which enables efficient
//! approximate matching of substrings anywhere within indexed text with correct
//! Unicode semantics. Unlike the byte-level `SuffixAutomaton`, this variant operates
//! on Unicode scalar values (`char`) for proper multi-byte UTF-8 handling.
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
//! use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
//!
//! let code = r#"
//! fn calculate_total(items: &[Item]) -> f64 {
//!     items.iter().map(|i| i.price).sum()
//! }
//! "#;
//!
//! let dict = SuffixAutomatonChar::<()>::from_text(code);
//!
//! // Exact (Unicode-aware) substring containment.
//! assert!(dict.contains("calculate_total"));
//! assert!(dict.contains("items.iter()"));
//! ```
//!
//! Approximate matching is provided by the downstream
//! [`liblevenshtein`](https://github.com/vinary-tree/liblevenshtein-rust)
//! crate's `Transducer`: wrap the `SuffixAutomatonChar` returned here and
//! query with a target distance. The transducer is intentionally
//! upstream-owned.
//!
//! ## Document Search
//!
//! ```rust
//! use libdictenstein::prelude::*;
//! use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
//!
//! let docs = vec![
//!     "Levenshtein automata for approximate matching",
//!     "Suffix trees and suffix arrays for pattern search",
//! ];
//!
//! let dict = SuffixAutomatonChar::<()>::from_texts(docs);
//!
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
//! use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
//!
//! let dict = SuffixAutomatonChar::<()>::new();
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
//! | Feature | PathMap/DAWG | SuffixAutomatonChar |
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
//! Unlike prefix-based dictionaries (DynamicDawgChar, DoubleArrayTrieChar), the
//! `remove()` method in SuffixAutomatonChar only removes metadata tracking which
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
//! **Recommendation**: Use `iter_entries()` to enumerate explicitly indexed terms, or
//! track indexed terms externally if precise removal semantics are required.
//!
//! # References
//!
//! - Blumer et al. (1985): "The smallest automaton recognizing the subwords of a text"
//! - Design document: `docs/SUFFIX_AUTOMATON_DESIGN.md`

use std::collections::HashMap;
use std::iter::FusedIterator;
use std::sync::Arc;

use super::char_zipper::SuffixAutomatonCharZipper;
use super::lockfree::LockFreeSuffixAutomaton;
use crate::iterator::{DictionaryIterator, DictionaryTermIterator};
use crate::value::DictionaryValue;
use crate::{Dictionary, DictionaryNode, SyncStrategy};

/// A state in the suffix automaton.
///
/// Each state represents an equivalence class of substrings that have the same
/// set of ending positions (endpos). This minimizes the number of states while
/// maintaining the ability to recognize all substrings.
// C3 step: byte-for-byte-identical local `SuffixNodeChar<V>` struct +
// impl block replaced with a type alias to the generic
// `super::core::SuffixNode<char, V>`. The canonical
// impl carries the same 5 methods (root, new, find_edge, add_edge,
// update_edge) generic over `U: CharUnit`, so call-sites resolve
// unchanged.
#[allow(dead_code)]
pub(crate) type SuffixNodeChar<V = ()> = super::core::SuffixNode<char, V>;

#[allow(dead_code)]
mod _suffix_node_char_legacy {
    // Original local impl preserved as a comment so the historical
    // method bodies remain in the source tree per the project's
    // never-delete-to-disable policy. The methods are now provided by
    // the canonical `super::core::node::SuffixNode<U, V>`
    // impl.
    //
    // fn root() -> Self {
    //     Self {
    //         edges: Vec::new(),
    //         suffix_link: None,
    //         max_length: 0,
    //         is_final: false,
    //         value: None,
    //     }
    // }
    //
    // fn new(max_length: usize) -> Self {
    //     Self {
    //         edges: Vec::new(),
    //         suffix_link: None,
    //         max_length,
    //         is_final: false,
    //         value: None,
    //     }
    // }
    //
    // fn find_edge(&self, label: char) -> Option<usize> {
    //     if self.edges.len() < 16 {
    //         self.edges.iter().find(|(b, _)| *b == label).map(|(_, t)| *t)
    //     } else {
    //         self.edges.binary_search_by_key(&label, |(b, _)| *b).ok()
    //             .map(|idx| self.edges[idx].1)
    //     }
    // }
    //
    // fn add_edge(&mut self, label: char, target: usize) {
    //     match self.edges.binary_search_by_key(&label, |(b, _)| *b) {
    //         Ok(idx) => { self.edges[idx].1 = target; }
    //         Err(idx) => { self.edges.insert(idx, (label, target)); }
    //     }
    // }
    //
    // fn update_edge(&mut self, label: char, new_target: usize) -> bool {
    //     if let Some(idx) = self.edges.iter().position(|(b, _)| *b == label) {
    //         self.edges[idx].1 = new_target;
    //         true
    //     } else {
    //         false
    //     }
    // }
}

// The original `fn update_edge` body lived here in the local impl. It
// is now provided by the canonical impl on
// `super::core::node::SuffixNode<U, V>` (with
// `U = char` for this file).

/// Internal state of the suffix automaton.
///
/// This is published through an atomic snapshot handle in
/// [`SuffixAutomatonChar`].
// C3 algorithmic dedup (char variant): mirror of the byte path.
// Local `SuffixAutomatonCharInner<V>` struct + 2-method impl block
// replaced with a type alias to the generic
// `super::core::SuffixAutomatonInner<char, V>`. The
// canonical impl carries `new()` + `extend(unit: char)`.
pub(crate) type SuffixAutomatonCharInner<V = ()> = super::core::SuffixAutomatonInner<char, V>;

// The original `fn extend(&mut self, ch: char) {...}` body (~60 LOC)
// lived here. It now lives on the canonical generic impl at
// `super::core::SuffixAutomatonInner::extend` (for
// `U = char` it resolves to the byte-identical implementation).

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
/// Exact (Unicode-aware) substring lookup is provided directly:
///
/// ```rust
/// use libdictenstein::prelude::*;
/// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
///
/// let dict = SuffixAutomatonChar::<()>::from_text("example text");
/// assert!(dict.contains("example"));
/// assert!(dict.contains("xampl"));
/// assert!(!dict.contains("missing"));
/// ```
///
/// For approximate matching wrap the automaton in
/// [`liblevenshtein`](https://github.com/vinary-tree/liblevenshtein-rust)'s
/// `Transducer` (upstream-owned, not part of this crate).
#[derive(Clone, Debug)]
pub struct SuffixAutomatonChar<V: DictionaryValue = ()> {
    pub(crate) inner: LockFreeSuffixAutomaton<char, V>,
}

/// UTF-8-profile spelling for the in-memory Unicode-scalar suffix automaton.
pub type SuffixAutomatonUtf8<V = ()> = SuffixAutomatonChar<V>;

/// Snapshot iterator over explicitly inserted Unicode source records.
pub struct SuffixAutomatonCharEntryIterator<V: DictionaryValue = ()> {
    inner: Arc<SuffixAutomatonCharInner<V>>,
    index: usize,
}

impl<V: DictionaryValue> Iterator for SuffixAutomatonCharEntryIterator<V> {
    type Item = (String, Option<V>);

    fn next(&mut self) -> Option<Self::Item> {
        let source_id = *self.inner.sorted_source_indices.get(self.index)?;
        self.index += 1;
        let text = self
            .inner
            .source_texts
            .get(source_id)
            .expect("the revision record index references a source")
            .clone();
        let value = self
            .inner
            .source_values
            .get(source_id)
            .cloned()
            .unwrap_or_else(|| SuffixAutomatonChar::<V>::value_from_inner(&self.inner, &text));
        Some((text, value))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self
            .inner
            .sorted_source_indices
            .len()
            .saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

impl<V: DictionaryValue> ExactSizeIterator for SuffixAutomatonCharEntryIterator<V> {}
impl<V: DictionaryValue> FusedIterator for SuffixAutomatonCharEntryIterator<V> {}

impl<V: DictionaryValue> SuffixAutomatonChar<V> {
    #[inline]
    fn from_inner(inner: SuffixAutomatonCharInner<V>) -> Self {
        Self {
            inner: LockFreeSuffixAutomaton::from_inner(inner),
        }
    }

    fn insert_text_into_inner(
        inner: &mut SuffixAutomatonCharInner<V>,
        text: &str,
        value: Option<V>,
    ) {
        inner.last_state = 0;
        let string_id = inner.source_texts.len();
        inner.source_texts.push(text.to_string());
        inner.source_values.push(value.clone());

        for ch in text.chars() {
            inner.extend(ch);
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
        inner.index_source(string_id);
        inner.string_count += 1;
        inner.last_state = 0;
    }

    fn from_records(records: Vec<(String, Option<V>)>) -> Self {
        let mut inner = SuffixAutomatonCharInner::new();
        for (text, value) in records {
            Self::insert_text_into_inner(&mut inner, &text, value);
        }
        Self::from_inner(inner)
    }

    fn extend_records(&self, records: Vec<(String, Option<V>)>) {
        if records.is_empty() {
            return;
        }
        self.inner.mutate(|inner| {
            for (text, value) in &records {
                Self::insert_text_into_inner(inner, text, value.clone());
            }
            ((), true)
        });
    }

    fn find_term_state(inner: &SuffixAutomatonCharInner<V>, term: &str) -> Option<usize> {
        let mut state = 0;
        for ch in term.chars() {
            state = inner.nodes.get(state)?.find_edge(ch)?;
        }
        Some(state)
    }

    fn value_from_inner(inner: &SuffixAutomatonCharInner<V>, term: &str) -> Option<V> {
        let state = Self::find_term_state(inner, term)?;
        inner.nodes.get(state).and_then(|node| node.value.clone())
    }

    #[cfg(feature = "serialization")]
    fn restore_missing_source_values(inner: &mut SuffixAutomatonCharInner<V>) {
        if inner.source_values.len() >= inner.source_texts.len() {
            return;
        }
        let restored: Vec<_> = inner.source_texts[inner.source_values.len()..]
            .iter()
            .map(|text| Self::value_from_inner(inner, text))
            .collect();
        inner.source_values.extend(restored);
    }

    fn update_source_record_value(
        inner: &mut SuffixAutomatonCharInner<V>,
        state: usize,
        term: &str,
        value: V,
    ) {
        let source_id = inner.positions.get(&state).and_then(|positions| {
            positions
                .iter()
                .filter_map(|(source_id, end)| {
                    (*end == term.len()
                        && inner
                            .source_texts
                            .get(*source_id)
                            .is_some_and(|text| text == term))
                    .then_some(*source_id)
                })
                .filter(|source_id| {
                    inner
                        .source_values
                        .get(*source_id)
                        .is_some_and(Option::is_some)
                })
                .max()
                .or_else(|| {
                    positions
                        .iter()
                        .filter_map(|(source_id, end)| {
                            (*end == term.len()
                                && inner
                                    .source_texts
                                    .get(*source_id)
                                    .is_some_and(|text| text == term))
                            .then_some(*source_id)
                        })
                        .max()
                })
        });
        if let Some(record_value) = source_id.and_then(|id| inner.source_values.get_mut(id)) {
            *record_value = Some(value);
        }
    }

    /// Create an empty suffix automaton.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let dict = SuffixAutomatonChar::<()>::new();
    /// dict.insert("hello");
    /// dict.insert("world");
    /// ```
    pub fn new() -> Self {
        Self::from_inner(SuffixAutomatonCharInner::new())
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
                    .map(|(b, t)| ((*b), t))
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
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let code = "fn main() { println!(\"Hello\"); }";
    /// let dict = SuffixAutomatonChar::<()>::from_text(code);
    /// ```
    pub fn from_text(text: &str) -> Self {
        let mut inner = SuffixAutomatonCharInner::new();
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
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let docs = vec![
    ///     "First document text",
    ///     "Second document text",
    ///     "Third document text",
    /// ];
    /// let dict = SuffixAutomatonChar::<()>::from_texts(docs);
    /// ```
    pub fn from_texts<I, S>(texts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut inner = SuffixAutomatonCharInner::new();
        for text in texts {
            Self::insert_text_into_inner(&mut inner, text.as_ref(), None);
        }
        Self::from_inner(inner)
    }

    /// Build from Unicode-scalar profile sequences without UTF-8 byte
    /// transitions or suffixes beginning inside a scalar.
    pub fn from_atom_sequences<P, I>(sequences: I) -> Self
    where
        P: crate::AtomProfile<Atom = char>,
        I: IntoIterator<Item = crate::AtomSequence<P>>,
    {
        Self::from_texts(
            sequences
                .into_iter()
                .map(|sequence| sequence.as_atoms().iter().copied().collect::<String>()),
        )
    }

    /// Build a value-bearing suffix automaton from Unicode-scalar profile
    /// sequences without introducing UTF-8 byte transitions.
    pub fn from_atom_sequences_with_values<P, I>(entries: I) -> Self
    where
        P: crate::AtomProfile<Atom = char>,
        I: IntoIterator<Item = (crate::AtomSequence<P>, V)>,
    {
        Self::from_records(
            entries
                .into_iter()
                .map(|(sequence, value)| {
                    (
                        sequence.as_atoms().iter().copied().collect::<String>(),
                        Some(value),
                    )
                })
                .collect(),
        )
    }

    /// Read a mapped value for a Unicode-scalar profile sequence.
    pub fn get_atom_sequence_value<P>(&self, sequence: &crate::AtomSequence<P>) -> Option<V>
    where
        P: crate::AtomProfile<Atom = char>,
    {
        let term: String = sequence.as_atoms().iter().collect();
        <Self as crate::MappedDictionary>::get_value(self, &term)
    }

    /// Insert a text string.
    ///
    /// Returns `true` if the operation succeeded (always true currently).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let dict = SuffixAutomatonChar::<()>::new();
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
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let dict = SuffixAutomatonChar::<()>::new();
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

            let removed_source = remove_location.map(|(source_id, _, _)| source_id);
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
                if let Some(source_id) = removed_source {
                    inner.unindex_source(source_id);
                }
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
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let dict = SuffixAutomatonChar::<()>::new();
    /// dict.insert("test");
    /// dict.clear();
    /// assert_eq!(dict.string_count(), 0);
    /// ```
    pub fn clear(&self) {
        self.inner.mutate(|inner| {
            if inner.string_count == 0 && inner.nodes.len() == 1 {
                ((), false)
            } else {
                *inner = SuffixAutomatonCharInner::new();
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
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let dict = SuffixAutomatonChar::<()>::new();
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
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let dict = SuffixAutomatonChar::<()>::new();
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
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let dict = SuffixAutomatonChar::<()>::new();
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
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let docs = vec!["testing", "test"];
    /// let dict = SuffixAutomatonChar::<()>::from_texts(docs);
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
        for ch in substring.chars() {
            match inner.nodes[state].find_edge(ch) {
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

        let mut result = Vec::new();
        for (source_id, source) in inner.source_texts.iter().enumerate() {
            if !active_sources.get(source_id).copied().unwrap_or(false) {
                continue;
            }

            for (start, _) in source.char_indices() {
                if source[start..].starts_with(substring) {
                    result.push((source_id, start + substring.len()));
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
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let dict: SuffixAutomatonChar<HashSet<String>> = SuffixAutomatonChar::new();
    ///
    /// // First call - inserts new term with default value
    /// let was_new = dict.update_or_insert(
    ///     "café",
    ///     HashSet::from(["value1".to_string()]),
    ///     |set| { set.insert("value1".to_string()); }
    /// );
    /// assert!(was_new);
    ///
    /// // Second call - updates existing value
    /// let was_new = dict.update_or_insert(
    ///     "café",
    ///     HashSet::new(),
    ///     |set| { set.insert("value2".to_string()); }
    /// );
    /// assert!(!was_new);
    ///
    /// // Now "café" contains {"value1", "value2"}
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
                let value = inner.nodes[state]
                    .value
                    .clone()
                    .expect("value remains present after an in-place update");
                Self::update_source_record_value(inner, state, term, value);
                (false, true)
            } else {
                inner.nodes[state].value = Some(default_value.clone());
                Self::update_source_record_value(inner, state, term, default_value.clone());
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
                    inner.source_values.push(Some(default_value.clone()));
                    inner
                        .positions
                        .entry(state)
                        .or_default()
                        .push((string_id, term.len()));
                    inner.index_source(string_id);
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
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let texts = vec!["hello world", "test string"];
    /// let dict = SuffixAutomatonChar::<()>::from_texts(texts.clone());
    ///
    /// let sources = dict.source_texts();
    /// assert_eq!(sources.len(), 2);
    /// ```
    pub fn source_texts(&self) -> Vec<String> {
        let inner = self.inner.load();
        inner.source_texts.clone()
    }

    /// Iterate over explicitly stored source records in lexicographic order.
    ///
    /// This is distinct from [`Self::iter_terms`], which enumerates the
    /// recognized substring language. One immutable revision is retained for
    /// the iterator's lifetime.
    pub fn iter_entries(&self) -> SuffixAutomatonCharEntryIterator<V> {
        SuffixAutomatonCharEntryIterator {
            inner: self.inner.load(),
            index: 0,
        }
    }

    /// Iterate over all substrings as character vectors (without values).
    ///
    /// Returns an iterator yielding `Vec<char>` in depth-first order.
    /// This is useful for dictionaries created without values.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let dict = SuffixAutomatonChar::<()>::from_text("日本");
    ///
    /// for chars in dict.iter_terms() {
    ///     let substring: String = chars.iter().collect();
    ///     println!("Substring: {}", substring);
    /// }
    /// ```
    pub fn iter_terms(&self) -> DictionaryTermIterator<SuffixAutomatonCharZipper<V>> {
        let zipper = SuffixAutomatonCharZipper::new_from_dict(self);
        DictionaryTermIterator::new(zipper)
    }

    /// Iterate over all `(substring, value)` pairs as character vectors.
    ///
    /// Returns an iterator yielding `(Vec<char>, V)` tuples in depth-first order.
    /// Note: This yields all indexed substrings, not just complete terms.
    ///
    /// This legacy language iterator omits recognized substrings without
    /// values. Use [`Self::iter_entries`] or borrowed `IntoIterator` for stored
    /// source records, and
    /// [`DictionaryLanguageEntries::language_entries`](crate::DictionaryLanguageEntries::language_entries)
    /// for lossless substring-language traversal.
    ///
    /// # Examples
    ///
    /// ```text
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let mut dict = SuffixAutomatonChar::<u32>::new();
    /// dict.insert_with_value("café", 42);
    ///
    /// for (chars, value) in dict.iter_chars() {
    ///     let substring: String = chars.iter().collect();
    ///     println!("{} -> {}", substring, value);
    /// }
    /// ```
    pub fn iter_chars(&self) -> DictionaryIterator<SuffixAutomatonCharZipper<V>> {
        let zipper = SuffixAutomatonCharZipper::new_from_dict(self);
        DictionaryIterator::new(zipper)
    }

    /// Iterate over all `(substring, value)` pairs as UTF-8 strings.
    ///
    /// Returns an iterator yielding `(String, V)` tuples in depth-first order.
    /// Note: This yields all indexed substrings, not just complete terms.
    /// Like `iter_chars()`, this legacy language iterator omits entries without
    /// values and does not represent the stored source-record collection.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let dict = SuffixAutomatonChar::<()>::from_text("café");
    ///
    /// for (substring, _) in dict.iter() {
    ///     println!("Substring: {}", substring);
    /// }
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (String, V)> + '_ {
        self.iter_chars()
            .map(|(chars, value)| (chars.into_iter().collect::<String>(), value))
    }
}

impl<V: DictionaryValue> FromIterator<String> for SuffixAutomatonChar<V> {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        Self::from_texts(iter)
    }
}

impl<'a, V: DictionaryValue> FromIterator<&'a str> for SuffixAutomatonChar<V> {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        Self::from_texts(iter)
    }
}

impl<V: DictionaryValue> FromIterator<(String, V)> for SuffixAutomatonChar<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        Self::from_records(
            iter.into_iter()
                .map(|(text, value)| (text, Some(value)))
                .collect(),
        )
    }
}

impl<'a, V: DictionaryValue> FromIterator<(&'a str, V)> for SuffixAutomatonChar<V> {
    fn from_iter<I: IntoIterator<Item = (&'a str, V)>>(iter: I) -> Self {
        Self::from_records(
            iter.into_iter()
                .map(|(text, value)| (text.to_owned(), Some(value)))
                .collect(),
        )
    }
}

impl<V: DictionaryValue> Extend<String> for SuffixAutomatonChar<V> {
    fn extend<I: IntoIterator<Item = String>>(&mut self, iter: I) {
        self.extend_records(iter.into_iter().map(|text| (text, None)).collect());
    }
}

impl<'a, V: DictionaryValue> Extend<&'a str> for SuffixAutomatonChar<V> {
    fn extend<I: IntoIterator<Item = &'a str>>(&mut self, iter: I) {
        <Self as Extend<String>>::extend(self, iter.into_iter().map(str::to_owned));
    }
}

impl<V: DictionaryValue> Extend<(String, V)> for SuffixAutomatonChar<V> {
    fn extend<I: IntoIterator<Item = (String, V)>>(&mut self, iter: I) {
        self.extend_records(
            iter.into_iter()
                .map(|(text, value)| (text, Some(value)))
                .collect(),
        );
    }
}

impl<'a, V: DictionaryValue> Extend<(&'a str, V)> for SuffixAutomatonChar<V> {
    fn extend<I: IntoIterator<Item = (&'a str, V)>>(&mut self, iter: I) {
        <Self as Extend<(String, V)>>::extend(
            self,
            iter.into_iter()
                .map(|(text, value)| (text.to_owned(), value)),
        );
    }
}

impl<V: DictionaryValue> Default for SuffixAutomatonChar<V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "serialization")]
impl<V: DictionaryValue + serde::Serialize> serde::Serialize for SuffixAutomatonChar<V> {
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
    for SuffixAutomatonChar<V>
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut inner = SuffixAutomatonCharInner::deserialize(deserializer)?;
        SuffixAutomatonChar::restore_missing_source_values(&mut inner);
        inner.rebuild_sorted_source_indices();
        Ok(SuffixAutomatonChar {
            inner: LockFreeSuffixAutomaton::from_inner(inner),
        })
    }
}

/// Deserialize implementation when `persistent-artrie` feature is enabled.
/// `DictionaryValue` already includes `DeserializeOwned`, so no additional bounds needed.
#[cfg(all(feature = "serialization", feature = "persistent-artrie"))]
impl<'de, V: DictionaryValue> serde::Deserialize<'de> for SuffixAutomatonChar<V> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut inner = SuffixAutomatonCharInner::deserialize(deserializer)?;
        SuffixAutomatonChar::restore_missing_source_values(&mut inner);
        inner.rebuild_sorted_source_indices();
        Ok(SuffixAutomatonChar {
            inner: LockFreeSuffixAutomaton::from_inner(inner),
        })
    }
}

/// Handle for traversing the suffix automaton.
///
/// Implements `DictionaryNode` trait for compatibility with existing
/// `Transducer` and query infrastructure.
#[derive(Clone, Debug)]
pub struct SuffixNodeCharHandle<V: DictionaryValue = ()> {
    /// Stable automaton snapshot for traversal.
    automaton: Arc<SuffixAutomatonCharInner<V>>,

    /// Current state index.
    state_id: usize,
}

impl<V: DictionaryValue> DictionaryNode for SuffixNodeCharHandle<V> {
    type Unit = char;
    type SnapshotCursor = crate::SnapshotTraversalCursor;
    type SnapshotGraphValueHandle = crate::SnapshotTraversalCursor;

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

    fn transition(&self, label: char) -> Option<Self> {
        self.automaton
            .nodes
            .get(self.state_id)?
            .find_edge(label)
            .map(|target| Self {
                automaton: Arc::clone(&self.automaton),
                state_id: target,
            })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (char, Self)> + '_> {
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
        F: FnMut(char, Self),
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
        P: FnMut(char) -> Option<T>,
        F: FnMut(char, Self, T),
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

    #[inline]
    fn supports_efficient_edge_paging(&self) -> bool {
        true
    }

    #[inline]
    fn visit_edge_page_and_finality<F>(
        &self,
        start: usize,
        capacity: usize,
        visitor: F,
    ) -> (bool, usize)
    where
        F: FnMut(char, Self),
    {
        let is_final = self.is_final();
        let total = self.visit_edge_page(start, capacity, visitor);
        (is_final, total)
    }

    #[inline]
    fn visit_edge_page<F>(&self, start: usize, capacity: usize, mut visitor: F) -> usize
    where
        F: FnMut(char, Self),
    {
        let Some(node) = self.automaton.nodes.get(self.state_id) else {
            return 0;
        };
        let total = node.edges.len();
        let end = start.saturating_add(capacity).min(total);
        for &(label, target) in node.edges.get(start.min(total)..end).unwrap_or_default() {
            visitor(
                label,
                Self {
                    automaton: Arc::clone(&self.automaton),
                    state_id: target,
                },
            );
        }
        total
    }

    fn has_edge(&self, label: char) -> bool {
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

impl<V: DictionaryValue> Dictionary for SuffixAutomatonChar<V> {
    type Node = SuffixNodeCharHandle<V>;

    fn root(&self) -> Self::Node {
        SuffixNodeCharHandle {
            automaton: self.inner.load(),
            state_id: 0,
        }
    }

    fn contains(&self, term: &str) -> bool {
        let mut node = self.root();
        for ch in term.chars() {
            match node.transition(ch) {
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

impl<V: DictionaryValue> MappedDictionaryNode for SuffixNodeCharHandle<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        self.automaton
            .nodes
            .get(self.state_id)
            .and_then(|node| node.value.clone())
    }
}

impl<V: DictionaryValue> MappedDictionary for SuffixAutomatonChar<V> {
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

impl<V: DictionaryValue> MutableMappedDictionary for SuffixAutomatonChar<V> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        self.insert_with_value_internal(term, value)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: Fn(&mut Self::Value),
    {
        SuffixAutomatonChar::update_or_insert(self, term, default_value, update_fn)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;

        // Iterate over the original source texts, not all suffixes
        // SuffixAutomatonChar stores values at ALL suffix positions, so iter_chars()
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
        let dict = SuffixAutomatonChar::<()>::new();
        assert_eq!(dict.string_count(), 0);
        assert!(!dict.needs_compaction());
    }

    #[test]
    fn test_single_character() {
        let dict = SuffixAutomatonChar::<()>::from_text("a");
        assert_eq!(dict.string_count(), 1);
        assert!(dict.contains("a"));
        assert!(!dict.contains("b"));
    }

    #[test]
    fn test_simple_string() {
        let dict = SuffixAutomatonChar::<()>::from_text("abc");
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
        let dict = SuffixAutomatonChar::<()>::from_text("aaa");
        assert_eq!(dict.string_count(), 1);

        assert!(dict.contains("aaa"));
        assert!(dict.contains("aa"));
        assert!(dict.contains("a"));
    }

    #[test]
    fn test_complex_string() {
        let dict = SuffixAutomatonChar::<()>::from_text("abcbc");
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
        let dict = SuffixAutomatonChar::<()>::from_texts(vec!["abc", "def"]);
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
        let dict = SuffixAutomatonChar::<()>::new();

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
        let dict = SuffixAutomatonChar::<()>::from_texts(vec!["abc", "def", "ghi"]);
        assert_eq!(dict.string_count(), 3);

        dict.clear();
        assert_eq!(dict.string_count(), 0);
        assert!(!dict.contains("abc"));
    }

    #[test]
    fn test_compaction() {
        let dict = SuffixAutomatonChar::<()>::new();

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
        let docs = vec!["aé日a", "café"];
        let dict = SuffixAutomatonChar::<()>::from_texts(docs);

        assert_eq!(dict.match_positions("é日"), vec![(0, 6)]);
        assert_eq!(dict.match_positions("日"), vec![(0, 6)]);
        assert_eq!(dict.match_positions("é"), vec![(0, 3), (1, 5)]);
        assert_eq!(dict.match_positions("a"), vec![(0, 1), (0, 7), (1, 2)]);
        assert_eq!(
            dict.match_positions("missing"),
            Vec::<(usize, usize)>::new()
        );

        assert!(dict.remove("aé日a"));
        assert_eq!(dict.match_positions("é"), vec![(1, 5)]);

        dict.compact();
        assert_eq!(dict.match_positions("é"), vec![(1, 5)]);
    }

    #[test]
    fn test_match_positions_duplicate_sources_removed_one_at_a_time() {
        let dict = SuffixAutomatonChar::<()>::from_texts(["aba", "aba", "ababa"]);

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
        let dict = SuffixAutomatonChar::<i32>::new();
        assert!(dict.insert_with_value("東京カフェ東京", 11));
        assert_eq!(dict.match_positions("東京"), vec![(0, 6), (0, 21)]);
        assert!(dict.remove("東京カフェ東京"));
        assert_eq!(dict.match_positions("東京"), Vec::<(usize, usize)>::new());

        assert!(dict.insert("aé日a"));
        assert!(dict.update_or_insert("é日", 7, |value| *value += 1));
        assert_eq!(dict.match_positions("é日"), vec![(1, 6), (2, 5)]);
    }

    #[test]
    fn test_dictionary_trait() {
        let dict = SuffixAutomatonChar::<()>::from_text("test");

        // Test Dictionary trait methods
        assert_eq!(dict.len(), Some(1));
        assert!(!dict.is_empty());
        assert_eq!(dict.sync_strategy(), SyncStrategy::InternalSync);

        // Test node traversal
        let root = dict.root();
        assert!(root.has_edge('t'));

        let node_t = root.transition('t').unwrap();
        assert!(node_t.has_edge('e'));
    }

    #[test]
    fn profile_sequences_preserve_values() {
        let dictionary = SuffixAutomatonChar::<u16>::from_atom_sequences_with_values::<
            crate::UnicodeScalar,
            _,
        >([(crate::AtomSequence::from_atoms(['λ', 'x']), 42)]);
        let sequence = crate::AtomSequence::<crate::UnicodeScalar>::from_atoms(['λ', 'x']);
        assert_eq!(dictionary.get_atom_sequence_value(&sequence), Some(42));
    }

    #[test]
    fn test_node_edges() {
        let dict = SuffixAutomatonChar::<()>::from_text("ab");
        let root = dict.root();

        let edges: Vec<_> = root.edges().collect();
        assert!(!edges.is_empty());

        // Should have edges for suffixes "ab" and "b"
        let labels: Vec<_> = edges.iter().map(|(l, _)| *l).collect();
        assert!(labels.contains(&'a') || labels.contains(&'b'));
    }

    #[test]
    fn test_mapped_dictionary_basic() {
        use crate::MappedDictionary;

        let dict: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        dict.insert_with_value("test", 42);
        dict.insert_with_value("hello", 100);

        assert_eq!(dict.get_value("test"), Some(42));
        assert_eq!(dict.get_value("hello"), Some(100));
        assert_eq!(dict.get_value("missing"), None);
    }

    #[test]
    fn test_mapped_dictionary_contains_with_value() {
        use crate::MappedDictionary;

        let dict: SuffixAutomatonChar<String> = SuffixAutomatonChar::new();
        dict.insert_with_value("test", "value1".to_string());
        dict.insert_with_value("hello", "value2".to_string());

        assert!(dict.contains_with_value("test", |v| v == "value1"));
        assert!(!dict.contains_with_value("test", |v| v == "wrong"));
        assert!(!dict.contains_with_value("missing", |v| v == "value1"));
    }

    #[test]
    fn test_mapped_dictionary_vec_values() {
        use crate::MappedDictionary;

        let dict: SuffixAutomatonChar<Vec<usize>> = SuffixAutomatonChar::new();
        dict.insert_with_value("scoped", vec![1, 2, 3]);
        dict.insert_with_value("global", vec![0]);

        assert_eq!(dict.get_value("scoped"), Some(vec![1, 2, 3]));
        assert!(dict.contains_with_value("scoped", |v| v.contains(&2)));
        assert!(!dict.contains_with_value("scoped", |v| v.contains(&999)));
    }

    #[test]
    fn test_mapped_node_value() {
        use crate::MappedDictionaryNode;

        let dict: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        dict.insert_with_value("test", 42);

        // Navigate to "test"
        let root = dict.root();
        let t = root.transition('t').unwrap();
        let e = t.transition('e').unwrap();
        let s = e.transition('s').unwrap();
        let t2 = s.transition('t').unwrap();

        // The final node should have the value
        assert_eq!(t2.value(), Some(42));

        // Non-final nodes should not have values
        assert_eq!(t.value(), None);
    }

    #[test]
    fn test_unicode_cafe() {
        // Test with accented characters (multi-byte UTF-8)
        let dict = SuffixAutomatonChar::<()>::from_text("café");

        // All suffixes should be present
        assert!(dict.contains("café")); // 4 chars, 5 bytes
        assert!(dict.contains("afé")); // 3 chars, 4 bytes
        assert!(dict.contains("fé")); // 2 chars, 3 bytes
        assert!(dict.contains("é")); // 1 char, 2 bytes

        // Prefixes should also be found
        assert!(dict.contains("caf"));
        assert!(dict.contains("ca"));
        assert!(dict.contains("c"));
    }

    #[test]
    fn test_unicode_emoji() {
        // Test with emoji (4-byte UTF-8)
        let dict = SuffixAutomatonChar::<()>::from_text("test🎉ing");

        assert!(dict.contains("test🎉ing"));
        assert!(dict.contains("🎉ing"));
        assert!(dict.contains("🎉"));
        assert!(dict.contains("ing"));
    }

    #[test]
    fn test_unicode_cjk() {
        // Test with CJK characters
        let dict = SuffixAutomatonChar::<()>::from_text("你好世界");

        assert!(dict.contains("你好世界"));
        assert!(dict.contains("好世界"));
        assert!(dict.contains("世界"));
        assert!(dict.contains("界"));
        assert!(dict.contains("你好"));
        assert!(dict.contains("你"));
    }

    #[test]
    fn test_unicode_mixed() {
        // Test with mixed Unicode content
        let dict = SuffixAutomatonChar::<String>::from_texts(vec!["café☕", "naïve🌟", "中文test"]);

        assert_eq!(dict.string_count(), 3);
        assert!(dict.contains("café"));
        assert!(dict.contains("☕"));
        assert!(dict.contains("naïve"));
        assert!(dict.contains("🌟"));
        assert!(dict.contains("中文"));
        assert!(dict.contains("test"));
    }

    #[test]
    fn test_union_with_both_empty() {
        let dict1: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        let dict2: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();

        let processed = dict1.union_with(&dict2, |a, b| a + b);
        assert_eq!(processed, 0);
        assert_eq!(dict1.string_count(), 0);
    }

    #[test]
    fn test_union_with_self_empty() {
        let dict1: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        let dict2: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        dict2.insert_with_value("hello", 10);
        dict2.insert_with_value("world", 20);

        let processed = dict1.union_with(&dict2, |a, b| a + b);
        assert!(processed > 0);
        assert_eq!(dict1.get_value("hello"), Some(10));
        assert_eq!(dict1.get_value("world"), Some(20));
    }

    #[test]
    fn test_union_with_other_empty() {
        let dict1: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        dict1.insert_with_value("hello", 10);
        let dict2: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();

        let processed = dict1.union_with(&dict2, |a, b| a + b);
        assert_eq!(processed, 0);
        assert_eq!(dict1.get_value("hello"), Some(10));
    }

    #[test]
    fn test_union_with_no_conflicts() {
        let dict1: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        dict1.insert_with_value("hello", 10);
        let dict2: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        dict2.insert_with_value("world", 20);

        let processed = dict1.union_with(&dict2, |a, b| a + b);
        assert!(processed > 0);
        assert_eq!(dict1.get_value("hello"), Some(10));
        assert_eq!(dict1.get_value("world"), Some(20));
    }

    #[test]
    fn test_union_with_conflicts_sum() {
        let dict1: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        dict1.insert_with_value("hello", 10);
        let dict2: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        dict2.insert_with_value("hello", 20);

        let processed = dict1.union_with(&dict2, |a, b| a + b);
        assert!(processed > 0);
        assert_eq!(dict1.get_value("hello"), Some(30));
    }

    #[test]
    fn test_union_with_conflicts_max() {
        let dict1: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        dict1.insert_with_value("hello", 10);
        let dict2: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        dict2.insert_with_value("hello", 20);

        let processed = dict1.union_with(&dict2, |a, b| *a.max(b));
        assert!(processed > 0);
        assert_eq!(dict1.get_value("hello"), Some(20));
    }

    #[test]
    fn test_union_with_partial_conflicts() {
        let dict1: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        dict1.insert_with_value("apple", 1);
        dict1.insert_with_value("banana", 2);
        let dict2: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        dict2.insert_with_value("banana", 3);
        dict2.insert_with_value("cherry", 4);

        let processed = dict1.union_with(&dict2, |a, b| a + b);
        assert!(processed > 0);
        assert_eq!(dict1.get_value("apple"), Some(1));
        assert_eq!(dict1.get_value("banana"), Some(5));
        assert_eq!(dict1.get_value("cherry"), Some(4));
    }

    #[test]
    fn test_union_with_unicode() {
        let dict1: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        dict1.insert_with_value("café", 10);
        dict1.insert_with_value("中文", 20);
        let dict2: SuffixAutomatonChar<u32> = SuffixAutomatonChar::new();
        dict2.insert_with_value("中文", 30);
        dict2.insert_with_value("日本語", 40);

        let processed = dict1.union_with(&dict2, |a, b| a + b);
        assert!(processed > 0);
        assert_eq!(dict1.get_value("café"), Some(10));
        assert_eq!(dict1.get_value("中文"), Some(50));
        assert_eq!(dict1.get_value("日本語"), Some(40));
    }
}
