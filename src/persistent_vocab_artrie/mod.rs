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

// Lock-free concurrent vocabulary access
pub mod concurrent;

// Truly lock-free vocabulary using persistent data structures
pub mod lockfree;

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
    PersistentVocabARTrie, SharedVocabARTrie, VocabSyncHandle,
};

pub use concurrent::{
    ConcurrentMode, ConcurrentVocabARTrie, ConcurrentVocabStats,
};

pub use lockfree::{
    LockFreeVocab, LockFreeVocabStats, InsertResult,
};

// Re-export DurabilityPolicy from base layer
pub use crate::persistent_artrie::dict_impl::DurabilityPolicy;

// Re-export eviction types from byte-level implementation (shared)
pub use crate::persistent_artrie::eviction::{
    AccessTracker, DiskLocationRegistry, EvictionConfig, EvictionCoordinator,
    EvictionStats, EvictionUrgency, LruRegistry,
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
        // Get root children info from the trie
        let children = self.get_root_children();
        VocabTrieNodeRef::new(false, children, Vec::new())
    }

    fn contains(&self, term: &str) -> bool {
        PersistentVocabARTrie::contains(self, term)
    }

    fn len(&self) -> Option<usize> {
        Some(PersistentVocabARTrie::len(self))
    }
}

/// Node reference for Dictionary trait implementation.
///
/// This provides a snapshot-based node reference for navigating the trie
/// through the Dictionary trait. Note that since PersistentVocabARTrie
/// requires internal iteration for navigation, this implementation creates
/// shallow copies of node data rather than holding direct pointers.
///
/// For full trie operations, use the PersistentVocabARTrie methods directly.
#[derive(Clone)]
pub struct VocabTrieNodeRef {
    /// Whether this node is final
    is_final: bool,
    /// Children of this node (label -> whether child is final)
    /// This is a snapshot taken at the time of construction
    children: Vec<(char, bool)>,
    /// Path from root to this node (for reconstruction)
    path: Vec<char>,
}

impl VocabTrieNodeRef {
    /// Create a new node reference with snapshot data
    fn new(is_final: bool, children: Vec<(char, bool)>, path: Vec<char>) -> Self {
        Self { is_final, children, path }
    }

    /// Create a root node reference (placeholder - requires trie access for real data)
    fn placeholder() -> Self {
        Self {
            is_final: false,
            children: Vec::new(),
            path: Vec::new(),
        }
    }
}

impl DictionaryNode for VocabTrieNodeRef {
    type Unit = char;

    fn is_final(&self) -> bool {
        self.is_final
    }

    fn transition(&self, label: char) -> Option<Self> {
        // Check if we have this child
        for &(child_label, child_is_final) in &self.children {
            if child_label == label {
                // Create a new path with this label appended
                let mut new_path = self.path.clone();
                new_path.push(label);
                // Note: We can't get the child's children without trie access,
                // so this returns a node with no children info.
                // For full navigation, use PersistentVocabARTrie methods directly.
                return Some(VocabTrieNodeRef::new(child_is_final, Vec::new(), new_path));
            }
        }
        None
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (char, Self)> + '_> {
        let path = self.path.clone();
        let edges: Vec<_> = self.children.iter()
            .map(move |&(label, is_final)| {
                let mut new_path = path.clone();
                new_path.push(label);
                (label, VocabTrieNodeRef::new(is_final, Vec::new(), new_path))
            })
            .collect();
        Box::new(edges.into_iter())
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
//
// NOTE: The BijectiveDictionary trait's `get_term()` method expects `&str`,
// but PersistentVocabARTrie reconstructs terms on-the-fly via parent pointer
// backtracking. We cannot return a reference to data that doesn't exist in
// memory until reconstruction.
//
// WORKAROUND: Use the inherent method `PersistentVocabARTrie::get_term(index)`
// which returns `Option<String>`. For SharedVocabARTrie, access via:
//   `vocab.read().get_term(index)` or cache the result.
//
// The other BijectiveDictionary methods (contains_value, bijection_len) work
// correctly since they don't require returning references.
impl BijectiveDictionary for PersistentVocabARTrie {
    fn get_term(&self, _value: &Self::Value) -> Option<&str> {
        // Cannot return reference to on-the-fly reconstructed data.
        // Use inherent `self.get_term(index)` method which returns Option<String>.
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

    fn open_with_recovery_and_slot_tracking<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        let (trie, report) = PersistentVocabARTrie::open_with_recovery(path)?;
        trie.enable_slot_tracking_internal();
        Ok((trie, report))
    }

    fn enable_slot_tracking(&self) {
        // PersistentVocabARTrie requires &mut self - this is a no-op for the immutable reference.
        // Use SharedVocabARTrie for mutable access.
    }

    fn flush_sequential(&self) -> Result<()> {
        // PersistentVocabARTrie requires &mut self - this is a no-op for the immutable reference.
        // Use SharedVocabARTrie for mutable access.
        Ok(())
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

    fn iter_prefix(&self, prefix: &str) -> Option<Box<dyn Iterator<Item = String> + '_>> {
        // Check if prefix exists in trie or leads to any children
        let prefix_exists = if prefix.is_empty() {
            true
        } else {
            let chars: Vec<char> = prefix.chars().collect();
            self.get_index(prefix).is_some() || !self.get_children_at_path(&chars).is_empty()
        };

        if prefix_exists {
            // Collect terms to avoid lifetime issues with the prefix parameter
            let prefix_owned = prefix.to_string();
            let terms: Vec<String> = self.iter_terms_with_prefix(&prefix_owned).collect();
            Some(Box::new(terms.into_iter()))
        } else {
            None
        }
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
        let guard = self.read();
        let children = guard.get_root_children();
        VocabTrieNodeRef::new(false, children, Vec::new())
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
        // Note: merge_fn is ignored since vocabulary indices are auto-assigned
        // First collect terms from other to avoid holding locks simultaneously
        let other_terms: Vec<String> = {
            let other_guard = other.read();
            other_guard.iter_terms().collect()
        };

        let mut count = 0;
        let mut self_guard = self.write();
        for term in other_terms {
            if !self_guard.contains(&term) {
                self_guard.insert(&term);
                count += 1;
            }
        }
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
        // Cannot return reference to temporary reconstructed data.
        // Use: `vocab.read().get_term(index)` which returns Option<String>.
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

    fn open_with_recovery_and_slot_tracking<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        let (mut trie, report) = PersistentVocabARTrie::open_with_recovery(path)?;
        trie.enable_slot_tracking();
        Ok((Arc::new(RwLock::new(trie)), report))
    }

    fn enable_slot_tracking(&self) {
        self.write().enable_slot_tracking();
    }

    fn flush_sequential(&self) -> Result<()> {
        self.write().flush_sequential()
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

    fn iter_prefix(&self, prefix: &str) -> Option<Box<dyn Iterator<Item = String> + '_>> {
        // For SharedVocabARTrie, we need to collect terms to avoid holding lock
        // during iteration. This collects all matching terms upfront.
        let guard = self.read();
        let prefix_chars: Vec<char> = prefix.chars().collect();

        // Check if prefix exists
        let prefix_exists = if prefix.is_empty() {
            true
        } else {
            guard.get_index(prefix).is_some() || !guard.get_children_at_path(&prefix_chars).is_empty()
        };

        if prefix_exists {
            // Collect terms while holding lock, then return iterator over collected Vec
            let terms: Vec<String> = guard.iter_terms_with_prefix(prefix).collect();
            Some(Box::new(terms.into_iter()))
        } else {
            None
        }
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
// EvictableARTrie Trait Implementation (on SharedVocabARTrie)
// ============================================================================

impl crate::artrie_trait::EvictableARTrie for SharedVocabARTrie {
    fn enable_eviction(&mut self, config: crate::persistent_artrie::eviction::EvictionConfig) -> crate::persistent_artrie::error::Result<()> {
        use crate::persistent_artrie::error::PersistentARTrieError;

        config.validate().map_err(|e| PersistentARTrieError::internal(&e))?;

        let mut guard = self.write();

        // Check if eviction is already enabled
        if guard.eviction_coordinator.is_some() {
            return Err(PersistentARTrieError::internal("Eviction already enabled"));
        }

        // Create the epoch manager reference
        let epoch_manager = Arc::new(crate::persistent_artrie::concurrency::EpochManager::new());

        // Create the eviction coordinator
        let coordinator = crate::persistent_artrie::eviction::EvictionCoordinator::new(config.clone(), epoch_manager);

        // Create a weak reference to self for the eviction callback
        let self_weak = Arc::downgrade(self);

        // Start the eviction coordinator with the eviction callback for char nodes
        // (VocabARTrie uses char-based nodes internally)
        coordinator.start_char(move |nodes_to_evict| {
            // Try to upgrade the weak reference
            let Some(trie) = self_weak.upgrade() else {
                return (0, 0);
            };

            let mut guard = trie.write();
            let mut evicted_count = 0;
            let mut bytes_freed = 0;

            for (_path_hash, path, disk_ptr) in nodes_to_evict {
                if guard.evict_node_at_path(&path, disk_ptr.clone()) {
                    evicted_count += 1;
                    bytes_freed += 256; // Estimate ~256 bytes per node

                    // Remove from LRU tracking
                    if let Some(ref coordinator) = guard.eviction_coordinator {
                        use crate::persistent_artrie::eviction::lru_tracker::hash_char_path;
                        coordinator.lru_registry().remove_hash(hash_char_path(&path));
                    }
                }
            }

            (evicted_count, bytes_freed)
        }).map_err(|e| PersistentARTrieError::internal(&e))?;

        // Start memory pressure monitor if configured
        coordinator.start_memory_monitor()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        guard.eviction_coordinator = Some(coordinator);

        Ok(())
    }

    fn disable_eviction(&mut self) -> crate::persistent_artrie::error::Result<()> {
        let mut guard = self.write();

        if let Some(coordinator) = guard.eviction_coordinator.take() {
            coordinator.shutdown();
        }

        Ok(())
    }

    fn eviction_enabled(&self) -> bool {
        let guard = self.read();
        guard.eviction_coordinator.is_some()
    }

    fn eviction_stats(&self) -> crate::persistent_artrie::eviction::EvictionStats {
        let guard = self.read();
        guard.eviction_coordinator
            .as_ref()
            .map(|c| c.stats())
            .unwrap_or_default()
    }

    fn force_eviction(&mut self, target_bytes: usize) -> crate::persistent_artrie::error::Result<(usize, usize)> {
        let guard = self.read();

        let Some(coordinator) = &guard.eviction_coordinator else {
            return Ok((0, 0));
        };

        Ok(coordinator.force_eviction(target_bytes))
    }

    fn touch_node(&self, path: &[Self::Unit]) {
        let guard = self.read();
        if let Some(coordinator) = &guard.eviction_coordinator {
            use crate::persistent_artrie::eviction::lru_tracker::hash_char_path;
            coordinator.lru_registry().touch_hash(hash_char_path(path));
        }
    }
}

// Helper methods for eviction on PersistentVocabARTrie
impl PersistentVocabARTrie {
    /// Evict a single node at the given path, replacing it with a DiskRef.
    ///
    /// Returns `true` if the node was successfully evicted, `false` if the
    /// node was not found or was already a DiskRef.
    pub(crate) fn evict_node_at_path(&mut self, _path: &[char], _disk_ptr: crate::persistent_artrie::swizzled_ptr::SwizzledPtr) -> bool {
        // Vocab trie eviction is more complex due to different node structure
        // with parent pointers. This is a simplified placeholder - full implementation
        // would need to navigate the vocab trie structure while preserving
        // parent pointer integrity.
        false
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
