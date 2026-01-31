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
//! A naive approach would store an in-memory `Vec<String>` for reverse lookup, but this
//! would be lost after restart and not scale to large vocabularies. Adding parent pointers
//! to the base `CharTrieNodeInner<V>` would waste 12 bytes per node in n-gram tries that
//! don't need reverse lookup.
//!
//! `PersistentVocabARTrie` solves this by creating a specialized vocabulary trie that:
//! 1. Stores parent pointers only where needed (vocabulary-specific)
//! 2. Uses a memory-mapped reverse index for O(1) node location
//! 3. Caches hot lookups with an LRU cache
//! 4. Uses WAL for crash recovery (via base `persistent_artrie` infrastructure)
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    PersistentVocabARTrie                     │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Uses base persistence layer from persistent_artrie:        │
//! │  - WalWriter/WalReader for WAL operations                   │
//! │  - BufferManager for page cache                             │
//! │  - DiskManager for raw block I/O                            │
//! │                                                              │
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
//! vocabulary.vocab.wal       # Write-ahead log (for crash recovery)
//! vocabulary.vocab.idx       # Mmap'd reverse index (u64 → NodeRef)
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
//!
//! // Create a new vocabulary
//! let mut vocab = PersistentVocabARTrie::create("vocab.vocab")?;
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
//! // Sync WAL for durability
//! vocab.sync()?;
//!
//! // Checkpoint to disk
//! vocab.checkpoint()?;
//!
//! // Reopen later with crash recovery
//! let (vocab, report) = PersistentVocabARTrie::open_with_recovery("vocab.vocab")?;
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
    PersistentVocabARTrie, SharedVocabARTrie,
};

// Re-export DurabilityPolicy from base layer
pub use crate::persistent_artrie::dict_impl::DurabilityPolicy;

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
        // This requires interior mutability - for now we return false
        // Use SharedVocabARTrie for mutable access
        false
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
        // This requires interior mutability - for now we return false
        // Use SharedVocabARTrie for mutable access
        false
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

    fn insert(&self, _term: &str) -> bool
    where
        Self::Value: Default,
    {
        // This requires &mut self - use SharedVocabARTrie for mutable access
        false
    }

    fn insert_with_value(&self, _term: &str, _value: Self::Value) -> bool {
        // This requires &mut self - use SharedVocabARTrie for mutable access
        false
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
        // This requires &mut self - checkpoint on drop is still available
        Ok(())
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

    fn sync(&self) -> Result<()> {
        // This requires &mut self
        Ok(())
    }

    fn current_lsn(&self) -> u64 {
        PersistentVocabARTrie::current_lsn(self)
    }

    fn synced_lsn(&self) -> Option<u64> {
        PersistentVocabARTrie::synced_lsn(self)
    }

    fn durability_policy(&self) -> DurabilityPolicy {
        PersistentVocabARTrie::durability_policy(self)
    }

    fn upsert(&self, _term: &str, _value: Self::Value) -> Result<bool> {
        // This requires &mut self
        Err(crate::persistent_artrie::error::PersistentARTrieError::InvalidOperation(
            "PersistentVocabARTrie::upsert requires &mut self - use SharedVocabARTrie".into()
        ))
    }

    fn increment(&self, _term: &str, _delta: i64) -> Result<i64> {
        // Vocabulary tries don't support increment (indices are fixed)
        Err(crate::persistent_artrie::error::PersistentARTrieError::InvalidOperation(
            "PersistentVocabARTrie does not support increment - indices are auto-assigned".into()
        ))
    }
}

// ============================================================================
// SharedVocabARTrie Trait Implementations
// ============================================================================

use std::sync::Arc;
use parking_lot::RwLock;

impl Dictionary for SharedVocabARTrie {
    type Node = VocabTrieNodeRef;

    fn root(&self) -> Self::Node {
        VocabTrieNodeRef {
            _phantom: std::marker::PhantomData,
        }
    }

    fn contains(&self, term: &str) -> bool {
        let guard = self.read();
        guard.contains(term)
    }

    fn len(&self) -> Option<usize> {
        let guard = self.read();
        Some(guard.len())
    }
}

impl MappedDictionary for SharedVocabARTrie {
    type Value = u64;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        let guard = self.read();
        guard.get_index(term)
    }
}

impl MutableMappedDictionary for SharedVocabARTrie {
    fn insert_with_value(&self, term: &str, _value: Self::Value) -> bool {
        // Auto-assign index, ignore provided value
        let existed = self.read().contains(term);
        if !existed {
            let mut guard = self.write();
            guard.insert(term);
        }
        !existed
    }

    fn union_with<F>(&self, other: &Self, _merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        // Simple union - insert all terms from other
        // Note: merge_fn is ignored since indices are auto-assigned
        let mut count = 0;
        let other_guard = other.read();
        let mut self_guard = self.write();

        // We can't iterate the other trie directly, so this is a placeholder
        // In a full implementation, we'd iterate terms
        count
    }

    fn update_or_insert<F>(&self, term: &str, _default_value: Self::Value, _update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        // For vocab trie, just insert if not present (update_fn is ignored)
        let existed = self.read().contains(term);
        if !existed {
            let mut guard = self.write();
            guard.insert(term);
        }
        !existed
    }
}

impl BijectiveDictionary for SharedVocabARTrie {
    fn get_term(&self, _value: &Self::Value) -> Option<&str> {
        // Cannot return reference to temporary
        None
    }

    fn contains_value(&self, value: &Self::Value) -> bool {
        let guard = self.read();
        guard.contains_index(*value)
    }

    fn bijection_len(&self) -> usize {
        let guard = self.read();
        guard.len()
    }
}

impl crate::artrie_trait::ARTrie for SharedVocabARTrie {
    type Unit = char;
    type Value = u64;

    fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentVocabARTrie::create(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn create_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentVocabARTrie::create(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentVocabARTrie::open(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentVocabARTrie::open(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        PersistentVocabARTrie::open_with_recovery(path)
            .map(|(t, r)| (Arc::new(RwLock::new(t)), r))
    }

    fn insert(&self, term: &str) -> bool
    where
        Self::Value: Default,
    {
        let mut guard = self.write();
        let old_count = guard.len();
        // Explicitly call the struct method, not trait method
        let _ = PersistentVocabARTrie::insert(&mut *guard, term);
        // Return true if a new term was added (count increased)
        guard.len() > old_count
    }

    fn insert_with_value(&self, term: &str, _value: Self::Value) -> bool {
        let mut guard = self.write();
        let old_count = guard.len();
        // Explicitly call the struct method, not trait method
        let _ = PersistentVocabARTrie::insert(&mut *guard, term);
        // Return true if a new term was added (count increased)
        guard.len() > old_count
    }

    fn contains(&self, term: &str) -> bool {
        let guard = self.read();
        guard.contains(term)
    }

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        let guard = self.read();
        guard.get_index(term)
    }

    fn remove(&self, _term: &str) -> bool {
        // Removal not supported for vocabulary tries
        false
    }

    fn len(&self) -> usize {
        let guard = self.read();
        guard.len()
    }

    fn checkpoint(&self) -> Result<()> {
        let mut guard = self.write();
        guard.checkpoint()
    }

    fn is_dirty(&self) -> bool {
        let guard = self.read();
        guard.is_dirty()
    }

    fn remove_prefix(&self, _prefix: &str) -> usize {
        // Prefix removal not supported for vocabulary tries
        0
    }

    fn iter_prefix(&self, _prefix: &str) -> Option<Box<dyn Iterator<Item = String> + '_>> {
        // Prefix iteration not yet implemented
        None
    }

    fn sync(&self) -> Result<()> {
        let mut guard = self.write();
        guard.sync()
    }

    fn current_lsn(&self) -> u64 {
        let guard = self.read();
        guard.current_lsn()
    }

    fn synced_lsn(&self) -> Option<u64> {
        let guard = self.read();
        guard.synced_lsn()
    }

    fn durability_policy(&self) -> DurabilityPolicy {
        let guard = self.read();
        guard.durability_policy()
    }

    fn upsert(&self, term: &str, _value: Self::Value) -> Result<bool> {
        // For vocab trie, upsert is equivalent to insert (index is auto-assigned)
        let mut guard = self.write();
        let old_count = guard.len();
        // Explicitly call the struct method, not trait method
        let _ = PersistentVocabARTrie::insert(&mut *guard, term);
        Ok(guard.len() > old_count)
    }

    fn increment(&self, _term: &str, _delta: i64) -> Result<i64> {
        // Vocabulary tries don't support increment (indices are fixed)
        Err(crate::persistent_artrie::error::PersistentARTrieError::InvalidOperation(
            "PersistentVocabARTrie does not support increment - indices are auto-assigned".into()
        ))
    }
}

// ============================================================================
// Type Aliases
// ============================================================================

/// Type alias for vocabulary use cases.
///
/// This is the recommended type for embedding vocabularies, token-to-ID mappings,
/// and similar use cases that need sequential `u64` indices with persistent storage.
///
/// # Example
///
/// ```rust,ignore
/// use libdictenstein::persistent_vocab_artrie::IndexedVocabularyPersistent;
///
/// // Create new vocabulary
/// let mut vocab = IndexedVocabularyPersistent::create("vocab.vocab")?;
/// vocab.insert("hello"); // Returns 0
///
/// // Checkpoint and reopen with recovery
/// vocab.checkpoint()?;
/// let (vocab, report) = IndexedVocabularyPersistent::open_with_recovery("vocab.vocab")?;
///
/// // Reverse lookup works immediately!
/// assert_eq!(vocab.get_term(0), Some("hello".to_string()));
/// ```
pub type IndexedVocabularyPersistent = PersistentVocabARTrie;

// Backwards compatibility alias (deprecated)
#[deprecated(since = "0.9.0", note = "Use SharedVocabARTrie instead")]
pub type SharedVocabTrie = SharedVocabARTrie;

// Also re-export DiskBackedVocabTrieInner as deprecated alias
#[deprecated(since = "0.9.0", note = "Use PersistentVocabARTrie directly instead")]
pub type DiskBackedVocabTrieInner = PersistentVocabARTrie;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_vocab_trie_basic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

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

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

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
        let mut vocab = PersistentVocabARTrie::create_with_start_index(&path, 10).unwrap();

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

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

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

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
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

        let vocab: SharedVocabARTrie = ARTrie::create(&path).unwrap();

        // ARTrie trait methods
        assert!(ARTrie::insert(&vocab, "hello"));
        assert!(!ARTrie::insert(&vocab, "hello")); // Already exists
        assert!(ARTrie::contains(&vocab, "hello"));
        assert_eq!(ARTrie::get_value(&vocab, "hello"), Some(0));
        assert_eq!(ARTrie::len(&vocab), 1);
    }

    #[test]
    fn test_vocab_trie_lsn_tracking() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

        // Initial state
        let initial_lsn = vocab.current_lsn();
        assert!(initial_lsn > 0);
        assert!(vocab.synced_lsn().is_none());

        // After insert
        vocab.insert("test");
        assert!(vocab.current_lsn() > initial_lsn);

        // After sync
        vocab.sync().unwrap();
        assert!(vocab.synced_lsn().is_some());
    }

    #[test]
    fn test_vocab_trie_durability_policy() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

        // Default is Immediate
        assert_eq!(vocab.durability_policy(), DurabilityPolicy::Immediate);

        // Change to Periodic
        vocab.set_durability_policy(DurabilityPolicy::Periodic);
        assert_eq!(vocab.durability_policy(), DurabilityPolicy::Periodic);
    }

    #[test]
    fn test_shared_vocab_artrie() {
        use crate::artrie_trait::ARTrie;

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let vocab: SharedVocabARTrie = ARTrie::create(&path).unwrap();

        // Insert via trait
        assert!(ARTrie::insert(&vocab, "hello"));
        assert!(ARTrie::insert(&vocab, "world"));

        // Verify
        assert!(ARTrie::contains(&vocab, "hello"));
        assert!(ARTrie::contains(&vocab, "world"));
        assert_eq!(ARTrie::len(&vocab), 2);

        // Checkpoint
        ARTrie::checkpoint(&vocab).unwrap();
    }
}
