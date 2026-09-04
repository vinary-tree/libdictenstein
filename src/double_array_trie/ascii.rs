//! Double-Array Trie (DAT) implementation with dynamic updates.
//!
//! A Double-Array Trie stores a trie structure using two parallel arrays (BASE and CHECK)
//! providing O(1) transitions and excellent cache locality.
//!
//! ## Structure
//!
//! - **BASE\[s\]**: Contains the offset for computing child state indices
//! - **CHECK\[s\]**: Verifies that a state `s` is valid (stores parent state)
//! - **IS_FINAL**: BitVec marking final states (end of valid terms)
//!
//! ## Transition Function
//!
//! ```text
//! next_state = BASE[current_state] + byte
//! if CHECK[next_state] == current_state:
//!     transition is valid
//! ```
//!
//! ## Performance Characteristics
//!
//! - **Memory**: 6-8 bytes per character (BASE: 4 bytes, CHECK: 4 bytes, flags: bits)
//! - **Transitions**: O(1) - single array lookup
//! - **Cache locality**: Excellent - contiguous arrays
//! - **Construction**: O(n²) worst case (BASE placement problem)
//! - **Dynamic updates**: Good with XOR-based relocation and free list
//!
//! ## Use Cases
//!
//! Best for:
//! - Large static or semi-static dictionaries
//! - Memory-constrained environments
//! - Cache-sensitive applications
//! - Scenarios requiring occasional updates

use super::core::builder::StaticDATBuilder;
use super::zipper::DoubleArrayTrieZipper;
use crate::iterator::{DictionaryIterator, DictionaryTermIterator};
use crate::value::DictionaryValue;
use crate::{
    Dictionary, DictionaryNode, MappedDictionary, MappedDictionaryNode, SnapshotTraversalCursor,
};
use std::sync::Arc;

// serde helpers for Arc<Vec<T>> / Arc<Vec<Vec<T>>> round-tripping moved to
// `crate::serialization::serde_helpers` (C2 dedup). Brought into scope here
// so the serde attribute strings ("serialize_arc_vec" etc.) resolve to the
// shared functions without changing every attribute.
#[cfg(feature = "serialization")]
#[allow(unused_imports)]
use crate::serialization::serde_helpers::{
    deserialize_arc_vec, deserialize_arc_vec_vec, serialize_arc_vec, serialize_arc_vec_vec,
};

// C5 step 2: byte DAT's local `DATShared<V>` struct is now a type alias
// for the generic `super::core::DATCoreShared<u8, V>`. The fields are
// byte-for-byte identical (same Arc<Vec<i32>>, Arc<Vec<bool>>,
// Arc<Vec<Vec<u8>>>, Arc<Vec<Option<V>>>) and the serde plumbing now
// flows through `crate::serialization::serde_helpers` (set up in C2).
//
// Call-sites throughout this file continue to reference `DATShared<V>`
// unchanged.
type DATRawShared<V = ()> = super::core::DATCoreShared<u8, V>;
pub(crate) type DATShared<V = ()> = super::core::shared::ValidatedDATCoreShared<u8, V, 1>;

/// A compact, cache-efficient dictionary implementation using the Double-Array Trie data structure.
///
/// # Overview
///
/// Double-Array Trie (DAT) is a space-efficient trie implementation that uses two parallel
/// arrays (BASE and CHECK) to represent state transitions. This provides:
///
/// - **Compact memory footprint**: O(n) space where n is alphabet size × number of states
/// - **Fast lookups**: O(m) time where m is the query length, with excellent cache locality
/// - **Static structure**: Optimized for read-heavy workloads after construction
///
/// # Performance Characteristics
///
/// - **Lookup**: O(m) where m is string length - excellent cache performance
/// - **Construction**: O(n × m) where n is term count, m is average length
/// - **Memory**: More compact than tree-based tries, comparable to DAWG
/// - **Thread-safety**: Fully concurrent reads via Arc-based sharing
///
/// # Use Cases
///
/// Best suited for:
/// - Static or rarely-modified dictionaries
/// - Memory-constrained environments
/// - High-throughput exact matching
/// - Applications requiring fast startup (quick deserialization)
///
/// # Serialization
///
/// Supports compact binary persistence:
/// - **Bincode** (`serialization`): efficient Rust-native storage
/// - **Protocol Buffers** (`protobuf`): schema-based cross-language interchange
/// - **Gzip** (`compression`): an optional wrapper around either binary format
///
/// # Example
///
/// ```
/// use libdictenstein::prelude::*;
///
/// let terms = vec!["apple", "application", "apply"];
/// let dict = DoubleArrayTrie::from_terms(terms);
///
/// assert!(dict.contains("apple"));
/// assert!(!dict.contains("apricot"));
/// ```
///
/// A double-array trie is immutable after construction, so it deliberately
/// does not implement [`std::iter::Extend`]:
///
/// ```compile_fail
/// use libdictenstein::double_array_trie::DoubleArrayTrie;
///
/// let mut dictionary = DoubleArrayTrie::from_terms(["built"]);
/// std::iter::Extend::extend(&mut dictionary, ["later".to_owned()]);
/// ```
#[cfg_attr(feature = "serialization", derive(serde::Serialize))]
#[cfg_attr(
    all(feature = "serialization", not(feature = "persistent-artrie")),
    serde(bound(serialize = "V: serde::Serialize"))
)]
#[cfg_attr(
    all(feature = "serialization", feature = "persistent-artrie"),
    serde(bound(serialize = ""))
)]
#[derive(Clone, Debug)]
pub struct DoubleArrayTrie<V: DictionaryValue = ()> {
    /// Shared data referenced by all nodes
    pub(crate) shared: DATShared<V>,

    /// Free list for deleted/unused states.
    ///
    /// # Reserved for future dynamic operations
    ///
    /// Read by no code path today. Preserved here (and serialized) because
    /// the field is part of the on-disk format — removing it without a
    /// format-version bump would silently corrupt any persisted DAT. When
    /// dynamic delete is implemented this field will hold the list of states
    /// freed by a delete that haven't yet been re-used by a subsequent
    /// insert. See plan item B5 + future format-version bump tracking issue.
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
    term_count: usize,

    /// Threshold for triggering rebuild (0.0 to 1.0, e.g., 0.2 = 20% deleted).
    ///
    /// # Reserved for future dynamic operations
    ///
    /// Read by no code path today. Preserved here (and serialized) because
    /// the field is part of the on-disk format. Will be consumed by the
    /// future dynamic-delete path to decide when accumulated deletes
    /// warrant a structural rebuild of the BASE/CHECK arrays.
    #[allow(dead_code)]
    rebuild_threshold: f64,
}

/// Exact legacy serde field layout used as the untrusted deserialization
/// representation. The validated wrapper is absent from the wire format.
#[cfg(feature = "serialization")]
#[derive(serde::Serialize, serde::Deserialize)]
#[cfg_attr(
    not(feature = "persistent-artrie"),
    serde(bound(deserialize = "V: serde::Deserialize<'de>"))
)]
#[cfg_attr(feature = "persistent-artrie", serde(bound(deserialize = "")))]
struct DoubleArrayTrieWire<V: DictionaryValue> {
    shared: DATRawShared<V>,
    #[serde(
        serialize_with = "serialize_arc_vec",
        deserialize_with = "deserialize_arc_vec"
    )]
    free_list: Arc<Vec<usize>>,
    term_count: usize,
    rebuild_threshold: f64,
}

#[cfg(feature = "serialization")]
impl<V: DictionaryValue> DoubleArrayTrie<V> {
    fn from_untrusted_wire(
        wire: DoubleArrayTrieWire<V>,
    ) -> Result<Self, super::core::shared::DatValidationError> {
        if !wire.rebuild_threshold.is_finite() || !(0.0..=1.0).contains(&wire.rebuild_threshold) {
            return Err(super::core::shared::DatValidationError::InvalidRebuildThreshold);
        }
        let shared = DATShared::try_from_untrusted(wire.shared, wire.term_count)?;
        shared.validate_free_list(wire.free_list.as_ref())?;
        debug_assert_eq!(shared.reachable_final_count(), wire.term_count);
        Ok(Self {
            shared,
            free_list: wire.free_list,
            term_count: wire.term_count,
            rebuild_threshold: wire.rebuild_threshold,
        })
    }
}

#[cfg(all(feature = "serialization", not(feature = "persistent-artrie")))]
impl<'de, V> serde::Deserialize<'de> for DoubleArrayTrie<V>
where
    V: DictionaryValue + serde::Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = DoubleArrayTrieWire::<V>::deserialize(deserializer)?;
        Self::from_untrusted_wire(wire).map_err(<D::Error as serde::de::Error>::custom)
    }
}

#[cfg(all(feature = "serialization", feature = "persistent-artrie"))]
impl<'de, V: DictionaryValue> serde::Deserialize<'de> for DoubleArrayTrie<V> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = DoubleArrayTrieWire::<V>::deserialize(deserializer)?;
        Self::from_untrusted_wire(wire).map_err(<D::Error as serde::de::Error>::custom)
    }
}

/// Builder for constructing a Double-Array Trie incrementally.
pub struct DoubleArrayTrieBuilder<V: DictionaryValue = ()> {
    /// BASE array being built
    base: Vec<i32>,

    /// CHECK array being built
    check: Vec<i32>,

    /// Final state markers
    is_final: Vec<bool>,

    /// Optional values for final states
    values: Vec<Option<V>>,

    /// Free list tracking unused states
    free_list: Vec<usize>,

    /// Number of terms inserted
    term_count: usize,

    /// Rebuild threshold
    rebuild_threshold: f64,
}

impl<V: DictionaryValue> DoubleArrayTrieBuilder<V> {
    /// Create a new DAT builder.
    pub fn new() -> Self {
        // State 0 is reserved as a sentinel/error state
        // State 1 is the root
        let base = vec![-1, 0]; // -1 for sentinel, 0 for root
        let check = vec![-1, -1]; // -1 means unused
        let is_final = vec![false, false];
        let values = vec![None, None]; // No values at sentinel or root initially

        Self {
            base,
            check,
            is_final,
            values,
            free_list: Vec::new(),
            term_count: 0,
            rebuild_threshold: 0.2, // Rebuild when 20% deleted
        }
    }

    /// Set the rebuild threshold (0.0 to 1.0).
    pub fn with_rebuild_threshold(mut self, threshold: f64) -> Self {
        self.rebuild_threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// Insert a term into the trie without a value.
    pub fn insert(&mut self, term: &str) -> bool {
        self.insert_with_value(term, None)
    }

    /// Insert a term into the trie with an optional value.
    pub fn insert_with_value(&mut self, term: &str, value: Option<V>) -> bool {
        // Handle empty string: mark root (state 1) as final
        if term.is_empty() {
            // Ensure arrays are large enough for root state (state 1)
            while self.is_final.len() <= 1 {
                self.is_final.push(false);
            }
            while self.values.len() <= 1 {
                self.values.push(None);
            }

            // Check if root is already final (empty string already inserted)
            if self.is_final[1] {
                // Update value if provided
                if value.is_some() {
                    self.values[1] = value;
                }
                return false; // Already exists
            }

            // Mark root as final and increment term count
            self.is_final[1] = true;
            self.values[1] = value;
            self.term_count += 1;
            return true;
        }

        let bytes = term.as_bytes();
        let mut state = 1; // Start at root

        // Traverse/create path
        for &byte in bytes {
            if let Some(next) = self.transition(state, byte) {
                state = next;
            } else {
                // Need to create new state for this transition
                state = self.add_transition(state, byte);
            }
        }

        // Mark final state
        if state < self.is_final.len() && self.is_final[state] {
            // Update value if provided
            if value.is_some() && state < self.values.len() {
                self.values[state] = value;
            }
            false // Already exists
        } else {
            while state >= self.is_final.len() {
                self.is_final.push(false);
            }
            while state >= self.values.len() {
                self.values.push(None);
            }
            self.is_final[state] = true;
            self.values[state] = value;
            self.term_count += 1;
            true
        }
    }

    /// Transition from a state via a byte.
    fn transition(&self, state: usize, byte: u8) -> Option<usize> {
        if state >= self.base.len() {
            return None;
        }

        let base = self.base[state];
        if base < 0 {
            return None; // No edges
        }

        let next = (base as usize).wrapping_add(byte as usize);

        if next < self.check.len() && self.check[next] == state as i32 {
            Some(next)
        } else {
            None
        }
    }

    /// Add a transition from state via byte, returning the new state.
    fn add_transition(&mut self, state: usize, byte: u8) -> usize {
        // Ensure state exists
        while state >= self.base.len() {
            self.base.push(-1);
            self.check.push(-1);
            self.is_final.push(false);
            self.values.push(None);
        }

        // Find a valid next_state based on BASE
        let next_state = if self.base[state] < 0 {
            // No BASE set yet - find a suitable BASE
            // Start searching from a position based on state to spread out allocations
            let start = (state * 31) % 1000 + byte as usize;
            let base = self.find_free_base(start, &[byte]);
            self.base[state] = base;
            (base as usize).wrapping_add(byte as usize)
        } else {
            // BASE already set, compute next_state
            (self.base[state] as usize).wrapping_add(byte as usize)
        };

        // Ensure next_state slot exists and is free
        while next_state >= self.check.len() {
            self.base.push(-1);
            self.check.push(-1);
            self.is_final.push(false);
            self.values.push(None);
        }

        if self.check[next_state] >= 0 {
            // Conflict! Need to find a new BASE that accommodates ALL children
            // Collect all existing children of this state
            let mut all_bytes = Vec::with_capacity(257);
            let old_base = self.base[state];

            // Find existing transitions
            for b in 0u8..=255 {
                let child = (old_base as usize).wrapping_add(b as usize);
                if child < self.check.len() && self.check[child] == state as i32 {
                    all_bytes.push(b);
                }
            }

            // Add the new byte we're trying to insert
            all_bytes.push(byte);

            // Find a BASE that works for ALL bytes
            let new_base = self.find_free_base(next_state + 1, &all_bytes);

            // Relocate all existing children to new BASE
            for &b in &all_bytes {
                if b == byte {
                    continue; // Skip the new one, we'll add it below
                }

                let old_child = (old_base as usize).wrapping_add(b as usize);
                let new_child = (new_base as usize).wrapping_add(b as usize);

                // Ensure new slot exists
                while new_child >= self.check.len() {
                    self.base.push(-1);
                    self.check.push(-1);
                    self.is_final.push(false);
                    self.values.push(None);
                }

                // Move the child's data
                self.check[new_child] = state as i32; // CHECK points to parent
                self.base[new_child] = self.base[old_child];
                self.is_final[new_child] = self.is_final[old_child];
                // Move the value if it exists
                if old_child < self.values.len() {
                    while new_child >= self.values.len() {
                        self.values.push(None);
                    }
                    self.values[new_child] = self.values[old_child].clone();
                }

                // Update all grandchildren's CHECK pointers
                if self.base[old_child] >= 0 {
                    let child_base = self.base[old_child] as usize;
                    for gc_byte in 0u8..=255 {
                        let grandchild = child_base + (gc_byte as usize);
                        if grandchild < self.check.len()
                            && self.check[grandchild] == old_child as i32
                        {
                            self.check[grandchild] = new_child as i32;
                        }
                    }
                }

                // Clear old slot
                self.check[old_child] = -1;
                self.base[old_child] = -1;
                self.is_final[old_child] = false;
                if old_child < self.values.len() {
                    self.values[old_child] = None;
                }
            }

            // Update state's BASE
            self.base[state] = new_base;
            let new_next = (new_base as usize).wrapping_add(byte as usize);

            while new_next >= self.check.len() {
                self.base.push(-1);
                self.check.push(-1);
                self.is_final.push(false);
                self.values.push(None);
            }

            self.check[new_next] = state as i32;
            new_next
        } else {
            self.check[next_state] = state as i32;
            next_state
        }
    }

    /// Find a free BASE value for a state that needs to have transitions for the given bytes.
    ///
    /// The double-array formula is: next_state = BASE[current_state] + byte
    ///
    /// This function finds a BASE value such that for each byte in `bytes`,
    /// the computed next_state position is available (CHECK[next_state] < 0).
    ///
    /// Returns the BASE value to store in BASE[current_state].
    fn find_free_base(&self, start: usize, bytes: &[u8]) -> i32 {
        if bytes.is_empty() {
            return 0;
        }

        // Search every representable BASE, beginning at the locality hint and
        // wrapping once. The former fixed 10,000-candidate search returned an
        // unchecked occupied fallback slot, corrupting sufficiently dense
        // tries during child relocation.
        let max_byte = usize::from(*bytes.iter().max().expect("non-empty byte set"));
        let max_base = (i32::MAX as usize).saturating_sub(max_byte);
        let start_base = start.min(max_base);
        let candidates = (start_base..=max_base).chain(0..start_base);

        for base in candidates {
            let all_free = bytes.iter().all(|&byte| {
                let next = base + usize::from(byte);
                // States 0 and 1 are the sentinel and root. Their CHECK cells
                // are negative by design, but they are not allocatable slots.
                next > 1 && (next >= self.check.len() || self.check[next] < 0)
            });
            if all_free {
                return i32::try_from(base).expect("candidate is bounded by i32::MAX");
            }
        }

        panic!("double-array trie exhausted every representable collision-free base")
    }

    /// Build the final DoubleArrayTrie.
    pub fn build(self) -> DoubleArrayTrie<V> {
        // Compute edge lists for each state to optimize edges() iteration
        let mut edges = vec![Vec::new(); self.base.len()];

        for (state, base_entry) in self.base.iter().enumerate() {
            if *base_entry >= 0 {
                let base = *base_entry as usize;

                // Find all valid edges for this state
                for byte in 0u8..=255 {
                    let next = base + (byte as usize);
                    if next < self.check.len() && self.check[next] == state as i32 {
                        edges[state].push(byte);
                    }
                }
            }
        }

        let term_count = self.term_count;
        let raw = DATRawShared {
            base: Arc::new(self.base),
            check: Arc::new(self.check),
            is_final: Arc::new(self.is_final),
            edges: Arc::new(edges),
            values: Arc::new(self.values),
        };
        // SAFETY: the incremental builder owns every parallel array and has
        // just derived EDGES from BASE/CHECK. Debug builds run the full proof.
        let shared = unsafe { DATShared::from_builder_parts_unchecked(raw, term_count) };
        DoubleArrayTrie {
            shared,
            free_list: Arc::new(self.free_list),
            term_count,
            rebuild_threshold: self.rebuild_threshold,
        }
    }
}

impl<V: DictionaryValue> Default for DoubleArrayTrieBuilder<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> DoubleArrayTrie<V> {
    fn from_static_builder(builder: StaticDATBuilder<u8, V>) -> Self {
        let built = builder.build(1);
        let term_count = built.term_count;
        let raw = DATRawShared {
            base: Arc::new(built.base),
            check: Arc::new(built.check),
            is_final: Arc::new(built.is_final),
            edges: Arc::new(built.edges),
            values: Arc::new(built.values),
        };
        // SAFETY: StaticDATBuilder assigns BASE/CHECK/EDGES together and never
        // exposes a partially constructed layout. Debug builds validate it.
        let shared = unsafe { DATShared::from_builder_parts_unchecked(raw, term_count) };
        Self {
            shared,
            free_list: Arc::new(Vec::new()),
            term_count,
            rebuild_threshold: 0.2,
        }
    }

    /// Create a new empty Double-Array Trie.
    pub fn new() -> Self {
        DoubleArrayTrieBuilder::new().build()
    }

    /// Build from fixed-width byte-profile sequences without text coercion.
    pub fn from_atom_sequences<P, I>(sequences: I) -> Self
    where
        P: crate::AtomProfile<Atom = u8>,
        I: IntoIterator<Item = crate::AtomSequence<P>>,
    {
        sequences
            .into_iter()
            .map(|sequence| sequence.as_atoms().to_vec())
            .collect::<Vec<Vec<u8>>>()
            .into_iter()
            .collect()
    }

    /// Create a DAT from an iterator of (term, value) pairs.
    ///
    /// For optimal space efficiency, terms should be sorted.
    pub fn from_terms_with_values<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        let mut term_value_pairs: Vec<(String, V)> = terms
            .into_iter()
            .map(|(s, v)| (s.as_ref().to_string(), v))
            .collect();

        // Sort by term
        term_value_pairs.sort_by(|a, b| a.0.cmp(&b.0));

        // Remove duplicates (keep last value)
        term_value_pairs.dedup_by(|a, b| {
            if a.0 == b.0 {
                // Swap to keep the later value
                std::mem::swap(&mut a.1, &mut b.1);
                true
            } else {
                false
            }
        });

        let mut builder = StaticDATBuilder::new();
        for (term, value) in term_value_pairs {
            builder.insert(term.bytes(), Some(value));
        }
        Self::from_static_builder(builder)
    }

    /// Get the value associated with a term.
    ///
    /// Returns `None` if the term doesn't exist in the dictionary.
    pub fn get_value(&self, term: &str) -> Option<V> {
        // Delegates to the generic `DATCoreShared::term_value` (C5
        // algorithmic dedup); the byte and char variants share the
        // BASE/CHECK walk.
        self.shared.term_value(term)
    }

    /// Get the number of terms in the dictionary.
    pub fn len(&self) -> Option<usize> {
        Some(self.term_count)
    }

    /// Check if the dictionary is empty.
    pub fn is_empty(&self) -> bool {
        self.term_count == 0
    }

    /// Check if a term exists in the dictionary.
    ///
    /// Delegates to the generic `DATCoreShared::contains_term` (C5 algorithmic
    /// dedup) so the byte and char variants share the BASE/CHECK walk.
    pub fn contains(&self, term: &str) -> bool {
        self.shared.contains_term(term)
    }

    /// Get the number of states in the trie.
    pub fn state_count(&self) -> usize {
        self.shared.base.len()
    }

    /// Get memory usage in bytes (estimated).
    pub fn memory_bytes(&self) -> usize {
        // BASE: 4 bytes/state, CHECK: 4 bytes/state, IS_FINAL: ~1 bit/state
        // EDGES: avg 3 bytes/state (small overhead)
        let state_count = self.state_count();
        let edges_bytes: usize = self.shared.edges.iter().map(|e| e.len()).sum();
        state_count * 4 + state_count * 4 + state_count.div_ceil(8) + edges_bytes
    }

    /// Iterate over all terms as raw byte vectors (without values).
    ///
    /// Returns an iterator yielding `Vec<u8>` in depth-first order.
    /// Use this for dictionaries created without values.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::double_array_trie::DoubleArrayTrie;
    ///
    /// let dict = DoubleArrayTrie::from_terms(vec!["cat", "dog", "cats"]);
    ///
    /// for term_bytes in dict.iter_terms() {
    ///     let term = String::from_utf8(term_bytes).unwrap();
    ///     println!("Term: {}", term);
    /// }
    /// ```
    pub fn iter_terms(&self) -> DictionaryTermIterator<DoubleArrayTrieZipper<V>> {
        let zipper = DoubleArrayTrieZipper::new_from_dict(self);
        DictionaryTermIterator::new(zipper)
    }

    /// Iterate over all `(term, value)` pairs as raw byte vectors.
    ///
    /// Returns an iterator yielding `(Vec<u8>, V)` tuples in depth-first order.
    /// This is more efficient than `iter()` as it avoids UTF-8 string allocation.
    ///
    /// This legacy mapped-only iterator omits present terms whose value is
    /// `None`. Use `(&dictionary).into_iter()` or
    /// [`DictionaryEntries::entries`](crate::DictionaryEntries::entries) for
    /// lossless [`DictionaryEntry`](crate::DictionaryEntry) snapshots.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::double_array_trie::DoubleArrayTrie;
    ///
    /// let dict = DoubleArrayTrie::from_terms_with_values(vec![
    ///     ("cat", 1), ("dog", 2), ("cats", 3)
    /// ]);
    ///
    /// for (term_bytes, value) in dict.iter_bytes() {
    ///     let term = String::from_utf8(term_bytes).unwrap();
    ///     println!("{} -> {}", term, value);
    /// }
    /// ```
    pub fn iter_bytes(&self) -> DictionaryIterator<DoubleArrayTrieZipper<V>> {
        let zipper = DoubleArrayTrieZipper::new_from_dict(self);
        DictionaryIterator::new(zipper)
    }

    /// Iterate over all `(term, value)` pairs as UTF-8 strings.
    ///
    /// Returns an iterator yielding `(String, V)` tuples in depth-first order.
    /// For better performance with raw bytes, use `iter_bytes()` instead.
    /// Like `iter_bytes()`, this legacy iterator omits term-only entries.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use libdictenstein::double_array_trie::DoubleArrayTrie;
    ///
    /// let dict = DoubleArrayTrie::from_terms_with_values(vec![
    ///     ("cat", 1), ("dog", 2)
    /// ]);
    ///
    /// for (term, value) in dict.iter() {
    ///     println!("{} -> {}", term, value);
    /// }
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (String, V)> + '_ {
        self.iter_bytes()
            .map(|(bytes, value)| (String::from_utf8_lossy(&bytes).into_owned(), value))
    }
}

impl<V: DictionaryValue> Default for DoubleArrayTrie<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> std::iter::FromIterator<String> for DoubleArrayTrie<V> {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        let mut builder = StaticDATBuilder::new();
        for term in iter {
            builder.insert(term.bytes(), None);
        }
        Self::from_static_builder(builder)
    }
}

impl<'a, V: DictionaryValue> std::iter::FromIterator<&'a str> for DoubleArrayTrie<V> {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        let mut builder = StaticDATBuilder::new();
        for term in iter {
            builder.insert(term.bytes(), None);
        }
        Self::from_static_builder(builder)
    }
}

impl<V: DictionaryValue> std::iter::FromIterator<Vec<u8>> for DoubleArrayTrie<V> {
    /// Streams keys into the two-phase static builder; no sequence of
    /// incrementally relocated DATs is constructed.
    fn from_iter<I: IntoIterator<Item = Vec<u8>>>(iter: I) -> Self {
        let mut builder = StaticDATBuilder::new();
        for key in iter {
            builder.insert(key, None);
        }
        Self::from_static_builder(builder)
    }
}

impl<'a, V: DictionaryValue> std::iter::FromIterator<&'a [u8]> for DoubleArrayTrie<V> {
    fn from_iter<I: IntoIterator<Item = &'a [u8]>>(iter: I) -> Self {
        let mut builder = StaticDATBuilder::new();
        for key in iter {
            builder.insert(key.iter().copied(), None);
        }
        Self::from_static_builder(builder)
    }
}

impl<V: DictionaryValue> std::iter::FromIterator<(String, V)> for DoubleArrayTrie<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        let mut builder = StaticDATBuilder::new();
        for (term, value) in iter {
            builder.insert(term.bytes(), Some(value));
        }
        Self::from_static_builder(builder)
    }
}

impl<'a, V: DictionaryValue> std::iter::FromIterator<(&'a str, V)> for DoubleArrayTrie<V> {
    fn from_iter<I: IntoIterator<Item = (&'a str, V)>>(iter: I) -> Self {
        let mut builder = StaticDATBuilder::new();
        for (term, value) in iter {
            builder.insert(term.bytes(), Some(value));
        }
        Self::from_static_builder(builder)
    }
}

impl<V: DictionaryValue> std::iter::FromIterator<(Vec<u8>, V)> for DoubleArrayTrie<V> {
    fn from_iter<I: IntoIterator<Item = (Vec<u8>, V)>>(iter: I) -> Self {
        let mut builder = StaticDATBuilder::new();
        for (key, value) in iter {
            builder.insert(key, Some(value));
        }
        Self::from_static_builder(builder)
    }
}

impl<'a, V: DictionaryValue> std::iter::FromIterator<(&'a [u8], V)> for DoubleArrayTrie<V> {
    fn from_iter<I: IntoIterator<Item = (&'a [u8], V)>>(iter: I) -> Self {
        let mut builder = StaticDATBuilder::new();
        for (key, value) in iter {
            builder.insert(key.iter().copied(), Some(value));
        }
        Self::from_static_builder(builder)
    }
}

// Backward-compatible impl for unit type (no values)
impl DoubleArrayTrie<()> {
    /// Create a DAT from an iterator of terms (without values).
    ///
    /// For optimal space efficiency, terms should be sorted.
    pub fn from_terms<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut sorted_terms: Vec<String> =
            terms.into_iter().map(|s| s.as_ref().to_string()).collect();
        sorted_terms.sort();
        sorted_terms.dedup();

        let mut builder = StaticDATBuilder::new();
        for term in sorted_terms {
            builder.insert(term.bytes(), None);
        }
        Self::from_static_builder(builder)
    }
}

/// Node reference for Dictionary trait implementation.
#[derive(Clone)]
pub struct DoubleArrayTrieNode<V: DictionaryValue = ()> {
    /// Current state index
    state: usize,

    /// One shared handle to all BASE/CHECK arrays.
    ///
    /// Keeping the five-array `DATShared` behind one outer `Arc` makes every
    /// traversed node clone and drop one reference count instead of five.
    shared: Arc<DATShared<V>>,
}

impl<V: DictionaryValue> DictionaryNode for DoubleArrayTrieNode<V> {
    type Unit = u8;
    type SnapshotCursor = SnapshotTraversalCursor;
    type SnapshotGraphValueHandle = SnapshotTraversalCursor;

    #[inline]
    fn snapshot_root_cursor(&self) -> Option<SnapshotTraversalCursor> {
        DATShared::<V>::traversal_cursor(self.state)
    }

    #[inline]
    fn contains_snapshot_cursor(&self, cursor: SnapshotTraversalCursor) -> bool {
        self.shared.contains_traversal_cursor(cursor, 1)
    }

    #[inline]
    fn supports_snapshot_cursor_nodes(&self) -> bool {
        true
    }

    #[inline]
    fn supports_snapshot_cursor_key_units(&self) -> bool {
        true
    }

    #[inline]
    unsafe fn snapshot_cursor_key_units(
        &self,
        cursor: SnapshotTraversalCursor,
    ) -> Option<Vec<Self::Unit>> {
        // SAFETY: delegated from DictionaryNode's snapshot-cursor contract;
        // `self.state` is the captured root for root-relative reconstruction.
        unsafe { self.shared.traversal_cursor_key_units(cursor, self.state) }
    }

    #[inline]
    unsafe fn snapshot_cursor_node(&self, cursor: SnapshotTraversalCursor) -> Option<Self> {
        // SAFETY: delegated from DictionaryNode's snapshot-cursor contract.
        let state = unsafe { self.shared.traversal_state(cursor) }?;
        Some(Self {
            state,
            shared: Arc::clone(&self.shared),
        })
    }

    #[inline]
    unsafe fn filter_map_snapshot_cursor_edges_and_finality<T, P, F>(
        &self,
        cursor: SnapshotTraversalCursor,
        project: P,
        visitor: F,
    ) -> Option<bool>
    where
        P: FnMut(Self::Unit) -> Option<T>,
        F: FnMut(Self::Unit, SnapshotTraversalCursor, T),
    {
        // SAFETY: delegated from DictionaryNode's snapshot-cursor contract.
        unsafe {
            self.shared
                .filter_map_traversal_cursor(cursor, project, visitor)
        }
    }

    #[inline]
    unsafe fn snapshot_cursor_is_final(&self, cursor: SnapshotTraversalCursor) -> Option<bool> {
        // SAFETY: delegated from DictionaryNode's snapshot-cursor contract.
        unsafe { self.shared.traversal_cursor_is_final(cursor) }
    }

    #[inline]
    unsafe fn snapshot_cursor_transition(
        &self,
        cursor: SnapshotTraversalCursor,
        label: Self::Unit,
    ) -> Option<Option<SnapshotTraversalCursor>> {
        // SAFETY: delegated from DictionaryNode's snapshot-cursor contract.
        unsafe { self.shared.traversal_cursor_transition(cursor, label) }
    }

    #[inline]
    fn supports_efficient_snapshot_cursor_edge_paging(&self) -> bool {
        true
    }

    #[inline]
    unsafe fn visit_snapshot_cursor_edge_page<F>(
        &self,
        cursor: SnapshotTraversalCursor,
        start: usize,
        capacity: usize,
        visitor: F,
    ) -> Option<(bool, usize)>
    where
        F: FnMut(Self::Unit, SnapshotTraversalCursor),
    {
        // SAFETY: delegated from DictionaryNode's snapshot-cursor contract.
        unsafe {
            self.shared
                .visit_traversal_cursor_edge_page(cursor, start, capacity, visitor)
        }
    }

    fn is_final(&self) -> bool {
        self.state < self.shared.is_final.len() && self.shared.is_final[self.state]
    }

    fn transition(&self, label: u8) -> Option<Self> {
        if self.state >= self.shared.base.len() {
            return None;
        }

        let base = self.shared.base[self.state];
        if base < 0 {
            return None; // No edges
        }

        let next = (base as usize).wrapping_add(label as usize);

        if next < self.shared.check.len() && self.shared.check[next] == self.state as i32 {
            Some(DoubleArrayTrieNode {
                state: next,
                shared: self.shared.clone(), // Single Arc clone
            })
        } else {
            None
        }
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (u8, Self)> + '_> {
        // OPTIMIZED: Only iterate over actual edges stored in edge list
        let state = self.state;

        if state >= self.shared.edges.len() {
            return Box::new(std::iter::empty());
        }

        let base = self.shared.base[state];
        if base < 0 {
            return Box::new(std::iter::empty());
        }

        // Iterate only over actual edges (typically 1-5 instead of 256)
        let edges: Vec<(u8, Self)> = self.shared.edges[state]
            .iter()
            .map(|&byte| {
                let next = (base as usize) + (byte as usize);
                (
                    byte,
                    DoubleArrayTrieNode {
                        state: next,
                        shared: self.shared.clone(), // Single Arc clone
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
        let state = self.state;
        if state >= self.shared.edges.len() {
            return;
        }
        let base = self.shared.base[state];
        if base < 0 {
            return;
        }
        for &byte in &self.shared.edges[state] {
            visitor(
                byte,
                DoubleArrayTrieNode {
                    state: (base as usize) + usize::from(byte),
                    shared: self.shared.clone(),
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
        let state = self.state;
        if state >= self.shared.edges.len() {
            return;
        }
        let base = self.shared.base[state];
        if base < 0 {
            return;
        }
        for &label in &self.shared.edges[state] {
            if let Some(projected) = project(label) {
                visitor(
                    label,
                    DoubleArrayTrieNode {
                        state: (base as usize) + usize::from(label),
                        shared: Arc::clone(&self.shared),
                    },
                    projected,
                );
            }
        }
    }

    fn edge_count(&self) -> Option<usize> {
        // Now we can efficiently return edge count
        if self.state < self.shared.edges.len() {
            Some(self.shared.edges[self.state].len())
        } else {
            Some(0)
        }
    }
}

// NOTE: Serialization support (DictionaryFromTerms impl) is provided in liblevenshtein
// since the trait lives there. See liblevenshtein::serialization for the implementation.

impl<V: DictionaryValue> Dictionary for DoubleArrayTrie<V> {
    type Node = DoubleArrayTrieNode<V>;

    fn root(&self) -> Self::Node {
        DoubleArrayTrieNode {
            state: 1, // Root is state 1
            shared: Arc::new(self.shared.clone()),
        }
    }

    fn len(&self) -> Option<usize> {
        Some(self.term_count)
    }

    fn contains(&self, term: &str) -> bool {
        self.contains(term)
    }
}

// MappedDictionary trait implementations
impl<V: DictionaryValue> MappedDictionaryNode for DoubleArrayTrieNode<V> {
    type Value = V;

    fn value(&self) -> Option<Self::Value> {
        if self.state < self.shared.values.len() {
            self.shared.values[self.state].clone()
        } else {
            None
        }
    }

    #[inline]
    fn supports_snapshot_cursor_values(&self) -> bool {
        true
    }

    #[inline]
    unsafe fn snapshot_cursor_value(
        &self,
        cursor: SnapshotTraversalCursor,
    ) -> Option<Option<Self::Value>> {
        // SAFETY: delegated from MappedDictionaryNode's snapshot-cursor contract.
        unsafe { self.shared.traversal_cursor_value(cursor) }
    }
}

impl<V: DictionaryValue> MappedDictionary for DoubleArrayTrie<V> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        // Delegate to the inherent method
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_cursor_traversal_preserves_edges_finality_nodes_and_values() {
        let dat = DoubleArrayTrie::from_terms_with_values([("", 1_u32), ("car", 2), ("cat", 3)]);
        let owner = dat.root();
        assert!(owner.supports_snapshot_cursor_nodes());
        assert!(owner.supports_snapshot_cursor_key_units());
        assert!(owner.supports_snapshot_cursor_values());
        let mut cursor = owner.snapshot_root_cursor().expect("DAT root cursor");
        assert_eq!(cursor.get(), 2, "the byte DAT root is state one");

        for expected in b"cat" {
            let mut child = None;
            // SAFETY: `cursor` is the root or a child produced by this exact
            // immutable owner in the preceding iteration.
            let finality = unsafe {
                owner.filter_map_snapshot_cursor_edges_and_finality(
                    cursor,
                    |label| (label == *expected).then_some(()),
                    |_label, next, ()| child = Some(next),
                )
            }
            .expect("DAT nodes support native cursor traversal");
            assert_eq!(finality, cursor.get() == 2);
            cursor = child.expect("the cat path exists");
        }

        // SAFETY: the cursor was produced by this retained owner.
        let finality = unsafe {
            owner.filter_map_snapshot_cursor_edges_and_finality(
                cursor,
                |_| None::<()>,
                |_, _, _| unreachable!(),
            )
        };
        assert_eq!(finality, Some(true));
        // SAFETY: the cursor was produced by this retained owner.
        assert_eq!(
            unsafe { owner.snapshot_cursor_value(cursor) },
            Some(Some(3))
        );
        // SAFETY: the cursor was produced by this retained owner.
        assert_eq!(
            unsafe { owner.snapshot_cursor_key_units(cursor) },
            Some(b"cat".to_vec())
        );

        let subtree = owner.transition(b'c').expect("the c subtree exists");
        let subtree_leaf = subtree
            .transition(b'a')
            .and_then(|node| node.transition(b't'))
            .expect("cat remains reachable from the c subtree");
        let subtree_cursor = subtree_leaf.snapshot_root_cursor().expect("leaf cursor");
        // SAFETY: `subtree_cursor` descends from the retained subtree root.
        assert_eq!(
            unsafe { subtree.snapshot_cursor_key_units(subtree_cursor) },
            Some(b"at".to_vec()),
            "cursor-key reconstruction is relative to the captured node"
        );
        // SAFETY: the cursor was produced by this retained owner.
        let materialized = unsafe { owner.snapshot_cursor_node(cursor) }.expect("valid node");
        assert!(materialized.is_final());
        assert_eq!(materialized.value(), Some(3));

        let invalid = SnapshotTraversalCursor::new(dat.shared.base.len() + 1).unwrap();
        // Invalid external tokens stop at the safe membership boundary. Passing
        // one to an unsafe cursor method would violate that method's contract.
        assert!(!owner.contains_snapshot_cursor(invalid));
    }

    #[test]
    fn test_empty_dat() {
        let dat: DoubleArrayTrie<()> = DoubleArrayTrie::new();
        assert_eq!(dat.len(), Some(0));
        assert!(dat.is_empty());
    }

    #[test]
    fn test_single_term() {
        let dat = DoubleArrayTrie::from_terms(vec!["test"]);
        assert_eq!(dat.len(), Some(1));
        assert!(dat.contains("test"));
        assert!(!dat.contains("testing"));
        assert!(!dat.contains("tes"));
    }

    #[test]
    fn test_multiple_terms() {
        let dat = DoubleArrayTrie::from_terms(vec!["test", "testing", "tested", "tester"]);
        assert_eq!(dat.len(), Some(4));
        assert!(dat.contains("test"));
        assert!(dat.contains("testing"));
        assert!(dat.contains("tested"));
        assert!(dat.contains("tester"));
        assert!(!dat.contains("tes"));
        assert!(!dat.contains("tests"));
    }

    #[test]
    fn test_prefix_sharing() {
        let dat = DoubleArrayTrie::from_terms(vec!["test", "best", "rest"]);
        assert_eq!(dat.len(), Some(3));

        // All three words share "est" suffix
        // DAT should be space-efficient (but our simplified implementation isn't optimal)
        // Just verify it works correctly
        assert!(dat.contains("test"));
        assert!(dat.contains("best"));
        assert!(dat.contains("rest"));
    }

    #[test]
    fn test_memory_efficiency() {
        let dat =
            DoubleArrayTrie::from_terms(vec!["band", "banana", "bandana", "can", "cane", "candy"]);

        let memory = dat.memory_bytes();
        let state_count = dat.state_count();

        println!("DAT memory: {} bytes for {} states", memory, state_count);
        println!(
            "  Approximately {} bytes/state",
            memory / state_count.max(1)
        );

        // Should be around 8-10 bytes per state (BASE + CHECK + flags)
        assert!(memory < state_count * 12);
    }

    #[test]
    fn test_dictionary_trait() {
        let dat = DoubleArrayTrie::from_terms(vec!["test", "testing"]);

        let root = dat.root();
        assert!(!root.is_final());

        // Follow 't'
        let t_node = root.transition(b't').expect("Should have 't' edge");
        assert!(!t_node.is_final());

        // Follow 'e'
        let e_node = t_node.transition(b'e').expect("Should have 'e' edge");
        assert!(!e_node.is_final());

        // Follow 's'
        let s_node = e_node.transition(b's').expect("Should have 's' edge");
        assert!(!s_node.is_final());

        // Follow 't'
        let final_node = s_node.transition(b't').expect("Should have 't' edge");
        assert!(final_node.is_final()); // "test" is a word
    }

    #[test]
    fn test_edge_iteration() {
        let dat = DoubleArrayTrie::from_terms(vec!["ab", "ac", "ad"]);

        let root = dat.root();
        let a_node = root.transition(b'a').expect("Should have 'a' edge");

        let edges: Vec<u8> = a_node.edges().map(|(label, _)| label).collect();

        // Should have edges for 'b', 'c', 'd'
        assert!(edges.contains(&b'b'));
        assert!(edges.contains(&b'c'));
        assert!(edges.contains(&b'd'));
        assert_eq!(edges.len(), 3);
    }

    #[test]
    fn test_incremental_construction() {
        let mut builder: DoubleArrayTrieBuilder<()> = DoubleArrayTrieBuilder::new();

        assert!(builder.insert("hello"));
        assert!(builder.insert("world"));
        assert!(builder.insert("test"));
        assert!(!builder.insert("test")); // Duplicate

        let dat = builder.build();
        assert_eq!(dat.len(), Some(3));
        assert!(dat.contains("hello"));
        assert!(dat.contains("world"));
        assert!(dat.contains("test"));
    }

    #[test]
    fn find_free_base_searches_beyond_the_old_unchecked_fallback() {
        let mut builder = DoubleArrayTrieBuilder::<()>::new();
        let start = 7usize;
        let old_search_width = 10_000usize;
        let byte = b'a';
        let occupied_through = start + old_search_width + usize::from(byte);
        builder.check.resize(occupied_through + 1, 1);

        let base = builder.find_free_base(start, &[byte]);

        assert!(base as usize > start + old_search_width);
        let slot = base as usize + usize::from(byte);
        assert!(slot >= builder.check.len() || builder.check[slot] < 0);
    }

    // MappedDictionary tests
    #[test]
    fn test_mapped_dictionary_with_values() {
        let terms = vec![("apple", 1), ("application", 2), ("apply", 3)];

        let dict = DoubleArrayTrie::from_terms_with_values(terms);

        assert_eq!(dict.get_value("apple"), Some(1));
        assert_eq!(dict.get_value("application"), Some(2));
        assert_eq!(dict.get_value("apply"), Some(3));
        assert_eq!(dict.get_value("apricot"), None);
    }

    #[test]
    fn test_mapped_dictionary_contains_with_value() {
        let dict = DoubleArrayTrie::from_terms_with_values(vec![("test", 42), ("testing", 100)]);

        assert!(dict.contains_with_value("test", |v| *v == 42));
        assert!(dict.contains_with_value("testing", |v| *v > 50));
        assert!(!dict.contains_with_value("test", |v| *v > 50));
        assert!(!dict.contains_with_value("missing", |v| *v == 42));
    }

    #[test]
    fn test_mapped_dictionary_node_value() {
        use crate::MappedDictionaryNode;

        let dict = DoubleArrayTrie::from_terms_with_values(vec![("cat", 1), ("catch", 2)]);

        let root = dict.root();
        // Navigate to "cat"
        let c = root.transition(b'c').unwrap();
        let a = c.transition(b'a').unwrap();
        let t = a.transition(b't').unwrap();

        assert!(t.is_final());
        assert_eq!(t.value(), Some(1));

        // Continue to "catch"
        let c2 = t.transition(b'c').unwrap();
        let h = c2.transition(b'h').unwrap();

        assert!(h.is_final());
        assert_eq!(h.value(), Some(2));
    }

    #[test]
    fn test_backward_compatibility_without_values() {
        // Default type parameter should be ()
        let dict: DoubleArrayTrie = DoubleArrayTrie::from_terms(vec!["test", "testing"]);

        assert!(dict.contains("test"));
        assert_eq!(dict.len(), Some(2));

        // get_value should return None for unit type
        assert_eq!(dict.get_value("test"), None);
    }

    #[test]
    fn test_builder_with_values() {
        let mut builder: DoubleArrayTrieBuilder<i32> = DoubleArrayTrieBuilder::new();

        builder.insert_with_value("hello", Some(10));
        builder.insert_with_value("world", Some(20));
        builder.insert_with_value("test", Some(30));

        let dat = builder.build();

        assert_eq!(dat.len(), Some(3));
        assert_eq!(dat.get_value("hello"), Some(10));
        assert_eq!(dat.get_value("world"), Some(20));
        assert_eq!(dat.get_value("test"), Some(30));
    }

    #[test]
    fn test_empty_string_with_value() {
        let mut builder: DoubleArrayTrieBuilder<i32> = DoubleArrayTrieBuilder::new();
        builder.insert_with_value("", Some(42));

        let dat = builder.build();
        assert_eq!(dat.get_value(""), Some(42));
    }

    #[test]
    fn test_duplicate_update_value() {
        let mut builder: DoubleArrayTrieBuilder<i32> = DoubleArrayTrieBuilder::new();

        assert!(builder.insert_with_value("test", Some(10)));
        assert!(!builder.insert_with_value("test", Some(20))); // Duplicate, updates value

        let dat = builder.build();

        assert_eq!(dat.len(), Some(1));
        assert_eq!(dat.get_value("test"), Some(20)); // Should have updated value
    }

    #[test]
    fn test_string_values() {
        let dict = DoubleArrayTrie::from_terms_with_values(vec![
            ("hello", "greeting".to_string()),
            ("world", "noun".to_string()),
            ("test", "verb".to_string()),
        ]);

        assert_eq!(dict.get_value("hello"), Some("greeting".to_string()));
        assert_eq!(dict.get_value("world"), Some("noun".to_string()));
        assert_eq!(dict.get_value("test"), Some("verb".to_string()));
    }

    #[cfg(feature = "serialization")]
    fn legacy_wire<V: DictionaryValue>(dict: &DoubleArrayTrie<V>) -> DoubleArrayTrieWire<V> {
        DoubleArrayTrieWire {
            shared: DATRawShared {
                base: Arc::clone(&dict.shared.base),
                check: Arc::clone(&dict.shared.check),
                is_final: Arc::clone(&dict.shared.is_final),
                edges: Arc::clone(&dict.shared.edges),
                values: Arc::clone(&dict.shared.values),
            },
            free_list: Arc::clone(&dict.free_list),
            term_count: dict.term_count,
            rebuild_threshold: dict.rebuild_threshold,
        }
    }

    #[cfg(feature = "serialization")]
    #[test]
    fn validated_wrapper_preserves_legacy_direct_serde_bytes() {
        let dict =
            DoubleArrayTrie::from_terms_with_values([("", 1_u32), ("alpha", 2), ("alpine", 3)]);
        let current = crate::serialization::bincode_compat::serialize(&dict).unwrap();
        let legacy = crate::serialization::bincode_compat::serialize(&legacy_wire(&dict)).unwrap();
        assert_eq!(current, legacy);

        let restored: DoubleArrayTrie<u32> =
            crate::serialization::bincode_compat::deserialize(&legacy).unwrap();
        assert_eq!(restored.len(), Some(3));
        assert_eq!(restored.get_value(""), Some(1));
        assert_eq!(restored.get_value("alpha"), Some(2));
        assert_eq!(
            crate::serialization::bincode_compat::serialize(&restored).unwrap(),
            legacy
        );

        let valueless = DoubleArrayTrie::from_terms(["", "cat"]);
        let valueless_current =
            crate::serialization::bincode_compat::serialize(&valueless).unwrap();
        let valueless_legacy =
            crate::serialization::bincode_compat::serialize(&legacy_wire(&valueless)).unwrap();
        assert_eq!(valueless_current, valueless_legacy);
    }

    #[cfg(feature = "serialization")]
    #[test]
    fn direct_serde_rejects_malformed_byte_dat_before_cursor_trust() {
        let dict = DoubleArrayTrie::from_terms_with_values([("cat", 7_u32)]);

        let mut wrong_parent = legacy_wire(&dict);
        let root_base = wrong_parent.shared.base[1] as usize;
        let child = root_base + usize::from(wrong_parent.shared.edges[1][0]);
        Arc::make_mut(&mut wrong_parent.shared.check)[child] = 0;
        let bytes = crate::serialization::bincode_compat::serialize(&wrong_parent).unwrap();
        assert!(
            crate::serialization::bincode_compat::deserialize::<DoubleArrayTrie<u32>>(&bytes)
                .is_err()
        );

        let mut wrong_count = legacy_wire(&dict);
        wrong_count.term_count += 1;
        let bytes = crate::serialization::bincode_compat::serialize(&wrong_count).unwrap();
        assert!(
            crate::serialization::bincode_compat::deserialize::<DoubleArrayTrie<u32>>(&bytes)
                .is_err()
        );

        let mut invalid_free_list = legacy_wire(&dict);
        invalid_free_list.free_list = Arc::new(vec![1]);
        let bytes = crate::serialization::bincode_compat::serialize(&invalid_free_list).unwrap();
        assert!(
            crate::serialization::bincode_compat::deserialize::<DoubleArrayTrie<u32>>(&bytes)
                .is_err()
        );

        let mut invalid_threshold = legacy_wire(&dict);
        invalid_threshold.rebuild_threshold = f64::NAN;
        let bytes = crate::serialization::bincode_compat::serialize(&invalid_threshold).unwrap();
        assert!(
            crate::serialization::bincode_compat::deserialize::<DoubleArrayTrie<u32>>(&bytes)
                .is_err()
        );
    }
}
