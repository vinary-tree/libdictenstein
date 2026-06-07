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
//! ```rust,no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
//!
//! // Create a new vocabulary
//! let mut vocab = PersistentVocabARTrie::create("vocab.vocab")?;
//!
//! // Insert terms (auto-assigns indices)
//! let idx1 = vocab.insert("hello")?; // Returns 0
//! let idx2 = vocab.insert("world")?; // Returns 1
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
//! # Ok(())
//! # }
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

// Vocabulary-specific file-header reader (relocated out of
// `persistent_artrie_core::block_storage` so core stays free of variant deps).
pub mod header;

// VocabSyncHandle (Phase-6 split out of dict_impl).
pub mod sync_handle;

// IoUringDiskManager-specific constructors (Phase-6 split out of dict_impl).
#[cfg(feature = "io-uring-backend")]
pub mod io_uring_ctor;

// MmapDiskManager-specific constructors (Phase-6 split out of dict_impl).
pub mod mmap_ctor;

// Lock-free CAS-based concurrent inserts (Phase-6 split out of dict_impl).
pub mod lockfree_cas;

// Persistence/durability/observability API (Phase-6 split out of dict_impl).
pub mod persistence_api;

// Public query API (get_index, get_term, contains, len) — Phase-6 split.
pub mod query_api;

// BloomFilter support — Phase-6 split out of dict_impl.
pub mod bloom_filter_api;

// Public mutation API (insert / insert_batch / insert_with_index) — Phase-6 split.
pub mod mutation_api;

// Path-based queries + iter_terms wrappers — Phase-6 split.
pub mod path_query;

// Disk I/O helpers (load/persist/serialize/rebuild_reverse_index) — Phase-6 split.
pub mod disk_io;

// DFS iterators (VocabTermIterator + VocabPrefixIterator) — Phase-6 split.
pub mod iterators;

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
    NodeRef, // Re-export from persistent_artrie_char
    VocabTrieFileHeader,
    VocabTrieNode,
    VocabTrieRoot,
    DEFAULT_REVERSE_CACHE_SIZE,
    DEFAULT_VOCAB_BUFFER_POOL_SIZE,
    FLAG_HAS_PARENT_POINTER,
    VOCAB_FILE_HEADER_SIZE,
    VOCAB_HEADER_VERSION_V1,
    VOCAB_TRIE_MAGIC,
};

pub use reverse_index::{
    ReverseIndexHeader, VocabReverseIndex, REVERSE_INDEX_HEADER_SIZE, REVERSE_INDEX_MAGIC,
};

pub use reverse_cache::{CacheStats, VocabReverseCache};

pub use serialization::{
    deserialize_vocab_node, serialize_vocab_node, vocab_serialized_size, SerializedVocabNodeHeader,
    FLAG_HAS_VALUE, VOCAB_FORMAT_VERSION, VOCAB_NODE_MAGIC, VOCAB_SERIALIZED_HEADER_SIZE,
};

pub use dict_impl::{PersistentVocabARTrie, SharedVocabARTrie, VocabSyncHandle};

pub use concurrent::{ConcurrentMode, ConcurrentVocabARTrie, ConcurrentVocabStats};

pub use lockfree::{InsertResult, LockFreeVocab, LockFreeVocabStats};

// Re-export DurabilityPolicy from base layer
pub use crate::persistent_artrie::dict_impl::DurabilityPolicy;

// Re-export eviction types from byte-level implementation (shared)
pub use crate::persistent_artrie::eviction::{
    AccessTracker, DiskLocationRegistry, EvictionConfig, EvictionCoordinator, EvictionStats,
    EvictionUrgency, LruRegistry,
};

// ============================================================================
// Trait Implementations
// ============================================================================

use crate::bijective::BijectiveDictionary;
use crate::persistent_artrie::error::Result;
use crate::persistent_artrie::recovery::RecoveryReport;
use crate::{Dictionary, DictionaryNode, MappedDictionary, MutableMappedDictionary};
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
        Self {
            is_final,
            children,
            path,
        }
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
        let edges: Vec<_> = self
            .children
            .iter()
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

// MutableMappedDictionary trait implementation.
//
// `PersistentVocabARTrie` has no interior mutability (mutation methods take
// `&mut self`), so the `&self`-shaped trait methods cannot do useful work
// here. All three return their "no-op" sentinel value (`false` or `0`) and
// emit `log::warn!` so the call site shows up under `RUST_LOG=warn`. To
// actually mutate, wrap the vocab in `SharedVocabARTrie` (which holds the
// trie behind an `Arc<RwLock<…>>`) and call its trait impl, or call the
// inherent `PersistentVocabARTrie::insert(&mut self, term)` directly.
impl MutableMappedDictionary for PersistentVocabARTrie {
    fn insert_with_value(&self, term: &str, _value: Self::Value) -> bool {
        log::warn!(
            "PersistentVocabARTrie::insert_with_value({term:?}, _) is a no-op \
             — this type has no interior mutability. Use \
             SharedVocabARTrie::insert_with_value, or call the inherent \
             PersistentVocabARTrie::insert via &mut self."
        );
        false
    }

    fn union_with<F>(&self, _other: &Self, _merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        log::warn!(
            "PersistentVocabARTrie::union_with is a no-op — vocab tries are \
             append-only and this type has no interior mutability. Use \
             SharedVocabARTrie::union_with (note: merge_fn will still be \
             ignored, vocab indices are auto-assigned)."
        );
        0
    }

    fn update_or_insert<F>(&self, term: &str, _default_value: Self::Value, _update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        log::warn!(
            "PersistentVocabARTrie::update_or_insert({term:?}, _, _) is a no-op \
             — this type has no interior mutability. Use \
             SharedVocabARTrie::update_or_insert (note: default_value and \
             update_fn will still be ignored, indices are auto-assigned)."
        );
        false
    }
}

// BijectiveDictionary trait implementation.
//
// Reverse lookup reconstructs the term on-the-fly via parent-pointer
// backtracking (`PersistentVocabARTrie::get_term(index)`), then wraps the
// resulting `String` in `Cow::Owned`. The previous `Option<&str>` trait
// signature couldn't be honored honestly here — there's no stable in-memory
// storage to point at, since terms are reconstructed per-call — so this used
// to be a `None` stub that silently violated the documented bijection
// invariant. Cow lets the caller see the actual term.
impl BijectiveDictionary for PersistentVocabARTrie {
    fn get_term(&self, value: &Self::Value) -> Option<std::borrow::Cow<'_, str>> {
        // Delegate to the inherent method, which returns Option<String>.
        Self::get_term(self, *value).map(std::borrow::Cow::Owned)
    }

    fn contains_value(&self, value: &Self::Value) -> bool {
        self.contains_index(*value)
    }

    fn bijection_len(&self) -> usize {
        self.len()
    }
}

// `PersistentVocabARTrie` does not implement `crate::artrie_trait::ARTrie`
// directly: the trait's mutation methods (`insert`, `remove`, `checkpoint`,
// `sync`, `upsert`, `increment`, `enable_slot_tracking`, `flush_sequential`,
// `insert_with_value`, `remove_prefix`) all take `&self` but the underlying
// trie semantics require `&mut self`. Use `SharedVocabARTrie`
// (`Arc<RwLock<PersistentVocabARTrie>>`) when an `ARTrie` impl is required;
// it satisfies the trait by acquiring the write lock per call. The
// `SharedVocabARTrie` impl lives below.

// ============================================================================
// SharedVocabARTrie Trait Implementations
// ============================================================================

use parking_lot::RwLock;
use std::sync::Arc;

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

// `SharedVocabARTrie` accepts mutations through its read/write guards, but
// it cannot honor the `value`/`merge_fn`/`update_fn` arguments that
// `MutableMappedDictionary` exposes — vocab indices are auto-assigned by the
// internal allocator, not chosen by the caller. The trait methods below
// insert the term and emit `log::warn!` so the discarded argument shows up
// under `RUST_LOG=warn`. Read the assigned index back with
// `MappedDictionary::get_value`.
impl MutableMappedDictionary for SharedVocabARTrie {
    fn insert_with_value(&self, term: &str, _value: Self::Value) -> bool {
        log::warn!(
            "SharedVocabARTrie::insert_with_value({term:?}, _) discards the \
             value argument — vocab indices are auto-assigned. Use \
             insert(term) and read the assigned index back via \
             MappedDictionary::get_value(term)."
        );
        let existed = self.read().contains(term);
        if !existed {
            let mut guard = self.write();
            if let Err(error) = guard.insert(term) {
                log::warn!("SharedVocabARTrie::insert_with_value failed: {error}");
                return false;
            }
        }
        !existed
    }

    fn union_with<F>(&self, other: &Self, _merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        log::warn!(
            "SharedVocabARTrie::union_with discards the merge_fn argument — \
             vocab indices are auto-assigned, so there is nothing to merge. \
             Terms missing from self will be inserted with fresh indices."
        );
        // First collect terms from other to avoid holding locks simultaneously
        let other_terms: Vec<String> = {
            let other_guard = other.read();
            other_guard.iter_terms().collect()
        };

        let mut count = 0;
        let mut self_guard = self.write();
        for term in other_terms {
            if !self_guard.contains(&term) {
                match self_guard.insert(&term) {
                    Ok(_) => count += 1,
                    Err(error) => {
                        log::warn!("SharedVocabARTrie::union_with failed for {term:?}: {error}");
                    }
                }
            }
        }
        count
    }

    fn update_or_insert<F>(&self, term: &str, _default_value: Self::Value, _update_fn: F) -> bool
    where
        F: FnOnce(&mut Self::Value),
    {
        log::warn!(
            "SharedVocabARTrie::update_or_insert({term:?}, _, _) discards \
             both default_value and update_fn — vocab indices are \
             auto-assigned and immutable. Inserts the term if absent and \
             returns whether a new term was added."
        );
        let existed = self.read().contains(term);
        if !existed {
            let mut guard = self.write();
            if let Err(error) = guard.insert(term) {
                log::warn!("SharedVocabARTrie::update_or_insert failed: {error}");
                return false;
            }
        }
        !existed
    }
}

impl BijectiveDictionary for SharedVocabARTrie {
    fn get_term(&self, value: &Self::Value) -> Option<std::borrow::Cow<'_, str>> {
        // Acquire the read guard, reconstruct the term, drop the guard, return
        // the owned String wrapped as Cow::Owned. The Cow doesn't borrow from
        // self because the underlying String is owned outright.
        let guard = self.read();
        guard.get_term(*value).map(std::borrow::Cow::Owned)
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
        PersistentVocabARTrie::open_with_recovery(path).map(|(t, r)| (Arc::new(RwLock::new(t)), r))
    }

    fn open_with_recovery_and_slot_tracking<P: AsRef<Path>>(
        path: P,
    ) -> Result<(Self, RecoveryReport)> {
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
        if let Err(error) = PersistentVocabARTrie::insert(&mut *guard, term) {
            log::warn!("SharedVocabARTrie::insert failed: {error}");
            return false;
        }
        // Return true if a new term was added (count increased)
        guard.len() > old_count
    }

    fn insert_with_value(&self, term: &str, _value: Self::Value) -> bool {
        log::warn!(
            "SharedVocabARTrie::insert_with_value (via ARTrie trait) for \
             {term:?} discards the value argument — vocab indices are \
             auto-assigned."
        );
        let mut guard = self.write();
        let old_count = guard.len();
        // Explicitly call the struct method, not trait method
        if let Err(error) = PersistentVocabARTrie::insert(&mut *guard, term) {
            log::warn!("SharedVocabARTrie::insert_with_value failed: {error}");
            return false;
        }
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

    fn remove(&self, term: &str) -> bool {
        log::warn!(
            "SharedVocabARTrie::remove({term:?}) is unsupported — vocab tries \
             are append-only to preserve the term ↔ index bijection. Returns \
             false unconditionally."
        );
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

    fn remove_prefix(&self, prefix: &str) -> usize {
        log::warn!(
            "SharedVocabARTrie::remove_prefix({prefix:?}) is unsupported — \
             vocab tries are append-only. Returns 0 unconditionally."
        );
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
            guard.get_index(prefix).is_some()
                || !guard.get_children_at_path(&prefix_chars).is_empty()
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
        log::warn!(
            "SharedVocabARTrie::upsert({term:?}, _) discards the value \
             argument — vocab indices are auto-assigned. Behaves as insert."
        );
        let mut guard = self.write();
        let old_count = guard.len();
        // Explicitly call the struct method, not trait method
        PersistentVocabARTrie::insert(&mut *guard, term)?;
        Ok(guard.len() > old_count)
    }

    // C1: `increment` removed from the `ARTrie` trait. Vocab never supported it
    // (indices are auto-assigned); the former runtime reject is now simply the
    // method's ABSENCE (more honest than a runtime Err). Commented out (not deleted)
    // per convention.
    // fn increment(&self, _term: &str, _delta: i64) -> Result<i64> {
    //     Err(crate::persistent_artrie::error::PersistentARTrieError::InvalidOperation(
    //         "PersistentVocabARTrie does not support increment - indices are auto-assigned".into(),
    //     ))
    // }
}

// ============================================================================
// EvictableARTrie Trait Implementation (on SharedVocabARTrie)
// ============================================================================

impl crate::artrie_trait::EvictableARTrie for SharedVocabARTrie {
    fn enable_eviction(
        &self,
        config: crate::persistent_artrie::eviction::EvictionConfig,
    ) -> crate::persistent_artrie::error::Result<()> {
        use crate::persistent_artrie::error::PersistentARTrieError;

        config
            .validate()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        let mut guard = self.write();

        // Check if eviction is already enabled
        if guard.eviction_coordinator.is_some() {
            return Err(PersistentARTrieError::internal("Eviction already enabled"));
        }

        // Create the epoch manager reference
        let epoch_manager = Arc::new(crate::persistent_artrie::concurrency::EpochManager::new());

        // Create the eviction coordinator
        let coordinator = crate::persistent_artrie::eviction::EvictionCoordinator::new(
            config.clone(),
            epoch_manager,
        );

        // Create a weak reference to self for the eviction callback
        let self_weak = Arc::downgrade(self);

        // Start the eviction coordinator with the eviction callback for char nodes
        // (VocabARTrie uses char-based nodes internally)
        coordinator
            .start_char(move |nodes_to_evict| {
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
                            coordinator
                                .lru_registry()
                                .remove_hash(hash_char_path(&path));
                        }
                    }
                }

                (evicted_count, bytes_freed)
            })
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        // Start memory pressure monitor if configured
        coordinator
            .start_memory_monitor()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        guard.eviction_coordinator = Some(coordinator);

        Ok(())
    }

    fn disable_eviction(&self) -> crate::persistent_artrie::error::Result<()> {
        // Drop-before-join (live-deadlock fix; red-team R3-2 SWEEP C, the 8th site):
        // take the coordinator out and RELEASE the write guard BEFORE `shutdown()`
        // joins the eviction worker. The worker's reclaim callback re-enters via
        // `trie.write()` (the `enable_eviction` closure), so holding the write guard
        // across the join deadlocks (worker waits on the guard; the joining thread
        // waits on the worker). char/byte `disable_eviction` already use this
        // statement-temporary; vocab was the missed site.
        let coordinator = {
            let mut guard = self.write();
            guard.eviction_coordinator.take()
        };
        if let Some(coordinator) = coordinator {
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
        guard
            .eviction_coordinator
            .as_ref()
            .map(|c| c.stats())
            .unwrap_or_default()
    }

    fn force_eviction(
        &self,
        target_bytes: usize,
    ) -> crate::persistent_artrie::error::Result<(usize, usize)> {
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
    /// Walks `path` from the root, descends through `path[..path.len()-1]`
    /// to reach the parent, and atomically `unswizzle`s the slot for
    /// `path.last()` to the disk location encoded in `disk_ptr`.
    ///
    /// Vocab-specific constraint: a vocab node carries a `parent: NodeRef`
    /// back-pointer used by `rebuild_reverse_index`. Evicting a node whose
    /// in-memory subtree is non-trivial would orphan those descendants
    /// (their `parent` would dangle once the in-memory node is dropped).
    /// We therefore refuse to evict any node that still has in-memory
    /// (swizzled) children — the eviction coordinator must descend
    /// leaf-first via repeated calls before draining a parent.
    ///
    /// Returns `true` on successful unswizzle, `false` on any of: empty
    /// path, navigation failure, parent slot already on disk, child slot
    /// already on disk, subtree-not-leaf violation, missing/non-disk
    /// `disk_ptr`, or CAS race loss.
    pub(crate) fn evict_node_at_path(
        &mut self,
        path: &[char],
        disk_ptr: crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
    ) -> bool {
        if path.is_empty() {
            return false; // The root is never evicted via this path.
        }

        let target_loc = match disk_ptr.disk_location() {
            Some(loc) => loc,
            None => return false,
        };

        let root_node: &mut crate::persistent_vocab_artrie::types::VocabTrieNode = match self.root {
            crate::persistent_vocab_artrie::types::VocabTrieRoot::Node(ref mut boxed) => {
                boxed.as_mut()
            }
            crate::persistent_vocab_artrie::types::VocabTrieRoot::Empty => return false,
        };

        // Navigate to the parent of the target. As in the char variant, this
        // uses raw-pointer hops; `&mut self` guarantees no concurrent access
        // and each pointer comes from a SwizzledPtr we just verified is
        // in-memory.
        let mut current: *mut crate::persistent_vocab_artrie::types::VocabTrieNode = root_node;
        let descent: &[char] = &path[..path.len() - 1];
        for &edge in descent {
            // SAFETY: same invariant as `evict_node_at_path` in
            // `persistent_artrie_char`. The pointer is either the original
            // `&mut root_node` (first iteration) or a pointer we just proved
            // points to an in-memory `VocabTrieNode` whose lifetime is at
            // least the lifetime of `&mut self`.
            let node = unsafe { &mut *current };
            let child_slot = match node.inner.find_child(edge as u32) {
                Some(slot) => slot,
                None => return false,
            };
            if !child_slot.is_swizzled() {
                return false; // Cannot descend through an on-disk parent slot.
            }
            let child_raw =
                match child_slot.as_ptr::<crate::persistent_vocab_artrie::types::VocabTrieNode>() {
                    Some(p) => p,
                    None => return false,
                };
            current = child_raw as *mut crate::persistent_vocab_artrie::types::VocabTrieNode;
        }

        // SAFETY: same invariant.
        let parent = unsafe { &mut *current };
        let last_edge = *path.last().expect("non-empty path verified above");
        let slot = match parent.inner.find_child_mut(last_edge as u32) {
            Some(s) => s,
            None => return false,
        };
        if !slot.is_swizzled() {
            return false; // Already on disk.
        }

        // Parent-pointer-integrity check: only evict if the target's
        // subtree has no in-memory descendants. Peek at the target's
        // children through the slot's in-memory pointer.
        let target_raw = match slot.as_ptr::<crate::persistent_vocab_artrie::types::VocabTrieNode>()
        {
            Some(p) => p,
            None => return false,
        };
        // SAFETY: slot is swizzled, target_raw points at a live
        // VocabTrieNode owned by this trie.
        let target = unsafe { &*target_raw };
        for (_edge, child_slot) in target.inner.iter_children() {
            if child_slot.is_swizzled() {
                // Descendant still in memory; refuse to evict.
                return false;
            }
        }

        match slot.unswizzle::<crate::persistent_vocab_artrie::types::VocabTrieNode>(
            target_loc.block_id,
            target_loc.offset,
            target_loc.node_type,
        ) {
            Ok(raw_ptr) => {
                let raw_const =
                    raw_ptr as *const crate::persistent_vocab_artrie::types::VocabTrieNode;
                self.node_map.retain(|_, node_ptr| *node_ptr != raw_const);

                // SAFETY: slot was just unswizzled; we now own the old
                // pointer, originally `Box::into_raw(Box::new(...))`.
                unsafe {
                    let _: Box<crate::persistent_vocab_artrie::types::VocabTrieNode> =
                        Box::from_raw(
                            raw_ptr as *mut crate::persistent_vocab_artrie::types::VocabTrieNode,
                        );
                }
                true
            }
            Err(_) => false,
        }
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
/// ```rust,no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use libdictenstein::persistent_vocab_artrie::IndexedVocabularyPersistent;
///
/// // Create new vocabulary
/// let mut vocab = IndexedVocabularyPersistent::create("vocab.vocab")?;
/// vocab.insert("hello")?; // Returns 0
///
/// // Checkpoint and reopen with recovery
/// vocab.checkpoint()?;
/// let (vocab, report) = IndexedVocabularyPersistent::open_with_recovery("vocab.vocab")?;
///
/// // Reverse lookup works immediately!
/// assert_eq!(vocab.get_term(0), Some("hello".to_string()));
/// # Ok(())
/// # }
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
    use crate::persistent_artrie::wal::WalConfig;
    use crate::persistent_artrie::{NodeType, SwizzledPtr};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use tempfile::tempdir;
    use xxhash_rust::xxh3::Xxh3DefaultBuilder;

    fn suppress_drop_checkpoint(vocab: &PersistentVocabARTrie) {
        vocab.entry_count.store(0, Ordering::Release);
        vocab.dirty.store(false, Ordering::Release);
    }

    fn heap_only_vocab_for_unsafe_tests() -> PersistentVocabARTrie {
        let root = Box::new(VocabTrieNode::new());
        let root_ref = NodeRef::new(0, 0);
        let root_ptr = root.as_ref() as *const VocabTrieNode;
        let mut node_map = HashMap::with_hasher(Xxh3DefaultBuilder);
        node_map.insert(root_ref, root_ptr);

        PersistentVocabARTrie {
            path: PathBuf::new(),
            root: VocabTrieRoot::Node(root),
            entry_count: AtomicUsize::new(0),
            start_index: 0,
            next_index: AtomicU64::new(0),
            dirty: AtomicBool::new(false),
            reverse_index: None,
            reverse_cache: VocabReverseCache::new(DEFAULT_REVERSE_CACHE_SIZE),
            node_map,
            next_slot: 1,
            wal_writer: None,
            wal_config: WalConfig::default(),
            next_lsn: AtomicU64::new(1),
            synced_lsn: AtomicU64::new(0),
            durability_policy: DurabilityPolicy::Periodic,
            arena_manager: None,
            buffer_manager: None,
            eviction_coordinator: None,
            bloom_filter: None,
            lockfree_root: None,
            lockfree_cache: None,
            cas_retries: AtomicU64::new(0),
        }
    }

    fn assert_rebuilt_node_map_parent_chain(vocab: &PersistentVocabARTrie) {
        let mut ptr_to_ref = HashMap::new();

        for (node_ref, node_ptr) in &vocab.node_map {
            assert!(
                !node_ptr.is_null(),
                "node_map must not contain null pointers"
            );
            assert!(
                ptr_to_ref.insert(*node_ptr as usize, *node_ref).is_none(),
                "node_map must assign each raw node pointer a single NodeRef"
            );
        }

        let root = match &vocab.root {
            VocabTrieRoot::Node(root) => root.as_ref(),
            VocabTrieRoot::Empty => panic!("checkpointed vocabulary root should be loaded"),
        };
        let root_ref = NodeRef::new(0, 0);
        let root_ptr = root as *const VocabTrieNode as usize;

        assert_eq!(ptr_to_ref.get(&root_ptr), Some(&root_ref));
        assert!(
            root.parent.is_null(),
            "reloaded root must not acquire a parent pointer"
        );

        let mut visited = Vec::new();
        let mut stack = vec![(root_ref, root)];

        while let Some((node_ref, node)) = stack.pop() {
            let node_ptr = node as *const VocabTrieNode as usize;
            visited.push(node_ptr);

            for (edge, child) in node.iter_children() {
                let child_ptr = child as *const VocabTrieNode as usize;
                let child_ref = ptr_to_ref
                    .get(&child_ptr)
                    .expect("in-memory child must be present in node_map");

                assert_eq!(
                    child.parent, node_ref,
                    "rebuild must set each child parent NodeRef"
                );
                assert_eq!(
                    child.parent_edge, edge as u32,
                    "rebuild must set the parent edge used for reverse lookup"
                );
                assert_eq!(
                    vocab.node_map.get(child_ref).map(|ptr| *ptr as usize),
                    Some(child_ptr),
                    "child NodeRef must resolve to the same raw pointer"
                );
                stack.push((*child_ref, child));
            }
        }

        visited.sort_unstable();
        visited.dedup();
        assert_eq!(
            visited.len(),
            vocab.node_map.len(),
            "node_map must not retain stale raw pointers after rebuild"
        );
    }

    #[test]
    fn test_vocab_trie_basic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

        // Insert
        let idx1 = vocab.insert("apple").expect("insert apple");
        let idx2 = vocab.insert("banana").expect("insert banana");
        let idx3 = vocab.insert("cherry").expect("insert cherry");

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

        let idx1 = vocab.insert("first").expect("insert first");
        let idx2 = vocab.insert("second").expect("insert second");

        assert_eq!(idx1, 10);
        assert_eq!(idx2, 11);
        assert_eq!(vocab.start_index(), 10);
    }

    #[test]
    fn test_vocab_trie_idempotent_insert() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

        let idx1 = vocab.insert("duplicate").expect("insert duplicate");
        let idx2 = vocab.insert("duplicate").expect("insert duplicate again");
        let idx3 = vocab
            .insert("duplicate")
            .expect("insert duplicate third time");

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

    #[test]
    fn vocab_parent_eviction_is_rejected_while_child_is_in_memory() {
        let mut vocab = heap_only_vocab_for_unsafe_tests();
        vocab.insert_with_index_no_wal("ab", 0).unwrap();

        let parent_ptr = match &vocab.root {
            VocabTrieRoot::Node(root) => {
                root.get_child('a').expect("parent child exists") as *const VocabTrieNode
            }
            VocabTrieRoot::Empty => panic!("vocab root is empty"),
        };

        let disk_ptr = SwizzledPtr::on_disk(1, 4096, NodeType::CharNode4);
        assert!(
            !vocab.evict_node_at_path(&['a'], disk_ptr),
            "parent with an in-memory descendant must not be evicted"
        );
        assert!(
            vocab.node_map.values().any(|ptr| *ptr == parent_ptr),
            "rejected eviction must leave node_map unchanged"
        );

        suppress_drop_checkpoint(&vocab);
    }

    #[test]
    fn vocab_leaf_eviction_invalidates_node_map_entry_before_drop() {
        let mut vocab = heap_only_vocab_for_unsafe_tests();
        vocab.insert_with_index_no_wal("ab", 0).unwrap();

        let leaf_ptr = match &vocab.root {
            VocabTrieRoot::Node(root) => {
                root.get_child('a')
                    .and_then(|node| node.get_child('b'))
                    .expect("leaf child exists") as *const VocabTrieNode
            }
            VocabTrieRoot::Empty => panic!("vocab root is empty"),
        };
        assert!(
            vocab.node_map.values().any(|ptr| *ptr == leaf_ptr),
            "leaf pointer should be registered before eviction"
        );

        let disk_ptr = SwizzledPtr::on_disk(1, 8192, NodeType::CharNode4);
        assert!(
            vocab.evict_node_at_path(&['a', 'b'], disk_ptr),
            "leaf eviction should succeed"
        );
        assert!(
            vocab.node_map.values().all(|ptr| *ptr != leaf_ptr),
            "successful eviction must not leave a stale raw node_map pointer"
        );

        let leaf_slot = match &vocab.root {
            VocabTrieRoot::Node(root) => root
                .get_child('a')
                .and_then(|node| node.inner.find_child('b' as u32))
                .expect("evicted child slot remains as disk pointer"),
            VocabTrieRoot::Empty => panic!("vocab root is empty"),
        };
        assert!(leaf_slot.disk_location().is_some());

        suppress_drop_checkpoint(&vocab);
    }

    #[test]
    fn vocab_leaf_eviction_keeps_sibling_queries_on_live_nodes() {
        let mut vocab = heap_only_vocab_for_unsafe_tests();
        vocab.insert_with_index_no_wal("ab", 0).unwrap();
        vocab.insert_with_index_no_wal("ac", 1).unwrap();
        let evicted_index = 0;
        let sibling_index = 1;

        let leaf_ptr = match &vocab.root {
            VocabTrieRoot::Node(root) => {
                root.get_child('a')
                    .and_then(|node| node.get_child('b'))
                    .expect("leaf child exists") as *const VocabTrieNode
            }
            VocabTrieRoot::Empty => panic!("vocab root is empty"),
        };

        let disk_ptr = SwizzledPtr::on_disk(1, 12_288, NodeType::CharNode4);
        assert!(
            vocab.evict_node_at_path(&['a', 'b'], disk_ptr),
            "leaf eviction should succeed"
        );
        assert!(
            vocab.node_map.values().all(|ptr| *ptr != leaf_ptr),
            "evicted leaf raw pointer must be removed before the Box is dropped"
        );

        assert_eq!(
            vocab.get_index("ac"),
            Some(sibling_index),
            "sibling path must remain backed by live in-memory nodes"
        );
        assert_eq!(
            vocab.get_index("ab"),
            None,
            "query traversal must not dereference an evicted disk-only child"
        );
        assert_ne!(evicted_index, sibling_index);

        suppress_drop_checkpoint(&vocab);
    }

    #[test]
    fn vocab_heap_node_map_parent_chain_tracks_live_nodes() {
        let mut vocab = heap_only_vocab_for_unsafe_tests();
        let expected = [("alpha", 0), ("alpine", 1), ("beta", 2), ("delta", 3)];

        for (term, index) in expected {
            assert!(vocab.insert_with_index_no_wal(term, index).unwrap());
        }

        assert_eq!(vocab.len(), expected.len());
        for (term, index) in expected {
            assert_eq!(vocab.get_index(term), Some(index));
            assert_eq!(vocab.get_term(index), Some(term.to_string()));
            assert!(vocab.contains_index(index));
        }
        assert_rebuilt_node_map_parent_chain(&vocab);

        suppress_drop_checkpoint(&vocab);
    }

    #[test]
    fn vocab_reopen_rebuilds_node_map_parent_chain_and_reverse_index() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("reopen_rebuilds_node_map.vocab");
        let terms = ["alpha", "alpine", "beta", "delta"];
        let mut expected = Vec::new();

        {
            let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
            for term in terms {
                expected.push((term.to_string(), vocab.insert(term).expect("insert term")));
            }

            vocab.checkpoint().unwrap();
            suppress_drop_checkpoint(&vocab);
        }

        let reopened = PersistentVocabARTrie::open(&path).unwrap();
        assert_eq!(reopened.len(), expected.len());

        for (term, index) in &expected {
            assert_eq!(reopened.get_index(term), Some(*index));
            assert_eq!(reopened.get_term(*index), Some(term.clone()));
            assert!(reopened.contains_index(*index));
        }

        assert_rebuilt_node_map_parent_chain(&reopened);
        suppress_drop_checkpoint(&reopened);
    }
}
