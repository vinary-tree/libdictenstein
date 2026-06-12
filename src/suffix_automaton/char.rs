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
//! [`liblevenshtein`](https://github.com/universal-automata/liblevenshtein-rust)
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
//! **Recommendation**: Use `iter()` to enumerate explicitly indexed terms, or
//! track indexed terms externally if precise removal semantics are required.
//!
//! # References
//!
//! - Blumer et al. (1985): "The smallest automaton recognizing the subwords of a text"
//! - Design document: `docs/SUFFIX_AUTOMATON_DESIGN.md`

use std::collections::HashMap;
use std::sync::Arc;

use crate::sync_compat::RwLock;

use super::char_zipper::SuffixAutomatonCharZipper;
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
/// This is wrapped in Arc<RwLock<...>> to provide thread-safe concurrent access
/// with dynamic mutation support.
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
/// Uses `Arc<RwLock<...>>` for safe concurrent access with dynamic updates.
/// Multiple readers can query simultaneously, with exclusive access for writes.
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
/// [`liblevenshtein`](https://github.com/universal-automata/liblevenshtein-rust)'s
/// `Transducer` (upstream-owned, not part of this crate).
#[derive(Clone, Debug)]
pub struct SuffixAutomatonChar<V: DictionaryValue = ()> {
    pub(crate) inner: Arc<RwLock<SuffixAutomatonCharInner<V>>>,
}

impl<V: DictionaryValue> SuffixAutomatonChar<V> {
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
        Self {
            inner: Arc::new(RwLock::new(SuffixAutomatonCharInner::new())),
        }
    }

    /// Get the number of states in the automaton (for debugging).
    pub fn state_count(&self) -> usize {
        self.inner.read().nodes.len()
    }

    /// Debug: print automaton structure (for development).
    #[allow(dead_code)]
    pub fn debug_print(&self) {
        let inner = self.inner.read();
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
        let dict = Self::new();
        dict.insert(text);
        dict
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
        let dict = Self::new();
        for text in texts {
            dict.insert(text.as_ref());
        }
        dict
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
        let mut inner = self.inner.write();
        let string_id = inner.string_count;

        // Store source text for serialization
        inner.source_texts.push(text.to_string());

        // Extend automaton with each character
        for ch in text.chars() {
            inner.extend(ch);
        }

        // Record position metadata for the end-of-string state
        // Note: is_final is already set to true during extend()
        let last_state = inner.last_state;
        inner
            .positions
            .entry(last_state)
            .or_default()
            .push((string_id, text.len()));

        inner.string_count += 1;

        // Reset to root for next insertion (generalized automaton)
        inner.last_state = 0;

        true
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
        let mut inner = self.inner.write();

        // Navigate to end state for this text
        let mut state = 0;
        for ch in text.chars() {
            match inner.nodes[state].find_edge(ch) {
                Some(next) => state = next,
                None => return false, // String not present
            }
        }

        // Check if this text's end position is recorded at this state
        let removed = if let Some(positions) = inner.positions.get_mut(&state) {
            let original_len = positions.len();
            positions.retain(|(_, end)| *end != text.len());
            positions.len() < original_len
        } else {
            false
        };

        if removed {
            // Remove from source_texts (mark as empty string to preserve indices)
            // We can't actually remove without reindexing, so we'll handle this in compact()
            // For now, we'll just track removal via positions metadata

            // Check if we need to remove the state from positions map
            let should_remove = inner
                .positions
                .get(&state)
                .map(|v| v.is_empty())
                .unwrap_or(false);

            if should_remove {
                // Note: We keep is_final=true because this state still represents
                // a valid substring (possibly from other indexed strings).
                // Only remove from positions map.
                inner.positions.remove(&state);
            }

            inner.needs_compaction = true;
            inner.string_count -= 1;
            true
        } else {
            false
        }
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
        let mut inner = self.inner.write();
        *inner = SuffixAutomatonCharInner::new();
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
        let mut inner = self.inner.write();

        if !inner.needs_compaction {
            return;
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
        let mut new_nodes = Vec::new();
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
        let mut new_positions = HashMap::new();
        for (old_state, positions) in inner.positions.drain() {
            if reachable[old_state] {
                new_positions.insert(old_to_new[old_state], positions);
            }
        }

        inner.nodes = new_nodes;
        inner.positions = new_positions;
        inner.last_state = 0;
        inner.needs_compaction = false;
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
        self.inner.read().string_count
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
        self.inner.read().needs_compaction
    }

    /// Get match positions for a substring.
    ///
    /// Returns a list of (string_id, end_position) tuples indicating where
    /// the substring appears in the indexed texts.
    ///
    /// **Note**: Currently only returns positions if the substring matches
    /// at the end of an indexed string. Full position tracking for all
    /// substrings is a future enhancement.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
    ///
    /// let docs = vec!["testing", "test"];
    /// let dict = SuffixAutomatonChar::<()>::from_texts(docs);
    ///
    /// // Note: Position tracking is currently limited to final states
    /// // This is a placeholder for future enhancement
    /// let positions = dict.match_positions("test");
    /// // positions will contain entries for strings ending exactly with "test"
    /// ```
    pub fn match_positions(&self, substring: &str) -> Vec<(usize, usize)> {
        let inner = self.inner.read();

        // Navigate to the state for this substring
        let mut state = 0;
        for ch in substring.chars() {
            match inner.nodes[state].find_edge(ch) {
                Some(next) => state = next,
                None => return Vec::new(), // Substring not found
            }
        }

        // Collect positions from this state and all reachable final states
        // via epsilon-like transitions (suffix links and forward edges)
        let mut result = Vec::new();

        // Add positions directly associated with this state
        if let Some(positions) = inner.positions.get(&state) {
            result.extend(positions.iter().copied());
        }

        // For a more complete implementation, we would need to traverse
        // all states reachable from here to find all occurrences.
        // This is left as a future enhancement for full position tracking.

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
        F: FnOnce(&mut V),
    {
        let mut inner = self.inner.write();

        // Try to navigate to the term
        let mut state = 0;
        for ch in term.chars() {
            match inner.nodes[state].find_edge(ch) {
                Some(next) => state = next,
                None => {
                    // Term doesn't exist - need to insert it
                    drop(inner);
                    return self.insert_with_value_internal(term, default_value);
                }
            }
        }

        // Term exists - check if it has a value
        if inner.nodes[state].value.is_some() {
            // Update existing value (guard above proves Some).
            update_fn(
                inner.nodes[state]
                    .value
                    .as_mut()
                    .expect("value.is_some() checked one line above"),
            );
            false
        } else {
            // Node exists but no value - set the default value
            inner.nodes[state].value = Some(default_value);
            inner.nodes[state].is_final = true;
            true
        }
    }

    /// Internal helper for insert_with_value to avoid deadlock in update_or_insert.
    fn insert_with_value_internal(&self, term: &str, value: V) -> bool {
        let mut inner = self.inner.write();

        // Reset to root for new string
        inner.last_state = 0;

        // Extend with all characters
        for ch in term.chars() {
            inner.extend(ch);
        }

        // Set the value at the final state
        let final_state = inner.last_state;
        inner.nodes[final_state].value = Some(value);

        // Track the new string
        inner.string_count += 1;
        inner.source_texts.push(term.to_string());

        // Reset last_state for future insertions
        inner.last_state = 0;

        true
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
        let inner = self.inner.read();
        inner.source_texts.clone()
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
    /// **Note**: This only works for dictionaries created with values.
    /// For dictionaries without values, use `iter_terms()` instead.
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

impl<V: DictionaryValue> IntoIterator for &SuffixAutomatonChar<V> {
    type Item = (Vec<char>, V);
    type IntoIter = DictionaryIterator<SuffixAutomatonCharZipper<V>>;

    /// Creates an iterator over all `(substring, value)` pairs as character vectors.
    fn into_iter(self) -> Self::IntoIter {
        self.iter_chars()
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
        // Extract the inner data by acquiring read lock
        let inner = self.inner.read();
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
        let inner = SuffixAutomatonCharInner::deserialize(deserializer)?;
        Ok(SuffixAutomatonChar {
            inner: Arc::new(RwLock::new(inner)),
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
        let inner = SuffixAutomatonCharInner::deserialize(deserializer)?;
        Ok(SuffixAutomatonChar {
            inner: Arc::new(RwLock::new(inner)),
        })
    }
}

/// Handle for traversing the suffix automaton.
///
/// Implements `DictionaryNode` trait for compatibility with existing
/// `Transducer` and query infrastructure.
#[derive(Clone, Debug)]
pub struct SuffixNodeCharHandle<V: DictionaryValue = ()> {
    /// Reference to the automaton (for traversal).
    automaton: Arc<RwLock<SuffixAutomatonCharInner<V>>>,

    /// Current state index.
    state_id: usize,
}

impl<V: DictionaryValue> DictionaryNode for SuffixNodeCharHandle<V> {
    type Unit = char;

    fn is_final(&self) -> bool {
        let inner = self.automaton.read();
        inner.nodes[self.state_id].is_final
    }

    fn transition(&self, label: char) -> Option<Self> {
        let inner = self.automaton.read();
        inner.nodes[self.state_id]
            .find_edge(label)
            .map(|target| Self {
                automaton: Arc::clone(&self.automaton),
                state_id: target,
            })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (char, Self)> + '_> {
        // Clone edges to avoid holding lock during iteration
        let inner = self.automaton.read();
        let edges = inner.nodes[self.state_id].edges.clone();
        drop(inner);

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

    fn has_edge(&self, label: char) -> bool {
        let inner = self.automaton.read();
        inner.nodes[self.state_id].find_edge(label).is_some()
    }

    fn edge_count(&self) -> Option<usize> {
        let inner = self.automaton.read();
        Some(inner.nodes[self.state_id].edges.len())
    }
}

impl<V: DictionaryValue> Dictionary for SuffixAutomatonChar<V> {
    type Node = SuffixNodeCharHandle<V>;

    fn root(&self) -> Self::Node {
        SuffixNodeCharHandle {
            automaton: Arc::clone(&self.inner),
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
        SyncStrategy::ExternalSync // Uses RwLock
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
        let inner = self.automaton.read();
        inner
            .nodes
            .get(self.state_id)
            .and_then(|node| node.value.clone())
    }
}

impl<V: DictionaryValue> MappedDictionary for SuffixAutomatonChar<V> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        // Navigate to the term
        let mut node = self.root();
        for ch in term.chars() {
            match node.transition(ch) {
                Some(next) => node = next,
                None => return None,
            }
        }

        // Return value if the node has one
        node.value()
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
        F: FnOnce(&mut Self::Value),
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
                self.update_or_insert(&term, new_value, move |v| *v = new_value_clone);
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
        // Position tracking currently works for strings that end at final states
        let docs = vec!["hello", "world"];
        let dict = SuffixAutomatonChar::<()>::from_texts(docs);

        // "hello" and "world" are complete strings, so they end at final states
        let positions_hello = dict.match_positions("hello");
        assert!(!positions_hello.is_empty());
        assert_eq!(positions_hello[0].0, 0); // Document 0

        let positions_world = dict.match_positions("world");
        assert!(!positions_world.is_empty());
        assert_eq!(positions_world[0].0, 1); // Document 1

        // Suffixes also work (they reach the same final states)
        let positions_ello = dict.match_positions("ello");
        assert!(!positions_ello.is_empty());
    }

    #[test]
    fn test_dictionary_trait() {
        let dict = SuffixAutomatonChar::<()>::from_text("test");

        // Test Dictionary trait methods
        assert_eq!(dict.len(), Some(1));
        assert!(!dict.is_empty());
        assert_eq!(dict.sync_strategy(), SyncStrategy::ExternalSync);

        // Test node traversal
        let root = dict.root();
        assert!(root.has_edge('t'));

        let node_t = root.transition('t').unwrap();
        assert!(node_t.has_edge('e'));
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
