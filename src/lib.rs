//! High-performance dictionary backends for fuzzy string matching.
//!
//! This crate provides multiple dictionary implementations (tries, DAWGs, suffix automata,
//! persistent ARTs) optimized for different use cases with Levenshtein automata.
//!
//! # Choosing a Dictionary Backend
//!
//! ## In-memory backends
//!
//! | Backend | Best For | Performance | Memory | Dynamic Updates | Unicode |
//! |---------|----------|-------------|--------|-----------------|---------|
//! | **[DoubleArrayTrie]** | General use (recommended) | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ✅ Insert-only | Byte-level |
//! | **[DoubleArrayTrieChar]** | Unicode text | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ✅ Insert-only | ✅ Character-level |
//! | **[DynamicDawg]** | Insert + Remove | ⭐⭐⭐ | ⭐⭐⭐ | ✅ Thread-safe | Byte-level |
//! | **[DynamicDawgChar]** | Unicode + Insert + Remove | ⭐⭐⭐ | ⭐⭐⭐ | ✅ Thread-safe | ✅ Character-level |
//! | **[DynamicDawgU64]** | Token sequences, time series | ⭐⭐⭐ | ⭐⭐ | ✅ Thread-safe | 64-bit labels |
//! | **[SuffixAutomaton]** | Substring search | ⭐⭐⭐ | ⭐⭐ | ✅ Insert + Remove | Byte-level |
//! | **[SuffixAutomatonChar]** | Unicode substring search | ⭐⭐⭐ | ⭐⭐ | ✅ Insert + Remove | ✅ Character-level |
//! | **[Scdawg]** | Substring search (static, compact) | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ✅ Insert-only | Byte-level |
//! | **[ScdawgChar]** | Unicode substring search (static) | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ✅ Insert-only | ✅ Character-level |
//! | **[PathMapDictionary]** (feature `pathmap-backend`) | Fast in-memory queries | ⭐⭐⭐⭐ | ⭐⭐⭐ | ✅ Thread-safe | Byte-level |
//! | **[PathMapDictionaryChar]** (feature `pathmap-backend`) | Fast in-memory queries (Unicode) | ⭐⭐⭐⭐ | ⭐⭐⭐ | ✅ Thread-safe | ✅ Character-level |
//!
//! ## Disk-backed backends (feature `persistent-artrie`)
//!
//! | Backend | Best For | Persistence | Concurrency | Unicode |
//! |---------|----------|-------------|-------------|---------|
//! | **[PersistentARTrie]** | Disk-backed key/value, byte keys | mmap + WAL | Lock-free CAS | Byte-level |
//! | **[PersistentARTrieChar]** | Disk-backed key/value, Unicode | mmap + WAL | Lock-free CAS | ✅ Character-level |
//! | **[PersistentVocabARTrie]** | Vocabulary trie (term ↔ u64 index) | mmap + WAL | RwLock | ✅ Character-level |
//!
//! Use the [`factory::DictionaryFactory`] for a unified construction API across
//! all in-memory backends. See [`bijective::BijectiveDictionary`] for the
//! bidirectional-lookup trait shared by `BijectiveMap` and the vocab tries.
//!
//! [DoubleArrayTrie]: double_array_trie::DoubleArrayTrie
//! [DoubleArrayTrieChar]: double_array_trie_char::DoubleArrayTrieChar
//! [DynamicDawg]: dynamic_dawg::DynamicDawg
//! [DynamicDawgChar]: dynamic_dawg_char::DynamicDawgChar
//! [DynamicDawgU64]: dynamic_dawg_u64::DynamicDawgU64
//! [SuffixAutomaton]: suffix_automaton::SuffixAutomaton
//! [SuffixAutomatonChar]: suffix_automaton_char::SuffixAutomatonChar
//! [Scdawg]: scdawg::Scdawg
//! [ScdawgChar]: scdawg_char::ScdawgChar
//! [PathMapDictionary]: pathmap::PathMapDictionary
//! [PathMapDictionaryChar]: pathmap_char::PathMapDictionaryChar
//! [PersistentARTrie]: persistent_artrie::PersistentARTrie
//! [PersistentARTrieChar]: persistent_artrie_char::PersistentARTrieChar
//! [PersistentVocabARTrie]: persistent_vocab_artrie::PersistentVocabARTrie

pub mod bijective;
pub mod bloom_filter;
pub mod char_unit;
pub mod dawg_core;
pub mod node_signature;
pub mod sync_compat;

pub mod dat_core;
pub mod difference_zipper;
pub mod double_array_trie;
pub mod double_array_trie_char;
pub mod double_array_trie_char_zipper;
pub mod double_array_trie_zipper;
pub mod dynamic_dawg;
pub mod dynamic_dawg_char;
pub mod dynamic_dawg_char_zipper;
pub mod dynamic_dawg_u64;
pub mod dynamic_dawg_u64_zipper;
pub mod dynamic_dawg_zipper;
pub mod excluding_prefix_zipper;
pub mod factory;
pub mod intersection_zipper;
pub mod iterator;
#[cfg(feature = "pathmap-backend")]
pub mod pathmap;
#[cfg(feature = "pathmap-backend")]
pub mod pathmap_char;
#[cfg(feature = "pathmap-backend")]
pub mod pathmap_zipper;

// === Persistent ARTrie modules (feature-gated at module level) ===
// These modules are gated here; internal code does NOT need feature gates.
//
// Layering: `persistent_artrie_core` is the shared substrate; the three
// variants depend on core, never on each other. See
// `persistent_artrie_core/mod.rs` for the invariant.
#[cfg(feature = "persistent-artrie")]
pub mod artrie_trait;
#[cfg(feature = "persistent-artrie")]
pub mod persistent_artrie;
#[cfg(feature = "persistent-artrie")]
pub mod persistent_artrie_char;
#[cfg(feature = "persistent-artrie")]
pub mod persistent_artrie_core;
#[cfg(feature = "persistent-artrie")]
pub mod persistent_vocab_artrie;

pub mod prefix_zipper;
pub mod scdawg;
pub mod scdawg_char;
pub mod scdawg_core;
pub mod substring;
pub mod suffix_automaton;
pub mod suffix_automaton_char;
pub mod suffix_automaton_char_zipper;
pub mod suffix_automaton_core;
pub mod suffix_automaton_zipper;
pub mod symmetric_difference_zipper;
pub mod union_zipper;
pub mod value;
pub mod value_diff_zipper;
pub mod zipper;

#[cfg(feature = "serialization")]
pub mod serialization;

// Re-export core types at crate root
pub use bijective::{BijectiveDictionary, BijectiveMap, InsertError};
pub use bloom_filter::BloomFilter;
pub use char_unit::CharUnit;
pub use dawg_core::{DawgCore, DawgNode};
pub use iterator::{DictionaryIterator, DictionaryTermIterator};
pub use node_signature::NodeSignature;
pub use substring::{
    BidirectionalDictionaryNode, ExtensionResult, SubstringDictionary, SubstringMatch,
};
pub use value::DictionaryValue;
pub use zipper::{DictZipper, ValuedDictZipper};

// Re-export persistent ARTrie types (only available with feature)
#[cfg(feature = "persistent-artrie")]
pub use artrie_trait::{ARTrie, EvictableARTrie};
// `ARTrieAtomicOps` is #[deprecated]; re-exported behind an allow so the
// re-export site itself doesn't spam warnings. External callers that name
// the trait still get the deprecation message.
#[cfg(feature = "persistent-artrie")]
#[allow(deprecated)]
pub use artrie_trait::ARTrieAtomicOps;
#[cfg(feature = "persistent-artrie")]
pub use persistent_artrie::wal::Lsn;
#[cfg(feature = "persistent-artrie")]
pub use persistent_artrie::{
    PersistentARTrie, PersistentARTrieZipper, RecoveryMode, RecoveryReport, WalConfig,
};
#[cfg(feature = "persistent-artrie")]
pub use persistent_artrie_char::{
    PersistentARTrieChar, PersistentARTrieCharNode, PersistentARTrieCharZipper,
};
#[cfg(feature = "persistent-artrie")]
pub use persistent_vocab_artrie::{IndexedVocabularyPersistent, PersistentVocabARTrie};

/// Synchronization strategy for dictionary operations.
///
/// Different dictionary backends may have different thread-safety guarantees.
/// This trait allows backends to specify their synchronization requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStrategy {
    /// Backend requires external synchronization (e.g., RwLock).
    ///
    /// Use this for backends that use interior mutability without
    /// internal synchronization.
    ExternalSync,

    /// Backend is internally synchronized and safe for concurrent access.
    ///
    /// Use this for backends that use atomic operations, locks, or
    /// lock-free data structures internally.
    InternalSync,

    /// Backend is a persistent/immutable data structure.
    ///
    /// Mutations create new versions with structural sharing.
    /// Reads require no synchronization. Writes can use atomic swaps.
    Persistent,
}

/// Core dictionary abstraction for approximate string matching.
///
/// A dictionary represents a collection of terms that can be efficiently
/// traversed character-by-character via graph-like nodes. This trait
/// allows different backend implementations (trie, DAWG, double-array trie,
/// etc.) to be used interchangeably.
pub trait Dictionary {
    /// The node type used for dictionary traversal
    type Node: DictionaryNode;

    /// Get the root node of the dictionary
    fn root(&self) -> Self::Node;

    /// Check if a term exists in the dictionary
    fn contains(&self, term: &str) -> bool {
        let mut node = self.root();
        for unit in <Self::Node as DictionaryNode>::Unit::iter_str(term) {
            match node.transition(unit) {
                Some(next) => node = next,
                None => return false,
            }
        }
        node.is_final()
    }

    /// Get the total number of terms (if available efficiently)
    fn len(&self) -> Option<usize>;

    /// Check if the dictionary is empty
    fn is_empty(&self) -> bool {
        self.len().map(|n| n == 0).unwrap_or(false)
    }

    /// Get the synchronization strategy for this dictionary backend.
    ///
    /// This allows wrappers to optimize synchronization based on
    /// the backend's thread-safety guarantees.
    ///
    /// Default: `ExternalSync` (conservative, always safe)
    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::ExternalSync
    }

    /// Check if this dictionary uses suffix-based matching (substring search).
    ///
    /// Suffix-based dictionaries (like SuffixAutomaton) match substrings anywhere
    /// in the indexed text, whereas prefix-based dictionaries match complete words
    /// from the beginning.
    ///
    /// This affects how the Levenshtein automaton computes match distances:
    /// - Prefix-based: penalizes unmatched query suffix
    /// - Suffix-based: allows partial query matches without penalty
    ///
    /// Default: `false` (prefix-based matching)
    fn is_suffix_based(&self) -> bool {
        false
    }
}

/// Traversable dictionary node.
///
/// Nodes form a graph structure representing the dictionary, where edges
/// are labeled with character units (bytes or Unicode characters) and final
/// nodes mark valid terms.
///
/// # Type Parameters
///
/// The node is generic over [`CharUnit`], which can be:
/// - [`u8`] for byte-level matching (faster, ASCII-optimized)
/// - [`char`] for character-level matching (correct Unicode semantics)
pub trait DictionaryNode: Clone + Send + Sync {
    /// The character unit type for edge labels.
    ///
    /// Use `u8` for byte-level (existing behavior, fastest).
    /// Use `char` for character-level (proper Unicode support).
    type Unit: CharUnit;

    /// Check if this node marks the end of a valid term
    fn is_final(&self) -> bool;

    /// Transition to a child node via the given character unit
    ///
    /// Returns `None` if no such transition exists
    fn transition(&self, label: Self::Unit) -> Option<Self>;

    /// Iterate over all outgoing edges as (unit, child_node) pairs
    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_>;

    /// Check if a specific edge exists
    fn has_edge(&self, label: Self::Unit) -> bool {
        self.transition(label).is_some()
    }

    /// Get the number of outgoing edges (if efficiently available)
    fn edge_count(&self) -> Option<usize> {
        None
    }
}

/// Extension trait for dictionaries that map terms to values.
///
/// This trait enables "fuzzy maps" - dictionaries that associate arbitrary values
/// with terms, allowing efficient filtered queries based on those values. This is
/// particularly useful for contextual code completion where terms are mapped to
/// scope IDs, categories, or other metadata.
pub trait MappedDictionary: Dictionary {
    /// The type of values associated with dictionary terms
    type Value: DictionaryValue;

    /// Get the value associated with a term.
    ///
    /// Returns `None` if the term doesn't exist in the dictionary.
    ///
    /// This is a required method. The previous default returned `None` for
    /// every term while pretending to be a real implementation, which silently
    /// broke any user expecting `MappedDictionary` semantics. Every backend in
    /// this crate now provides an explicit override.
    fn get_value(&self, term: &str) -> Option<Self::Value>;

    /// Check if a term exists and its value matches a predicate
    ///
    /// This is more efficient than `get_value` + predicate test, as it can
    /// short-circuit early if the term doesn't exist.
    fn contains_with_value<F>(&self, term: &str, predicate: F) -> bool
    where
        F: Fn(&Self::Value) -> bool,
    {
        self.get_value(term).is_some_and(|v| predicate(&v))
    }
}

/// Extension trait for dictionary nodes that provide access to values.
///
/// This trait allows nodes to expose values during graph traversal, enabling
/// efficient filtering at query time without materializing all results first.
pub trait MappedDictionaryNode: DictionaryNode {
    /// The type of values associated with terms at this node
    type Value: DictionaryValue;

    /// Get the value at this node if it's a final node
    ///
    /// Returns `None` if this is not a final node, or if no value is associated.
    fn value(&self) -> Option<Self::Value>;
}

/// Trait for dictionaries supporting set-like term insertion and removal.
///
/// This trait extends [`Dictionary`] with mutation capabilities. It is the
/// set-like interface — `insert(&str)` adds a term, `remove(&str)` removes
/// one, no values involved. For dictionaries that carry values along with
/// terms, see [`MutableMappedDictionary`].
///
/// # Overlap with `MutableMappedDictionary`
///
/// `MutableMappedDictionary` covers most write-with-value operations
/// (`insert_with_value`, `update_or_insert`, `union_with`, …) but
/// **deliberately omits** `remove` and the value-free `insert`. The two
/// traits are complementary, not redundant:
///
/// - Dictionaries that are set-like only (or have `Value = ()`): impl
///   [`MutableDictionary`].
/// - Dictionaries that carry meaningful values: impl
///   [`MutableMappedDictionary`] for value-aware writes and (if removal is
///   supported) [`MutableDictionary`] for set-like removal.
///
/// Several backends in this crate (`DynamicDawg`, `DynamicDawgChar`,
/// `DynamicDawgU64`) implement both.
///
/// # Default Implementations
///
/// The trait provides default implementations for batch operations
/// (`extend`, `remove_many`) built on top of the required `insert` and
/// `remove` methods.
pub trait MutableDictionary: Dictionary {
    /// Insert a term into the dictionary.
    ///
    /// Returns `true` if the term was newly inserted, `false` if it already existed.
    fn insert(&self, term: &str) -> bool;

    /// Remove a term from the dictionary.
    ///
    /// Returns `true` if the term was present and removed, `false` otherwise.
    fn remove(&self, term: &str) -> bool;

    /// Batch insert multiple terms.
    ///
    /// Returns the number of new terms added (not counting duplicates).
    ///
    /// The default implementation calls `insert` for each term. Implementations
    /// may override this for better performance.
    fn extend<I, S>(&self, terms: I) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        terms
            .into_iter()
            .filter(|term| self.insert(term.as_ref()))
            .count()
    }

    /// Batch remove multiple terms.
    ///
    /// Returns the number of terms removed.
    ///
    /// The default implementation calls `remove` for each term. Implementations
    /// may override this for better performance.
    fn remove_many<I, S>(&self, terms: I) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        terms
            .into_iter()
            .filter(|term| self.remove(term.as_ref()))
            .count()
    }
}

/// Trait for dictionaries supporting compaction and minimization.
///
/// Dictionaries that support dynamic modifications (insertions and deletions)
/// may accumulate internal fragmentation or redundant structure over time.
/// This trait provides methods to restore optimal structure.
///
/// # Compaction vs Minimization
///
/// - **`compact()`**: Full rebuild - extracts all terms, sorts them, and reconstructs
///   the structure from scratch. Achieves perfect minimality but is O(n log n + m).
///
/// - **`minimize()`**: Incremental optimization - merges equivalent nodes without
///   full rebuild. Faster for localized changes but may not achieve perfect minimality.
pub trait CompactableDictionary: MutableDictionary {
    /// Check if compaction would be beneficial.
    ///
    /// Returns `true` if deletions have occurred or the structure has degraded
    /// significantly from optimal.
    fn needs_compaction(&self) -> bool;

    /// Compact the dictionary to restore optimal structure.
    ///
    /// This performs a full rebuild, extracting all terms, sorting them for
    /// optimal prefix sharing, and reconstructing the dictionary.
    ///
    /// Returns the number of nodes/elements removed or optimized away.
    fn compact(&self) -> usize;

    /// Minimize the dictionary using incremental optimization.
    ///
    /// Unlike `compact()`, this method:
    /// - Makes no assumptions about insertion order
    /// - Only examines affected nodes and their neighbors
    /// - Preserves existing structure where possible
    /// - Is faster than `compact()` for localized updates
    ///
    /// Returns the number of nodes merged.
    ///
    /// The default implementation delegates to `compact()`. Dictionaries
    /// with more efficient incremental algorithms should override this.
    fn minimize(&self) -> usize {
        self.compact()
    }
}

/// Extension trait for dictionaries that support inserting values.
///
/// This trait enables mutation of mapped dictionaries, allowing terms to be
/// added or updated with associated values.
pub trait MutableMappedDictionary: MappedDictionary {
    /// Insert or update a term with an associated value.
    ///
    /// # Arguments
    ///
    /// * `term` - The term to insert
    /// * `value` - The value to associate with the term
    ///
    /// # Returns
    ///
    /// `true` if this is a new term, `false` if updating an existing term's value.
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool;

    /// Union this dictionary with another, applying a merge function for conflicting values.
    ///
    /// Iterates through all terms in `other` and:
    /// - Inserts new terms directly
    /// - For existing terms, merges values using `merge_fn`
    ///
    /// # Arguments
    ///
    /// * `other` - The dictionary to union with
    /// * `merge_fn` - Function to merge values when term exists in both dictionaries.
    ///   Takes `(existing_value, other_value)` and returns the merged value.
    ///
    /// # Returns
    ///
    /// Number of terms processed from `other`
    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone;

    /// Union with another dictionary, keeping the right (other's) value on conflicts.
    ///
    /// Convenience method equivalent to `union_with(other, |_, right| right.clone())`.
    fn union_replace(&self, other: &Self) -> usize
    where
        Self::Value: Clone,
    {
        self.union_with(other, |_, right| right.clone())
    }

    /// Update an existing term's value in place, or insert a new term with a default value.
    ///
    /// This method is useful when you want to incrementally modify a value (e.g., adding
    /// elements to a `HashSet` or `Vec`) without replacing it entirely.
    ///
    /// # Arguments
    ///
    /// * `term` - The term to update or insert
    /// * `default_value` - The value to use if the term doesn't exist
    /// * `update_fn` - Function to apply to the existing value if the term exists
    ///
    /// # Returns
    ///
    /// `true` if this was a new term (inserted with default), `false` if an existing term was updated.
    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value);
}

/// Prelude module for convenient imports.
pub mod prelude {
    pub use crate::{
        BijectiveDictionary, BijectiveMap, CharUnit, CompactableDictionary, DictZipper, Dictionary,
        DictionaryNode, DictionaryValue, InsertError, MappedDictionary, MappedDictionaryNode,
        MutableDictionary, MutableMappedDictionary, SyncStrategy, ValuedDictZipper,
    };

    // Re-export common dictionary types
    pub use crate::double_array_trie::DoubleArrayTrie;
    pub use crate::double_array_trie_char::DoubleArrayTrieChar;
    pub use crate::dynamic_dawg::DynamicDawg;
    pub use crate::dynamic_dawg_char::DynamicDawgChar;
    pub use crate::dynamic_dawg_u64::DynamicDawgU64;
    pub use crate::scdawg::Scdawg;
    pub use crate::scdawg_char::ScdawgChar;
    pub use crate::suffix_automaton::SuffixAutomaton;
    pub use crate::suffix_automaton_char::SuffixAutomatonChar;
}
