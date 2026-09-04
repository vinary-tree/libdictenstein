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

use super::lockfree::LockFreeScdawg;
use crate::substring::{BidirectionalDictionaryNode, SubstringDictionary, SubstringMatch};
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
    inner: LockFreeScdawg<char, V>,
}

/// Snapshot-owning iterator over exact SCDAWG terms and optional values.
///
/// The iterator retains one atomically published SCDAWG revision and clones
/// only the entry currently yielded; it never clones or materializes the full
/// term collection.
#[derive(Clone)]
pub struct ScdawgCharEntryIterator<V: DictionaryValue = ()> {
    inner: Arc<ScdawgCharInner<V>>,
    index: usize,
}

impl<V: DictionaryValue> Iterator for ScdawgCharEntryIterator<V> {
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

impl<V: DictionaryValue> ExactSizeIterator for ScdawgCharEntryIterator<V> {}
impl<V: DictionaryValue> std::iter::FusedIterator for ScdawgCharEntryIterator<V> {}

#[derive(Clone, Debug)]
struct OccurrenceFrame {
    node: usize,
    term_end_index: usize,
    left_edge_index: usize,
}

enum OccurrenceState {
    EmptyPattern { term_index: usize },
    Missing,
    Found { stack: Vec<OccurrenceFrame> },
}

/// Snapshot-owning iterator over SCDAWG substring occurrence locations.
///
/// Positions are Unicode scalar indices, matching [`ScdawgChar::locations`].
/// Traversal uses an explicit stack and retains only O(graph depth) state.
pub struct ScdawgCharOccurrenceIterator<V: DictionaryValue = ()> {
    inner: Arc<ScdawgCharInner<V>>,
    pattern_len: usize,
    state: OccurrenceState,
}

impl<V: DictionaryValue> Iterator for ScdawgCharOccurrenceIterator<V> {
    type Item = (String, usize);

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.state {
            OccurrenceState::EmptyPattern { term_index } => {
                let term = self.inner.terms.get(*term_index)?.clone();
                *term_index += 1;
                Some((term, 0))
            }
            OccurrenceState::Missing => None,
            OccurrenceState::Found { stack } => loop {
                let frame = stack.last_mut()?;
                let node = &self.inner.nodes[frame.node];

                while let Some(&(term_index, end_position)) =
                    node.term_ends.get(frame.term_end_index)
                {
                    frame.term_end_index += 1;
                    if end_position + 1 >= self.pattern_len {
                        if let Some(term) = self.inner.terms.get(term_index) {
                            return Some((term.clone(), end_position + 1 - self.pattern_len));
                        }
                    }
                }

                if let Some(&(_, child)) = node.left_edges.get(frame.left_edge_index) {
                    frame.left_edge_index += 1;
                    stack.push(OccurrenceFrame {
                        node: child,
                        term_end_index: 0,
                        left_edge_index: 0,
                    });
                } else {
                    stack.pop();
                }
            },
        }
    }
}

impl<V: DictionaryValue> Default for ScdawgChar<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> ScdawgChar<V> {
    #[inline]
    fn from_inner(inner: ScdawgCharInner<V>) -> Self {
        Self {
            inner: LockFreeScdawg::from_inner(inner),
        }
    }

    /// Create a new empty Unicode-aware SCDAWG.
    pub fn new() -> Self {
        Self::from_inner(ScdawgCharInner::new())
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

        let mut inner = ScdawgCharInner::with_capacity(term_count, total_chars);
        for term in terms_vec {
            inner.insert(term.as_ref());
        }
        inner.compute_left_edges();
        Self::from_inner(inner)
    }

    /// Build from Unicode-scalar profile sequences without introducing
    /// UTF-8 byte transitions or suffixes inside a scalar.
    pub fn from_atom_sequences<P, I>(sequences: I) -> Self
    where
        P: crate::AtomProfile<Atom = char>,
        I: IntoIterator<Item = crate::AtomSequence<P>>,
    {
        Self::from_terms(
            sequences
                .into_iter()
                .map(|sequence| sequence.as_atoms().iter().copied().collect::<String>()),
        )
    }

    /// Create from an iterator of (term, value) pairs.
    pub fn from_terms_with_values<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        let entries: Vec<(String, V)> = terms
            .into_iter()
            .map(|(term, value)| (term.as_ref().to_string(), value))
            .collect();
        let total_chars: usize = entries.iter().map(|(term, _)| term.chars().count()).sum();
        let mut inner = ScdawgCharInner::with_capacity(entries.len(), total_chars);
        for (term, value) in entries {
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
    pub fn iter_entries(&self) -> ScdawgCharEntryIterator<V> {
        ScdawgCharEntryIterator {
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
    pub fn root_with_term_count(&self) -> (ScdawgCharNodeHandle<V>, usize) {
        let inner = self.inner.load();
        let term_count = inner.term_count();
        (ScdawgCharNodeHandle { inner, node_idx: 0 }, term_count)
    }

    #[cfg(feature = "bindings-core")]
    pub(crate) fn root_with_term_count_and_entries(
        &self,
    ) -> (ScdawgCharNodeHandle<V>, usize, ScdawgCharEntryIterator<V>) {
        let inner = self.inner.load();
        let term_count = inner.term_count();
        let entries = ScdawgCharEntryIterator {
            inner: Arc::clone(&inner),
            index: 0,
        };
        (
            ScdawgCharNodeHandle { inner, node_idx: 0 },
            term_count,
            entries,
        )
    }

    /// Get the number of nodes in the SCDAWG.
    pub fn node_count(&self) -> usize {
        self.inner.load().nodes.len()
    }

    /// Get the value associated with a term.
    pub fn get_value(&self, term: &str) -> Option<V>
    where
        V: Clone,
    {
        let inner = self.inner.load();
        if let Some(value) = inner.term_values.get(term) {
            return Some(value.clone());
        }

        let mut current = 0;
        for ch in term.chars() {
            {
                let next = inner.nodes[current].get_edge(ch)?;
                current = next
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
        let inner = self.inner.load();
        inner
            .find_substring_fast(pattern)
            .map(|node_idx| ScdawgCharNodeHandle {
                inner: Arc::clone(&inner),
                node_idx,
            })
    }

    /// Get the frequency (occurrence count) of a substring pattern.
    ///
    /// This is the `freq(x)` operation from Blumer et al. (1987).
    pub fn freq(&self, pattern: &str) -> usize {
        let inner = self.inner.load();
        inner.frequency(pattern)
    }

    /// Get the frequency at a specific SCDAWG node handle.
    pub fn freq_at(&self, handle: &ScdawgCharNodeHandle<V>) -> usize {
        let mut count = 0;
        handle.inner.count_occurrences(handle.node_idx, &mut count);
        count
    }

    /// Get all occurrence locations of a substring pattern.
    ///
    /// This is the `locations(x)` operation from Blumer et al. (1987).
    /// Returns (term, start_position) pairs where position is in characters.
    pub fn locations(&self, pattern: &str) -> Vec<(String, usize)> {
        self.locations_iter(pattern).collect()
    }

    /// Lazily enumerate all occurrence locations of a substring pattern.
    pub fn locations_iter(&self, pattern: &str) -> ScdawgCharOccurrenceIterator<V> {
        let inner = self.inner.load();
        let pattern_len = pattern.chars().count();
        let state = if pattern.is_empty() {
            OccurrenceState::EmptyPattern { term_index: 0 }
        } else {
            match inner.find_substring_fast(pattern) {
                Some(node) => OccurrenceState::Found {
                    stack: vec![OccurrenceFrame {
                        node,
                        term_end_index: 0,
                        left_edge_index: 0,
                    }],
                },
                None => OccurrenceState::Missing,
            }
        };
        ScdawgCharOccurrenceIterator {
            inner,
            pattern_len,
            state,
        }
    }

    /// Get all occurrence locations from a specific SCDAWG node handle.
    pub fn locations_at(
        &self,
        handle: &ScdawgCharNodeHandle<V>,
        pattern_len: usize,
    ) -> Vec<(String, usize)> {
        self.locations_at_iter(handle, pattern_len).collect()
    }

    /// Lazily enumerate occurrence locations from a captured state handle.
    pub fn locations_at_iter(
        &self,
        handle: &ScdawgCharNodeHandle<V>,
        pattern_len: usize,
    ) -> ScdawgCharOccurrenceIterator<V> {
        ScdawgCharOccurrenceIterator {
            inner: Arc::clone(&handle.inner),
            pattern_len,
            state: OccurrenceState::Found {
                stack: vec![OccurrenceFrame {
                    node: handle.node_idx,
                    term_end_index: 0,
                    left_edge_index: 0,
                }],
            },
        }
    }
}

impl<V: DictionaryValue> FromIterator<String> for ScdawgChar<V> {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        Self::from_terms(iter)
    }
}

impl<'a, V: DictionaryValue> FromIterator<&'a str> for ScdawgChar<V> {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        Self::from_terms(iter)
    }
}

impl<V: DictionaryValue> FromIterator<(String, V)> for ScdawgChar<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        Self::from_terms_with_values(iter)
    }
}

impl<'a, V: DictionaryValue> FromIterator<(&'a str, V)> for ScdawgChar<V> {
    fn from_iter<I: IntoIterator<Item = (&'a str, V)>>(iter: I) -> Self {
        Self::from_terms_with_values(iter)
    }
}

impl<V: DictionaryValue> Extend<String> for ScdawgChar<V> {
    fn extend<I: IntoIterator<Item = String>>(&mut self, iter: I) {
        self.extend_records(iter.into_iter().map(|term| (term, None)).collect());
    }
}

impl<'a, V: DictionaryValue> Extend<&'a str> for ScdawgChar<V> {
    fn extend<I: IntoIterator<Item = &'a str>>(&mut self, iter: I) {
        <Self as Extend<String>>::extend(self, iter.into_iter().map(str::to_owned));
    }
}

impl<V: DictionaryValue> Extend<(String, V)> for ScdawgChar<V> {
    fn extend<I: IntoIterator<Item = (String, V)>>(&mut self, iter: I) {
        self.extend_records(
            iter.into_iter()
                .map(|(term, value)| (term, Some(value)))
                .collect(),
        );
    }
}

impl<'a, V: DictionaryValue> Extend<(&'a str, V)> for ScdawgChar<V> {
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

impl<V: DictionaryValue> Dictionary for ScdawgChar<V> {
    type Node = ScdawgCharNodeHandle<V>;

    fn len(&self) -> Option<usize> {
        Some(self.inner.load().term_count())
    }

    fn contains(&self, term: &str) -> bool {
        self.inner.load().contains(term)
    }

    fn root(&self) -> Self::Node {
        ScdawgCharNodeHandle {
            inner: self.inner.load(),
            node_idx: 0,
        }
    }

    fn sync_strategy(&self) -> crate::SyncStrategy {
        crate::SyncStrategy::InternalSync
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
    inner: Arc<ScdawgCharInner<V>>,
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
            .then(|| ScdawgCharNodeHandle {
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

    fn transition(&self, label: char) -> Option<Self> {
        self.inner
            .nodes
            .get(self.node_idx)?
            .get_edge(label)
            .map(|idx| ScdawgCharNodeHandle {
                inner: Arc::clone(&self.inner),
                node_idx: idx,
            })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (char, Self)> + '_> {
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
                    ScdawgCharNodeHandle {
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
        F: FnMut(char, Self),
    {
        let Some(node) = self.inner.nodes.get(self.node_idx) else {
            return;
        };
        for &(label, node_idx) in &node.forward_edges {
            visitor(
                label,
                ScdawgCharNodeHandle {
                    inner: Arc::clone(&self.inner),
                    node_idx,
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
        let Some(node) = self.inner.nodes.get(self.node_idx) else {
            return;
        };
        for &(label, node_idx) in &node.forward_edges {
            if let Some(projected) = project(label) {
                visitor(
                    label,
                    ScdawgCharNodeHandle {
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

impl<V: DictionaryValue> crate::MappedDictionaryNode for ScdawgCharNodeHandle<V> {
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

unsafe impl<V: DictionaryValue> Send for ScdawgCharNodeHandle<V> {}
unsafe impl<V: DictionaryValue> Sync for ScdawgCharNodeHandle<V> {}

// ============================================================================
// BidirectionalDictionaryNode Implementation
// ============================================================================

impl<V: DictionaryValue> BidirectionalDictionaryNode for ScdawgCharNodeHandle<V> {
    fn parent(&self) -> Option<Self> {
        let node = self.inner.nodes.get(self.node_idx)?;
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
        let node = self.inner.nodes.get(self.node_idx)?;
        if node.parent == NIL {
            None
        } else {
            Some(node.parent_label)
        }
    }

    fn reverse_edges(&self) -> Box<dyn Iterator<Item = (char, Self)> + '_> {
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
                    ScdawgCharNodeHandle {
                        inner: Arc::clone(&inner),
                        node_idx: idx,
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }

    fn reverse_transition(&self, label: char) -> Vec<Self> {
        self.inner
            .nodes
            .get(self.node_idx)
            .map(|node| node.left_edges.iter())
            .into_iter()
            .flatten()
            .filter(|(l, _)| *l == label)
            .map(|(_, idx)| ScdawgCharNodeHandle {
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

impl<V: DictionaryValue> SubstringDictionary for ScdawgChar<V> {
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
                        inner: Arc::clone(&inner),
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
    use crate::MappedDictionaryNode;
    use std::collections::HashSet;

    #[test]
    fn native_cursor_traversal_exactly_matches_unicode_node_graph() {
        let scdawg = ScdawgChar::from_terms_with_values([
            ("café", 21_u32),
            ("猫", 22),
            ("猫咪", 23),
            ("🎉", 24),
        ]);
        let owner = scdawg.root();
        let root_cursor = owner
            .snapshot_root_cursor()
            .expect("Unicode SCDAWG root cursor");

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
                "native cursor traversal must preserve Unicode scalar edge order"
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
            // SAFETY: test terms contain no NUL edge and `cursor` belongs to
            // this retained revision.
            assert_eq!(
                unsafe { owner.snapshot_cursor_transition(cursor, '\0') },
                Some(None)
            );
        }

        assert!(visited.contains(&root_cursor.index()));
        let invalid = crate::SnapshotTraversalCursor::from_index(owner.inner.nodes.len())
            .expect("one-past-end cursor remains representable");
        assert!(!owner.contains_snapshot_cursor(invalid));
    }

    #[test]
    fn native_cursor_owner_retains_unicode_snapshot_across_publication() {
        let scdawg = ScdawgChar::from_terms_with_values([("猫", 1_u32), ("café", 2)]);
        let old_owner = scdawg.root();
        let old_root = old_owner
            .snapshot_root_cursor()
            .expect("old Unicode SCDAWG root cursor");
        let old_node_count = old_owner.inner.nodes.len();

        assert!(scdawg.insert_with_value("雪豹", 99));
        let fresh_owner = scdawg.root();
        let fresh_root = fresh_owner
            .snapshot_root_cursor()
            .expect("fresh Unicode SCDAWG root cursor");

        assert!(!Arc::ptr_eq(&old_owner.inner, &fresh_owner.inner));
        assert_eq!(old_owner.inner.nodes.len(), old_node_count);
        assert!(fresh_owner.inner.nodes.len() > old_node_count);
        // SAFETY: each cursor is used only with the owner that created it.
        assert_eq!(
            unsafe { old_owner.snapshot_cursor_transition(old_root, '雪') },
            Some(None),
            "the retained revision must not observe later publications"
        );
        // SAFETY: `fresh_root` belongs to `fresh_owner`.
        let snow = unsafe { fresh_owner.snapshot_cursor_transition(fresh_root, '雪') }
            .expect("cursor traversal supported")
            .expect("fresh revision contains 雪");
        // SAFETY: `snow` was emitted by `fresh_owner` immediately above.
        let leopard = unsafe { fresh_owner.snapshot_cursor_transition(snow, '豹') }
            .expect("cursor traversal supported")
            .expect("fresh revision contains 雪豹");
        // SAFETY: `leopard` descends from `fresh_root` in this retained revision.
        assert_eq!(
            unsafe { fresh_owner.snapshot_cursor_is_final(leopard) },
            Some(true)
        );
        // SAFETY: `leopard` descends from `fresh_root` in this retained revision.
        assert_eq!(
            unsafe { fresh_owner.snapshot_cursor_value(leopard) },
            Some(Some(99))
        );
    }

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
