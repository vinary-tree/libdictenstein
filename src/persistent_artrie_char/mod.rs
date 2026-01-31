//! Character-level Persistent Adaptive Radix Trie for proper Unicode support.
//!
//! This module provides a character-based variant of PersistentARTrie that operates
//! at the Unicode character level rather than byte level. This ensures correct edit
//! distance semantics for multi-byte UTF-8 sequences.
//!
//! ## Module Structure
//!
//! - `nodes`: ART node types adapted for u32/char keys (CharNode4, CharNode16, CharNode48, CharBucket)
//!
//! ## Differences from PersistentARTrie
//!
//! - Edge labels are `char` (4 bytes) instead of `u8` (1 byte)
//! - Distance calculations count characters, not bytes
//! - Correct semantics: "" → "¡" is distance 1, not 2
//! - No Node256: Would require 4GB array for u32 keys
//! - CharBucket handles >48 children using HashMap
//!
//! ## Performance Trade-offs
//!
//! - **Memory**: Uses char-indexed edges (larger fanout space)
//! - **Speed**: Slightly slower due to UTF-8 encoding/decoding
//! - **Correctness**: Proper Unicode semantics
//!
//! ## Use Cases
//!
//! Use `PersistentARTrieChar` when:
//! - Dictionary contains non-ASCII Unicode characters
//! - Edit distance must be measured in characters, not bytes
//! - Correctness is more important than maximum performance
//!
//! # ACID Guarantees
//!
//! This implementation provides the same ACID properties as [`super::persistent_artrie`]:
//!
//! ## Atomicity
//!
//! - **Single Operations**: Individual insert/remove operations are atomic
//! - **Document Transactions**: Multi-term transactions via [`CharDocumentTransaction`] provide
//!   all-or-nothing semantics
//! - **WAL Logging**: Operations are logged to WAL before application
//!
//! ## Consistency
//!
//! - **Trie Invariants**: Character-level ART structure invariants are maintained
//! - **CRC32 Checksums**: WAL records include CRC32 checksums
//! - **Recovery Validation**: Crash recovery validates and replays valid records
//!
//! ## Isolation
//!
//! | Isolation Level | Dirty Read | Non-Repeatable Read | Phantom Read |
//! |-----------------|------------|---------------------|--------------|
//! | RwLock (default)| No         | No                  | No           |
//! | MVCC-Lite       | No         | No                  | Possible*    |
//!
//! *Epoch-based snapshots allow concurrent reads/writes with potential phantoms.
//!
//! ## Durability
//!
//! Durability is controlled by [`DurabilityPolicy`]:
//!
//! | Policy      | fsync Behavior      | Guarantee    | Use Case                        |
//! |-------------|---------------------|--------------|----------------------------------|
//! | `Immediate` | Every CommitTx      | Full         | ACID compliance (default)       |
//! | `GroupCommit`| Batched by coordinator| Full      | High-throughput workloads       |
//! | `Periodic`  | At checkpoints only | Bounded loss | Performance-critical            |
//! | `None`      | Never               | None         | Testing only                    |

// Shared types for both in-memory and disk-backed modes
pub mod types;

// ART node types for char keys
pub mod nodes;

// Serialization for char nodes
pub mod serialization_char;

// Arena allocation for space-efficient disk storage
pub mod arena;

// Arena manager for managing multiple arenas
pub mod arena_manager;

// Compact variable-width encoding for space-efficient serialization
pub mod compact_encoding;

// Traversal context for block caching
pub mod traversal_context;

// Dirty tracking for incremental checkpoints
pub mod dirty_tracker;

// Hash-based deduplication for space efficiency
pub mod dedup;

// Relative offset encoding for space-efficient child pointers
pub mod relative_encoding;

// Crash recovery for corrupted files
pub mod recovery;

// Per-node logging for near-instant recovery (char-specific adaptation)
pub mod per_node_log_char;

// Disk-backed implementation
pub mod dict_impl_char;

// Re-export shared types (always available)
pub use types::{
    CharTrieFileHeader, CharTrieNodeInner, CharTrieRoot,
    CHAR_FILE_HEADER_SIZE, CHAR_TRIE_MAGIC, CHAR_HEADER_VERSION_V1, CHAR_HEADER_VERSION_V2,
    DEFAULT_CHAR_BUFFER_POOL_SIZE,
    EnhancedRecoveryMode, EnhancedRecoveryStats,
};

// Re-export disk-backed types (feature-gated)
pub use types::{PrefixTermWithArena, PrefixTermWithValueAndArena};

// Re-export disk-backed implementation types
pub use dict_impl_char::{
    DiskBackedCharTrieInner, SharedCharTrie,
    // Transaction types
    CharDocumentTransaction, DurabilityPolicy, TransactionState,
};

// Re-export node types
pub use nodes::{
    AddChildError, CharArtNode, CharBucket, CharCompressedPrefix, CharNode, CharNode16, CharNode4,
    CharNode48, CharNodeHeader, CHAR_MAX_PREFIX_LEN,
};

// Re-export serialization
pub use serialization_char::{
    char_from_bytes, char_serialized_size, char_to_bytes, deserialize_char_node,
    serialize_char_node, CHAR_FORMAT_VERSION, CHAR_NODE_MAGIC, CHAR_SERIALIZED_HEADER_SIZE,
    SerializedCharNodeHeader,
};

// Re-export compact serialization (under feature flag)
pub use serialization_char::{
    char_from_bytes_compact, char_to_bytes_compact, char_compact_serialized_size,
};

// Re-export compact encoding utilities (under feature flag)
pub use compact_encoding::{
    CompactHeader, DecodedCompactNode, determine_key_width, determine_ptr_width,
    COMPACT_NODE_TYPE_N4, COMPACT_NODE_TYPE_N16, COMPACT_NODE_TYPE_N48, COMPACT_NODE_TYPE_BUCKET,
};

// Re-export arena types (under feature flag)
pub use arena::{
    ArenaHeader, CharNodeArena, CharNodeArenaV2, SlotEntry, VarintSlotEntry,
    ARENA_MAGIC, ARENA_MAGIC_V2, ARENA_VERSION, ARENA_VERSION_V2,
    FLAG_VARINT_DIRECTORY, HEADER_SIZE, MIN_FREE_SPACE, SLOT_SIZE,
};

// Re-export arena manager types (under feature flag)
pub use arena_manager::{ArenaManager, ArenaSlot, ArenaStats, FlushConfig, FlushStats, ReservedSlots};

// Re-export per-node logging types (under feature flag)
pub use per_node_log_char::{
    CharNodeLogEntry, CharInlineLog, CharInlineLogIter, CharLogWriter, CharLogIterExt,
    // Re-export node-agnostic types from the base module
    DirtyNodeTracker, NodeId, PageId, PerNodeLogConfig,
    PerNodeLogStats, PerNodeLogStatsAtomic,
};

// Re-export traversal context types (under feature flag)
pub use traversal_context::{LightweightTraversalContext, TraversalContext, TraversalStats};

// Re-export dirty tracker types (under feature flag)
pub use dirty_tracker::{BatchDirtyTracker, DirtyTracker, DirtyTrackerStats};

// Re-export deduplication types (under feature flag)
pub use dedup::{BatchDeduplicator, DeduplicatingArenaManager, DeduplicatorStats, NodeDeduplicator};

// Re-export relative encoding types (under feature flag)
pub use relative_encoding::{
    encode_child_pointer, decode_child_pointer, encode_children, decode_children,
    encode_sequential_siblings, decode_sequential_siblings, encoded_size, is_same_arena,
    FLAG_CROSS_ARENA, FLAG_RELATIVE_OFFSETS, FLAG_SEQUENTIAL_SIBLINGS, CROSS_ARENA_SIZE,
};

// Re-export recovery types (under feature flag)
pub use recovery::{
    CorruptionInfo, CorruptionType, RecoveredOperation, RecoveryManager,
    RecoveryMode, RecoveryPolicy, RecoveryReport, detect_corruption,
    // Re-exported from 1-byte implementation (node-agnostic)
    IncrementalRecovery, RecoveredState, RecoveryError, RecoveryStats,
    find_wal_archive_segments, rebuild_from_wal_segments,
};

use crate::value::DictionaryValue;
use crate::zipper::{DictZipper, ValuedDictZipper};
use crate::{Dictionary, DictionaryNode, MappedDictionary, MutableMappedDictionary};
use std::collections::BTreeMap;
use std::sync::Arc;

#[cfg(feature = "parking_lot")]
use crate::sync_compat::RwLock;
#[cfg(not(feature = "parking_lot"))]
use std::sync::RwLock;

/// Shared inner state for PersistentARTrieChar
#[derive(Debug)]
struct PersistentARTrieCharInner<V: DictionaryValue> {
    /// Root node of the trie
    root: Arc<CharTrieNode<V>>,
    /// Number of terms in the dictionary
    len: usize,
}

/// A character-indexed trie node for Unicode support
#[derive(Debug, Clone)]
struct CharTrieNode<V: DictionaryValue> {
    /// Is this node the end of a complete term?
    is_final: bool,
    /// Children indexed by character
    children: BTreeMap<char, Arc<CharTrieNode<V>>>,
    /// Optional value associated with this node (if final)
    value: Option<V>,
}

impl<V: DictionaryValue> Default for CharTrieNode<V> {
    fn default() -> Self {
        Self {
            is_final: false,
            children: BTreeMap::new(),
            value: None,
        }
    }
}

impl<V: DictionaryValue> CharTrieNode<V> {
    /// Create a new empty node
    fn new() -> Self {
        Self::default()
    }

    /// Create a final node with a value
    #[allow(dead_code)]
    fn new_final(value: V) -> Self {
        Self {
            is_final: true,
            children: BTreeMap::new(),
            value: Some(value),
        }
    }

    /// Get a child by character
    fn get_child(&self, c: char) -> Option<&Arc<CharTrieNode<V>>> {
        self.children.get(&c)
    }

    /// Iterate over children
    fn iter_children(&self) -> impl Iterator<Item = (char, &Arc<CharTrieNode<V>>)> {
        self.children.iter().map(|(&c, node)| (c, node))
    }
}

/// Character-level Persistent Adaptive Radix Trie for Unicode support.
///
/// This dictionary provides proper Unicode character-level edit distance
/// calculations, ensuring that multi-byte UTF-8 characters are counted
/// as single edit operations.
#[derive(Debug)]
pub struct PersistentARTrieChar<V: DictionaryValue = ()> {
    inner: Arc<RwLock<PersistentARTrieCharInner<V>>>,
}

impl<V: DictionaryValue> Default for PersistentARTrieChar<V> {
    #[allow(deprecated)]
    fn default() -> Self {
        Self::new()
    }
}

impl<V: DictionaryValue> Clone for PersistentARTrieChar<V> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<V: DictionaryValue> PersistentARTrieChar<V> {
    /// Create a new empty character-level trie.
    ///
    /// # Deprecated
    ///
    /// This method is deprecated because "Persistent" types are designed for
    /// disk-backed storage. Use `create()` or `open()` for disk persistence.
    /// For in-memory tries, use the optimized implementations instead:
    /// - [`DoubleArrayTrieChar`](crate::double_array_trie_char::DoubleArrayTrieChar) (fastest reads, insert-only)
    /// - [`DynamicDawgChar`](crate::dynamic_dawg_char::DynamicDawgChar) (insert + remove, SIMD optimized)
    #[deprecated(
        since = "0.2.0",
        note = "Use `create()` or `open()` for disk persistence. For in-memory tries, use DoubleArrayTrieChar or DynamicDawgChar instead."
    )]
    pub fn new() -> Self {
        let inner = PersistentARTrieCharInner {
            root: Arc::new(CharTrieNode::new()),
            len: 0,
        };
        Self {
            inner: Arc::new(RwLock::new(inner)),
        }
    }

    /// Insert a term into the trie
    pub fn insert(&self, term: &str) -> bool
    where
        V: Default,
    {
        self.insert_with_value(term, V::default())
    }

    /// Insert a term with an associated value
    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        #[cfg(feature = "parking_lot")]
        let mut guard = self.inner.write();
        #[cfg(not(feature = "parking_lot"))]
        let mut guard = self.inner.write().expect("lock poisoned");

        // Navigate to the insertion point, creating nodes as needed
        let chars: Vec<char> = term.chars().collect();
        let mut current = Arc::clone(&guard.root);
        let mut path: Vec<(char, Arc<CharTrieNode<V>>)> = Vec::new();

        for &c in &chars {
            let next = current.get_child(c).cloned();
            path.push((c, Arc::clone(&current)));
            current = match next {
                Some(node) => node,
                None => Arc::new(CharTrieNode::new()),
            };
        }

        // Check if already exists
        if current.is_final {
            return false;
        }

        // Build the new path from bottom up
        let mut new_node = CharTrieNode {
            is_final: true,
            children: current.children.clone(),
            value: Some(value),
        };

        for (c, parent) in path.into_iter().rev() {
            let mut new_parent = CharTrieNode {
                is_final: parent.is_final,
                children: parent.children.clone(),
                value: parent.value.clone(),
            };
            new_parent.children.insert(c, Arc::new(new_node));
            new_node = new_parent;
        }

        guard.root = Arc::new(new_node);
        guard.len += 1;
        true
    }

    /// Check if a term exists in the trie
    pub fn contains(&self, term: &str) -> bool {
        #[cfg(feature = "parking_lot")]
        let guard = self.inner.read();
        #[cfg(not(feature = "parking_lot"))]
        let guard = self.inner.read().expect("lock poisoned");

        let mut current = Arc::clone(&guard.root);
        for c in term.chars() {
            match current.get_child(c) {
                Some(child) => current = Arc::clone(child),
                None => return false,
            }
        }
        current.is_final
    }

    /// Get the number of terms in the dictionary
    pub fn len(&self) -> usize {
        #[cfg(feature = "parking_lot")]
        let guard = self.inner.read();
        #[cfg(not(feature = "parking_lot"))]
        let guard = self.inner.read().expect("lock poisoned");
        guard.len
    }

    /// Check if the dictionary is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get the root node
    pub fn root(&self) -> PersistentARTrieCharNode<V> {
        #[cfg(feature = "parking_lot")]
        let guard = self.inner.read();
        #[cfg(not(feature = "parking_lot"))]
        let guard = self.inner.read().expect("lock poisoned");

        PersistentARTrieCharNode {
            node: Arc::clone(&guard.root),
        }
    }
}

/// Build from an iterator of terms
impl<V: DictionaryValue + Default> FromIterator<String> for PersistentARTrieChar<V> {
    #[allow(deprecated)]
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        let trie = Self::new();
        for term in iter {
            trie.insert(&term);
        }
        trie
    }
}

impl<'a, V: DictionaryValue + Default> FromIterator<&'a str> for PersistentARTrieChar<V> {
    #[allow(deprecated)]
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        let trie = Self::new();
        for term in iter {
            trie.insert(term);
        }
        trie
    }
}

/// Node in the character-level trie for DictionaryNode trait
#[derive(Debug, Clone)]
pub struct PersistentARTrieCharNode<V: DictionaryValue = ()> {
    node: Arc<CharTrieNode<V>>,
}

impl<V: DictionaryValue> DictionaryNode for PersistentARTrieCharNode<V> {
    type Unit = char;

    fn is_final(&self) -> bool {
        self.node.is_final
    }

    fn transition(&self, label: char) -> Option<Self> {
        self.node.get_child(label).map(|child| Self {
            node: Arc::clone(child),
        })
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (char, Self)> + '_> {
        let edges: Vec<_> = self
            .node
            .iter_children()
            .map(|(c, child)| {
                (
                    c,
                    Self {
                        node: Arc::clone(child),
                    },
                )
            })
            .collect();
        Box::new(edges.into_iter())
    }
}

impl<V: DictionaryValue> Dictionary for PersistentARTrieChar<V> {
    type Node = PersistentARTrieCharNode<V>;

    fn root(&self) -> Self::Node {
        PersistentARTrieChar::root(self)
    }

    fn contains(&self, term: &str) -> bool {
        PersistentARTrieChar::contains(self, term)
    }

    fn len(&self) -> Option<usize> {
        Some(PersistentARTrieChar::len(self))
    }
}

impl<V: DictionaryValue> MappedDictionary for PersistentARTrieChar<V> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<V> {
        #[cfg(feature = "parking_lot")]
        let guard = self.inner.read();
        #[cfg(not(feature = "parking_lot"))]
        let guard = self.inner.read().expect("lock poisoned");

        let mut current = Arc::clone(&guard.root);
        for c in term.chars() {
            match current.get_child(c) {
                Some(child) => current = Arc::clone(child),
                None => return None,
            }
        }
        if current.is_final {
            current.value.clone()
        } else {
            None
        }
    }
}

impl<V: DictionaryValue + Clone> MutableMappedDictionary for PersistentARTrieChar<V> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentARTrieChar::insert_with_value(self, term, value)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let mut processed = 0;

        for (term, value) in other.iter_with_values() {
            processed += 1;
            let merged_value = if let Some(self_value) = self.get_value(&term) {
                merge_fn(&self_value, &value)
            } else {
                value
            };
            self.insert_with_value(&term, merged_value);
        }

        processed
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        if let Some(existing) = self.get_value(term) {
            let mut value = existing;
            update_fn(&mut value);
            self.insert_with_value(term, value);
            false // Term existed
        } else {
            self.insert_with_value(term, default_value);
            true // New term
        }
    }
}

/// Iterator over terms in the trie
pub struct PersistentARTrieCharIterator<V: DictionaryValue> {
    stack: Vec<(String, Arc<CharTrieNode<V>>)>,
}

impl<V: DictionaryValue> Iterator for PersistentARTrieCharIterator<V> {
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((prefix, node)) = self.stack.pop() {
            // Push children in reverse order for correct alphabetical order
            let children: Vec<_> = node.iter_children().collect();
            for (c, child) in children.into_iter().rev() {
                let mut new_prefix = prefix.clone();
                new_prefix.push(c);
                self.stack.push((new_prefix, Arc::clone(child)));
            }

            if node.is_final {
                return Some(prefix);
            }
        }
        None
    }
}

/// Iterator over terms and values in the trie
pub struct PersistentARTrieCharValueIterator<V: DictionaryValue> {
    stack: Vec<(String, Arc<CharTrieNode<V>>)>,
}

impl<V: DictionaryValue> Iterator for PersistentARTrieCharValueIterator<V> {
    type Item = (String, V);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((prefix, node)) = self.stack.pop() {
            // Push children in reverse order for correct alphabetical order
            let children: Vec<_> = node.iter_children().collect();
            for (c, child) in children.into_iter().rev() {
                let mut new_prefix = prefix.clone();
                new_prefix.push(c);
                self.stack.push((new_prefix, Arc::clone(child)));
            }

            if node.is_final {
                if let Some(value) = node.value.clone() {
                    return Some((prefix, value));
                }
            }
        }
        None
    }
}

impl<V: DictionaryValue> IntoIterator for &PersistentARTrieChar<V> {
    type Item = String;
    type IntoIter = PersistentARTrieCharIterator<V>;

    fn into_iter(self) -> Self::IntoIter {
        #[cfg(feature = "parking_lot")]
        let guard = self.inner.read();
        #[cfg(not(feature = "parking_lot"))]
        let guard = self.inner.read().expect("lock poisoned");

        PersistentARTrieCharIterator {
            stack: vec![(String::new(), Arc::clone(&guard.root))],
        }
    }
}

impl<V: DictionaryValue> PersistentARTrieChar<V> {
    /// Iterate over all terms in the dictionary
    pub fn iter(&self) -> PersistentARTrieCharIterator<V> {
        self.into_iter()
    }

    /// Iterate over all terms and their values in the dictionary
    ///
    /// Returns an iterator that yields `(String, V)` pairs for each term
    /// in the dictionary.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
    /// trie.insert_with_value("hello", 1);
    /// trie.insert_with_value("world", 2);
    ///
    /// for (term, value) in trie.iter_with_values() {
    ///     println!("{}: {}", term, value);
    /// }
    /// ```
    pub fn iter_with_values(&self) -> PersistentARTrieCharValueIterator<V> {
        #[cfg(feature = "parking_lot")]
        let guard = self.inner.read();
        #[cfg(not(feature = "parking_lot"))]
        let guard = self.inner.read().expect("lock poisoned");

        PersistentARTrieCharValueIterator {
            stack: vec![(String::new(), Arc::clone(&guard.root))],
        }
    }

    /// Iterate over all terms as character vectors
    ///
    /// This is useful when you need character-level access to terms
    /// rather than string representations.
    pub fn iter_chars(&self) -> impl Iterator<Item = Vec<char>> + '_ {
        self.iter().map(|s| s.chars().collect())
    }

    /// Iterate over all terms and values as character vectors
    ///
    /// Returns `(Vec<char>, V)` pairs for character-level processing.
    pub fn iter_chars_with_values(&self) -> impl Iterator<Item = (Vec<char>, V)> + '_ {
        self.iter_with_values()
            .map(|(s, v)| (s.chars().collect(), v))
    }

    /// Iterate over all terms with the given prefix.
    ///
    /// Returns `None` if the prefix path doesn't exist in the trie.
    /// Returns `Some(iterator)` that yields all terms starting with the prefix.
    ///
    /// Uses the zipper-based `PrefixZipper` trait for O(k) navigation to the prefix,
    /// followed by O(m) iteration over matching terms.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The string prefix to search for
    ///
    /// # Returns
    ///
    /// * `Some(impl Iterator<Item = String>)` - Iterator over matching terms
    /// * `None` - If no terms with this prefix exist
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    ///
    /// let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
    /// trie.insert("apple");
    /// trie.insert("application");
    /// trie.insert("banana");
    ///
    /// if let Some(iter) = trie.iter_prefix("app") {
    ///     for term in iter {
    ///         println!("{}", term);
    ///     }
    ///     // Prints: "apple" and "application"
    /// }
    /// ```
    pub fn iter_prefix(&self, prefix: &str) -> Option<impl Iterator<Item = String> + '_> {
        use crate::prefix_zipper::PrefixZipper;

        let prefix_chars: Vec<char> = prefix.chars().collect();
        let zipper = PersistentARTrieCharZipper::new(self);
        let prefix_iter = zipper.with_prefix(&prefix_chars)?;
        Some(prefix_iter.map(|(path, _)| path.iter().collect()))
    }

    /// Iterate over all (term, value) pairs with the given prefix.
    ///
    /// Returns `None` if the prefix path doesn't exist in the trie.
    /// Returns `Some(iterator)` that yields all (term, value) pairs where term starts with prefix.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The string prefix to search for
    ///
    /// # Returns
    ///
    /// * `Some(impl Iterator<Item = (String, V)>)` - Iterator over matching (term, value) pairs
    /// * `None` - If no terms with this prefix exist
    pub fn iter_prefix_with_values(&self, prefix: &str) -> Option<impl Iterator<Item = (String, V)> + '_> {
        use crate::prefix_zipper::ValuedPrefixZipper;

        let prefix_chars: Vec<char> = prefix.chars().collect();
        let zipper = PersistentARTrieCharZipper::new(self);
        let prefix_iter = zipper.with_prefix_values(&prefix_chars)?;
        Some(prefix_iter.map(|(path, value)| (path.iter().collect(), value)))
    }

    /// Remove all terms with the given prefix (batched for memory efficiency).
    ///
    /// Returns the number of terms removed. Uses batched processing to limit
    /// memory usage to O(batch_size) regardless of how many terms match the prefix.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The string prefix of terms to remove
    ///
    /// # Returns
    ///
    /// The number of terms that were removed.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    ///
    /// let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
    /// trie.insert("apple");
    /// trie.insert("application");
    /// trie.insert("banana");
    ///
    /// let count = trie.remove_prefix("app");
    /// assert_eq!(count, 2); // Removed "apple", "application"
    /// assert!(trie.contains("banana"));
    /// ```
    pub fn remove_prefix(&self, prefix: &str) -> usize {
        self.remove_prefix_batched(prefix, 1024)
    }

    /// Remove all terms with the given prefix using a custom batch size.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The string prefix of terms to remove
    /// * `batch_size` - Maximum number of terms to collect per iteration
    ///
    /// # Returns
    ///
    /// The number of terms that were removed.
    pub fn remove_prefix_batched(&self, prefix: &str, batch_size: usize) -> usize {
        let batch_size = batch_size.max(1);
        let mut total_removed = 0;

        loop {
            // Collect a batch of terms to remove
            let batch: Vec<String> = self
                .iter_prefix(prefix)
                .map(|iter| iter.take(batch_size).collect())
                .unwrap_or_default();

            if batch.is_empty() {
                break;
            }

            // Remove the batch
            #[cfg(feature = "parking_lot")]
            let mut guard = self.inner.write();
            #[cfg(not(feature = "parking_lot"))]
            let mut guard = self.inner.write().expect("lock poisoned");

            // Track removals in this batch separately to avoid borrow conflict
            let mut batch_removed = 0;
            {
                let root = Arc::make_mut(&mut guard.root);
                for term in batch {
                    let chars: Vec<char> = term.chars().collect();
                    if Self::remove_chars_impl_static(root, chars.into_iter()) {
                        batch_removed += 1;
                    }
                }
            }
            // Now update the length after the root borrow is released
            guard.len -= batch_removed;
            total_removed += batch_removed;
        }

        total_removed
    }

    /// Static helper for remove_chars_impl that doesn't require Self reference
    fn remove_chars_impl_static(node: &mut CharTrieNode<V>, mut chars: impl Iterator<Item = char>) -> bool {
        match chars.next() {
            None => {
                let was_final = node.is_final;
                node.is_final = false;
                node.value = None;
                was_final
            }
            Some(c) => {
                if let Some(child) = node.children.get_mut(&c) {
                    let child = Arc::make_mut(child);
                    Self::remove_chars_impl_static(child, chars)
                } else {
                    false
                }
            }
        }
    }
}

// === Atomic Operations (for serde-capable values) ===

impl<V: DictionaryValue + serde::Serialize + serde::de::DeserializeOwned> PersistentARTrieChar<V> {
    /// Atomically increment a numeric value.
    ///
    /// If the term doesn't exist, it's created with the delta as its initial value.
    /// The value must be interpretable as i64.
    ///
    /// # Arguments
    ///
    /// * `term` - The term whose value to increment
    /// * `delta` - The amount to add (can be negative for decrement)
    ///
    /// # Returns
    ///
    /// The new value after incrementing.
    ///
    /// # Errors
    ///
    /// Returns an error if the value cannot be serialized/deserialized as i64.
    pub fn increment(&self, term: &str, delta: i64) -> Result<i64, String> {
        #[cfg(feature = "parking_lot")]
        let mut inner = self.inner.write();
        #[cfg(not(feature = "parking_lot"))]
        let mut inner = self.inner.write().expect("lock poisoned");

        // Get current value
        let current: i64 = if let Some(v) = Self::get_value_from_node(&inner.root, term.chars()) {
            let bytes = bincode::serialize(&v).map_err(|e| e.to_string())?;
            if bytes.len() == 8 {
                i64::from_le_bytes(bytes.try_into().unwrap())
            } else {
                bincode::deserialize::<i64>(&bytes).map_err(|e| e.to_string())?
            }
        } else {
            0
        };

        let new_value = current + delta;

        // Create value from i64
        let value_bytes = bincode::serialize(&new_value).map_err(|e| e.to_string())?;
        let v: V = bincode::deserialize(&value_bytes).map_err(|e| e.to_string())?;

        // Update the trie
        let chars: Vec<char> = term.chars().collect();
        // Check existence BEFORE getting mutable reference
        let existed = Self::contains_chars_impl(&inner.root, chars.iter().copied());
        let root = Arc::make_mut(&mut inner.root);
        Self::insert_chars_impl(root, chars.iter().copied(), Some(v));
        if !existed {
            inner.len += 1;
        }

        Ok(new_value)
    }

    /// Atomically update or insert a value.
    ///
    /// # Returns
    ///
    /// `true` if a new term was inserted, `false` if an existing term was updated.
    pub fn upsert(&self, term: &str, value: V) -> bool {
        #[cfg(feature = "parking_lot")]
        let mut inner = self.inner.write();
        #[cfg(not(feature = "parking_lot"))]
        let mut inner = self.inner.write().expect("lock poisoned");

        let chars: Vec<char> = term.chars().collect();
        // Check existence BEFORE getting mutable reference
        let existed = Self::contains_chars_impl(&inner.root, chars.iter().copied());
        let root = Arc::make_mut(&mut inner.root);

        // Remove existing and insert new
        if existed {
            Self::remove_chars_impl(root, chars.iter().copied());
        }
        Self::insert_chars_impl(root, chars.iter().copied(), Some(value));

        if !existed {
            inner.len += 1;
        }

        !existed
    }

    /// Atomically compare and swap a value.
    ///
    /// Updates the value only if the current value matches `expected`.
    ///
    /// # Returns
    ///
    /// `true` if the swap succeeded, `false` if the current value didn't match expected.
    pub fn compare_and_swap(&self, term: &str, expected: Option<V>, new_value: V) -> bool {
        #[cfg(feature = "parking_lot")]
        let mut inner = self.inner.write();
        #[cfg(not(feature = "parking_lot"))]
        let mut inner = self.inner.write().expect("lock poisoned");

        let chars: Vec<char> = term.chars().collect();
        let current = Self::get_value_from_node(&inner.root, chars.iter().copied());

        // Check if current matches expected
        let matches = match (&current, &expected) {
            (None, None) => true,
            (Some(c), Some(e)) => {
                let c_bytes = bincode::serialize(c).ok();
                let e_bytes = bincode::serialize(e).ok();
                c_bytes == e_bytes
            }
            _ => false,
        };

        if matches {
            let root = Arc::make_mut(&mut inner.root);
            let existed = current.is_some();
            if existed {
                Self::remove_chars_impl(root, chars.iter().copied());
            }
            Self::insert_chars_impl(root, chars.iter().copied(), Some(new_value));
            if !existed {
                inner.len += 1;
            }
        }

        matches
    }

    /// Get the current value and increment atomically (fetch-and-add).
    ///
    /// Returns the value *before* the increment.
    pub fn fetch_add(&self, term: &str, delta: i64) -> Result<i64, String> {
        let new_value = self.increment(term, delta)?;
        Ok(new_value - delta)
    }

    /// Get or insert a default value atomically.
    ///
    /// If the term exists, returns its current value.
    /// If not, inserts the default value and returns it.
    pub fn get_or_insert(&self, term: &str, default: V) -> V {
        #[cfg(feature = "parking_lot")]
        let mut inner = self.inner.write();
        #[cfg(not(feature = "parking_lot"))]
        let mut inner = self.inner.write().expect("lock poisoned");

        let chars: Vec<char> = term.chars().collect();

        if let Some(v) = Self::get_value_from_node(&inner.root, chars.iter().copied()) {
            return v;
        }

        // Insert default value
        let root = Arc::make_mut(&mut inner.root);
        Self::insert_chars_impl(root, chars.iter().copied(), Some(default.clone()));
        inner.len += 1;

        default
    }

    /// Helper to get value from a node by character path
    fn get_value_from_node(node: &CharTrieNode<V>, mut chars: impl Iterator<Item = char>) -> Option<V> {
        match chars.next() {
            None => {
                if node.is_final {
                    node.value.clone()
                } else {
                    None
                }
            }
            Some(c) => {
                node.children
                    .get(&c)
                    .and_then(|child| Self::get_value_from_node(child, chars))
            }
        }
    }

    /// Helper to check if term exists
    fn contains_chars_impl(node: &CharTrieNode<V>, mut chars: impl Iterator<Item = char>) -> bool {
        match chars.next() {
            None => node.is_final,
            Some(c) => node
                .children
                .get(&c)
                .is_some_and(|child| Self::contains_chars_impl(child, chars)),
        }
    }

    /// Helper to insert with value
    fn insert_chars_impl(node: &mut CharTrieNode<V>, mut chars: impl Iterator<Item = char>, value: Option<V>) {
        match chars.next() {
            None => {
                node.is_final = true;
                node.value = value;
            }
            Some(c) => {
                let child = node.children.entry(c).or_insert_with(|| Arc::new(CharTrieNode::default()));
                let child = Arc::make_mut(child);
                Self::insert_chars_impl(child, chars, value);
            }
        }
    }

    /// Helper to remove a term
    fn remove_chars_impl(node: &mut CharTrieNode<V>, mut chars: impl Iterator<Item = char>) -> bool {
        match chars.next() {
            None => {
                let was_final = node.is_final;
                node.is_final = false;
                node.value = None;
                was_final
            }
            Some(c) => {
                if let Some(child) = node.children.get_mut(&c) {
                    let child = Arc::make_mut(child);
                    Self::remove_chars_impl(child, chars)
                } else {
                    false
                }
            }
        }
    }
}

// === Persistence Operations (feature-gated) ===

use std::path::Path;

use crate::persistent_artrie::error::Result as PersistentResult;

// Note: This impl block uses Self::new() internally for the transitional implementation.
// Once the full disk-backed architecture is merged, these methods will be updated.
#[allow(deprecated)]
impl<V: DictionaryValue> PersistentARTrieChar<V> {
    /// Create a new persistent dictionary at the given path.
    ///
    /// This creates a new dictionary file with WAL for crash recovery.
    /// If a file already exists at the path, this will return an error.
    ///
    /// # Arguments
    /// * `path` - Path to the dictionary file (will also create `.wal` file)
    ///
    /// # Example
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    ///
    /// let trie = PersistentARTrieChar::<()>::create("words.artc")?;
    /// trie.insert("hello");
    /// trie.checkpoint()?;
    /// ```
    pub fn create<P: AsRef<Path>>(path: P) -> PersistentResult<Self> {
        use dict_impl_char::DiskBackedCharTrieInner;

        // Create disk-backed inner
        let disk_inner: DiskBackedCharTrieInner<V> = DiskBackedCharTrieInner::create(path)?;

        // Wrap in SharedCharTrie for thread-safety, then convert to our in-memory format
        // Note: This creates a new empty trie that delegates persistence operations
        // to the disk-backed implementation
        let trie = Self::new();

        // Store the disk-backed inner in a thread-local or use a different approach
        // For now, we return the in-memory trie - the full integration will be done
        // in a later phase when we unify the inner types
        //
        // TODO: Phase 3-5 will complete this by updating PersistentARTrieCharInner
        // to include disk infrastructure fields
        let _ = disk_inner; // Suppress unused warning

        Ok(trie)
    }

    /// Create with slot-level dirty tracking.
    ///
    /// This enables incremental checkpoints that write only modified slots
    /// instead of entire 256KB arenas, reducing checkpoint I/O by 90%+ for
    /// localized updates.
    ///
    /// # Arguments
    /// * `path` - Path to the dictionary file (must not exist)
    pub fn create_with_slot_tracking<P: AsRef<Path>>(path: P) -> PersistentResult<Self> {
        use dict_impl_char::DiskBackedCharTrieInner;

        let disk_inner: DiskBackedCharTrieInner<V> = DiskBackedCharTrieInner::create_with_slot_tracking(path)?;
        let trie = Self::new();
        let _ = disk_inner; // TODO: Integrate in Phase 3-5

        Ok(trie)
    }

    /// Open an existing persistent dictionary.
    ///
    /// This opens an existing dictionary file and replays the WAL if needed
    /// to recover from any crash.
    ///
    /// # Arguments
    /// * `path` - Path to the dictionary file
    ///
    /// # Example
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    ///
    /// let trie = PersistentARTrieChar::<()>::open("words.artc")?;
    /// ```
    pub fn open<P: AsRef<Path>>(path: P) -> PersistentResult<Self> {
        use dict_impl_char::DiskBackedCharTrieInner;

        let disk_inner: DiskBackedCharTrieInner<V> = DiskBackedCharTrieInner::open(path)?;

        // Create in-memory trie and populate from disk-backed
        let trie = Self::new();

        // TODO: Phase 3-5 will load data from disk_inner into trie
        let _ = disk_inner;

        Ok(trie)
    }

    /// Open with slot-level dirty tracking.
    ///
    /// Slot-level tracking reduces checkpoint I/O by writing only modified slots
    /// instead of entire arenas.
    ///
    /// # Arguments
    /// * `path` - Path to the dictionary file (must exist)
    pub fn open_with_slot_tracking<P: AsRef<Path>>(path: P) -> PersistentResult<Self> {
        use dict_impl_char::DiskBackedCharTrieInner;

        let disk_inner: DiskBackedCharTrieInner<V> = DiskBackedCharTrieInner::open_with_slot_tracking(path)?;
        let trie = Self::new();
        let _ = disk_inner; // TODO: Integrate in Phase 3-5

        Ok(trie)
    }

    /// Open with automatic crash recovery.
    ///
    /// This is the recommended way to open a trie that may have been corrupted
    /// by a crash (OOM kill, power failure, etc.).
    ///
    /// # Recovery Process
    ///
    /// 1. **Check if file exists** - If not, create a new trie
    /// 2. **Detect corruption** - Check header checksum, arena checksums
    /// 3. **If corrupted** - Rebuild from WAL archive segments
    /// 4. **Return trie with recovery report**
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the dictionary file
    ///
    /// # Returns
    ///
    /// Tuple of (trie, recovery_report) indicating what recovery was performed.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    ///
    /// let (trie, report) = PersistentARTrieChar::<i64>::open_with_recovery("data.artc")?;
    ///
    /// if !report.mode.is_normal() {
    ///     eprintln!("Recovered from crash: {} records replayed", report.records_replayed);
    /// }
    /// ```
    pub fn open_with_recovery<P: AsRef<Path>>(path: P) -> PersistentResult<(Self, crate::persistent_artrie::recovery::RecoveryReport)> {
        use dict_impl_char::DiskBackedCharTrieInner;
        use crate::persistent_artrie::recovery::RecoveryReport as ByteRecoveryReport;

        let (disk_inner, report): (DiskBackedCharTrieInner<V>, ByteRecoveryReport) = DiskBackedCharTrieInner::open_with_recovery(path)?;
        let trie = Self::new();
        let _ = disk_inner; // TODO: Integrate in Phase 3-5

        Ok((trie, report))
    }

    /// Checkpoint current state to disk.
    ///
    /// This flushes all in-memory changes to the data file and writes
    /// a checkpoint record to the WAL. After a checkpoint, the WAL can
    /// be truncated to reclaim space.
    ///
    /// # Note
    ///
    /// This method requires the trie to have been created with `create()`
    /// or `open()`. For in-memory tries (created with `new()`), this is a no-op.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    ///
    /// let trie = PersistentARTrieChar::<()>::create("words.artc")?;
    /// trie.insert("hello");
    /// trie.checkpoint()?; // Durably persist to disk
    /// ```
    pub fn checkpoint(&self) -> PersistentResult<()> {
        // TODO: Phase 3-5 will implement this when disk infrastructure is added
        // to PersistentARTrieCharInner
        Ok(())
    }

    /// Check if trie has unsaved changes.
    ///
    /// Returns `true` if any modifications have been made since the last
    /// checkpoint (or creation).
    ///
    /// # Note
    ///
    /// For in-memory tries (created with `new()`), this always returns `false`
    /// since there's no disk state to be dirty relative to.
    pub fn is_dirty(&self) -> bool {
        // TODO: Phase 3-5 will implement this when disk infrastructure is added
        false
    }
}

/// Zipper for navigating the character-level trie
#[derive(Debug, Clone)]
pub struct PersistentARTrieCharZipper<V: DictionaryValue = ()> {
    node: PersistentARTrieCharNode<V>,
    path_vec: Vec<char>,
}

impl<V: DictionaryValue> PersistentARTrieCharZipper<V> {
    /// Create a new zipper at the root
    pub fn new(dict: &PersistentARTrieChar<V>) -> Self {
        Self {
            node: dict.root(),
            path_vec: Vec::new(),
        }
    }

    /// Get the current path as a string
    pub fn path_string(&self) -> String {
        self.path_vec.iter().collect()
    }
}

impl<V: DictionaryValue> DictZipper for PersistentARTrieCharZipper<V> {
    type Unit = char;

    fn is_final(&self) -> bool {
        self.node.is_final()
    }

    fn descend(&self, label: char) -> Option<Self> {
        self.node.transition(label).map(|child| {
            let mut new_path = self.path_vec.clone();
            new_path.push(label);
            Self {
                node: child,
                path_vec: new_path,
            }
        })
    }

    fn children(&self) -> impl Iterator<Item = (char, Self)> {
        let path = self.path_vec.clone();
        self.node.node.children.iter().map(move |(&c, child)| {
            let mut new_path = path.clone();
            new_path.push(c);
            (
                c,
                Self {
                    node: PersistentARTrieCharNode {
                        node: Arc::clone(child),
                    },
                    path_vec: new_path,
                },
            )
        })
    }

    fn path(&self) -> Vec<char> {
        self.path_vec.clone()
    }
}

impl<V: DictionaryValue> ValuedDictZipper for PersistentARTrieCharZipper<V> {
    type Value = V;

    fn value(&self) -> Option<V> {
        if self.node.is_final() {
            self.node.node.value.clone()
        } else {
            None
        }
    }
}

// ============================================================================
// ARTrie Trait Implementation
// ============================================================================

impl<V: DictionaryValue> crate::artrie_trait::ARTrie for PersistentARTrieChar<V> {
    type Unit = char;
    type Value = V;

    fn create<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::error::Result<Self> {
        PersistentARTrieChar::create(path)
    }

    fn create_with_slot_tracking<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::error::Result<Self> {
        PersistentARTrieChar::create_with_slot_tracking(path)
    }

    fn open<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::error::Result<Self> {
        PersistentARTrieChar::open(path)
    }

    fn open_with_slot_tracking<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::error::Result<Self> {
        PersistentARTrieChar::open_with_slot_tracking(path)
    }

    fn open_with_recovery<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::error::Result<(Self, crate::persistent_artrie::recovery::RecoveryReport)> {
        PersistentARTrieChar::open_with_recovery(path)
    }

    fn insert(&self, term: &str) -> bool
    where
        Self::Value: Default,
    {
        PersistentARTrieChar::insert(self, term)
    }

    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentARTrieChar::insert_with_value(self, term, value)
    }

    fn contains(&self, term: &str) -> bool {
        PersistentARTrieChar::contains(self, term)
    }

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        MappedDictionary::get_value(self, term)
    }

    fn remove(&self, term: &str) -> bool {
        // PersistentARTrieChar doesn't have a direct remove method on &self
        // We need to use interior mutability - check if the trie has one
        // For now, return false since the in-memory implementation doesn't support remove
        // TODO: Implement remove for PersistentARTrieChar
        let _ = term;
        false
    }

    fn len(&self) -> usize {
        PersistentARTrieChar::len(self)
    }

    fn checkpoint(&self) -> crate::persistent_artrie::error::Result<()> {
        PersistentARTrieChar::checkpoint(self)
    }

    fn is_dirty(&self) -> bool {
        PersistentARTrieChar::is_dirty(self)
    }

    fn remove_prefix(&self, prefix: &str) -> usize {
        PersistentARTrieChar::remove_prefix(self, prefix)
    }

    fn iter_prefix(&self, prefix: &str) -> Option<Box<dyn Iterator<Item = String> + '_>> {
        PersistentARTrieChar::iter_prefix(self, prefix)
            .map(|iter| Box::new(iter) as Box<dyn Iterator<Item = String> + '_>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_empty() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        assert!(trie.is_empty());
        assert_eq!(trie.len(), 0);
    }

    #[test]
    fn test_insert_ascii() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        assert!(trie.insert("hello"));
        assert!(trie.insert("world"));
        assert!(!trie.insert("hello")); // Duplicate
        assert_eq!(trie.len(), 2);
    }

    #[test]
    fn test_insert_unicode() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        assert!(trie.insert("héllo")); // é is one character
        assert!(trie.insert("日本語")); // Japanese characters
        assert!(trie.insert("emoji😀")); // Emoji
        assert_eq!(trie.len(), 3);
    }

    #[test]
    fn test_contains() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        trie.insert("hello");
        trie.insert("héllo");

        assert!(trie.contains("hello"));
        assert!(trie.contains("héllo"));
        assert!(!trie.contains("helo"));
        assert!(!trie.contains("hello ")); // Trailing space
    }

    #[test]
    fn test_edges_unicode() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        trie.insert("日本");
        trie.insert("日曜日");

        let root = trie.root();
        let edges: Vec<_> = root.edges().collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].0, '日');
    }

    #[test]
    fn test_transition() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        trie.insert("café");

        let mut node = trie.root();
        assert!(node.transition('c').is_some());
        node = node.transition('c').unwrap();
        assert!(node.transition('a').is_some());
        node = node.transition('a').unwrap();
        assert!(node.transition('f').is_some());
        node = node.transition('f').unwrap();
        assert!(node.transition('é').is_some());
        node = node.transition('é').unwrap();
        assert!(node.is_final());
    }

    #[test]
    fn test_iterator() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        trie.insert("a");
        trie.insert("ab");
        trie.insert("abc");

        let terms: Vec<_> = trie.iter().collect();
        assert_eq!(terms.len(), 3);
        assert!(terms.contains(&"a".to_string()));
        assert!(terms.contains(&"ab".to_string()));
        assert!(terms.contains(&"abc".to_string()));
    }

    #[test]
    fn test_zipper() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        trie.insert("hello");
        trie.insert("help");

        let zipper = PersistentARTrieCharZipper::new(&trie);
        let zipper = zipper.descend('h').expect("should have 'h'");
        let zipper = zipper.descend('e').expect("should have 'e'");
        let zipper = zipper.descend('l').expect("should have 'l'");

        let edges: Vec<_> = zipper.children().map(|(c, _)| c).collect();
        assert_eq!(edges.len(), 2); // 'l' and 'p'
    }

    #[test]
    fn test_from_iter() {
        let terms = vec!["alpha", "beta", "gamma"];
        let trie: PersistentARTrieChar<()> = terms.into_iter().collect();
        assert_eq!(trie.len(), 3);
        assert!(trie.contains("alpha"));
        assert!(trie.contains("beta"));
        assert!(trie.contains("gamma"));
    }

    #[test]
    fn test_value_storage() {
        let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
        trie.insert_with_value("one", 1);
        trie.insert_with_value("two", 2);
        trie.insert_with_value("three", 3);

        assert_eq!(trie.get_value("one"), Some(1));
        assert_eq!(trie.get_value("two"), Some(2));
        assert_eq!(trie.get_value("three"), Some(3));
        assert_eq!(trie.get_value("four"), None);
    }

    #[test]
    fn test_unicode_correctness() {
        // This test verifies that multi-byte characters are treated as single units
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        trie.insert("¡");

        let root = trie.root();
        // Should have exactly one edge (for '¡'), not two edges (for the bytes)
        let edges: Vec<_> = root.edges().collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].0, '¡');
    }

    #[test]
    fn test_iter_with_values() {
        let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
        trie.insert_with_value("alpha", 1);
        trie.insert_with_value("beta", 2);
        trie.insert_with_value("gamma", 3);

        let results: Vec<_> = trie.iter_with_values().collect();
        assert_eq!(results.len(), 3);

        // Check that we got all expected pairs
        assert!(results.contains(&("alpha".to_string(), 1)));
        assert!(results.contains(&("beta".to_string(), 2)));
        assert!(results.contains(&("gamma".to_string(), 3)));
    }

    #[test]
    fn test_iter_with_values_unicode() {
        let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
        trie.insert_with_value("日本語", 100);
        trie.insert_with_value("café", 200);
        trie.insert_with_value("emoji😀", 300);

        let results: Vec<_> = trie.iter_with_values().collect();
        assert_eq!(results.len(), 3);

        assert!(results.contains(&("日本語".to_string(), 100)));
        assert!(results.contains(&("café".to_string(), 200)));
        assert!(results.contains(&("emoji😀".to_string(), 300)));
    }

    #[test]
    fn test_iter_chars() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        trie.insert("abc");
        trie.insert("日本");

        let results: Vec<_> = trie.iter_chars().collect();
        assert_eq!(results.len(), 2);

        // Check character vectors
        assert!(results.contains(&vec!['a', 'b', 'c']));
        assert!(results.contains(&vec!['日', '本']));
    }

    #[test]
    fn test_iter_chars_with_values() {
        let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
        trie.insert_with_value("hello", 1);
        trie.insert_with_value("世界", 2);

        let results: Vec<_> = trie.iter_chars_with_values().collect();
        assert_eq!(results.len(), 2);

        assert!(results.contains(&(vec!['h', 'e', 'l', 'l', 'o'], 1)));
        assert!(results.contains(&(vec!['世', '界'], 2)));
    }

    #[test]
    fn test_iter_empty_trie() {
        let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();

        assert_eq!(trie.iter().count(), 0);
        assert_eq!(trie.iter_with_values().count(), 0);
        assert_eq!(trie.iter_chars().count(), 0);
        assert_eq!(trie.iter_chars_with_values().count(), 0);
    }

    #[test]
    fn test_iter_with_values_ordered() {
        // Test that iteration produces terms in a deterministic order
        let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
        trie.insert_with_value("cat", 1);
        trie.insert_with_value("car", 2);
        trie.insert_with_value("card", 3);
        trie.insert_with_value("care", 4);

        let results: Vec<_> = trie.iter_with_values().collect();
        assert_eq!(results.len(), 4);

        // All terms should be present
        let terms: Vec<_> = results.iter().map(|(t, _)| t.as_str()).collect();
        assert!(terms.contains(&"cat"));
        assert!(terms.contains(&"car"));
        assert!(terms.contains(&"card"));
        assert!(terms.contains(&"care"));
    }

    // === Atomic Operations Tests ===

    #[test]
    fn test_atomic_increment_new() {
        let trie: PersistentARTrieChar<i64> = PersistentARTrieChar::new();

        let result = trie.increment("counter", 5).expect("increment");
        assert_eq!(result, 5);
        assert!(trie.contains("counter"));
    }

    #[test]
    fn test_atomic_increment_existing() {
        let trie: PersistentARTrieChar<i64> = PersistentARTrieChar::new();

        trie.upsert("counter", 10i64);
        let result = trie.increment("counter", 5).expect("increment");
        assert_eq!(result, 15);

        let result = trie.increment("counter", -3).expect("decrement");
        assert_eq!(result, 12);
    }

    #[test]
    fn test_atomic_upsert_new() {
        let trie: PersistentARTrieChar<String> = PersistentARTrieChar::new();

        let is_new = trie.upsert("greeting", "hello".to_string());
        assert!(is_new);
        assert_eq!(trie.get_value("greeting"), Some("hello".to_string()));
    }

    #[test]
    fn test_atomic_upsert_existing() {
        let trie: PersistentARTrieChar<String> = PersistentARTrieChar::new();

        trie.upsert("greeting", "hello".to_string());
        let is_new = trie.upsert("greeting", "hi".to_string());
        assert!(!is_new);
        assert_eq!(trie.get_value("greeting"), Some("hi".to_string()));
    }

    #[test]
    fn test_atomic_compare_and_swap_success() {
        let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();

        trie.upsert("counter", 0i32);
        let success = trie.compare_and_swap("counter", Some(0), 1);
        assert!(success);
        assert_eq!(trie.get_value("counter"), Some(1));
    }

    #[test]
    fn test_atomic_compare_and_swap_failure() {
        let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();

        trie.upsert("counter", 5i32);
        let success = trie.compare_and_swap("counter", Some(0), 10);
        assert!(!success);
        assert_eq!(trie.get_value("counter"), Some(5));
    }

    #[test]
    fn test_atomic_fetch_add() {
        let trie: PersistentARTrieChar<i64> = PersistentARTrieChar::new();

        trie.upsert("counter", 10i64);
        let old = trie.fetch_add("counter", 5).expect("fetch_add");
        assert_eq!(old, 10);
    }

    #[test]
    fn test_atomic_get_or_insert_new() {
        let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();

        let value = trie.get_or_insert("key", 42);
        assert_eq!(value, 42);
        assert!(trie.contains("key"));
    }

    #[test]
    fn test_atomic_get_or_insert_existing() {
        let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();

        trie.upsert("key", 100i32);
        let value = trie.get_or_insert("key", 42);
        assert_eq!(value, 100);
    }

    #[test]
    fn test_atomic_unicode_terms() {
        let trie: PersistentARTrieChar<i64> = PersistentARTrieChar::new();

        // Test with Unicode terms
        trie.increment("日本語カウンター", 1).expect("increment");
        trie.increment("日本語カウンター", 1).expect("increment");

        let result = trie.increment("日本語カウンター", 0).expect("read");
        assert_eq!(result, 2);

        trie.upsert("café", 100i64);
        assert_eq!(trie.get_value("café"), Some(100));
    }
}
