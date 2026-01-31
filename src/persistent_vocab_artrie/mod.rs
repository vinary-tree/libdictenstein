//! Persistent Vocabulary ARTrie with Parent Pointers for O(k) Reverse Lookups.
//!
//! This module provides [`PersistentVocabARTrie`], a specialized UTF-8 vocabulary trie
//! with parent pointers that enable efficient bidirectional lookups:
//!
//! - **Forward lookup** (term → index): O(k) via trie traversal
//! - **Reverse lookup** (index → term): O(1) cache hit, O(k) via parent backtracking
//!
//! # Design Rationale
//!
//! When using `IndexedVocabulary::with_backend(PersistentARTrieChar::open(path)?)`,
//! the reverse lookup table (`Vec<String>`) starts empty and is never populated from disk.
//! Adding parent pointers to the base `CharTrieNodeInner<V>` would waste 12 bytes per node
//! in n-gram tries that don't need reverse lookup.
//!
//! `PersistentVocabARTrie` solves this by creating a specialized vocabulary trie that:
//! 1. Stores parent pointers only where needed (vocabulary-specific)
//! 2. Uses a memory-mapped reverse index for O(1) node location
//! 3. Caches hot lookups with an LRU cache
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    PersistentVocabARTrie                     │
//! ├─────────────────────────────────────────────────────────────┤
//! │  VocabTrieNode (wrapper with parent pointers)               │
//! │  ┌───────────────────────────────────────────────────────┐  │
//! │  │  inner: CharNode        // Reuses existing ART nodes  │  │
//! │  │  parent: NodeRef        // Parent for backtracking    │  │
//! │  │  parent_edge: u32       // Edge label from parent     │  │
//! │  │  value: Option<u64>     // Vocabulary index (inline)  │  │
//! │  └───────────────────────────────────────────────────────┘  │
//! │                                                              │
//! │  Forward Lookup: term → u64 index (O(k) via trie traversal) │
//! │  Reverse Lookup: u64 index → term                           │
//! │    1. LRU Cache check (O(1) hit)                            │
//! │    2. Mmap'd index: u64 → NodeRef (O(1))                    │
//! │    3. Backtrack parent pointers (O(k))                      │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # File Layout
//!
//! ```text
//! vocabulary.vocab           # Main trie (nodes with parent pointers)
//! vocabulary.vocab.wal       # Write-ahead log (future)
//! vocabulary.vocab.idx       # Mmap'd reverse index (u64 → NodeRef)
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
//!
//! // Create a new vocabulary
//! let vocab = PersistentVocabARTrie::create("vocab.vocab")?;
//!
//! // Insert terms (auto-assigns indices)
//! let idx1 = vocab.insert("hello"); // Returns 0
//! let idx2 = vocab.insert("world"); // Returns 1
//!
//! // Forward lookup: term → index
//! assert_eq!(vocab.get_index("hello"), Some(0));
//! assert_eq!(vocab.get_index("world"), Some(1));
//!
//! // Reverse lookup: index → term (O(1) cache hit or O(k) backtracking)
//! assert_eq!(vocab.get_term(0), Some("hello".to_string()));
//! assert_eq!(vocab.get_term(1), Some("world".to_string()));
//!
//! // Checkpoint to disk
//! vocab.checkpoint()?;
//!
//! // Reopen later
//! let vocab = PersistentVocabARTrie::open("vocab.vocab")?;
//! assert_eq!(vocab.get_term(0), Some("hello".to_string()));
//! ```
//!
//! # Performance
//!
//! | Operation | Complexity | Notes |
//! |-----------|------------|-------|
//! | Forward lookup | O(k) | k = term length |
//! | Reverse lookup (hot) | O(1) | LRU cache hit |
//! | Reverse lookup (cold) | O(k) | Parent backtracking |
//! | Insert | O(k) | Sets parent atomically |
//!
//! # Memory Overhead
//!
//! Per-node overhead vs base `CharTrieNodeInner`:
//! - `parent: NodeRef` = 8 bytes
//! - `parent_edge: u32` = 4 bytes
//! - Total: **12 bytes per node**
//!
//! For 100K terms with avg depth 8 (~800K nodes): ~9.6 MB additional

// Core types
pub mod types;

// Reverse index infrastructure
pub mod reverse_index;

// LRU cache for hot lookups
pub mod reverse_cache;

// Serialization with parent pointers
pub mod serialization;

// Disk-backed implementation
pub mod dict_impl;

// Re-export main types
pub use types::{
    VocabTrieNode, VocabTrieRoot, VocabTrieFileHeader,
    NodeRef, // Re-export from persistent_artrie_char
    VOCAB_TRIE_MAGIC, VOCAB_FILE_HEADER_SIZE, VOCAB_HEADER_VERSION_V1,
    DEFAULT_VOCAB_BUFFER_POOL_SIZE, DEFAULT_REVERSE_CACHE_SIZE,
    FLAG_HAS_PARENT_POINTER,
};

pub use reverse_index::{
    VocabReverseIndex, ReverseIndexHeader,
    REVERSE_INDEX_MAGIC, REVERSE_INDEX_HEADER_SIZE,
};

pub use reverse_cache::{VocabReverseCache, CacheStats};

pub use serialization::{
    SerializedVocabNodeHeader,
    serialize_vocab_node, deserialize_vocab_node,
    vocab_serialized_size,
    VOCAB_NODE_MAGIC, VOCAB_FORMAT_VERSION, VOCAB_SERIALIZED_HEADER_SIZE,
    FLAG_HAS_VALUE,
};

pub use dict_impl::{
    DiskBackedVocabTrieInner, SharedVocabTrie, PersistentVocabARTrie,
};

// ============================================================================
// Trait Implementations
// ============================================================================

use crate::persistent_artrie::error::Result;
use crate::persistent_artrie::recovery::RecoveryReport;
use crate::{Dictionary, DictionaryNode, MappedDictionary, MutableMappedDictionary};
use crate::bijective::BijectiveDictionary;
use std::path::Path;

// Dictionary trait implementation
impl Dictionary for PersistentVocabARTrie {
    type Node = VocabTrieNodeRef;

    fn root(&self) -> Self::Node {
        VocabTrieNodeRef {
            // Placeholder - full impl would return actual root
            _phantom: std::marker::PhantomData,
        }
    }

    fn contains(&self, term: &str) -> bool {
        PersistentVocabARTrie::contains(self, term)
    }

    fn len(&self) -> Option<usize> {
        Some(PersistentVocabARTrie::len(self))
    }
}

/// Node reference for Dictionary trait implementation.
#[derive(Clone)]
pub struct VocabTrieNodeRef {
    _phantom: std::marker::PhantomData<()>,
}

impl DictionaryNode for VocabTrieNodeRef {
    type Unit = char;

    fn is_final(&self) -> bool {
        false // Placeholder
    }

    fn transition(&self, _label: char) -> Option<Self> {
        None // Placeholder
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (char, Self)> + '_> {
        Box::new(std::iter::empty())
    }
}

// MappedDictionary trait implementation
impl MappedDictionary for PersistentVocabARTrie {
    type Value = u64;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        self.get_index(term)
    }
}

// MutableMappedDictionary trait implementation
impl MutableMappedDictionary for PersistentVocabARTrie {
    fn insert_with_value(&self, term: &str, _value: Self::Value) -> bool {
        // Note: PersistentVocabARTrie auto-assigns indices, so we ignore the value
        // and check if the term already existed
        let existed = self.contains(term);
        self.insert(term);
        !existed
    }

    fn union_with<F>(&self, _other: &Self, _merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        // Not implemented for vocabulary tries
        0
    }

    fn update_or_insert<F>(&self, term: &str, _default_value: Self::Value, _update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        let existed = self.contains(term);
        self.insert(term);
        !existed
    }
}

// BijectiveDictionary trait implementation
impl BijectiveDictionary for PersistentVocabARTrie {
    fn get_term(&self, value: &Self::Value) -> Option<&str> {
        // Note: BijectiveDictionary expects &str, but we return owned String
        // This is a limitation of the trait design - we can't return a reference
        // to data that's reconstructed on the fly.
        // For now, we return None and users should use get_term() directly.
        None
    }

    fn contains_value(&self, value: &Self::Value) -> bool {
        self.contains_index(*value)
    }

    fn bijection_len(&self) -> usize {
        self.len()
    }
}

// ARTrie trait implementation
impl crate::artrie_trait::ARTrie for PersistentVocabARTrie {
    type Unit = char;
    type Value = u64;

    fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentVocabARTrie::create(path)
    }

    fn create_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        // Slot tracking is inherent in this implementation
        PersistentVocabARTrie::create(path)
    }

    fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentVocabARTrie::open(path)
    }

    fn open_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentVocabARTrie::open(path)
    }

    fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        PersistentVocabARTrie::open_with_recovery(path)
    }

    fn insert(&self, term: &str) -> bool
    where
        Self::Value: Default,
    {
        let existed = self.contains(term);
        PersistentVocabARTrie::insert(self, term);
        !existed
    }

    fn insert_with_value(&self, term: &str, _value: Self::Value) -> bool {
        let existed = self.contains(term);
        PersistentVocabARTrie::insert(self, term);
        !existed
    }

    fn contains(&self, term: &str) -> bool {
        PersistentVocabARTrie::contains(self, term)
    }

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        self.get_index(term)
    }

    fn remove(&self, _term: &str) -> bool {
        // Removal not supported for vocabulary tries
        false
    }

    fn len(&self) -> usize {
        PersistentVocabARTrie::len(self)
    }

    fn checkpoint(&self) -> Result<()> {
        PersistentVocabARTrie::checkpoint(self)
    }

    fn is_dirty(&self) -> bool {
        PersistentVocabARTrie::is_dirty(self)
    }

    fn remove_prefix(&self, _prefix: &str) -> usize {
        // Prefix removal not supported for vocabulary tries
        0
    }

    fn iter_prefix(&self, _prefix: &str) -> Option<Box<dyn Iterator<Item = String> + '_>> {
        // Prefix iteration not yet implemented
        None
    }
}

// ============================================================================
// Type Aliases for IndexedVocabulary Integration
// ============================================================================

/// Type alias for IndexedVocabulary using PersistentVocabARTrie backend.
///
/// This provides a drop-in replacement for `IndexedVocabularyART` that properly
/// populates the reverse lookup table on disk reopen.
///
/// # Example
///
/// ```rust,ignore
/// use libdictenstein::persistent_vocab_artrie::IndexedVocabularyPersistent;
///
/// // Create new vocabulary
/// let vocab = IndexedVocabularyPersistent::create("vocab.vocab")?;
/// vocab.insert("hello"); // Returns 0
///
/// // Checkpoint and reopen
/// vocab.checkpoint()?;
/// let vocab = IndexedVocabularyPersistent::open("vocab.vocab")?;
///
/// // Reverse lookup works immediately!
/// assert_eq!(vocab.get_term(0), Some("hello".to_string()));
/// ```
pub type IndexedVocabularyPersistent = PersistentVocabARTrie;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_vocab_trie_basic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).unwrap();

        // Insert
        let idx1 = vocab.insert("apple");
        let idx2 = vocab.insert("banana");
        let idx3 = vocab.insert("cherry");

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 1);
        assert_eq!(idx3, 2);
        assert_eq!(vocab.len(), 3);

        // Forward lookup
        assert_eq!(vocab.get_index("apple"), Some(0));
        assert_eq!(vocab.get_index("banana"), Some(1));
        assert_eq!(vocab.get_index("cherry"), Some(2));
        assert_eq!(vocab.get_index("durian"), None);

        // Reverse lookup
        assert_eq!(vocab.get_term(0), Some("apple".to_string()));
        assert_eq!(vocab.get_term(1), Some("banana".to_string()));
        assert_eq!(vocab.get_term(2), Some("cherry".to_string()));
        assert_eq!(vocab.get_term(999), None);
    }

    #[test]
    fn test_vocab_trie_unicode() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).unwrap();

        vocab.insert("日本語");
        vocab.insert("中文");
        vocab.insert("한글");
        vocab.insert("العربية");
        vocab.insert("emoji😀");

        assert_eq!(vocab.get_index("日本語"), Some(0));
        assert_eq!(vocab.get_index("中文"), Some(1));
        assert_eq!(vocab.get_index("한글"), Some(2));
        assert_eq!(vocab.get_index("العربية"), Some(3));
        assert_eq!(vocab.get_index("emoji😀"), Some(4));

        assert_eq!(vocab.get_term(0), Some("日本語".to_string()));
        assert_eq!(vocab.get_term(4), Some("emoji😀".to_string()));
    }

    #[test]
    fn test_vocab_trie_custom_start() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        // Reserve 0-9 for special tokens
        let vocab = PersistentVocabARTrie::create_with_start_index(&path, 10).unwrap();

        let idx1 = vocab.insert("first");
        let idx2 = vocab.insert("second");

        assert_eq!(idx1, 10);
        assert_eq!(idx2, 11);
        assert_eq!(vocab.start_index(), 10);
    }

    #[test]
    fn test_vocab_trie_idempotent_insert() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).unwrap();

        let idx1 = vocab.insert("duplicate");
        let idx2 = vocab.insert("duplicate");
        let idx3 = vocab.insert("duplicate");

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 0);
        assert_eq!(idx3, 0);
        assert_eq!(vocab.len(), 1);
    }

    #[test]
    fn test_vocab_trie_traits() {
        use crate::Dictionary;
        use crate::MappedDictionary;

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).unwrap();
        vocab.insert("test");

        // Dictionary trait
        assert!(Dictionary::contains(&vocab, "test"));
        assert!(!Dictionary::contains(&vocab, "missing"));
        assert_eq!(Dictionary::len(&vocab), Some(1));

        // MappedDictionary trait
        assert_eq!(MappedDictionary::get_value(&vocab, "test"), Some(0));
        assert_eq!(MappedDictionary::get_value(&vocab, "missing"), None);
    }

    #[test]
    fn test_vocab_trie_artrie_trait() {
        use crate::artrie_trait::ARTrie;

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).unwrap();

        // ARTrie trait methods
        assert!(ARTrie::insert(&vocab, "hello"));
        assert!(!ARTrie::insert(&vocab, "hello")); // Already exists
        assert!(ARTrie::contains(&vocab, "hello"));
        assert_eq!(ARTrie::get_value(&vocab, "hello"), Some(0));
        assert_eq!(ARTrie::len(&vocab), 1);
    }
}
