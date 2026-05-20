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

// Char-side MVCC TrieRoot implementation (paired with generic core mvcc).
pub mod mvcc;

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
    // Transaction types
    CharDocumentTransaction, DurabilityPolicy, TransactionState,
};

// Note: CharTrieNodeInner is already re-exported from types

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

// Re-export eviction types from byte-level implementation (shared)
pub use crate::persistent_artrie::eviction::{
    AccessTracker, DiskLocationRegistry, EvictionConfig, EvictionCoordinator,
    EvictionStats, EvictionUrgency, LruRegistry,
};

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};

use parking_lot::RwLock;

use crate::persistent_artrie::error::Result as PersistentResult;
use crate::persistent_artrie::wal::AsyncWalWriter;
use crate::persistent_artrie::wal_managed::WalManaged;
use crate::value::DictionaryValue;
use crate::zipper::{DictZipper, ValuedDictZipper};
use crate::{Dictionary, DictionaryNode, MappedDictionary, MutableMappedDictionary};

/// Thread-safe wrapper for `PersistentARTrieChar`.
///
/// This type alias provides `Arc<RwLock<...>>` semantics for concurrent access
/// to the disk-backed character trie.
pub type SharedCharARTrie<V, S = crate::persistent_artrie::disk_manager::MmapDiskManager> = Arc<RwLock<PersistentARTrieChar<V, S>>>;

/// Deprecated alias for backward compatibility.
#[deprecated(since = "0.9.0", note = "Use SharedCharARTrie instead")]
pub type SharedCharTrie<V> = SharedCharARTrie<V>;

/// Character-level Persistent Adaptive Radix Trie for Unicode support.
///
/// This dictionary provides proper Unicode character-level edit distance
/// calculations, ensuring that multi-byte UTF-8 characters are counted
/// as single edit operations.
///
/// # Thread Safety
///
/// This type provides interior mutability via RwLock. For concurrent access,
/// use `SharedCharTrie<V>` which is `Arc<RwLock<PersistentARTrieChar<V>>>`.
pub struct PersistentARTrieChar<V: DictionaryValue = (), S: crate::persistent_artrie::block_storage::BlockStorage = crate::persistent_artrie::disk_manager::MmapDiskManager> {
    /// Root of the trie
    pub(crate) root: CharTrieRoot<V>,
    /// Number of terms (atomic for lock-free access)
    pub(crate) len: AtomicUsize,
    /// Dirty flag (atomic for lock-free access)
    pub(crate) dirty: AtomicBool,

    // Storage infrastructure (optional - None for in-memory mode)
    pub(crate) buffer_manager: Option<Arc<RwLock<crate::persistent_artrie::buffer_manager::BufferManager<S>>>>,
    /// Async WAL writer for durability (handles synchronization internally)
    pub(crate) wal_writer: Option<Arc<crate::persistent_artrie::wal::AsyncWalWriter>>,
    /// WAL configuration (archive mode, segment limits, etc.)
    pub(crate) wal_config: crate::persistent_artrie::wal::WalConfig,
    pub(crate) next_lsn: std::sync::atomic::AtomicU64,
    pub(crate) file_path: Option<std::path::PathBuf>,
    /// Arena manager for space-efficient node storage
    /// Packs multiple nodes into 256KB blocks instead of one node per block
    pub(crate) arena_manager: Option<Arc<RwLock<ArenaManager<S>>>>,

    // Concurrency infrastructure
    /// Version for optimistic concurrency control
    pub(crate) version: crate::persistent_artrie::concurrency::OptimisticVersion,
    /// Epoch manager for safe memory reclamation
    pub(crate) epoch_manager: crate::persistent_artrie::concurrency::EpochManager,
    /// Retry statistics for monitoring
    pub(crate) retry_stats: crate::persistent_artrie::concurrency::RetryStats,

    // Group commit infrastructure (optional - for high-throughput write batching)
    #[cfg(feature = "group-commit")]
    /// Group commit coordinator for WAL write batching.
    /// When enabled, WAL writes are batched for better throughput.
    pub(crate) group_commit: Option<Arc<crate::persistent_artrie::group_commit::GroupCommitCoordinator>>,

    // Performance infrastructure
    /// Memory pressure monitor for adaptive memory management.
    /// When enabled, automatically adjusts buffer pool size based on system memory pressure.
    pub(crate) memory_monitor: Option<Arc<crate::persistent_artrie::memory_monitor::MemoryPressureMonitor>>,
    /// Cache statistics for monitoring buffer pool performance.
    pub(crate) cache_stats: crate::persistent_artrie::adaptive_pool::CacheStats,
    /// Epoch-based checkpoint manager for automatic checkpointing.
    ///
    /// When enabled, the checkpoint manager tracks operation counts and WAL size,
    /// triggering automatic checkpoints based on configurable thresholds.
    /// This provides bounded WAL size and faster recovery.
    pub(crate) checkpoint_manager: Option<Arc<crate::persistent_artrie::epoch::CheckpointManager>>,
    /// Durability policy for WAL synchronization.
    /// Controls when fsync is called after WAL writes.
    pub(crate) durability_policy: DurabilityPolicy,

    // === Eviction Support ===
    /// Eviction coordinator for memory pressure-driven eviction
    pub(crate) eviction_coordinator: Option<Arc<crate::persistent_artrie::eviction::EvictionCoordinator>>,

    // === Prefetching Support ===
    /// Prefetcher for multi-level I/O optimization.
    ///
    /// When traversing disk-backed tries, the prefetcher initiates background I/O
    /// for children that may be visited soon, improving cold lookup performance
    /// by 15-30%.
    pub(crate) prefetcher: crate::persistent_artrie::prefetch::Prefetcher,

    /// Phantom for value type
    pub(crate) _phantom: std::marker::PhantomData<V>,

    // === Lock-Free Infrastructure (per plan Phase 4) ===
    /// Lock-free root using PersistentCharNode with im::Vector for CAS operations.
    /// When present, `insert_cas()` uses this for lock-free concurrent inserts.
    pub(crate) lockfree_root: Option<nodes::AtomicNodePtr>,

    /// Lock-free cache for term lookups (DashMap for O(1) sharded access).
    pub(crate) lockfree_cache: Option<dashmap::DashMap<String, bool>>,

    /// Statistics: CAS retries for monitoring contention.
    pub(crate) cas_retries: std::sync::atomic::AtomicU64,
}

// Manual Debug implementation to avoid requiring Debug on BufferManager and WalWriter
impl<V: DictionaryValue, S: crate::persistent_artrie::block_storage::BlockStorage> std::fmt::Debug for PersistentARTrieChar<V, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentARTrieChar")
            .field("root", &self.root)
            .field("len", &self.len)
            .field("dirty", &self.dirty)
            .finish_non_exhaustive()
    }
}

impl<V: DictionaryValue> Default for PersistentARTrieChar<V> {
    #[allow(deprecated)]
    fn default() -> Self {
        Self::new()
    }
}

// === WalManaged Trait Implementation ===

impl<V: DictionaryValue, S: crate::persistent_artrie::block_storage::BlockStorage> WalManaged for PersistentARTrieChar<V, S> {
    fn wal_writer(&self) -> Option<&Arc<AsyncWalWriter>> {
        self.wal_writer.as_ref()
    }
}

// Note: Most methods are implemented in dict_impl_char.rs on `impl super::PersistentARTrieChar<V>`
// These wrapper methods provide convenience APIs

impl<V: DictionaryValue, S: crate::persistent_artrie::block_storage::BlockStorage> PersistentARTrieChar<V, S> {
    /// Check if trie has unsaved changes.
    ///
    /// Returns `true` if any modifications have been made since the last
    /// checkpoint (or creation).
    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(AtomicOrdering::Acquire)
    }

    /// Get the number of terms in the dictionary.
    #[inline]
    pub fn len(&self) -> usize {
        self.len.load(AtomicOrdering::Acquire)
    }

    /// Get the number of terms in the dictionary (alias for `len()`).
    #[inline]
    pub fn term_count(&self) -> usize {
        self.len.load(AtomicOrdering::Acquire)
    }

    /// Check if the dictionary is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len.load(AtomicOrdering::Acquire) == 0
    }

    /// Get the root node for dictionary traversal.
    pub fn root(&self) -> PersistentARTrieCharNode<V> {
        PersistentARTrieCharNode::from_trie(self)
    }

    /// Iterate over all terms in the dictionary.
    ///
    /// Returns an iterator over all terms. For large tries, consider using
    /// `iter_prefix("")` with batching instead.
    pub fn iter(&self) -> impl Iterator<Item = String> + '_ {
        // Use iter_prefix with empty prefix to get all terms
        self.iter_prefix("")
            .ok()
            .flatten()
            .unwrap_or_default()
            .into_iter()
    }

    /// Iterate over all terms and their values.
    ///
    /// Returns an iterator over (term, value) pairs. For large tries, consider
    /// using `iter_prefix_with_values("")` with batching instead.
    pub fn iter_with_values(&self) -> impl Iterator<Item = (String, V)> + '_
    where
        V: Clone,
    {
        // Use iter_prefix_with_values with empty prefix to get all terms
        self.iter_prefix_with_values("")
            .ok()
            .flatten()
            .unwrap_or_default()
            .into_iter()
    }

    /// Iterate over all terms with the given prefix (convenience wrapper returning iterator).
    ///
    /// Returns `None` if the prefix doesn't exist in the trie.
    pub fn iter_prefix_vec(&self, prefix: &str) -> Option<impl Iterator<Item = String> + '_> {
        self.iter_prefix(prefix)
            .ok()
            .flatten()
            .map(|v| v.into_iter())
    }

    /// Iterate over all terms with the given prefix, including values (convenience wrapper).
    ///
    /// Returns `None` if the prefix doesn't exist in the trie.
    pub fn iter_prefix_with_values_vec(&self, prefix: &str) -> Option<impl Iterator<Item = (String, V)> + '_>
    where
        V: Clone,
    {
        self.iter_prefix_with_values(prefix)
            .ok()
            .flatten()
            .map(|v| v.into_iter())
    }
}

// Note: Atomic operations (increment, upsert, compare_and_swap, fetch_add, get_or_insert)
// are implemented in dict_impl_char.rs on `impl super::PersistentARTrieChar<V>`

/// Build from an iterator of terms
impl<V: DictionaryValue + Default> FromIterator<String> for PersistentARTrieChar<V> {
    #[allow(deprecated)]
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        let mut trie = Self::new();
        for term in iter {
            trie.insert(&term);
        }
        trie
    }
}

impl<'a, V: DictionaryValue + Default> FromIterator<&'a str> for PersistentARTrieChar<V> {
    #[allow(deprecated)]
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        let mut trie = Self::new();
        for term in iter {
            trie.insert(term);
        }
        trie
    }
}

/// Node in the character-level trie for DictionaryNode trait
#[derive(Debug, Clone)]
pub struct PersistentARTrieCharNode<V: DictionaryValue = ()> {
    /// Reference to the node in the trie
    node: Option<*const CharTrieNodeInner<V>>,
    /// Whether this is the root node
    is_root: bool,
    /// Whether the root is empty (no children)
    root_empty: bool,
}

// Safety: The node pointer is valid for the lifetime of the trie
unsafe impl<V: DictionaryValue> Send for PersistentARTrieCharNode<V> {}
unsafe impl<V: DictionaryValue> Sync for PersistentARTrieCharNode<V> {}

impl<V: DictionaryValue> PersistentARTrieCharNode<V> {
    /// Create a node from the trie's root
    fn from_trie<S: crate::persistent_artrie::block_storage::BlockStorage>(trie: &PersistentARTrieChar<V, S>) -> Self {
        match &trie.root {
            CharTrieRoot::Empty => Self {
                node: None,
                is_root: true,
                root_empty: true,
            },
            CharTrieRoot::Node(node) => Self {
                node: Some(node.as_ref() as *const _),
                is_root: true,
                root_empty: false,
            },
        }
    }

    /// Create a node from a node pointer
    fn from_ptr(ptr: *const CharTrieNodeInner<V>) -> Self {
        Self {
            node: Some(ptr),
            is_root: false,
            root_empty: false,
        }
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentARTrieCharNode<V> {
    type Unit = char;

    fn is_final(&self) -> bool {
        if self.root_empty {
            return false;
        }
        if let Some(ptr) = self.node {
            // Safety: pointer is valid for the lifetime of the trie
            unsafe { (*ptr).is_final() }
        } else {
            false
        }
    }

    fn transition(&self, label: char) -> Option<Self> {
        if self.root_empty {
            return None;
        }
        if let Some(ptr) = self.node {
            // Safety: pointer is valid for the lifetime of the trie
            unsafe {
                (*ptr).get_child(label)
                    .map(|child| Self::from_ptr(child as *const _))
            }
        } else {
            None
        }
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (char, Self)> + '_> {
        if self.root_empty || self.node.is_none() {
            return Box::new(std::iter::empty());
        }

        let ptr = self.node.unwrap();
        // Safety: pointer is valid for the lifetime of the trie
        let edges: Vec<_> = unsafe {
            (*ptr).iter_children()
                .map(|(c, child)| (c, Self::from_ptr(child as *const _)))
                .collect()
        };
        Box::new(edges.into_iter())
    }
}

impl<V: DictionaryValue, S: crate::persistent_artrie::block_storage::BlockStorage> Dictionary for PersistentARTrieChar<V, S> {
    type Node = PersistentARTrieCharNode<V>;

    fn root(&self) -> Self::Node {
        PersistentARTrieChar::root(self)
    }

    fn contains(&self, term: &str) -> bool {
        PersistentARTrieChar::contains(self, term)
    }

    #[inline]
    fn len(&self) -> Option<usize> {
        Some(self.len.load(AtomicOrdering::Acquire))
    }
}

impl<V: DictionaryValue + Clone, S: crate::persistent_artrie::block_storage::BlockStorage> MappedDictionary for PersistentARTrieChar<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<V> {
        self.get(term).cloned()
    }
}

// Note: MutableMappedDictionary is NOT implemented for PersistentARTrieChar because
// its methods require &self (interior mutability) but PersistentARTrieChar::insert_with_value
// requires &mut self. Use SharedCharTrie for interior mutability.

// ============================================================================
// SharedCharARTrie trait implementations
// ============================================================================

impl<V: DictionaryValue> Dictionary for SharedCharARTrie<V> {
    type Node = PersistentARTrieCharNode<V>;

    fn root(&self) -> Self::Node {
        let guard = self.read();
        PersistentARTrieCharNode::from_trie(&guard)
    }

    fn contains(&self, term: &str) -> bool {
        let guard = self.read();
        guard.contains(term)
    }

    #[inline]
    fn len(&self) -> Option<usize> {
        let guard = self.read();
        Some(guard.len.load(AtomicOrdering::Acquire))
    }
}

impl<V: DictionaryValue + Clone> MappedDictionary for SharedCharARTrie<V> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<V> {
        let guard = self.read();
        guard.get(term).cloned()
    }
}

impl<V: DictionaryValue + Clone> MutableMappedDictionary for SharedCharARTrie<V> {
    fn insert_with_value(&self, term: &str, value: V) -> bool {
        let mut guard = self.write();
        guard.insert_with_value(term, value).unwrap_or(false)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let other_guard = other.read();
        let mut self_guard = self.write();
        self_guard.merge_from(&*other_guard, merge_fn).unwrap_or(0)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        let mut guard = self.write();
        if let Some(existing) = guard.get(term).cloned() {
            let mut value = existing;
            update_fn(&mut value);
            guard.upsert(term, value).unwrap_or(false);
            false // Term existed
        } else {
            guard.insert_with_value(term, default_value).unwrap_or(false);
            true // New term
        }
    }
}

// ============================================================================
// Zipper Implementation
// ============================================================================

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
        self.node.edges().map(move |(c, child)| {
            let mut new_path = path.clone();
            new_path.push(c);
            (c, Self {
                node: child,
                path_vec: new_path,
            })
        }).collect::<Vec<_>>().into_iter()
    }

    fn path(&self) -> Vec<char> {
        self.path_vec.clone()
    }
}

impl<V: DictionaryValue + Clone> ValuedDictZipper for PersistentARTrieCharZipper<V> {
    type Value = V;

    fn value(&self) -> Option<V> {
        if !self.node.is_final() {
            return None;
        }
        if let Some(ptr) = self.node.node {
            // Safety: pointer is valid for the lifetime of the trie
            unsafe { (*ptr).value.clone() }
        } else {
            None
        }
    }
}

// ============================================================================
// ARTrie Trait Implementation (on SharedCharTrie for Clone requirement)
// ============================================================================

impl<V: DictionaryValue> crate::artrie_trait::ARTrie for SharedCharARTrie<V> {
    type Unit = char;
    type Value = V;

    fn create<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::error::Result<Self> {
        PersistentARTrieChar::create(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn create_with_slot_tracking<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::error::Result<Self> {
        PersistentARTrieChar::create_with_slot_tracking(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::error::Result<Self> {
        PersistentARTrieChar::open(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open_with_slot_tracking<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::error::Result<Self> {
        PersistentARTrieChar::open_with_slot_tracking(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open_with_recovery<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::error::Result<(Self, crate::persistent_artrie::recovery::RecoveryReport)> {
        PersistentARTrieChar::open_with_recovery(path).map(|(t, r)| (Arc::new(RwLock::new(t)), r))
    }

    fn open_with_recovery_and_slot_tracking<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::error::Result<(Self, crate::persistent_artrie::recovery::RecoveryReport)> {
        let (trie, report) = PersistentARTrieChar::open_with_recovery(path)?;
        if let Some(ref am) = trie.arena_manager {
            am.write().enable_slot_tracking();
        }
        Ok((Arc::new(RwLock::new(trie)), report))
    }

    fn enable_slot_tracking(&self) {
        let guard = self.read();
        if let Some(ref am) = guard.arena_manager {
            am.write().enable_slot_tracking();
        }
    }

    fn flush_sequential(&self) -> crate::persistent_artrie::error::Result<()> {
        let guard = self.read();
        if let Some(ref am) = guard.arena_manager {
            am.write().flush_sequential()?;
        }
        Ok(())
    }

    fn insert(&self, term: &str) -> bool
    where
        Self::Value: Default,
    {
        let mut guard = self.write();
        guard.insert(term).unwrap_or(false)
    }

    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        let mut guard = self.write();
        guard.insert_with_value(term, value).unwrap_or(false)
    }

    fn contains(&self, term: &str) -> bool {
        let guard = self.read();
        guard.contains(term)
    }

    fn get_value(&self, term: &str) -> Option<Self::Value>
    where
        V: Clone,
    {
        let guard = self.read();
        guard.get(term).cloned()
    }

    fn remove(&self, term: &str) -> bool {
        let mut guard = self.write();
        guard.remove(term).unwrap_or(false)
    }

    #[inline]
    fn len(&self) -> usize {
        let guard = self.read();
        guard.len.load(AtomicOrdering::Acquire)
    }

    fn checkpoint(&self) -> crate::persistent_artrie::error::Result<()> {
        let mut guard = self.write();
        guard.checkpoint()
    }

    fn is_dirty(&self) -> bool {
        let guard = self.read();
        guard.is_dirty()
    }

    fn remove_prefix(&self, prefix: &str) -> usize {
        let mut guard = self.write();
        guard.remove_prefix(prefix).unwrap_or(0)
    }

    fn iter_prefix(&self, prefix: &str) -> Option<Box<dyn Iterator<Item = String> + '_>> {
        // Note: This returns owned data because we need to release the lock
        let guard = self.read();
        guard.iter_prefix(prefix)
            .ok()
            .flatten()
            .map(|v| Box::new(v.into_iter()) as Box<dyn Iterator<Item = String> + '_>)
    }

    fn sync(&self) -> crate::persistent_artrie::error::Result<()> {
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

    fn durability_policy(&self) -> crate::persistent_artrie::dict_impl::DurabilityPolicy {
        let guard = self.read();
        guard.durability_policy()
    }

    fn upsert(&self, term: &str, value: Self::Value) -> crate::persistent_artrie::error::Result<bool> {
        let mut guard = self.write();
        guard.upsert(term, value)
    }

    fn increment(&self, term: &str, delta: i64) -> crate::persistent_artrie::error::Result<i64> {
        let mut guard = self.write();
        guard.increment(term, delta)
    }
}

// ============================================================================
// EvictableARTrie Trait Implementation (on SharedCharARTrie)
// ============================================================================

impl<V: DictionaryValue> crate::artrie_trait::EvictableARTrie for SharedCharARTrie<V> {
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

// Helper methods for eviction on PersistentARTrieChar
impl<V: DictionaryValue, S: crate::persistent_artrie::block_storage::BlockStorage> PersistentARTrieChar<V, S> {
    /// Evict a single node at the given path, replacing it with a DiskRef.
    ///
    /// Returns `true` if the node was successfully evicted, `false` if the
    /// node was not found or was already a DiskRef.
    pub(crate) fn evict_node_at_path(&mut self, _path: &[char], _disk_ptr: crate::persistent_artrie::swizzled_ptr::SwizzledPtr) -> bool {
        // Char trie eviction is more complex due to different node structure
        // This is a simplified placeholder - full implementation would need to
        // navigate the char trie structure
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(deprecated)]
    fn test_new_empty() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        assert!(trie.is_empty());
        assert_eq!(trie.len(), 0);
    }

    #[test]
    #[allow(deprecated)]
    fn test_insert_ascii() {
        let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        assert!(trie.insert("hello").expect("insert failed"));
        assert!(trie.insert("world").expect("insert failed"));
        assert!(!trie.insert("hello").expect("insert failed")); // Duplicate
        assert_eq!(trie.len(), 2);
    }

    #[test]
    #[allow(deprecated)]
    fn test_insert_unicode() {
        let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        assert!(trie.insert("héllo").expect("insert failed")); // é is one character
        assert!(trie.insert("日本語").expect("insert failed")); // Japanese characters
        assert!(trie.insert("emoji😀").expect("insert failed")); // Emoji
        assert_eq!(trie.len(), 3);
    }

    #[test]
    #[allow(deprecated)]
    fn test_contains() {
        let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        let _ = trie.insert("hello");
        let _ = trie.insert("héllo");

        assert!(trie.contains("hello"));
        assert!(trie.contains("héllo"));
        assert!(!trie.contains("helo"));
        assert!(!trie.contains("hello ")); // Trailing space
    }

    #[test]
    #[allow(deprecated)]
    fn test_value_storage() {
        let mut trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
        let _ = trie.insert_with_value("one", 1);
        let _ = trie.insert_with_value("two", 2);
        let _ = trie.insert_with_value("three", 3);

        assert_eq!(trie.get("one"), Some(&1));
        assert_eq!(trie.get("two"), Some(&2));
        assert_eq!(trie.get("three"), Some(&3));
        assert_eq!(trie.get("four"), None);
    }

    #[test]
    #[allow(deprecated)]
    fn test_unicode_correctness() {
        // This test verifies that multi-byte characters are treated as single units
        let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        let _ = trie.insert("¡");

        let root = trie.root();
        // Should have exactly one edge (for '¡'), not two edges (for the bytes)
        let edges: Vec<_> = root.edges().collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].0, '¡');
    }

    #[test]
    #[allow(deprecated)]
    fn test_from_iter() {
        let terms = vec!["alpha", "beta", "gamma"];
        let trie: PersistentARTrieChar<()> = terms.into_iter().collect();
        assert_eq!(trie.len(), 3);
        assert!(trie.contains("alpha"));
        assert!(trie.contains("beta"));
        assert!(trie.contains("gamma"));
    }

    #[test]
    #[allow(deprecated)]
    fn test_zipper() {
        let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        let _ = trie.insert("hello");
        let _ = trie.insert("help");

        let zipper = PersistentARTrieCharZipper::new(&trie);
        let zipper = zipper.descend('h').expect("should have 'h'");
        let zipper = zipper.descend('e').expect("should have 'e'");
        let zipper = zipper.descend('l').expect("should have 'l'");

        let edges: Vec<_> = zipper.children().map(|(c, _)| c).collect();
        assert_eq!(edges.len(), 2); // 'l' and 'p'
    }

    #[test]
    #[allow(deprecated)]
    fn test_iter() {
        let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        let _ = trie.insert("apple");
        let _ = trie.insert("banana");
        let _ = trie.insert("cherry");

        let terms: Vec<_> = trie.iter().collect();
        assert_eq!(terms.len(), 3);
        assert!(terms.contains(&"apple".to_string()));
        assert!(terms.contains(&"banana".to_string()));
        assert!(terms.contains(&"cherry".to_string()));
    }
}
