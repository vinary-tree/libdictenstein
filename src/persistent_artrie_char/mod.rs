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

// Page-aware prefix-iteration result types (Phase-6 split out of dict_impl_char).
pub mod prefix_term;

// EnhancedRecoveryMode + EnhancedRecoveryStats (Phase-6 split out of dict_impl_char).
pub mod recovery_stats;

// CharTrieFileHeader (Phase-6 split out of dict_impl_char).
pub mod file_header;

// CharDocumentTransaction (Phase-6 split out of dict_impl_char).
pub mod transactions;

// Char-side MVCC TrieRoot implementation (paired with generic core mvcc).
pub mod mvcc;

// Disk-backed implementation
pub mod dict_impl_char;

// IoUringDiskManager-specific constructors (Phase-6 split out of dict_impl_char).
#[cfg(feature = "io-uring-backend")]
pub mod io_uring_ctor;

// MmapDiskManager-specific constructors (Phase-6 split out of dict_impl_char).
pub mod mmap_ctor;

// Disk loading + child resolution helpers (Phase-6 split out of dict_impl_char).
pub mod disk_io;

// Lock-free CAS-based concurrent insert/contains/get/increment (Phase-6 split).
pub mod lockfree_cas;

// Public read-path API + optimistic concurrency variants (Phase-6 split).
pub mod query_api;

// Prefix navigation + term collection helpers (Phase-6 split out of dict_impl_char).
pub mod prefix_helpers;

// Public prefix iter + remove API (Phase-6 split out of dict_impl_char).
pub mod prefix_api;

// Merge API (merge_from + variants) — Phase-6 split out of dict_impl_char.
pub mod merge_api;

// Document-transaction execution (begin/tx_insert/commit/abort) — Phase-6 split.
pub mod document_tx;

// Batch-insert API (insert_batch + 9 variants) — Phase-6 split.
pub mod batch_insert;

// Rayon-based parallel merge (feature-gated) — Phase-6 split.
#[cfg(feature = "parallel-merge")]
pub mod parallel_merge;

// Observability / durability / group-commit / memory-monitor / cache-stats API.
pub mod observability;

// Epoch-based checkpoint tracking (Phase-6 split out of dict_impl_char).
pub mod epoch_checkpointing;

// Atomic read-modify-write operations (Phase-6 split out of dict_impl_char).
pub mod atomic_ops;

// On-disk persistence (checkpoint + persist_to_disk + serialize_*) — Phase-6 split.
pub mod persist;

// Prefetch helpers (stats + bounded depth) — Phase-6 split out of dict_impl_char.
pub mod prefetch_api;

// WAL + durability helpers (append_to_wal, sync_wal, *durability_policy).
pub mod wal_helpers;

// Public mutation API (insert / insert_with_value / remove) — Phase-6 split.
pub mod mutation_api;

// Core mutation implementations (_no_wal helpers) — Phase-6 split.
pub mod mutation_core;

/// Epoch-deferred reclamation of evicted subtrees (eviction-safety for the
/// lock-free `DictionaryNode` walk).
pub(crate) mod reclaim;

// In-crate white-box tests for the eviction-registry wiring (commit f10c43e):
// state oracle (slot swizzled -> on-disk) + the async eviction path, both of
// which need private node/coordinator internals.
#[cfg(test)]
mod eviction_registry_tests;

// Re-export shared types (always available)
pub use types::{
    CharTrieFileHeader, CharTrieNodeInner, CharTrieRoot, EnhancedRecoveryMode,
    EnhancedRecoveryStats, CHAR_FILE_HEADER_SIZE, CHAR_HEADER_VERSION_V1, CHAR_HEADER_VERSION_V2,
    CHAR_TRIE_MAGIC, DEFAULT_CHAR_BUFFER_POOL_SIZE,
};

// Re-export disk-backed types (feature-gated)
pub use types::{PrefixTermWithArena, PrefixTermWithValueAndArena};

// Re-export disk-backed implementation types
pub use dict_impl_char::{
    // Transaction types
    CharDocumentTransaction,
    DurabilityPolicy,
    TransactionState,
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
    serialize_char_node, SerializedCharNodeHeader, CHAR_FORMAT_VERSION, CHAR_NODE_MAGIC,
    CHAR_SERIALIZED_HEADER_SIZE,
};

// Re-export compact serialization (under feature flag)
pub use serialization_char::{
    char_compact_serialized_size, char_from_bytes_compact, char_to_bytes_compact,
};

// Re-export compact encoding utilities (under feature flag)
pub use compact_encoding::{
    determine_key_width, determine_ptr_width, CompactHeader, DecodedCompactNode,
    COMPACT_NODE_TYPE_BUCKET, COMPACT_NODE_TYPE_N16, COMPACT_NODE_TYPE_N4, COMPACT_NODE_TYPE_N48,
};

// Re-export arena types (under feature flag)
pub use arena::{
    ArenaHeader, CharNodeArena, CharNodeArenaV2, SlotEntry, VarintSlotEntry, ARENA_MAGIC,
    ARENA_MAGIC_V2, ARENA_VERSION, ARENA_VERSION_V2, FLAG_VARINT_DIRECTORY, HEADER_SIZE,
    MIN_FREE_SPACE, SLOT_SIZE,
};

// Re-export arena manager types (under feature flag)
pub use arena_manager::{
    ArenaManager, ArenaSlot, ArenaStats, FlushConfig, FlushStats, ReservedSlots,
};

// Re-export per-node logging types (under feature flag)
pub use per_node_log_char::{
    CharInlineLog,
    CharInlineLogIter,
    CharLogIterExt,
    CharLogWriter,
    CharNodeLogEntry,
    // Re-export node-agnostic types from the base module
    DirtyNodeTracker,
    NodeId,
    PageId,
    PerNodeLogConfig,
    PerNodeLogStats,
    PerNodeLogStatsAtomic,
};

// Re-export traversal context types (under feature flag)
pub use traversal_context::{LightweightTraversalContext, TraversalContext, TraversalStats};

// Re-export dirty tracker types (under feature flag)
pub use dirty_tracker::{BatchDirtyTracker, DirtyTracker, DirtyTrackerStats};

// Re-export deduplication types (under feature flag)
pub use dedup::{
    BatchDeduplicator, DeduplicatingArenaManager, DeduplicatorStats, NodeDeduplicator,
};

// Re-export relative encoding types (under feature flag)
pub use relative_encoding::{
    decode_child_pointer, decode_children, decode_sequential_siblings, encode_child_pointer,
    encode_children, encode_sequential_siblings, encoded_size, is_same_arena,
    try_decode_child_pointer, try_decode_children, try_decode_full, try_decode_relative,
    try_decode_sequential_siblings, try_encode_child_pointer, try_encode_children,
    try_encode_sequential_siblings, try_encoded_size, RelativeEncodingError,
    RelativeEncodingResult, CROSS_ARENA_SIZE, FLAG_CROSS_ARENA, FLAG_RELATIVE_OFFSETS,
    FLAG_SEQUENTIAL_SIBLINGS,
};

// Re-export recovery types (under feature flag)
pub use recovery::{
    detect_corruption,
    find_wal_archive_segments,
    rebuild_from_wal_segments,
    CorruptionInfo,
    CorruptionType,
    // Re-exported from 1-byte implementation (node-agnostic)
    IncrementalRecovery,
    RecoveredOperation,
    RecoveredState,
    RecoveryError,
    RecoveryManager,
    RecoveryMode,
    RecoveryPolicy,
    RecoveryReport,
    RecoveryStats,
};

// Re-export eviction types from byte-level implementation (shared)
pub use crate::persistent_artrie::eviction::{
    AccessTracker, DiskLocationRegistry, EvictionConfig, EvictionCoordinator, EvictionStats,
    EvictionUrgency, LruRegistry,
};

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::persistent_artrie::wal::AsyncWalWriter;
use crate::persistent_artrie::wal_managed::WalManaged;
use crate::value::DictionaryValue;
use crate::zipper::{DictZipper, ValuedDictZipper};
use crate::{
    Dictionary, DictionaryNode, MappedDictionary, MappedDictionaryNode, MutableMappedDictionary,
};

/// Thread-safe wrapper for `PersistentARTrieChar`.
///
/// This type alias provides `Arc<RwLock<...>>` semantics for concurrent access
/// to the disk-backed character trie.
pub type SharedCharARTrie<V, S = crate::persistent_artrie::disk_manager::MmapDiskManager> =
    Arc<RwLock<PersistentARTrieChar<V, S>>>;

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
    /// Epoch manager for safe memory reclamation.
    ///
    /// `Arc` so the eviction coordinator can SHARE this exact manager (see
    /// `enable_eviction`) and a `DictionaryNode` walk can pin it for the walk's
    /// lifetime (see `CharWalkGuard`). A reader's `enter_read` and the
    /// coordinator's `wait_for_quiescence` must observe the same `active_readers`
    /// counter, or quiescence is vacuous and eviction frees nodes out from under a
    /// live walk.
    pub(crate) epoch_manager: Arc<crate::persistent_artrie::concurrency::EpochManager>,
    /// Epoch-deferred retire list for evicted subtrees. Eviction `unswizzle`s a
    /// subtree's parent slot then retires the subtree root here instead of freeing
    /// it inline; the reclaimer frees it only after a quiescence drain proves no
    /// live `DictionaryNode` walk holds a pointer into it. See [`reclaim`].
    pub(crate) retire_list: Arc<reclaim::CharRetireList<V>>,
    /// Monotonic counter bumped on every durable in-place structural mutation (via
    /// `invalidate_eviction_registry` at the `append_to_wal` chokepoint). A
    /// `DictionaryNode` walk snapshots it at `root()`; in debug builds each handle
    /// rechecks it before dereferencing its raw node pointer and panics on a
    /// mismatch — surfacing the contract violation "a handle was used across a
    /// concurrent structural mutation" (which would dangle the raw pointer) as a
    /// loud failure instead of silent UB. NOT bumped by eviction (EBR-safe) or by
    /// faulting (a read-path operation), so a walk concurrent with eviction/faulting
    /// does not trip it.
    pub(crate) structural_generation: std::sync::atomic::AtomicU64,
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
    /// Epoch-based checkpoint manager for WAL/metadata tracking.
    ///
    /// When enabled, the checkpoint manager tracks operation counts and WAL size,
    /// advancing epoch metadata based on configurable thresholds. Explicit
    /// forced epoch checkpoints publish trie data before durable metadata.
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
impl<V: DictionaryValue, S: crate::persistent_artrie::block_storage::BlockStorage> std::fmt::Debug
    for PersistentARTrieChar<V, S>
{
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

impl<V: DictionaryValue, S: crate::persistent_artrie::block_storage::BlockStorage> Drop
    for PersistentARTrieChar<V, S>
{
    /// Stop + join the trie's background daemon threads before the rest of the
    /// trie (mmap buffers, WAL files, arena) is torn down. See
    /// [`PersistentARTrieChar::close`].
    fn drop(&mut self) {
        self.close();
    }
}

// === WalManaged Trait Implementation ===

impl<V: DictionaryValue, S: crate::persistent_artrie::block_storage::BlockStorage> WalManaged
    for PersistentARTrieChar<V, S>
{
    fn wal_writer(&self) -> Option<&Arc<AsyncWalWriter>> {
        self.wal_writer.as_ref()
    }
}

// Note: Most methods are implemented in dict_impl_char.rs on `impl super::PersistentARTrieChar<V>`
// These wrapper methods provide convenience APIs

impl<V: DictionaryValue, S: crate::persistent_artrie::block_storage::BlockStorage>
    PersistentARTrieChar<V, S>
{
    /// Stop and join all background daemon threads owned by this trie: the
    /// WAL-sync thread, the eviction thread, and the memory-pressure monitor.
    ///
    /// Idempotent and safe to call repeatedly — each underlying
    /// `stop()`/`shutdown()` takes its `JoinHandle` via `Option::take`, so a
    /// second call is a no-op. Called automatically by `Drop`, and exposed
    /// publicly so an owner can reclaim the threads *deterministically* (e.g.
    /// before swapping a freshly-rebuilt trie into a cache) instead of relying
    /// on `Arc`-refcount drop order.
    ///
    /// Historically each background thread captured a strong `Arc` to its
    /// manager, so the manager's `Drop` could never run and the OS thread
    /// leaked once per trie instance (≈3 threads per trie). The workers now
    /// hold only a `Weak`, so this teardown — and the managers' own `Drop`
    /// backstops — actually run.
    pub fn close(&self) {
        if let Some(coordinator) = &self.eviction_coordinator {
            coordinator.shutdown();
        }
        if let Some(monitor) = &self.memory_monitor {
            monitor.shutdown();
        }
        if let Some(wal) = &self.wal_writer {
            wal.stop_sync();
        }
    }

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
    pub fn iter_prefix_with_values_vec(
        &self,
        prefix: &str,
    ) -> Option<impl Iterator<Item = (String, V)> + '_>
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

/// Object-safe (over the block-storage parameter `S`) faulting capability that
/// lets a [`PersistentARTrieCharNode`] resolve on-disk (swizzled) child pointers
/// during a lock-free `DictionaryNode` walk without naming `S`.
///
/// After a trie is reopened from disk (or after eviction), a node's child slots
/// hold swizzled [`SwizzledPtr`](crate::persistent_artrie::swizzled_ptr::SwizzledPtr)s
/// whose `as_ptr()` returns `None`. The non-faulting
/// `CharTrieNodeInner::{get_child, iter_children}` therefore drop those children,
/// which silently truncates any `DictionaryNode`-driven traversal (e.g.
/// liblevenshtein's `Transducer`) to the resident subtree — the root alone after
/// a fresh reopen. The node carries a type-erased pointer to this trait so
/// `transition`/`edges` can fault children in via the trie's
/// `get_child_lazy_u32`/`resolve_swizzled_ptr`, exactly as the `iter_prefix` path
/// already does. Implemented for every `PersistentARTrieChar<V, S>`; the concrete
/// `S` is erased at the `from_trie` call site (the only place that names it).
pub(crate) trait CharNodeFaulter<V: DictionaryValue> {
    /// Fault in (loading from disk if the child is swizzled) and return the child
    /// of `node` reached by `key` (a `char` as `u32`). Returns `None` when there
    /// is no such edge or the load fails — both degrade to "no transition", the
    /// only non-panicking mapping available through the infallible
    /// `DictionaryNode` API (a transient I/O error yields a miss, never UB).
    fn fault_child_u32(
        &self,
        node: &CharTrieNodeInner<V>,
        key: u32,
    ) -> Option<&CharTrieNodeInner<V>>;

    /// Fault in (loading from disk if swizzled) and return the child behind an
    /// already-located child slot, or `None` if it is null/unresolvable.
    fn fault_slot(
        &self,
        slot: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
    ) -> Option<&CharTrieNodeInner<V>>;

    /// Current structural generation of the owning trie. Used only by the
    /// debug-only walk-contract detector (see
    /// `PersistentARTrieChar::structural_generation`).
    fn structural_generation(&self) -> u64;
}

impl<V: DictionaryValue, S: crate::persistent_artrie::block_storage::BlockStorage>
    CharNodeFaulter<V> for PersistentARTrieChar<V, S>
{
    #[inline]
    fn fault_child_u32(
        &self,
        node: &CharTrieNodeInner<V>,
        key: u32,
    ) -> Option<&CharTrieNodeInner<V>> {
        self.get_child_lazy_u32(node, key).ok().flatten()
    }

    #[inline]
    fn fault_slot(
        &self,
        slot: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
    ) -> Option<&CharTrieNodeInner<V>> {
        self.resolve_swizzled_ptr(slot).ok()
    }

    #[inline]
    fn structural_generation(&self) -> u64 {
        self.structural_generation
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

/// RAII pin carried by every [`PersistentARTrieCharNode`] handle produced by a
/// [`SharedCharARTrie`] walk. While ANY handle of the walk (the root, its
/// transitive children, and all their clones) is alive:
///
/// - `_keepalive` holds a type-erased `Arc` clone of the owning
///   `Arc<RwLock<PersistentARTrieChar<V, S>>>`, keeping the trie allocation alive
///   at a fixed address — so the node's raw `node`/`faulter` pointers stay valid
///   even if the caller drops its own trie handle mid-walk.
/// - `_epoch` pins the trie's (shared) `EpochManager` — one `enter_read` on
///   construction, one `exit_read` on `Drop` — so the eviction coordinator's
///   `wait_for_quiescence` blocks until this walk drains before it reclaims any
///   node the walk may still hold.
///
/// Shared behind an `Arc` and propagated by clone, so the pin is acquired once at
/// `root()` and released exactly once when the last handle of the walk drops.
#[allow(dead_code)] // `_epoch`/`_keepalive` held for their `Drop` side effects (RAII);
                    // `gen_snapshot` read only by the debug-only contract detector
struct CharWalkGuard {
    _epoch: crate::persistent_artrie_core::mvcc::EpochGuard,
    _keepalive: Arc<dyn std::any::Any + Send + Sync>,
    /// `PersistentARTrieChar::structural_generation` snapshot at `root()`; the
    /// debug-only detector compares the trie's current generation against this on
    /// each handle deref to catch a handle used across a structural mutation.
    gen_snapshot: u64,
}

impl std::fmt::Debug for CharWalkGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CharWalkGuard")
            .field("epoch", &self._epoch.epoch())
            .field("gen", &self.gen_snapshot)
            .finish_non_exhaustive()
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
    /// Type-erased handle to the owning trie's faulting capability. Used by
    /// `transition`/`edges` to load on-disk (swizzled) children during a
    /// lock-free transducer walk. Validity is guaranteed by `pin` (below): the
    /// pin's `_keepalive` Arc keeps the trie — and thus this pointer's target —
    /// alive for the walk. `None` only for a node never built from a trie.
    faulter: Option<*const dyn CharNodeFaulter<V>>,
    /// Walk pin (see [`CharWalkGuard`]). `Some` for handles from
    /// [`SharedCharARTrie::root`] — the only path where concurrent eviction can
    /// run — where it keeps the trie alive and pins the shared epoch so eviction
    /// cannot reclaim a node this walk holds until the walk drains. `None` for an
    /// owned `PersistentARTrieChar::root()` walk (an owned trie cannot enable
    /// eviction, so nothing frees concurrently). Propagated by clone to every
    /// child handle; released when the last handle drops.
    pin: Option<Arc<CharWalkGuard>>,
}

// Safety: the `node` and `faulter` raw pointers reference trie-owned storage.
// They are kept valid for the walk by `pin` (a `CharWalkGuard`): the pin holds an
// `Arc` clone of the owning `Arc<RwLock<trie>>` (so the allocation stays put) and
// pins the shared `EpochManager` (so eviction's `wait_for_quiescence` drains this
// walk before reclaiming any node it holds). Only `&`-references are formed on the
// read path (no `&mut` aliasing); faulting transitions slots on-disk -> in-memory
// via atomic CAS. For an owned-trie walk (`pin == None`) no concurrent free is
// possible, so the pointers are valid for as long as the caller holds the trie.
unsafe impl<V: DictionaryValue> Send for PersistentARTrieCharNode<V> {}
unsafe impl<V: DictionaryValue> Sync for PersistentARTrieCharNode<V> {}

impl<V: DictionaryValue> PersistentARTrieCharNode<V> {
    /// Create a node from the trie's root
    fn from_trie<S: crate::persistent_artrie::block_storage::BlockStorage>(
        trie: &PersistentARTrieChar<V, S>,
    ) -> Self {
        // Erase `S`: capture the trie as a `dyn CharNodeFaulter<V>` so children
        // can be faulted in during traversal without the node naming `S`.
        let faulter: Option<*const dyn CharNodeFaulter<V>> =
            Some(trie as &dyn CharNodeFaulter<V> as *const dyn CharNodeFaulter<V>);
        // `pin` is `None` here; `SharedCharARTrie::root` attaches the walk pin
        // after building the root node (only it has the owning `Arc<RwLock>`).
        match &trie.root {
            CharTrieRoot::Empty => Self {
                node: None,
                is_root: true,
                root_empty: true,
                faulter,
                pin: None,
            },
            CharTrieRoot::Node(node) => Self {
                node: Some(node.as_ref() as *const _),
                is_root: true,
                root_empty: false,
                faulter,
                pin: None,
            },
        }
    }

    /// Create a child node from a node pointer, inheriting the parent's faulter
    /// and walk pin so descent can continue to load on-disk grandchildren and the
    /// epoch stays pinned for the whole walk.
    fn from_ptr(
        ptr: *const CharTrieNodeInner<V>,
        faulter: Option<*const dyn CharNodeFaulter<V>>,
        pin: Option<Arc<CharWalkGuard>>,
    ) -> Self {
        Self {
            node: Some(ptr),
            is_root: false,
            root_empty: false,
            faulter,
            pin,
        }
    }

    /// Debug-only detector for the walk contract: a `DictionaryNode` handle must
    /// not be used across a structural mutation of the same trie. Compares the
    /// trie's current `structural_generation` against the snapshot taken at
    /// `root()`; a mismatch means a concurrent in-place insert/remove ran while
    /// this handle was alive — the handle's raw pointer may dangle. Compiled to a
    /// no-op in release builds (the contract, not the detector, is the guarantee).
    #[inline]
    fn debug_check_no_concurrent_mutation(&self) {
        #[cfg(debug_assertions)]
        if let (Some(pin), Some(faulter)) = (self.pin.as_ref(), self.faulter) {
            // Safety: `faulter` points at the trie, kept alive by the pin's `_keepalive`.
            let current = unsafe { (*faulter).structural_generation() };
            debug_assert_eq!(
                current, pin.gen_snapshot,
                "a `DictionaryNode` handle was used across a structural mutation of \
                 the same trie — walk-contract violation: a handle must not outlive a \
                 concurrent insert/remove on the same trie (concurrent bulk writes \
                 must go through the lock-free overlay). The handle's raw pointer may \
                 now dangle."
            );
        }
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentARTrieCharNode<V> {
    type Unit = char;

    fn is_final(&self) -> bool {
        self.debug_check_no_concurrent_mutation();
        if self.root_empty {
            return false;
        }
        if let Some(ptr) = self.node {
            // Safety: pointer validity is guaranteed by `pin` for the walk.
            unsafe { (*ptr).is_final() }
        } else {
            false
        }
    }

    fn transition(&self, label: char) -> Option<Self> {
        self.debug_check_no_concurrent_mutation();
        if self.root_empty {
            return None;
        }
        let ptr = self.node?;
        // Safety: `ptr` references trie-owned storage, valid for the trie's lifetime.
        let node_ref = unsafe { &*ptr };
        match self.faulter {
            // Fault the child in (loading from disk if its slot is swizzled) so
            // traversal descends correctly on a reopened/evicted trie. A missing
            // edge or a transient load error both map to `None` (no transition).
            Some(faulter) => {
                // Safety: `faulter` points at the owning trie, valid for its lifetime.
                let faulter_ref = unsafe { &*faulter };
                faulter_ref
                    .fault_child_u32(node_ref, label as u32)
                    .map(|child| Self::from_ptr(child as *const _, self.faulter, self.pin.clone()))
            }
            // No faulter (node never built from a trie): resident-only lookup,
            // never worse than the previous behavior.
            None => node_ref
                .get_child(label)
                .map(|child| Self::from_ptr(child as *const _, None, self.pin.clone())),
        }
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (char, Self)> + '_> {
        self.debug_check_no_concurrent_mutation();
        if self.root_empty || self.node.is_none() {
            return Box::new(std::iter::empty());
        }
        let ptr = self.node.unwrap();
        // Safety: `ptr` references trie-owned storage, valid for the trie's lifetime.
        let node_ref = unsafe { &*ptr };

        let Some(faulter) = self.faulter else {
            // Resident-only fallback (no faulter); preserves the prior behavior.
            let edges: Vec<_> = node_ref
                .iter_children()
                .map(|(c, child)| (c, Self::from_ptr(child as *const _, None, self.pin.clone())))
                .collect();
            return Box::new(edges.into_iter());
        };
        // Safety: `faulter` points at the owning trie, valid for its lifetime.
        let faulter_ref = unsafe { &*faulter };

        // Iterate ALL child slots (including swizzled/on-disk ones) via the inner
        // `CharNode` iterator — unlike `CharTrieNodeInner::iter_children`, which
        // drops swizzled slots through `as_ptr` — and fault each in on demand.
        // Preallocated to the known child count.
        let mut edges = Vec::with_capacity(node_ref.num_children());
        for (key, slot) in node_ref.node.iter_children() {
            if slot.is_null() {
                continue;
            }
            let Some(c) = char::from_u32(key) else {
                continue;
            };
            if let Some(child) = faulter_ref.fault_slot(slot) {
                edges.push((
                    c,
                    Self::from_ptr(child as *const _, Some(faulter), self.pin.clone()),
                ));
            }
        }
        Box::new(edges.into_iter())
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentARTrieCharNode<V> {
    type Value = V;

    /// The value stored at this node (if it terminates a key). Reads the
    /// node's `value` field directly — this unlocks liblevenshtein's
    /// value-aware transducer queries (value-yielding + `query_filtered` /
    /// `query_by_value_set`) over the persistent char trie.
    fn value(&self) -> Option<V> {
        self.debug_check_no_concurrent_mutation();
        // Safety: the node pointer's validity is guaranteed by `pin` (same
        // invariant the `DictionaryNode` methods rely on).
        self.node.and_then(|ptr| unsafe { (*ptr).value.clone() })
    }
}

impl<V: DictionaryValue, S: crate::persistent_artrie::block_storage::BlockStorage> Dictionary
    for PersistentARTrieChar<V, S>
{
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

impl<V: DictionaryValue + Clone, S: crate::persistent_artrie::block_storage::BlockStorage>
    MappedDictionary for PersistentARTrieChar<V, S>
{
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
        // Build the walk pin while STILL holding the read guard (so the epoch is
        // entered before the guard drops, leaving no window for eviction to
        // advance+drain past us): pin the shared epoch and capture a type-erased
        // `Arc` clone of the trie to keep it alive for the walk. This is the only
        // `root()` path subject to concurrent eviction; the owned
        // `PersistentARTrieChar::root` carries no pin.
        let trie_arc: SharedCharARTrie<V> = Arc::clone(self);
        let keepalive: Arc<dyn std::any::Any + Send + Sync> = trie_arc;
        let gen_snapshot = guard
            .structural_generation
            .load(std::sync::atomic::Ordering::Acquire);
        let pin = Arc::new(CharWalkGuard {
            _epoch: crate::persistent_artrie_core::mvcc::EpochGuard::new(Arc::clone(
                &guard.epoch_manager,
            )),
            _keepalive: keepalive,
            gen_snapshot,
        });
        let mut node = PersistentARTrieCharNode::from_trie(&guard);
        node.pin = Some(pin);
        node
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
            guard
                .insert_with_value(term, default_value)
                .unwrap_or(false);
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
        self.node
            .edges()
            .map(move |(c, child)| {
                let mut new_path = path.clone();
                new_path.push(c);
                (
                    c,
                    Self {
                        node: child,
                        path_vec: new_path,
                    },
                )
            })
            .collect::<Vec<_>>()
            .into_iter()
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

    fn create_with_slot_tracking<P: AsRef<std::path::Path>>(
        path: P,
    ) -> crate::persistent_artrie::error::Result<Self> {
        PersistentARTrieChar::create_with_slot_tracking(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::error::Result<Self> {
        PersistentARTrieChar::open(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open_with_slot_tracking<P: AsRef<std::path::Path>>(
        path: P,
    ) -> crate::persistent_artrie::error::Result<Self> {
        PersistentARTrieChar::open_with_slot_tracking(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open_with_recovery<P: AsRef<std::path::Path>>(
        path: P,
    ) -> crate::persistent_artrie::error::Result<(
        Self,
        crate::persistent_artrie::recovery::RecoveryReport,
    )> {
        PersistentARTrieChar::open_with_recovery(path).map(|(t, r)| (Arc::new(RwLock::new(t)), r))
    }

    fn open_with_recovery_and_slot_tracking<P: AsRef<std::path::Path>>(
        path: P,
    ) -> crate::persistent_artrie::error::Result<(
        Self,
        crate::persistent_artrie::recovery::RecoveryReport,
    )> {
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
        // Non-blocking checkpoint: capture the snapshot under an exclusive write
        // guard (serialization must exclude concurrent inserts), then ATOMICALLY
        // downgrade the guard to a read guard so concurrent readers (`contains`/
        // `get_value`) run during the fsync-bound publish phase. The downgrade
        // never releases the lock, so no writer can race in (no GAP_LEDGER #41
        // window): writers stay excluded for the whole checkpoint, and two
        // checkpoints serialize on the write lock (no separate checkpoint mutex
        // needed).
        let guard = self.write();
        // C2 invariant: the lock-free overlay (whose `insert_cas` bypasses
        // `L1.write`) must not be active under the shared durable-checkpoint
        // path, or a writer could race the snapshot. It is never exposed on
        // `SharedCharARTrie`; assert it to fail loudly if that ever changes.
        debug_assert!(
            guard.lockfree_root.is_none(),
            "SharedCharARTrie non-blocking checkpoint requires the lock-free \
             overlay to be disabled (insert_cas would bypass L1.write)"
        );
        // Phase A: serialize the in-memory tree into fresh arenas, epoch-pinned
        // so a concurrent prior-round eviction reclaim cannot free a node the
        // walk dereferences. The pin is dropped before the downgrade — Phase B/C
        // touch only the serialized arena image, never in-memory node pointers.
        let snapshot = {
            let _pin = crate::persistent_artrie_core::mvcc::EpochGuard::new(Arc::clone(
                &guard.epoch_manager,
            ));
            guard.capture_snapshot()?
        };
        // Atomic write -> read downgrade (parking_lot guarantees no intermediate
        // state a waiting writer can acquire). Readers admitted; writers excluded.
        let read_guard = parking_lot::RwLockWriteGuard::downgrade(guard);
        // Phase B + C under the read guard: durable publish + WAL reclaim.
        read_guard.publish_durable_and_reclaim(snapshot)
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
        guard
            .iter_prefix(prefix)
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

    fn upsert(
        &self,
        term: &str,
        value: Self::Value,
    ) -> crate::persistent_artrie::error::Result<bool> {
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

/// Reclaim a batch of in-memory char nodes via non-blocking epoch-based
/// reclamation. Shared by the asynchronous eviction loop (started in
/// [`SharedCharARTrie::enable_eviction`]) and the synchronous
/// [`force_eviction`](crate::artrie_trait::EvictableARTrie::force_eviction) path,
/// so both reclaim identically. Returns `(nodes_evicted, bytes_freed)`.
///
/// Ordering (the heart of the eviction-vs-walk safety):
/// 1. **Unlink + retire under the write lock.** Each victim's parent slot is
///    `unswizzle`d (so the subtree is unreachable to NEW readers) and the subtree
///    root is RETIRED — never freed inline.
/// 2. **Release the write lock**, then **drain**. Holding the write lock across the
///    drain would block `root()`'s read lock, so readers could never enter/exit and
///    `active_readers` could never reach zero — a stall. The `fence(SeqCst)` orders
///    the unlinks before the reader-count read (pairing with readers' `SeqCst`
///    `enter_read`): either we observe an active reader (and defer), or any such
///    reader observes the unlink and re-faults a fresh node instead of the retired
///    one.
/// 3. **Free only when no reader is active** (inline fast path) **or after a
///    successful quiescence drain**. On a timed-out drain, leave the retirees for a
///    later cycle — NEVER free under a possibly-live reader. Because the drain is to
///    ZERO readers, a successful drain authorizes freeing the WHOLE accumulated
///    retire list (no live reader holds any retired pointer), so no per-generation
///    bookkeeping is needed.
///
/// Callers MUST NOT hold any trie or eviction-registry lock when invoking it
/// (parking_lot locks are not re-entrant).
fn evict_char_nodes<V: DictionaryValue>(
    trie: &SharedCharARTrie<V>,
    nodes_to_evict: Vec<(
        u64,
        Vec<char>,
        crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
    )>,
    quiescence_timeout: std::time::Duration,
    quiescence_poll: std::time::Duration,
) -> (usize, usize) {
    // Clone the shared epoch manager + retire list out under a brief read lock so
    // the drain and the (possibly O(subtree)) reclaim hold NO trie lock.
    let (epoch_manager, retire_list) = {
        let guard = trie.read();
        (
            Arc::clone(&guard.epoch_manager),
            Arc::clone(&guard.retire_list),
        )
    };

    // Phase 1: unlink + retire under the write lock.
    let (evicted_count, bytes_freed) = {
        let mut guard = trie.write();
        let mut evicted_count = 0;
        let mut bytes_freed = 0;
        for (_path_hash, path, disk_ptr) in nodes_to_evict {
            if guard.evict_node_at_path(&path, disk_ptr.clone()) {
                evicted_count += 1;
                bytes_freed += 256; // Estimate ~256 bytes per node

                // Remove from LRU tracking so a later reload starts fresh.
                if let Some(ref coordinator) = guard.eviction_coordinator {
                    use crate::persistent_artrie::eviction::lru_tracker::hash_char_path;
                    coordinator
                        .lru_registry()
                        .remove_hash(hash_char_path(&path));
                }
            }
        }
        (evicted_count, bytes_freed)
    }; // write lock released here

    // Phase 2 (NO lock held): drain, then reclaim.
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    let drained = epoch_manager.active_reader_count() == 0
        || epoch_manager
            .wait_for_quiescence(quiescence_timeout, quiescence_poll)
            .is_ok();
    if drained {
        // SAFETY: a zero-reader observation (inline or via a successful drain),
        // ordered after the unlinks by the `SeqCst` fence + readers' `SeqCst`
        // `enter_read`, guarantees no live walk holds a pointer into ANY retired
        // subtree. Freeing the unlinked, now-private subtrees is sound.
        unsafe { retire_list.reclaim_all() };
    }

    (evicted_count, bytes_freed)
}

impl<V: DictionaryValue> crate::artrie_trait::EvictableARTrie for SharedCharARTrie<V> {
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

        // Capture the quiescence parameters for the reclaim path: the eviction
        // callback drains this shared epoch before freeing retired subtrees.
        let quiescence_timeout = config.quiescence_timeout;
        let quiescence_poll = config.quiescence_poll_interval;

        // Share the trie's OWN epoch manager with the coordinator. Previously this
        // minted a FRESH manager, so the coordinator's `wait_for_quiescence` drained
        // a counter no reader ever incremented (vacuous quiescence) — eviction could
        // then free a node a live `DictionaryNode` walk still held. A walk pins this
        // same manager via `CharWalkGuard`, so the coordinator now genuinely waits
        // for active walks to drain before reclaiming.
        let epoch_manager = Arc::clone(&guard.epoch_manager);

        // Create the eviction coordinator
        let coordinator = crate::persistent_artrie::eviction::EvictionCoordinator::new(
            config.clone(),
            epoch_manager,
        );

        // Create a weak reference to self for the eviction callback
        let self_weak = Arc::downgrade(self);

        // Start the eviction coordinator with the eviction callback for char nodes
        coordinator
            .start_char(move |nodes_to_evict| {
                // Try to upgrade the weak reference
                let Some(trie) = self_weak.upgrade() else {
                    return (0, 0);
                };
                evict_char_nodes(&trie, nodes_to_evict, quiescence_timeout, quiescence_poll)
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
        // Take the coordinator out under a short-lived write guard, then RELEASE
        // the guard before `shutdown()` joins the eviction thread: the eviction
        // callback itself takes `trie.write()`, so joining while holding the trie
        // lock deadlocks (the same rule `force_eviction` already documents).
        let coordinator = self.write().eviction_coordinator.take();
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
        // Clone the coordinator Arc out under a short-lived read guard, then
        // release the guard before reclaiming: the reclaim callback takes the
        // trie WRITE lock and parking_lot's RwLock is not re-entrant, so no trie
        // lock may be held across `force_eviction_char`.
        let coordinator = {
            let guard = self.read();
            match &guard.eviction_coordinator {
                Some(c) => Arc::clone(c),
                None => return Ok((0, 0)),
            }
        };

        // Route to the char-aware path: the byte `force_eviction` reads the byte
        // `locations` map and would always return (0, 0) for a char trie, whose
        // nodes are registered in `char_locations`. `force_eviction_char` selects
        // from `char_locations` and reclaims inline via `evict_char_nodes`.
        // The eviction callback drains the shared epoch (after unlinking) before
        // freeing retired subtrees; pass the quiescence parameters from the config.
        let quiescence_timeout = coordinator.quiescence_timeout();
        let quiescence_poll = coordinator.quiescence_poll_interval();
        let trie = Arc::clone(self);
        Ok(coordinator.force_eviction_char(target_bytes, move |nodes| {
            evict_char_nodes(&trie, nodes, quiescence_timeout, quiescence_poll)
        }))
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
impl<V: DictionaryValue, S: crate::persistent_artrie::block_storage::BlockStorage>
    PersistentARTrieChar<V, S>
{
    /// Invalidate any published eviction [`DiskLocationRegistry`] because the
    /// in-memory trie is about to diverge from the last checkpoint's on-disk
    /// image.
    ///
    /// After [`checkpoint`](Self::checkpoint) publishes a registry, it maps each
    /// node's char-path to a *durable, verified* on-disk location. A subsequent
    /// mutation rewrites in-memory nodes (new value, grown node type, added/
    /// removed children) while those on-disk images stay frozen at the last
    /// checkpoint. If eviction then reclaimed such a node it would unswizzle the
    /// *newer* in-memory box onto the *stale* on-disk pointer and drop the box,
    /// so the next read would reload the old value — a lost update. Marking the
    /// registry invalid makes the coordinator refuse to select any node for
    /// eviction (`select_char_for_eviction` / `perform_eviction_char` both gate
    /// on `is_valid()`) until the next checkpoint rebuilds and republishes a
    /// fresh, durable registry.
    ///
    /// This is the formal `Mutate` action of
    /// `formal-verification/tla+/EvictionRegistryPublication.tla`, which clears
    /// the registry on any write so the `RegistryEntriesAreDurable` invariant is
    /// preserved across mutations. It is a no-op when eviction is disabled.
    ///
    /// Called from the single mutation chokepoint
    /// [`append_to_wal`](Self::append_to_wal): every durable public mutation —
    /// `insert`/`insert_with_value`/`remove`/`upsert`/the `insert_batch*` family/
    /// `insert_cas`/document transactions, and the `merge_from`/`remove_prefix`
    /// helpers that delegate to them — logs through it, while `checkpoint` (which
    /// writes its WAL record directly) and WAL recovery replay do not.
    pub(crate) fn invalidate_eviction_registry(&self) {
        // Bump the structural generation on every durable in-place mutation (this is
        // the `append_to_wal` chokepoint). A concurrent `DictionaryNode` walk's
        // debug detector compares its `root()` snapshot against this and panics on a
        // mismatch, surfacing the "handle used across a structural mutation" contract
        // violation (which would dangle the handle's raw pointer) as a loud failure.
        self.structural_generation
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        if let Some(ref coordinator) = self.eviction_coordinator {
            coordinator.invalidate_registry();
        }
    }

    /// Number of char nodes registered as evictable in the disk-location
    /// registry published at the last [`checkpoint`](Self::checkpoint).
    ///
    /// Returns `None` when eviction is disabled. Returns `Some(0)` before the
    /// first checkpoint. After a checkpoint with eviction enabled it reflects how
    /// many on-disk node locations the coordinator may reclaim in-memory boxes
    /// for. Note that a post-checkpoint mutation *invalidates* the registry
    /// (eviction then selects nothing) without immediately changing this count —
    /// observe invalidation via [`force_eviction`](crate::artrie_trait::EvictableARTrie::force_eviction)
    /// returning `(0, 0)`; the count is refreshed by the next checkpoint.
    pub fn evictable_node_count(&self) -> Option<usize> {
        self.eviction_coordinator
            .as_ref()
            .map(|c| c.disk_registry_char_len())
    }

    /// Evict a single node at the given path, replacing it with a DiskRef.
    ///
    /// Walks `path` from the root: descends through `path[..path.len()-1]`
    /// edges to reach the parent node (refusing to descend through any slot
    /// that is itself already on-disk, since the in-memory chain we hold has
    /// to be intact), then atomically `unswizzle`s the slot for `path.last()`
    /// to the disk location encoded in `disk_ptr`. On success the orphaned
    /// in-memory node is reclaimed (its `Box` is dropped); on race or
    /// already-on-disk the parent slot is left unchanged.
    ///
    /// Returns `true` if the slot was successfully unswizzled, `false` if
    /// the path could not be navigated, the slot was already on disk, the
    /// caller-supplied `disk_ptr` does not actually encode a disk location,
    /// or the CAS-based `unswizzle` raced and lost.
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

        let root_node: &mut types::CharTrieNodeInner<V> = match self.root {
            types::CharTrieRoot::Node(ref mut boxed) => boxed.as_mut(),
            types::CharTrieRoot::Empty => return false,
        };

        // Walk to the parent of the target. The borrow checker would refuse
        // a fully safe descent through chained `find_child_mut` lifetimes;
        // we hold `&mut self`, so the chain of `*mut CharTrieNodeInner`
        // raw-pointer hops below is sound: each pointer comes from a
        // SwizzledPtr we just verified is in-memory, and `&mut self`
        // guarantees no concurrent access.
        let mut current: *mut types::CharTrieNodeInner<V> = root_node;
        let descent: &[char] = &path[..path.len() - 1];
        for &edge in descent {
            // SAFETY: `current` was derived from `&mut root_node` (first
            // iteration) or from a SwizzledPtr we already proved to be
            // in-memory and dereferenced as `&mut CharTrieNodeInner<V>`
            // (subsequent iterations). `&mut self` precludes concurrent use.
            let node = unsafe { &mut *current };
            let child_slot = match node.node.find_child(edge as u32) {
                Some(slot) => slot,
                None => return false,
            };
            if !child_slot.is_swizzled() {
                return false; // Cannot descend through an on-disk parent slot.
            }
            let child_raw = match child_slot.as_ptr::<types::CharTrieNodeInner<V>>() {
                Some(p) => p,
                None => return false,
            };
            current = child_raw as *mut types::CharTrieNodeInner<V>;
        }

        // SAFETY: same invariant as above; we now hold &mut access to the
        // parent of the target node.
        let parent = unsafe { &mut *current };
        let last_edge = *path.last().expect("non-empty path verified above");
        let slot = match parent.node.find_child_mut(last_edge as u32) {
            Some(s) => s,
            None => return false,
        };
        if !slot.is_swizzled() {
            return false; // Already on disk.
        }

        match slot.unswizzle::<types::CharTrieNodeInner<V>>(
            target_loc.block_id,
            target_loc.offset,
            target_loc.node_type,
        ) {
            Ok(raw_ptr) => {
                // Do NOT free inline. The slot was just `unswizzle`d to an on-disk
                // reference, so this (possibly non-leaf) subtree is now UNLINKED —
                // unreachable to any NEW reader, which re-faults a fresh box from
                // disk. But a concurrent lock-free `DictionaryNode` walk may still
                // hold a raw pointer to this node or one of its resident
                // descendants. RETIRE the subtree root; the reclaimer
                // (`evict_char_nodes`) frees the whole now-private subtree via its
                // recursive `Drop` only after a quiescence drain proves no reader
                // active-at-unlink remains. See [`reclaim`].
                self.retire_list
                    .retire(raw_ptr as *mut types::CharTrieNodeInner<V>);
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: after a trie is reopened from disk, the root's children are
    /// swizzled (on-disk) `SwizzledPtr`s. The `DictionaryNode` traversal that
    /// external transducers (e.g. liblevenshtein) drive MUST fault those
    /// children in. Before the swizzle-aware fix, `transition`/`edges` used the
    /// non-faulting `get_child`/`iter_children`, which drop swizzled children via
    /// `as_ptr`, so the walk saw an empty subtree and every fuzzy query returned
    /// zero hits after a daemon restart. (liblevenshtein is not a dev-dependency
    /// here, so this drives the `DictionaryNode` API the transducer relies on
    /// directly; the end-to-end transducer test lives in pgmcp.)
    #[test]
    fn dictionary_node_traversal_descends_after_reopen() {
        use crate::{DictionaryNode, MappedDictionaryNode};
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("reopen_traversal.artc");

        // Build + checkpoint + DROP so the in-memory node boxes are released and
        // only the on-disk image remains.
        {
            let mut trie = PersistentARTrieChar::<i32>::create(&path).expect("create");
            trie.insert_with_value("receive", 1).expect("insert");
            trie.insert_with_value("recipe", 2).expect("insert");
            trie.insert_with_value("decide", 3).expect("insert");
            trie.checkpoint().expect("checkpoint");
        }

        // Reopen: root resident, children swizzled (on-disk, `eager_depth=None`).
        let trie = PersistentARTrieChar::<i32>::open(&path).expect("open");
        assert_eq!(trie.len(), 3);

        // Before the fix this count was 0 (swizzled children dropped by `as_ptr`).
        assert!(
            trie.root().edges().count() > 0,
            "root edges empty after reopen — swizzle-fault regression"
        );

        // Descend the full path of an inserted term; every step must fault the
        // next on-disk child in, ending on a final node that carries its value.
        let mut node = trie.root();
        for ch in "receive".chars() {
            node = node
                .transition(ch)
                .unwrap_or_else(|| panic!("transition '{ch}' lost after reopen"));
        }
        assert!(node.is_final(), "terminal node not final after reopen");
        assert_eq!(node.value(), Some(1), "value lost after reopen");

        // An absent first character still yields no transition (no false edge).
        assert!(
            trie.root().transition('x').is_none(),
            "spurious transition for absent edge"
        );
    }

    /// Regression variant: forced eviction swizzles resident node boxes back to
    /// disk. The `DictionaryNode` traversal must re-fault them on demand. The
    /// background eviction thread is stopped before traversal so the assertions
    /// are deterministic and race-free (the production tool instances likewise
    /// never run eviction concurrently with a query).
    #[test]
    fn dictionary_node_traversal_descends_after_forced_eviction() {
        use crate::{Dictionary, DictionaryNode, EvictableARTrie};
        use parking_lot::RwLock;
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evict_traversal.artc");
        {
            let mut trie = PersistentARTrieChar::<i32>::create(&path).expect("create");
            for (t, v) in [
                ("receive", 1),
                ("recipe", 2),
                ("recital", 3),
                ("reception", 4),
                ("decide", 5),
            ] {
                trie.insert_with_value(t, v).expect("insert");
            }
            trie.checkpoint().expect("checkpoint");
        }

        let shared: SharedCharARTrie<i32> = Arc::new(RwLock::new(
            PersistentARTrieChar::<i32>::open(&path).expect("open"),
        ));

        // Enable eviction + checkpoint to populate the disk-location registry,
        // force a one-shot reclaim, then stop the background thread BEFORE
        // traversing. `force_eviction` may legitimately reclaim zero nodes; the
        // assertions require only query correctness, so the test holds either way.
        let _ = shared.enable_eviction(EvictionConfig::default());
        shared.write().checkpoint().expect("checkpoint");
        let _ = shared.force_eviction(usize::MAX);
        let _ = shared.disable_eviction();

        assert!(
            shared.root().edges().count() > 0,
            "root edges empty after eviction — re-fault regression"
        );
        let mut node = shared.root();
        for ch in "reception".chars() {
            node = node
                .transition(ch)
                .unwrap_or_else(|| panic!("transition '{ch}' lost after eviction"));
        }
        assert!(node.is_final(), "terminal node not final after eviction");
    }

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
