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

use super::lockfree::LockFreeScdawg;
use crate::substring::{BidirectionalDictionaryNode, SubstringDictionary, SubstringMatch};
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
// with a type alias to the generic `super::core::ScdawgNode<u8, V>`.
// The canonical impl carries the same methods with `label: U` instead
// of `label: u8`; for `U = u8` they resolve identically.
//
// Clone + Debug derives are already on the generic struct, so the
// alias inherits them automatically.
#[allow(dead_code)]
type ScdawgNode<V = ()> = super::core::ScdawgNode<u8, V>;

// ============================================================================
// True SCDAWG Inner State
// ============================================================================

// C4b algorithmic dedup: byte-for-byte-identical local ScdawgInner<V>
// struct + ~340-LOC impl block replaced with a type alias to the
// generic super::core::ScdawgCoreInner<u8, V>. Every algorithmic
// method (sa_extend, insert, compute_left_edges, find_substring_fast,
// contains_substring, find_exact_substring, frequency,
// count_occurrences, term_count, contains, iter_terms) now lives on the
// canonical generic core.
type ScdawgInner<V = ()> = super::core::ScdawgCoreInner<u8, V>;

// C4b: the original ~340-LOC impl<V> ScdawgInner<V> block lived
// here. All algorithmic methods (sa_extend, insert,
// compute_left_edges, find_substring_fast, contains_substring,
// find_exact_substring, collect_term_positions, frequency,
// count_occurrences, term_count, contains, iter_terms) now live on
// the canonical generic super::core::ScdawgCoreInner<U, V>.
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
    inner: LockFreeScdawg<u8, V>,
}

/// Snapshot-owning iterator over exact byte SCDAWG terms and optional values.
#[derive(Clone)]
pub struct ScdawgEntryIterator<V: DictionaryValue = ()> {
    inner: Arc<ScdawgInner<V>>,
    index: usize,
}

impl<V: DictionaryValue> Iterator for ScdawgEntryIterator<V> {
    type Item = (String, Option<V>);

    fn next(&mut self) -> Option<Self::Item> {
        let term_index = *self.inner.sorted_term_indices.get(self.index)?;
        let term = self.inner.terms.get(term_index)?.clone();
        self.index += 1;
        let value = self.inner.term_values.get(&term).cloned();
        Some((term, value))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self
            .inner
            .sorted_term_indices
            .len()
            .saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

impl<V: DictionaryValue> ExactSizeIterator for ScdawgEntryIterator<V> {}
impl<V: DictionaryValue> std::iter::FusedIterator for ScdawgEntryIterator<V> {}

impl<V: DictionaryValue> Default for Scdawg<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> Scdawg<V> {
    #[inline]
    fn from_inner(inner: ScdawgInner<V>) -> Self {
        Self {
            inner: LockFreeScdawg::from_inner(inner),
        }
    }

    /// Create a new empty true SCDAWG.
    pub fn new() -> Self {
        Self::from_inner(ScdawgInner::new())
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

        let mut inner = ScdawgInner::with_capacity(term_count, total_chars);
        for term in terms_vec {
            inner.insert(term.as_ref());
        }
        inner.compute_left_edges();
        Self::from_inner(inner)
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

        let mut inner = ScdawgInner::with_capacity(pairs.len(), total_chars);
        for (term, value) in pairs {
            inner.insert_with_value(&term, value);
        }
        inner.compute_left_edges();
        Self::from_inner(inner)
    }

    /// Insert a term.
    pub fn insert(&self, term: &str) -> bool {
        self.inner.mutate(|inner| {
            let result = inner.insert(term);
            if result {
                inner.compute_left_edges();
            }
            (result, result)
        })
    }

    /// Insert a term with a value.
    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        self.inner.mutate(|inner| {
            let result = inner.insert_with_value(term, value.clone());
            if result {
                inner.compute_left_edges();
            }
            (result, true)
        })
    }

    fn extend_records(&self, records: Vec<(String, Option<V>)>) {
        if records.is_empty() {
            return;
        }
        self.inner.mutate(|inner| {
            let mut added = false;
            let mut changed = false;
            for (term, value) in &records {
                match value {
                    Some(value) => {
                        added |= inner.insert_with_value(term, value.clone());
                        changed = true;
                    }
                    None => {
                        let inserted = inner.insert(term);
                        added |= inserted;
                        changed |= inserted;
                    }
                }
            }
            if added {
                inner.compute_left_edges();
            }
            ((), changed)
        });
    }

    /// Get the value associated with a term.
    ///
    /// Matches `ScdawgChar::get_value` (B3 parity backfill).
    pub fn get_value(&self, term: &str) -> Option<V>
    where
        V: Clone,
    {
        let inner = self.inner.load();
        if let Some(value) = inner.term_values.get(term) {
            return Some(value.clone());
        }

        let mut current = 0;
        for byte in term.bytes() {
            {
                let next = inner.nodes[current].get_edge(byte)?;
                current = next
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
        let inner = self.inner.load();
        inner.contains_substring(pattern)
    }

    /// Iterate over all terms.
    pub fn iter(&self) -> impl Iterator<Item = String> {
        self.iter_entries().map(|(term, _)| term)
    }

    /// Iterate lazily over all exact terms and their optional values.
    pub fn iter_entries(&self) -> ScdawgEntryIterator<V> {
        ScdawgEntryIterator {
            inner: self.inner.load(),
            index: 0,
        }
    }

    /// Get the number of terms in the SCDAWG.
    pub fn term_count(&self) -> usize {
        self.inner.load().term_count()
    }

    /// Capture the current root handle together with its term count from
    /// one atomically published revision.
    ///
    /// [`Dictionary::root`] and
    /// [`Dictionary::len`] load the inner SCDAWG
    /// revision independently, so a concurrent insert can tear the pair
    /// (finding LDICT-B4). Snapshot capture uses this coherent accessor.
    pub fn root_with_term_count(&self) -> (ScdawgNodeHandle<V>, usize) {
        let inner = self.inner.load();
        let term_count = inner.term_count();
        (ScdawgNodeHandle { inner, node_idx: 0 }, term_count)
    }

    #[cfg(feature = "bindings-core")]
    pub(crate) fn root_with_term_count_and_entries(
        &self,
    ) -> (ScdawgNodeHandle<V>, usize, ScdawgEntryIterator<V>) {
        let inner = self.inner.load();
        let term_count = inner.term_count();
        let entries = ScdawgEntryIterator {
            inner: Arc::clone(&inner),
            index: 0,
        };
        (ScdawgNodeHandle { inner, node_idx: 0 }, term_count, entries)
    }

    /// Get the number of nodes in the SCDAWG.
    pub fn node_count(&self) -> usize {
        self.inner.load().nodes.len()
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
        let inner = self.inner.load();
        inner
            .find_substring_fast(pattern)
            .map(|node_idx| ScdawgNodeHandle {
                inner: Arc::clone(&inner),
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
        let inner = self.inner.load();
        inner.frequency(pattern)
    }

    /// Get the frequency at a specific SCDAWG node handle.
    ///
    /// Use this with `find()` for efficient repeated frequency queries.
    pub fn freq_at(&self, handle: &ScdawgNodeHandle<V>) -> usize {
        let mut count = 0;
        handle.inner.count_occurrences(handle.node_idx, &mut count);
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
        let inner = self.inner.load();
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
        let mut results = Vec::with_capacity(handle.inner.nodes[handle.node_idx].term_ends.len());
        handle
            .inner
            .collect_term_positions(handle.node_idx, pattern_len, &mut results);
        results
    }
}

impl<V: DictionaryValue> FromIterator<String> for Scdawg<V> {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        Self::from_terms(iter)
    }
}

impl<'a, V: DictionaryValue> FromIterator<&'a str> for Scdawg<V> {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        Self::from_terms(iter)
    }
}

impl<V: DictionaryValue> FromIterator<(String, V)> for Scdawg<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        Self::from_terms_with_values(iter)
    }
}

impl<'a, V: DictionaryValue> FromIterator<(&'a str, V)> for Scdawg<V> {
    fn from_iter<I: IntoIterator<Item = (&'a str, V)>>(iter: I) -> Self {
        Self::from_terms_with_values(iter)
    }
}

impl<V: DictionaryValue> Extend<String> for Scdawg<V> {
    fn extend<I: IntoIterator<Item = String>>(&mut self, iter: I) {
        self.extend_records(iter.into_iter().map(|term| (term, None)).collect());
    }
}

impl<'a, V: DictionaryValue> Extend<&'a str> for Scdawg<V> {
    fn extend<I: IntoIterator<Item = &'a str>>(&mut self, iter: I) {
        <Self as Extend<String>>::extend(self, iter.into_iter().map(str::to_owned));
    }
}

impl<V: DictionaryValue> Extend<(String, V)> for Scdawg<V> {
    fn extend<I: IntoIterator<Item = (String, V)>>(&mut self, iter: I) {
        self.extend_records(
            iter.into_iter()
                .map(|(term, value)| (term, Some(value)))
                .collect(),
        );
    }
}

impl<'a, V: DictionaryValue> Extend<(&'a str, V)> for Scdawg<V> {
    fn extend<I: IntoIterator<Item = (&'a str, V)>>(&mut self, iter: I) {
        <Self as Extend<(String, V)>>::extend(
            self,
            iter.into_iter()
                .map(|(term, value)| (term.to_owned(), value)),
        );
    }
}

// ============================================================================
// Dictionary Trait Implementation
// ============================================================================

impl<V: DictionaryValue> Dictionary for Scdawg<V> {
    type Node = ScdawgNodeHandle<V>;

    fn len(&self) -> Option<usize> {
        Some(self.inner.load().term_count())
    }

    fn contains(&self, term: &str) -> bool {
        self.inner.load().contains(term)
    }

    fn root(&self) -> Self::Node {
        ScdawgNodeHandle {
            inner: self.inner.load(),
            node_idx: 0,
        }
    }

    fn sync_strategy(&self) -> crate::SyncStrategy {
        crate::SyncStrategy::InternalSync
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
    inner: Arc<ScdawgInner<V>>,
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
    type SnapshotCursor = crate::SnapshotTraversalCursor;
    type SnapshotGraphValueHandle = crate::SnapshotTraversalCursor;

    #[inline]
    fn snapshot_node_identity(&self) -> Option<crate::SnapshotNodeIdentity> {
        crate::SnapshotNodeIdentity::from_index(self.node_idx)
    }

    #[inline]
    fn snapshot_root_cursor(&self) -> Option<Self::SnapshotCursor> {
        self.inner.snapshot_cursor(self.node_idx)
    }

    #[inline]
    fn contains_snapshot_cursor(&self, cursor: Self::SnapshotCursor) -> bool {
        self.inner.contains_snapshot_cursor(cursor)
    }

    #[inline]
    fn supports_snapshot_cursor_nodes(&self) -> bool {
        true
    }

    #[inline]
    unsafe fn snapshot_cursor_node(&self, cursor: Self::SnapshotCursor) -> Option<Self> {
        self.inner
            .contains_snapshot_cursor(cursor)
            .then(|| ScdawgNodeHandle {
                inner: Arc::clone(&self.inner),
                node_idx: cursor.index(),
            })
    }

    #[inline]
    unsafe fn filter_map_snapshot_cursor_edges_and_finality<T, P, F>(
        &self,
        cursor: Self::SnapshotCursor,
        project: P,
        visitor: F,
    ) -> Option<bool>
    where
        P: FnMut(Self::Unit) -> Option<T>,
        F: FnMut(Self::Unit, Self::SnapshotCursor, T),
    {
        self.inner
            .filter_map_snapshot_cursor_edges_and_finality(cursor, project, visitor)
    }

    #[inline]
    unsafe fn snapshot_cursor_is_final(&self, cursor: Self::SnapshotCursor) -> Option<bool> {
        self.inner.snapshot_cursor_is_final(cursor)
    }

    #[inline]
    unsafe fn snapshot_cursor_transition(
        &self,
        cursor: Self::SnapshotCursor,
        label: Self::Unit,
    ) -> Option<Option<Self::SnapshotCursor>> {
        self.inner.snapshot_cursor_transition(cursor, label)
    }

    #[inline]
    fn supports_efficient_snapshot_cursor_edge_paging(&self) -> bool {
        true
    }

    #[inline]
    unsafe fn visit_snapshot_cursor_edge_page<F>(
        &self,
        cursor: Self::SnapshotCursor,
        start: usize,
        capacity: usize,
        visitor: F,
    ) -> Option<(bool, usize)>
    where
        F: FnMut(Self::Unit, Self::SnapshotCursor),
    {
        self.inner
            .visit_snapshot_cursor_edge_page(cursor, start, capacity, visitor)
    }

    fn is_final(&self) -> bool {
        self.inner
            .nodes
            .get(self.node_idx)
            .map(|node| node.is_final)
            .unwrap_or(false)
    }

    fn transition(&self, label: u8) -> Option<Self> {
        self.inner
            .nodes
            .get(self.node_idx)?
            .get_edge(label)
            .map(|idx| ScdawgNodeHandle {
                inner: Arc::clone(&self.inner),
                node_idx: idx,
            })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (u8, Self)> + '_> {
        let edges = self
            .inner
            .nodes
            .get(self.node_idx)
            .map(|node| node.forward_edges.clone())
            .unwrap_or_default();
        let inner = Arc::clone(&self.inner);
        let edges: Vec<_> = edges
            .into_iter()
            .map(|(label, idx)| {
                (
                    label,
                    ScdawgNodeHandle {
                        inner: Arc::clone(&inner),
                        node_idx: idx,
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }

    #[inline]
    fn for_each_edge<F>(&self, mut visitor: F)
    where
        F: FnMut(u8, Self),
    {
        let Some(node) = self.inner.nodes.get(self.node_idx) else {
            return;
        };
        for &(label, node_idx) in &node.forward_edges {
            visitor(
                label,
                ScdawgNodeHandle {
                    inner: Arc::clone(&self.inner),
                    node_idx,
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
        let Some(node) = self.inner.nodes.get(self.node_idx) else {
            return;
        };
        for &(label, node_idx) in &node.forward_edges {
            if let Some(projected) = project(label) {
                visitor(
                    label,
                    ScdawgNodeHandle {
                        inner: Arc::clone(&self.inner),
                        node_idx,
                    },
                    projected,
                );
            }
        }
    }

    fn edge_count(&self) -> Option<usize> {
        Some(
            self.inner
                .nodes
                .get(self.node_idx)
                .map(|node| node.forward_edges.len())
                .unwrap_or(0),
        )
    }
}

impl<V: DictionaryValue> crate::MappedDictionaryNode for ScdawgNodeHandle<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        self.inner
            .nodes
            .get(self.node_idx)
            .filter(|node| node.is_final)
            .and_then(|node| node.value.clone())
    }

    #[inline]
    fn supports_snapshot_cursor_values(&self) -> bool {
        true
    }

    #[inline]
    unsafe fn snapshot_cursor_value(
        &self,
        cursor: Self::SnapshotCursor,
    ) -> Option<Option<Self::Value>> {
        self.inner.snapshot_cursor_value(cursor)
    }
}

unsafe impl<V: DictionaryValue> Send for ScdawgNodeHandle<V> {}
unsafe impl<V: DictionaryValue> Sync for ScdawgNodeHandle<V> {}

// ============================================================================
// BidirectionalDictionaryNode Implementation
// ============================================================================

impl<V: DictionaryValue> BidirectionalDictionaryNode for ScdawgNodeHandle<V> {
    fn parent(&self) -> Option<Self> {
        let node = self.inner.nodes.get(self.node_idx)?;
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
        let node = self.inner.nodes.get(self.node_idx)?;
        if node.parent == NIL {
            None
        } else {
            Some(node.parent_label)
        }
    }

    fn reverse_edges(&self) -> Box<dyn Iterator<Item = (u8, Self)> + '_> {
        let edges = self
            .inner
            .nodes
            .get(self.node_idx)
            .map(|node| node.left_edges.clone())
            .unwrap_or_default();
        let inner = Arc::clone(&self.inner);
        let edges: Vec<_> = edges
            .into_iter()
            .map(|(label, idx)| {
                (
                    label,
                    ScdawgNodeHandle {
                        inner: Arc::clone(&inner),
                        node_idx: idx,
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }

    fn reverse_transition(&self, label: u8) -> Vec<Self> {
        self.inner
            .nodes
            .get(self.node_idx)
            .map(|node| node.left_edges.iter())
            .into_iter()
            .flatten()
            .filter(|(l, _)| *l == label)
            .map(|(_, idx)| ScdawgNodeHandle {
                inner: Arc::clone(&self.inner),
                node_idx: *idx,
            })
            .collect()
    }

    fn depth(&self) -> usize {
        self.inner
            .nodes
            .get(self.node_idx)
            .map(|node| node.depth)
            .unwrap_or(0)
    }
}

// ============================================================================
// SubstringDictionary Implementation
// ============================================================================

impl<V: DictionaryValue> SubstringDictionary for Scdawg<V> {
    fn find_exact_substring_in_snapshot(
        snapshot_root: &Self::Node,
        pattern: &str,
    ) -> Vec<SubstringMatch<Self::Node>> {
        debug_assert_eq!(
            snapshot_root.node_idx, 0,
            "substring snapshots start at root"
        );
        let inner = Arc::clone(&snapshot_root.inner);
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
                        inner: Arc::clone(&inner),
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
    use crate::MappedDictionaryNode;
    use log::debug;
    use std::collections::HashSet;

    #[test]
    fn native_cursor_traversal_exactly_matches_byte_node_graph() {
        let scdawg =
            Scdawg::from_terms_with_values([("cab", 11_u32), ("car", 12), ("dog", 13), ("z", 14)]);
        let owner = scdawg.root();
        let root_cursor = owner.snapshot_root_cursor().expect("SCDAWG root cursor");

        assert!(owner.supports_snapshot_cursor_nodes());
        assert!(owner.supports_snapshot_cursor_values());
        assert!(owner.supports_efficient_snapshot_cursor_edge_paging());
        assert!(owner.contains_snapshot_cursor(root_cursor));

        let mut pending = vec![(owner.clone(), root_cursor)];
        let mut visited = HashSet::new();
        while let Some((node, cursor)) = pending.pop() {
            if !visited.insert(cursor.index()) {
                continue;
            }

            let mut owned_edges = Vec::new();
            node.for_each_edge(|label, child| {
                owned_edges.push((label, child.node_idx, child));
            });

            let mut cursor_edges = Vec::new();
            // SAFETY: `cursor` is the retained root cursor or a child emitted
            // by this exact immutable owner earlier in this traversal.
            let finality = unsafe {
                owner.filter_map_snapshot_cursor_edges_and_finality(
                    cursor,
                    |_| Some(()),
                    |label, child, ()| cursor_edges.push((label, child)),
                )
            };
            assert_eq!(finality, Some(node.is_final()));
            assert_eq!(
                cursor_edges
                    .iter()
                    .map(|(label, child)| (*label, child.index()))
                    .collect::<Vec<_>>(),
                owned_edges
                    .iter()
                    .map(|(label, child_idx, _)| (*label, *child_idx))
                    .collect::<Vec<_>>(),
                "native cursor traversal must preserve the SCDAWG's sorted edge order"
            );

            // SAFETY: `cursor` has the same retained-revision provenance.
            assert_eq!(
                unsafe { owner.snapshot_cursor_is_final(cursor) },
                Some(node.is_final())
            );
            // SAFETY: `cursor` has the same retained-revision provenance.
            assert_eq!(
                unsafe { owner.snapshot_cursor_value(cursor) },
                Some(node.value())
            );
            // SAFETY: `cursor` has the same retained-revision provenance.
            let materialized = unsafe { owner.snapshot_cursor_node(cursor) }
                .expect("every valid dense cursor materializes a node");
            assert_eq!(materialized.node_idx, node.node_idx);

            let mut paged_edges = Vec::new();
            for start in 0..=owned_edges.len() {
                let mut page = Vec::new();
                // SAFETY: `cursor` has the same retained-revision provenance.
                let page_metadata = unsafe {
                    owner.visit_snapshot_cursor_edge_page(cursor, start, 1, |label, child| {
                        page.push((label, child.index()));
                    })
                };
                assert_eq!(page_metadata, Some((node.is_final(), owned_edges.len())));
                paged_edges.extend(page);
            }
            assert_eq!(
                paged_edges,
                owned_edges
                    .iter()
                    .map(|(label, child_idx, _)| (*label, *child_idx))
                    .collect::<Vec<_>>()
            );

            for ((label, child_idx, child), (_, child_cursor)) in
                owned_edges.into_iter().zip(cursor_edges)
            {
                // SAFETY: `cursor` has the same retained-revision provenance.
                let transitioned = unsafe { owner.snapshot_cursor_transition(cursor, label) };
                assert_eq!(
                    transitioned.map(|result| result.map(|next| next.index())),
                    Some(Some(child_idx))
                );
                pending.push((child, child_cursor));
            }
            // SAFETY: test terms contain no 0xff edge and `cursor` belongs to
            // this retained revision.
            assert_eq!(
                unsafe { owner.snapshot_cursor_transition(cursor, u8::MAX) },
                Some(None)
            );
        }

        assert!(visited.contains(&root_cursor.index()));
        let invalid = crate::SnapshotTraversalCursor::from_index(owner.inner.nodes.len())
            .expect("one-past-end cursor remains representable");
        assert!(!owner.contains_snapshot_cursor(invalid));
    }

    #[test]
    fn native_cursor_owner_retains_byte_snapshot_across_publication() {
        let scdawg = Scdawg::from_terms_with_values([("cab", 1_u32), ("dog", 2)]);
        let old_owner = scdawg.root();
        let old_root = old_owner
            .snapshot_root_cursor()
            .expect("old SCDAWG root cursor");
        let old_node_count = old_owner.inner.nodes.len();

        assert!(scdawg.insert_with_value("zoo", 99));
        let fresh_owner = scdawg.root();
        let fresh_root = fresh_owner
            .snapshot_root_cursor()
            .expect("fresh SCDAWG root cursor");

        assert!(!Arc::ptr_eq(&old_owner.inner, &fresh_owner.inner));
        assert_eq!(old_owner.inner.nodes.len(), old_node_count);
        assert!(fresh_owner.inner.nodes.len() > old_node_count);
        // SAFETY: each cursor is used only with the owner that created it.
        assert_eq!(
            unsafe { old_owner.snapshot_cursor_transition(old_root, b'z') },
            Some(None),
            "the retained revision must not observe later publications"
        );
        // SAFETY: `fresh_root` belongs to `fresh_owner`.
        let z = unsafe { fresh_owner.snapshot_cursor_transition(fresh_root, b'z') }
            .expect("cursor traversal supported")
            .expect("fresh revision contains z");
        // SAFETY: `z` was emitted by `fresh_owner` immediately above.
        let o = unsafe { fresh_owner.snapshot_cursor_transition(z, b'o') }
            .expect("cursor traversal supported")
            .expect("fresh revision contains zo");
        // SAFETY: `o` was emitted by `fresh_owner` immediately above.
        let zoo = unsafe { fresh_owner.snapshot_cursor_transition(o, b'o') }
            .expect("cursor traversal supported")
            .expect("fresh revision contains zoo");
        // SAFETY: `zoo` descends from `fresh_root` in this retained revision.
        assert_eq!(
            unsafe { fresh_owner.snapshot_cursor_is_final(zoo) },
            Some(true)
        );
        // SAFETY: `zoo` descends from `fresh_root` in this retained revision.
        assert_eq!(
            unsafe { fresh_owner.snapshot_cursor_value(zoo) },
            Some(Some(99))
        );
    }

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
        let inner = scdawg.inner.load();

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
