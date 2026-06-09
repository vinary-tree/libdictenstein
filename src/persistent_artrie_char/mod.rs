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
pub mod overlay_read;
pub mod prefix_api;

// Char overlay fault-in handles (`OverlayFaulter` impls) that let the
// overlay-backed `DictionaryNode` resolve `Child::OnDisk` overlay children during a
// graph walk. F7 BLOCKER-1.
pub(crate) mod overlay_fault;

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

// Committed-LSN watermark for the lock-free Order-A durable write path
// (Migration Phase E; the executable refinement of LockFreeDurableCheckpoint.tla).
pub(crate) mod committed_watermark;

// Thin production-write-path router for the lock-free overlay (the SOLE
// representation since L3.3). `route_overlay()` + `flip_to_overlay()` live here.
pub(crate) mod overlay_write_mode;

// Per-monomorph routing for the valued production mutators (insert_with_value /
// increment / upsert): routes to the overlay only for V = u64 (SAFE Any downcast),
// owned tree otherwise. Flip F0.
pub(crate) mod lockfree_value_route;

// Public mutation API (insert / insert_with_value / remove) — Phase-6 split.
pub mod mutation_api;

// Core mutation implementations (_no_wal helpers) — Phase-6 split.
pub mod mutation_core;

// F5 (Slice 3): the direct dense→overlay reopen loader — `load_root_immutable`
// (eager-load owned + iterative walk-converter owned→overlay) + the per-variant
// glue for the generic WAL-tail-into-overlay applier. Gated OFF by default
// (`LockFreeOverlay::USE_F5_REOPEN_LOADER`); see `docs/design/slice3-f5-loader-impl.md`.
pub(crate) mod f5_loader;

// In-crate white-box tests for the eviction-registry wiring (commit f10c43e):
// state oracle (slot swizzled -> on-disk) + the async eviction path, both of
// which need private node/coordinator internals.
#[cfg(test)]
mod eviction_registry_tests;

// OE1–OE4 correspondence tests for the reversible overlay-eviction driver
// (`evict_overlay_node_at_path` / `evict_overlay_nodes`). In-crate because they
// drive the private driver + `pub(crate)` `OverlayEvictOutcome` and the overlay
// internals. The Rust witness for `formal-verification/tla+/OverlayEvictionCas.tla`.
#[cfg(test)]
mod overlay_eviction_driver_correspondence;

// F7 BLOCKER-1: in-crate coverage that the overlay-backed `DictionaryNode` faults
// EVICTED (`Child::OnDisk`) overlay children back in during a graph walk (never
// dropping them). In-crate because it drives the `pub(crate)` overlay-eviction
// driver that is the only way to create OnDisk overlay children.
#[cfg(test)]
mod overlay_dictionary_node_faulting_tests;

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
use crate::persistent_artrie_core::key_encoding::CharKey;
use crate::value::DictionaryValue;
use crate::zipper::{DictZipper, ValuedDictZipper};
use crate::{
    Dictionary, DictionaryNode, MappedDictionary, MappedDictionaryNode, MutableMappedDictionary,
};

/// Thread-safe handle for `PersistentARTrieChar`.
///
/// **F4 (the lock collapse):** now a bare `Arc<PersistentARTrieChar<V,S>>` — the
/// outer `RwLock` is DELETED. Overlay reads AND writes are fully lock-free; the
/// only operations needing mutual exclusion take dedicated inner locks
/// (`checkpoint_lock`, the wrapped `root` `RwLock`, the `eviction_coordinator`
/// `Mutex`, `merge_lock`) — never the handle. A backward-compatible
/// `.read()`/`.write()` API is preserved by
/// [`SharedTrieAccess`](crate::persistent_artrie_core::shared_access::SharedTrieAccess)
/// (both return a transparent guard that derefs to `&T`; no lock).
pub type SharedCharARTrie<V, S = crate::persistent_artrie::disk_manager::MmapDiskManager> =
    Arc<PersistentARTrieChar<V, S>>;

/// Deprecated alias for backward compatibility.
#[deprecated(since = "0.9.0", note = "Use SharedCharARTrie instead")]
pub type SharedCharTrie<V> = SharedCharARTrie<V>;

#[doc(inline)]
pub use crate::persistent_artrie_core::shared_access::SharedTrieAccess;

// F4: the concrete `.read()/.write()` shim impl on the collapsed char handle
// (CONCRETE, never a blanket `Arc<T>` — see the byte twin + the trait docs).
impl<V: DictionaryValue, S: crate::persistent_artrie::block_storage::BlockStorage>
    crate::persistent_artrie_core::shared_access::SharedTrieAccess
    for Arc<PersistentARTrieChar<V, S>>
{
    type Target = PersistentARTrieChar<V, S>;

    #[inline]
    fn read(
        &self,
    ) -> crate::persistent_artrie_core::shared_access::TrieAccessGuard<'_, PersistentARTrieChar<V, S>>
    {
        crate::persistent_artrie_core::shared_access::TrieAccessGuard::from_ref(self.as_ref())
    }

    #[inline]
    fn write(
        &self,
    ) -> crate::persistent_artrie_core::shared_access::TrieAccessGuard<'_, PersistentARTrieChar<V, S>>
    {
        crate::persistent_artrie_core::shared_access::TrieAccessGuard::from_ref(self.as_ref())
    }
}

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
    /// Root of the trie.
    ///
    /// **F4 (PF-1, OR lock):** wrapped in a `RwLock` (the byte twin's rationale) so
    /// the now-`&self` owned mutators / WAL-replay keep SAFE interior mutability
    /// after the `Arc<RwLock>`→`Arc` collapse — NO new `unsafe`. The **OR** lock in
    /// `CK > merge_lock > OR > EC`: owned path only (overlay writes are lock-free
    /// CAS). `get_mut()` at open (single-threaded), `write()` for runtime owned
    /// mutators, `read()` for owned-checkpoint capture. The old write→read
    /// `downgrade` is DELETED with the outer lock.
    pub(crate) root: RwLock<CharTrieRoot<V>>,
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
    /// Committed-LSN watermark for the lock-free Order-A durable write path
    /// (Migration Phase E). The largest contiguously-committed LSN; the only safe
    /// `checkpoint_lsn` under out-of-order lock-free commit. See
    /// [`committed_watermark::CommittedWatermark`].
    pub(crate) committed_watermark: committed_watermark::CommittedWatermark,
    /// **F3 / NF-3 — serializes concurrent checkpoints.** The overlay-arm
    /// non-blocking checkpoint holds only `self.read()`, so two concurrent
    /// `checkpoint()` calls on a `SharedCharARTrie` would otherwise interleave their
    /// block-0 descriptor / arena writes → a torn on-disk image. This lock is taken
    /// for the whole checkpoint body (cloned out of a brief read guard so the trie
    /// `RwLock` is NOT held); readers/writers never touch it → the overlay stays
    /// lock-free, only checkpoints serialize. `Arc<Mutex>` so it survives the F4
    /// `Arc<RwLock>`→`Arc` collapse unchanged. Formally verified:
    /// `formal-verification/tla+/ConcurrentCheckpointSerialization.tla`.
    pub(crate) checkpoint_lock: std::sync::Arc<parking_lot::Mutex<()>>,
    /// **F4 / V11.2 — serializes concurrent merge drivers** (mirrors
    /// `checkpoint_lock` EXACTLY). The per-key merge CAS-retry loop is
    /// obstruction-free; this lock kills merge‖merge livelock by serializing the
    /// whole-trie merge entry points. A dedicated lock in `CK > merge_lock > OR >
    /// EC`: the merge driver takes `merge_lock` then (owned path) OR, never the
    /// reverse; checkpoint (CK) snapshots the lock-free root concurrently and never
    /// holds `merge_lock`. Single acquisition site; public wrappers must NOT re-take
    /// it (parking_lot is non-reentrant).
    pub(crate) merge_lock: std::sync::Arc<parking_lot::Mutex<()>>,
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
    ///
    /// **F4 (subsystem family, uniform):** `Mutex<Option<Arc<…>>>` so the now-`&self`
    /// `enable/disable_group_commit` toggle it; drop-before-join on disable + the
    /// take-old-then-drop re-arm on enable (V11.3).
    pub(crate) group_commit:
        std::sync::Mutex<Option<Arc<crate::persistent_artrie::group_commit::GroupCommitCoordinator>>>,

    // Performance infrastructure
    /// Memory pressure monitor for adaptive memory management.
    /// When enabled, automatically adjusts buffer pool size based on system memory pressure.
    ///
    /// **F4 (subsystem family, uniform):** `Mutex<Option<Arc<…>>>` (`&self`
    /// enable/disable; drop-before-join — its callback can re-enter the trie, so
    /// holding the field mutex across the join would deadlock, V11.3 GAP 2).
    pub(crate) memory_monitor:
        std::sync::Mutex<Option<Arc<crate::persistent_artrie::memory_monitor::MemoryPressureMonitor>>>,
    /// Cache statistics for monitoring buffer pool performance.
    pub(crate) cache_stats: crate::persistent_artrie::adaptive_pool::CacheStats,
    /// Epoch-based checkpoint manager for WAL/metadata tracking.
    ///
    /// When enabled, the checkpoint manager tracks operation counts and WAL size,
    /// advancing epoch metadata based on configurable thresholds. Explicit
    /// forced epoch checkpoints publish trie data before durable metadata.
    ///
    /// **F4 (subsystem family, uniform):** `Mutex<Option<Arc<…>>>` (`&self`
    /// enable/disable; drop-before-join on disable, V11.3).
    pub(crate) checkpoint_manager:
        std::sync::Mutex<Option<Arc<crate::persistent_artrie::epoch::CheckpointManager>>>,
    /// Durability policy for WAL synchronization.
    /// Controls when fsync is called after WAL writes.
    ///
    /// **F4:** an `AtomicEnumCell` (`&self` `set_durability_policy`; lock-free read
    /// on the write path).
    pub(crate) durability_policy:
        crate::persistent_artrie_core::shared_access::AtomicEnumCell<DurabilityPolicy>,

    // === Eviction Support ===
    /// Eviction coordinator for memory pressure-driven eviction.
    ///
    /// **F4 (EC leaf lock):** `Mutex<Option<Arc<…>>>` — the **EC** leaf in
    /// `CK > merge_lock > OR > EC`: never held across CK/merge_lock/OR, NEVER across
    /// a worker `.join()` (drop-before-join in `disable_eviction`/`close`/`Drop`).
    pub(crate) eviction_coordinator:
        std::sync::Mutex<Option<Arc<crate::persistent_artrie::eviction::EvictionCoordinator>>>,

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
    ///
    /// G1: the overlay node is generic over the trie's value type `V`, so a
    /// membership trie (`V=()`) carries `<()>` (unchanged behavior) and a counter
    /// trie (`V=u64`) carries the `u64` count in the overlay leaf's `Option<V>`.
    pub(crate) lockfree_root: Option<nodes::AtomicNodePtr<V>>,

    /// Lock-free cache for term lookups (DashMap for O(1) sharded access).
    pub(crate) lockfree_cache: Option<dashmap::DashMap<String, bool>>,

    /// Statistics: CAS retries for monitoring contention.
    pub(crate) cas_retries: std::sync::atomic::AtomicU64,

    /// DG0 (D2.8 D4): the durable global commit-sequence counter. Seeded on open
    /// from `max(header.commit_seq_floor, scan-max-of-CommitRank)`; a future
    /// `next_commit_seq()` is a claim-before-CAS `fetch_add` (the visibility-order
    /// replay key). Plumbed here (default 0); it becomes load-bearing only when
    /// DG-RECON stamps it into `CommitRank.generation` — until then it is inert.
    pub(crate) commit_seq: std::sync::atomic::AtomicU64,

    /// DG0 (D2.8 §4.2): index `data_lsn -> commit_seq` for the reclaimed-set floor
    /// (`floor = max{commit_seq : data_lsn <= checkpoint_lsn}`). A *cache* (the
    /// durable `CommitRank` records are truth); bounded with a scan-fallback so it
    /// cannot grow unbounded under a never-checkpoint overlay. Updated via `&self`
    /// in `append_commit_rank` (DG-RECON) ⇒ wrapped in a `Mutex`. The key is a WAL
    /// `Lsn` (a `u64` alias); kept as `u64` here to avoid an Lsn import.
    pub(crate) commit_seq_by_data_lsn: std::sync::Mutex<std::collections::BTreeMap<u64, u64>>,
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
        // **F4 drop-before-join (V11.3 sites 3+5):** take each background
        // coordinator OUT of its field `Mutex` into a statement-temporary so the
        // field guard DROPS before `shutdown()`/`Drop` joins the worker thread — the
        // eviction + memory-monitor callbacks re-enter the trie (OR/EC), so joining
        // while holding the field mutex would deadlock. Runs on EVERY teardown.
        let coordinator = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .take();
        if let Some(coordinator) = coordinator {
            coordinator.shutdown();
        }
        let monitor = self
            .memory_monitor
            .lock()
            .expect("memory_monitor mutex poisoned")
            .take();
        if let Some(monitor) = monitor {
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
        // E1 read-flip: `self.len` tracks the OWNED tree, which is cleared under the
        // overlay regime; count the overlay's resident finals instead.
        if self.route_overlay() {
            return self.overlay_len();
        }
        self.len.load(AtomicOrdering::Acquire)
    }

    /// Get the number of terms in the dictionary (alias for `len()`).
    #[inline]
    pub fn term_count(&self) -> usize {
        self.len()
    }

    /// Check if the dictionary is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        // E1 read-flip: cheap any-final early-out over the overlay (not `overlay_len()
        // == 0`, which would be O(N)).
        if self.route_overlay() {
            return self.overlay_is_empty();
        }
        self.len.load(AtomicOrdering::Acquire) == 0
    }

    /// Get the root node for dictionary traversal.
    ///
    /// F7 BLOCKER-1: under the lock-free overlay regime this returns an
    /// OVERLAY-backed `DictionaryNode` that navigates the overlay lazily, so zipper /
    /// transducer / fuzzy traversal works on a flipped trie (was: an EMPTY owned tree
    /// + a `log::warn!` deferral). Additive + reversible — the owned arm is unchanged
    /// and returned whenever `!route_overlay()`.
    ///
    /// The inherent `&self` path passes **no** overlay faulter: eviction (the only
    /// source of an `OnDisk` overlay child) is impossible on a non-`Shared` owned
    /// trie, so the overlay handed out here is fully `Child::InMem`. The
    /// eviction-capable `SharedCharARTrie::root` attaches a faulter.
    pub fn root(&self) -> PersistentARTrieCharNode<V> {
        if self.route_overlay() {
            use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
            let root = <Self as LockFreeOverlay<CharKey, V, S>>::overlay_root_node(self)
                .unwrap_or_else(|| {
                    Arc::new(crate::persistent_artrie_core::overlay::OverlayNode::<
                        CharKey,
                        V,
                    >::new())
                });
            return PersistentARTrieCharNode::from_overlay_root(root, None);
        }
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

/// Node in the character-level trie for DictionaryNode trait.
///
/// Two representations share one struct:
/// - the **owned** arm (the historical one): `node`/`is_root`/`root_empty` +
///   `faulter`/`pin` — a raw pointer into trie-owned (arena) storage, faulting
///   swizzled children, kept valid by the epoch `pin`.
/// - the **overlay** arm (F7 BLOCKER-1, used under `route_overlay()`):
///   `overlay`/`overlay_faulter` — an owned `Arc<OverlayNode>` snapshot (immutable
///   + reference-counted, so descent needs no pin and no `unsafe`) + an optional
///   SAFE [`OverlayFaulter`] for `Child::OnDisk` overlay children. When `overlay`
///   is `Some`, every method dispatches to the overlay arm and the owned fields are
///   unused (`node == None`).
///
/// `Clone` is derived (every field is `Clone`/`Copy`); `Debug` is hand-written
/// (below) because `Arc<dyn OverlayFaulter>` is not `Debug`.
#[derive(Clone)]
pub struct PersistentARTrieCharNode<V: DictionaryValue = ()> {
    /// Reference to the node in the trie (owned arm)
    node: Option<*const CharTrieNodeInner<V>>,
    /// Whether this is the root node (owned arm)
    is_root: bool,
    /// Whether the root is empty (no children) (owned arm)
    root_empty: bool,
    /// Owned overlay node snapshot (the **overlay arm**). `Some` ⇒ this handle
    /// navigates the lock-free overlay (returned by `root()` under
    /// `route_overlay()`); the owned fields above are then unused. The `Arc` keeps
    /// the node + its in-memory subtree alive, so descent needs no pin/`unsafe`.
    overlay: Option<Arc<crate::persistent_artrie_core::overlay::OverlayNode<CharKey, V>>>,
    /// SAFE fault-in capability for `Child::OnDisk` overlay children (overlay arm),
    /// or `None` for a resident-only overlay walk (an owned-trie `root()`, where
    /// eviction — hence an OnDisk overlay child — is impossible). `Arc<dyn ..>`
    /// (owned), so it keeps the trie alive for the walk and clones cheaply. No raw
    /// pointer, no `unsafe`.
    overlay_faulter:
        Option<Arc<dyn crate::persistent_artrie_core::overlay::OverlayFaulter<CharKey, V>>>,
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

// Hand-written `Debug` (the derived one cannot see through `Arc<dyn OverlayFaulter>`):
// summarize whichever arm is active without recursing or dereferencing raw pointers.
impl<V: DictionaryValue> std::fmt::Debug for PersistentARTrieCharNode<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.overlay {
            Some(node) => f
                .debug_struct("PersistentARTrieCharNode::Overlay")
                .field("node", node)
                .field("has_faulter", &self.overlay_faulter.is_some())
                .finish(),
            None => f
                .debug_struct("PersistentARTrieCharNode::Owned")
                .field("is_root", &self.is_root)
                .field("root_empty", &self.root_empty)
                .field("has_node", &self.node.is_some())
                .field("has_faulter", &self.faulter.is_some())
                .field("has_pin", &self.pin.is_some())
                .finish(),
        }
    }
}

// Safety: the `node` and `faulter` raw pointers reference trie-owned storage.
// They are kept valid for the walk by `pin` (a `CharWalkGuard`): the pin holds an
// `Arc` clone of the owning `Arc<RwLock<trie>>` (so the allocation stays put) and
// pins the shared `EpochManager` (so eviction's `wait_for_quiescence` drains this
// walk before reclaiming any node it holds). Only `&`-references are formed on the
// read path (no `&mut` aliasing); faulting transitions slots on-disk -> in-memory
// via atomic CAS. For an owned-trie walk (`pin == None`) no concurrent free is
// possible, so the pointers are valid for as long as the caller holds the trie.
// The OVERLAY arm (`overlay == Some`) holds only owned `Arc`s (no raw pointer), so
// it is `Send`/`Sync` on its own merits; these `unsafe impl`s remain required for
// the owned arm's raw pointers and are unchanged.
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
        // after building the root node (only it has the owning `Arc`).
        //
        // **F4:** capture the owned root's raw pointer under a BRIEF OR read guard.
        // The root `Box`'s heap address is stable while the `Node` variant lives
        // (owned mutators mutate in place / only `Empty → Node`), so the captured
        // `*const` stays valid for the walk's `&self`/trie lifetime — the same
        // stability the rest of this raw-pointer `DictionaryNode` walk relies on.
        let root_node_ptr: Option<*const types::CharTrieNodeInner<V>> = {
            let guard = trie.root.read();
            match &*guard {
                types::CharTrieRoot::Empty => None,
                types::CharTrieRoot::Node(node) => Some(node.as_ref() as *const _),
            }
        };
        match root_node_ptr {
            None => Self {
                node: None,
                is_root: true,
                root_empty: true,
                overlay: None,
                overlay_faulter: None,
                faulter,
                pin: None,
            },
            Some(ptr) => Self {
                node: Some(ptr),
                is_root: true,
                root_empty: false,
                overlay: None,
                overlay_faulter: None,
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
            overlay: None,
            overlay_faulter: None,
            faulter,
            pin,
        }
    }

    /// Create an **overlay-backed** root node (the `root()` node under
    /// `route_overlay()`). Navigates the lock-free overlay lazily. `overlay_faulter`
    /// is the SAFE fault-in capability for `Child::OnDisk` overlay children (or
    /// `None` for a resident-only walk). The owned fields are inert in this arm.
    fn from_overlay_root(
        node: Arc<crate::persistent_artrie_core::overlay::OverlayNode<CharKey, V>>,
        overlay_faulter: Option<
            Arc<dyn crate::persistent_artrie_core::overlay::OverlayFaulter<CharKey, V>>,
        >,
    ) -> Self {
        Self {
            node: None,
            is_root: true,
            root_empty: false,
            overlay: Some(node),
            overlay_faulter,
            faulter: None,
            pin: None,
        }
    }

    /// Create an overlay child node, inheriting the parent's overlay faulter.
    fn from_overlay_node(
        node: Arc<crate::persistent_artrie_core::overlay::OverlayNode<CharKey, V>>,
        overlay_faulter: Option<
            Arc<dyn crate::persistent_artrie_core::overlay::OverlayFaulter<CharKey, V>>,
        >,
    ) -> Self {
        Self {
            node: None,
            is_root: false,
            root_empty: false,
            overlay: Some(node),
            overlay_faulter,
            faulter: None,
            pin: None,
        }
    }

    /// Resolve an overlay child slot into a child overlay node, faulting a
    /// `Child::OnDisk` slot in via `overlay_faulter` (never dropping it). Returns
    /// `None` for a null/absent slot, or an OnDisk slot that cannot be faulted in
    /// (no faulter / I/O error) — the same conservative degrade the production
    /// point-read uses (liveness-only, never a fabricated term).
    fn overlay_child_node(
        child: &crate::persistent_artrie_core::overlay::Child<CharKey, V>,
        overlay_faulter: &Option<
            Arc<dyn crate::persistent_artrie_core::overlay::OverlayFaulter<CharKey, V>>,
        >,
    ) -> Option<Self> {
        if let Some(child_arc) = child.as_in_mem() {
            return Some(Self::from_overlay_node(
                Arc::clone(child_arc),
                overlay_faulter.clone(),
            ));
        }
        if let Some(on_disk) = child.as_on_disk() {
            if !on_disk.is_null() {
                let loaded = overlay_faulter.as_ref()?.fault_overlay_slot(on_disk)?;
                return Some(Self::from_overlay_node(loaded, overlay_faulter.clone()));
            }
        }
        None
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
        // OVERLAY arm: pure owned-`Arc` read, no pin / no `unsafe`.
        if let Some(node) = &self.overlay {
            return node.is_final();
        }
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
        // OVERLAY arm: one `u32` edge per overlay child (un-path-compressed). InMem
        // ⇒ wrap directly; OnDisk ⇒ fault in via the SAFE overlay faulter (never
        // dropped). No pin / no `unsafe`.
        if let Some(node) = &self.overlay {
            let child = node.find_child(label as u32)?;
            return Self::overlay_child_node(child, &self.overlay_faulter);
        }
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
        // OVERLAY arm: one edge per overlay child slot (InMem direct, OnDisk faulted
        // in — never dropped). Keys are `u32`; an unmappable scalar (impossible for
        // real data) is skipped. Preallocated to the known child count. No `unsafe`.
        if let Some(node) = &self.overlay {
            let mut edges = Vec::with_capacity(node.num_children());
            for (&key, child) in node.iter_children() {
                let Some(c) = char::from_u32(key) else {
                    continue;
                };
                if let Some(child_node) = Self::overlay_child_node(child, &self.overlay_faulter) {
                    edges.push((c, child_node));
                }
            }
            return Box::new(edges.into_iter());
        }
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

    fn edge_count(&self) -> Option<usize> {
        // OVERLAY arm: the overlay node's child count is exact and O(1).
        if let Some(node) = &self.overlay {
            return Some(node.num_children());
        }
        // Owned arm: keep the trait default (`None`) — the owned `DictionaryNode`
        // did not previously provide an exact count here.
        None
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentARTrieCharNode<V> {
    type Value = V;

    /// The value stored at this node (if it terminates a key). Reads the
    /// node's `value` field directly — this unlocks liblevenshtein's
    /// value-aware transducer queries (value-yielding + `query_filtered` /
    /// `query_by_value_set`) over the persistent char trie.
    fn value(&self) -> Option<V> {
        // OVERLAY arm: read the overlay leaf's `Option<V>` directly (owned `Arc`,
        // no pin / no `unsafe`). For `V = ()` membership finals this is `None`,
        // matching the owned node.
        if let Some(node) = &self.overlay {
            return node.get_value();
        }
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
        // D2: delegate to the inherent `len()`, which routes to the overlay under the
        // flip (this trait body read `self.len` directly, bypassing the route).
        Some(self.len())
    }
}

impl<V: DictionaryValue + Clone, S: crate::persistent_artrie::block_storage::BlockStorage>
    MappedDictionary for PersistentARTrieChar<V, S>
{
    type Value = V;

    fn get_value(&self, term: &str) -> Option<V> {
        // D2/S3″: delegate to the inherent `get_value` (which value-routes to the
        // overlay), NOT `self.get(..).cloned()` — `get` returns `None` under the flip.
        // The inherent method shadows this trait method in `.get_value()` call syntax.
        self.get_value(term)
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
        // F7 BLOCKER-1 — OVERLAY arm: under `route_overlay()` return an
        // overlay-backed `DictionaryNode`. The held `Arc<OverlayNode>` root snapshot
        // (captured under the read guard for consistency) keeps its whole in-memory
        // subtree alive on its own (every `Child::InMem` is an owned `Arc`), so no
        // epoch pin is needed for memory safety: a concurrent overlay eviction
        // CAS-publishes a NEW root with the slot unswizzled, but THIS snapshot still
        // holds the pre-eviction in-memory Arcs (no UAF). The SAFE
        // `SharedOverlayFaulter` keeps the trie (and its buffer/arena managers) alive
        // for the walk and faults any `Child::OnDisk` slot in on demand — never
        // dropping it. No raw pointer, no `unsafe` in this arm.
        if guard.route_overlay() {
            use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
            let root =
                <PersistentARTrieChar<V> as LockFreeOverlay<CharKey, V, _>>::overlay_root_node(
                    &guard,
                )
                .unwrap_or_else(|| {
                    Arc::new(crate::persistent_artrie_core::overlay::OverlayNode::<
                        CharKey,
                        V,
                    >::new())
                });
            drop(guard);
            let faulter: Arc<
                dyn crate::persistent_artrie_core::overlay::OverlayFaulter<CharKey, V>,
            > = Arc::new(overlay_fault::SharedOverlayFaulter::new(Arc::clone(self)));
            return PersistentARTrieCharNode::from_overlay_root(root, Some(faulter));
        }
        // OWNED arm (unchanged): build the walk pin while STILL holding the read
        // guard (so the epoch is entered before the guard drops, leaving no window
        // for eviction to advance+drain past us): pin the shared epoch and capture a
        // type-erased `Arc` clone of the trie to keep it alive for the walk. This is
        // the only owned `root()` path subject to concurrent eviction; the owned
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
        // D3/S2: delegate to the inner inherent `len()` (overlay-routed under the
        // flip); this read `guard.len` directly, bypassing the route.
        let guard = self.read();
        Some(guard.len())
    }
}

impl<V: DictionaryValue + Clone> MappedDictionary for SharedCharARTrie<V> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<V> {
        // D3/S3: delegate to the inner inherent `get_value` (value-routed), NOT
        // `guard.get(..).cloned()` (which is `None` under the flip).
        let guard = self.read();
        guard.get_value(term)
    }
}

impl<V: DictionaryValue + Clone> MutableMappedDictionary for SharedCharARTrie<V> {
    fn insert_with_value(&self, term: &str, value: V) -> bool {
        let guard = self.write();
        guard.insert_with_value(term, value).unwrap_or(false)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        // C2 deadlock fix (AB/BA, red-team R3-1/R4-1 BLOCKER): snapshot `other`'s
        // entries under its read lock and DROP that guard BEFORE taking `self`'s write
        // lock, then merge via the shared funnel — NEVER holding two `RwLock`s at once.
        // The old body held `other.read()` across `self.write()`, deadlocking
        // `A.union_with(&B)` ‖ `B.union_with(&A)`. Mirrors the vocab `union_with`
        // snapshot-then-release pattern (persistent_vocab_artrie/mod.rs:476).
        let entries: Vec<(String, V)> = {
            let other_guard = other.read();
            match other_guard.iter_prefix_with_values_and_arena("") {
                Ok(Some(terms)) => terms.into_iter().map(|i| (i.term, i.value)).collect(),
                _ => Vec::new(),
            }
        };
        // **F4 / V11.2 — merge_lock (merge‖merge serializer).** `union_with` is a
        // `Shared*`-reachable merge driver, so it takes `merge_lock` (a near-leaf in
        // `CK > merge_lock > OR > EC`: `merge_entries` takes OR / runs lock-free CAS
        // UNDER it, never the reverse). `other`'s guard is already dropped (snapshot
        // above), so this never holds two trie locks at once (the AB/BA fix).
        let merge_lock = self.merge_lock.clone();
        let _merge_guard = merge_lock.lock();
        self.merge_entries(entries, merge_fn).unwrap_or(0)
    }

    fn update_or_insert<F>(&self, term: &str, default_value: V, update_fn: F) -> bool
    where
        F: FnOnce(&mut V),
    {
        let guard = self.write();
        // MUST route the read to the overlay (`get_value`, NOT the legacy `get`): under
        // the overlay default `get` returned None for a present term, so this took the
        // insert branch and OVERWROTE the existing value with `default_value` (silent
        // data corruption). `get_value`/`upsert`/`insert_with_value` all overlay-route.
        if let Some(existing) = guard.get_value(term) {
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
        PersistentARTrieChar::create(path).map(Arc::new)
    }

    fn create_with_slot_tracking<P: AsRef<std::path::Path>>(
        path: P,
    ) -> crate::persistent_artrie::error::Result<Self> {
        PersistentARTrieChar::create_with_slot_tracking(path).map(Arc::new)
    }

    fn open<P: AsRef<std::path::Path>>(path: P) -> crate::persistent_artrie::error::Result<Self> {
        PersistentARTrieChar::open(path).map(Arc::new)
    }

    fn open_with_slot_tracking<P: AsRef<std::path::Path>>(
        path: P,
    ) -> crate::persistent_artrie::error::Result<Self> {
        PersistentARTrieChar::open_with_slot_tracking(path).map(Arc::new)
    }

    fn open_with_recovery<P: AsRef<std::path::Path>>(
        path: P,
    ) -> crate::persistent_artrie::error::Result<(
        Self,
        crate::persistent_artrie::recovery::RecoveryReport,
    )> {
        PersistentARTrieChar::open_with_recovery(path).map(|(t, r)| (Arc::new(t), r))
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
        Ok((Arc::new(trie), report))
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
        let guard = self.write();
        guard.insert(term).unwrap_or(false)
    }

    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        let guard = self.write();
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
        // D3/S3′: inner inherent `get_value` (value-routed under the flip).
        let guard = self.read();
        guard.get_value(term)
    }

    fn remove(&self, term: &str) -> bool {
        let guard = self.write();
        guard.remove(term).unwrap_or(false)
    }

    #[inline]
    fn len(&self) -> usize {
        // D3/S2′: inner inherent `len()` (overlay-routed under the flip).
        let guard = self.read();
        guard.len()
    }

    fn checkpoint(&self) -> crate::persistent_artrie::error::Result<()> {
        // **F3 / NF-3 — serialize concurrent checkpoints.** The overlay arm below
        // captures under only `self.read()` (lock-free), so two concurrent
        // `checkpoint()` calls would otherwise interleave their block-0 descriptor /
        // arena writes → a torn on-disk image (lost/corrupt terms on reopen). Take
        // the `checkpoint_lock` for the WHOLE body: clone it out of a brief read
        // guard (so the trie `RwLock` is NOT held while we lock), then hold it.
        // Readers/writers never touch this mutex → the overlay stays lock-free; only
        // checkpoints serialize. Formally verified
        // (`formal-verification/tla+/ConcurrentCheckpointSerialization.tla`:
        // USE_LOCK=TRUE holds NoTornDescriptor; USE_LOCK=FALSE negative control fires).
        // **F4:** there is no outer trie lock. `checkpoint_lock` (CK) is the SOLE
        // concurrent-checkpoint serializer. `self.read()`/`self.write()` are the
        // no-lock shim (return `&T`); they exist only to keep the historical call
        // shape — the real exclusion is CK (here) + OR-read (inside the owned
        // capture). Lock order: CK > OR.
        let ckpt_lock = self.checkpoint_lock.clone();
        let _ckpt_guard = ckpt_lock.lock();
        // S5-9 route-split (RES-4, total-loss guard): under the overlay write mode the
        // checkpoint MUST capture the IMMUTABLE OVERLAY (the live data), not the empty
        // owned tree. `capture_snapshot_immutable` is lock-free (reads the atomic
        // overlay root) — no guard needed for memory safety; CK serializes the
        // descriptor/arena publish.
        if self.route_overlay() {
            let snapshot = self.capture_snapshot_immutable()?;
            return if self
                .eviction_coordinator
                .lock()
                .expect("eviction_coordinator mutex poisoned")
                .is_some()
            {
                self.publish_immutable_snapshot_retaining_wal_with_eviction(snapshot)
            } else {
                self.publish_immutable_snapshot_retaining_wal(&snapshot)
            };
        }
        // Non-blocking OWNED-tree checkpoint (dormant / kill-switch-only post-flip).
        // **F4 (NF-2):** the old write→read `downgrade` is DELETED with the outer
        // lock it operated on. The owned capture now takes the inner `root` RwLock
        // for READ (OR), which admits concurrent owned readers and excludes concurrent
        // owned writers — exactly the exclusion the downgrade used to provide, scoped
        // to the owned representation. No GAP_LEDGER #41 window: an owned writer takes
        // `root.write()`, which the capture's `root.read()` excludes for the snapshot.
        //
        // C2 invariant (F3 fix; RES-4): the OWNED arm runs only when NOT
        // overlay-routed. `!route_overlay()` is the branch predicate (NOT
        // `lockfree_root.is_none()`, which would FALSELY panic a legitimate
        // kill-switched-owned checkpoint whose dormant overlay root is still
        // installed). Debug-only.
        debug_assert!(
            !self.route_overlay(),
            "SharedCharARTrie owned-arm non-blocking checkpoint requires \
             !route_overlay() (writes must not be overlay-routed under the owned \
             capture, or a lock-free writer could race the snapshot)"
        );
        // Phase A: capture (serialize the in-memory owned tree into fresh arenas),
        // epoch-pinned so a concurrent prior-round eviction reclaim cannot free a
        // node the walk dereferences. `capture_snapshot` reads the owned tree under
        // OR-read internally.
        let snapshot = {
            let _pin = crate::persistent_artrie_core::mvcc::EpochGuard::new(Arc::clone(
                &self.epoch_manager,
            ));
            self.capture_snapshot()?
        };
        // Phase B + C: durable publish + WAL reclaim (touch only the serialized arena
        // image, never in-memory node pointers; lock-free — owned readers run
        // concurrently).
        self.publish_durable_and_reclaim(snapshot)
    }

    fn is_dirty(&self) -> bool {
        let guard = self.read();
        guard.is_dirty()
    }

    fn remove_prefix(&self, prefix: &str) -> usize {
        let guard = self.write();
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
        let guard = self.write();
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
        let guard = self.write();
        guard.upsert(term, value)
    }

    // C1: `increment` removed from the `ARTrie` trait (now an inherent `V: Counter`
    // method on PersistentARTrieChar). Delegation commented out (not deleted) per
    // convention; counter callers use the inner inherent method.
    // fn increment(&self, term: &str, delta: i64) -> crate::persistent_artrie::error::Result<i64> {
    //     let guard = self.write();
    //     guard.increment(term, delta)
    // }
}

// ============================================================================
// REVERSIBLE OVERLAY-EVICTION DRIVER (design g4-overlay-eviction-reclamation)
// ============================================================================
//
// `cfg(any(test, feature = "bench-internals"))` — NOT a production path. This is
// the reversible driver that makes the eviction-ON benchmark's TREATMENT arm do
// REAL in-memory reclamation of COLD overlay subtrees (vs the §E structural
// no-op): it path-copies
// the `lockfree_root` spine and swaps a COLD in-memory child for an `OnDisk`
// reference via a loser-safe root CAS. ZERO `unsafe` (reuses the proven Phase-D
// safe `Arc`/`arc-swap` primitive). The CAS-arbitration safety (loser-safe,
// cold-only, no-UAF) is TLC-verified in
// `formal-verification/tla+/OverlayEvictionCas.tla`.
//
// Rollback (design §4): delete this whole section + `bench_evict_overlay_cold_nodes`
// + the §F bench arm + the TLA spec + its 3 verify-script lines. The write path,
// recovery, production eviction, and `checkpoint()` are untouched.

// Phase 4 (DRY K-generic lift): the per-node evict outcome + the per-attempt evict
// primitive (`evict_overlay_node_at_path`) + the read-path fault-in walk
// (`find_leaf_faulting`) now live ONCE, K-generic, in
// `persistent_artrie_core::overlay::evict` as default methods of the
// `OverlayEvictable<K, V, S>` subtrait of `OverlayFaulter`. Char re-exports the
// shared `OverlayEvictOutcome` (so `evict_overlay_nodes` + the OE tests name a
// single type) and IMPLEMENTS the trait below (the three variant-specific
// accessors + the `cas_retries` fault-counter hook). The lifted primitives are
// behavior-identical to the prior char-only inherent methods — OE1–OE8 + every
// eviction test pass unchanged. Phase 7.4 (GO-LIVE): the `OverlayEvictOutcome`
// re-export + the `evict_overlay_nodes` batch driver are now UN-GATED to production —
// the checkpoint-tail resident-budget eviction (Phase 7.5) is their production caller.
pub(crate) use crate::persistent_artrie_core::overlay::evict::OverlayEvictOutcome;

/// Char impl of the SHARED GENERIC [`OverlayEvictable`] (the per-attempt overlay
/// evict + read-fault primitives, K-generic over `OverlayNode<CharKey, V>`). Supplies
/// the three variant-specific accessors (`lockfree_root` / `epoch_manager` /
/// `eviction_coordinator`) + the `cas_retries` fault-counter hook; the primitives
/// themselves are the trait defaults. The `OverlayFaulter<CharKey, V>` super-trait
/// requirement is satisfied by char's existing impl (the `load_overlay_node_from_disk`
/// loader). NOT `#[cfg]`-gated: the trait default `find_leaf_faulting` is called on
/// char's UN-GATED production read/remove/valued-insert/increment paths (Flip F0), so
/// the impl must exist in non-test builds; only the per-node evict primitive's
/// production caller + the batch `evict_overlay_nodes` driver stay gated.
impl<V: DictionaryValue, S: crate::persistent_artrie::block_storage::BlockStorage>
    crate::persistent_artrie_core::overlay::evict::OverlayEvictable<
        crate::persistent_artrie_core::key_encoding::CharKey,
        V,
        S,
    > for PersistentARTrieChar<V, S>
{
    #[inline]
    fn overlay_root_slot(
        &self,
    ) -> Option<
        &crate::persistent_artrie_core::overlay::AtomicNodePtr<
            crate::persistent_artrie_core::key_encoding::CharKey,
            V,
        >,
    > {
        self.lockfree_root.as_ref()
    }

    #[inline]
    fn overlay_epoch_manager(&self) -> &crate::persistent_artrie_core::concurrency::EpochManager {
        &self.epoch_manager
    }

    #[inline]
    fn overlay_eviction_coordinator(
        &self,
    ) -> Option<Arc<crate::persistent_artrie::eviction::EvictionCoordinator>> {
        self.eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
            .map(Arc::clone)
    }

    #[inline]
    fn note_faultin_cas(&self) {
        // Char's pre-lift `find_leaf_faulting` bumped `cas_retries` on both the win
        // and the loss arm of the fault-in install CAS; preserve that observable
        // behavior (the contention monitor `cas_retry_count()`).
        self.cas_retries
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Reclaim a batch of COLD OVERLAY nodes (the overlay evictor). Evicts
/// LEAF-FIRST (descending depth) so a node is
/// evicted before any ancestor — keeping each victim's parent spine in memory at
/// eviction time (a later shallower candidate whose spine now passes through an
/// already-on-disk slot is reported `NotEvictable` and skipped). Each victim gets
/// up to `max_rebase_retries` root-CAS attempts: a `RootCasLost` (a concurrent
/// writer won) rebases and retries; on exhaustion the victim is SKIPPED (a missed
/// eviction is liveness-only — loser-safe).
///
/// Returns `(evicted, bytes_freed)` where `bytes_freed` is the registry
/// `size_bytes` sum of the successfully-evicted nodes (nominal; the peak-RSS pass
/// is the physical witness). Takes NO lock and uses NO `unsafe`.
///
/// Phase 7.4: UN-GATED to production (the checkpoint-tail resident-budget eviction
/// calls it). The `bench_*` enablers stay gated; this driver does not.
pub(crate) fn evict_overlay_nodes<
    V: DictionaryValue,
    S: crate::persistent_artrie::block_storage::BlockStorage,
>(
    trie: &PersistentARTrieChar<V, S>,
    mut nodes: Vec<(
        u64,
        Vec<char>,
        crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
    )>,
    max_rebase_retries: usize,
) -> (usize, usize) {
    // Phase 4: the per-attempt evict primitive is the K-generic trait default; bring
    // `OverlayEvictable` (+ its `evict_overlay_node_at_path`) into scope. The batch
    // driver itself (LEAF-FIRST ordering, `Vec<char>` registry-path conversion, LRU
    // remove_hash) stays char-specific — only the primitive is shared.
    use crate::persistent_artrie_core::overlay::evict::OverlayEvictable;

    // LEAF-FIRST: sort by DESCENDING path length (depth). Deeper nodes evict
    // first, so an ancestor's spine is still fully in memory when we reach it.
    nodes.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    let mut evicted = 0usize;
    let mut bytes_freed = 0usize;
    for (_path_hash, path, disk_ptr) in nodes {
        // The registry stores the path as `Vec<char>`; the overlay keys on u32
        // code points. Convert once (preallocated).
        let mut char_path: Vec<u32> = Vec::with_capacity(path.len());
        char_path.extend(path.iter().map(|&c| c as u32));

        // Bounded loser-safe retry: rebase on a lost root CAS; stop on the first
        // terminal outcome (Evicted or NotEvictable) or on retry exhaustion.
        let mut attempt = 0;
        loop {
            match trie.evict_overlay_node_at_path(&char_path, disk_ptr.clone()) {
                OverlayEvictOutcome::Evicted => {
                    evicted += 1;
                    // Nominal byte estimate per evicted overlay node (parity with
                    // `evict_char_nodes`' ~256 B/node estimate; the RSS pass is the
                    // physical witness).
                    bytes_freed += 256;
                    // Drop the LRU entry so a later (re)insert of this cold path
                    // starts fresh (parity with `evict_char_nodes`).
                    if let Some(coordinator) = trie.overlay_eviction_coordinator() {
                        use crate::persistent_artrie::eviction::lru_tracker::hash_char_path;
                        coordinator
                            .lru_registry()
                            .remove_hash(hash_char_path(&path));
                    }
                    break;
                }
                OverlayEvictOutcome::RootCasLost => {
                    attempt += 1;
                    if attempt > max_rebase_retries {
                        break; // exhausted → SKIP (liveness-only miss)
                    }
                    // else: rebase (re-load the root) on the next iteration.
                }
                OverlayEvictOutcome::NotEvictable => break, // skip; never retried
            }
        }
    }
    (evicted, bytes_freed)
}

// ============================================================================
// EvictableARTrie Trait Implementation (on SharedCharARTrie)
// ============================================================================

impl<V: DictionaryValue> crate::artrie_trait::EvictableARTrie for SharedCharARTrie<V> {
    fn enable_eviction(
        &self,
        config: crate::persistent_artrie::eviction::EvictionConfig,
    ) -> crate::persistent_artrie::error::Result<()> {
        use crate::persistent_artrie::error::PersistentARTrieError;

        config
            .validate()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        // F4 (EC leaf): check + install under a BRIEF EC lock; build/start the
        // coordinator OUTSIDE the lock. Already-enabled ⇒ error (no old to join).
        if self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .is_some()
        {
            return Err(PersistentARTrieError::internal("Eviction already enabled"));
        }

        // Share the trie's OWN epoch manager with the coordinator (the field is a
        // bare `Arc`, already interior-mutable — no wrap). A walk pins this same
        // manager via `CharWalkGuard`, so the coordinator genuinely waits for active
        // walks to drain before reclaiming.
        let epoch_manager = Arc::clone(&self.epoch_manager);

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
                // L0.1/L3.3: always reclaim the overlay (the owned tree is gone).
                // `evict_overlay_nodes` locks EC for its LRU remove; safe here
                // (this callback holds no EC).
                evict_overlay_nodes(&trie, nodes_to_evict, 4)
            })
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        // Start memory pressure monitor if configured
        coordinator
            .start_memory_monitor()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        // Install under a brief EC lock; re-check (first writer wins; a loser shuts
        // its own coordinator down OUTSIDE the lock — drop-before-join).
        let mut slot = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned");
        if slot.is_some() {
            drop(slot);
            coordinator.shutdown();
            return Err(PersistentARTrieError::internal("Eviction already enabled"));
        }
        *slot = Some(coordinator);
        Ok(())
    }

    fn disable_eviction(&self) -> crate::persistent_artrie::error::Result<()> {
        // **F4 drop-before-join (V11.3 site 2):** take the coordinator into a
        // statement-temporary so the EC guard DROPS before `shutdown()` joins the
        // eviction thread — the callback takes OR (and briefly EC), so joining while
        // holding EC would deadlock.
        let coordinator = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .take();
        if let Some(coordinator) = coordinator {
            coordinator.shutdown();
        }
        Ok(())
    }

    fn eviction_enabled(&self) -> bool {
        self.eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .is_some()
    }

    fn eviction_stats(&self) -> crate::persistent_artrie::eviction::EvictionStats {
        self.eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
            .map(|c| c.stats())
            .unwrap_or_default()
    }

    fn force_eviction(
        &self,
        target_bytes: usize,
    ) -> crate::persistent_artrie::error::Result<(usize, usize)> {
        // Clone the coordinator Arc out under a BRIEF EC lock, then release EC before
        // reclaiming: the reclaim callback takes OR (order OR > EC; parking_lot is
        // non-reentrant — no lock held across `force_eviction_char`).
        let coordinator = {
            match self
                .eviction_coordinator
                .lock()
                .expect("eviction_coordinator mutex poisoned")
                .as_ref()
            {
                Some(c) => Arc::clone(c),
                None => return Ok((0, 0)),
            }
        };

        // Route to the char-aware path: the byte `force_eviction_bytes` reads the byte
        // `locations` map and would always return (0, 0) for a char trie, whose nodes are
        // registered in `char_locations`. `force_eviction_char` selects from
        // `char_locations` and reclaims inline via the overlay evictor.
        let trie = Arc::clone(self);
        Ok(coordinator.force_eviction_char(target_bytes, move |nodes| {
            // L0.1: owned-eviction arm DELETED — always reclaim the overlay.
            evict_overlay_nodes(&trie, nodes, 4)
        }))
    }

    fn touch_node(&self, path: &[Self::Unit]) {
        if let Some(coordinator) = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
        {
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
        if let Some(coordinator) = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
        {
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
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
            .map(|c| c.disk_registry_char_len())
    }

    /// **REVERSIBLE BENCH ENABLER — EVICTION-ON** (gated entirely behind the
    /// existing `bench-internals` feature). Constructs and installs an
    /// [`EvictionCoordinator`] directly on this bare `PersistentARTrieChar` so the
    /// `lockfree_flip_benchmark` `--eviction` TREATMENT arm — which holds the trie
    /// as a bare `Arc<PersistentARTrieChar>` (the lock-free path needs only `&self`),
    /// NOT a `SharedCharARTrie<_>` (`Arc<RwLock<…>>`) — can run eviction-ON
    /// checkpoints (`bench_immutable_checkpoint_with_eviction`). The
    /// `bench_immutable_checkpoint*` methods are `PersistentARTrieChar` methods, so
    /// the TREATMENT trie cannot be a `SharedCharARTrie`; this enabler is the
    /// bare-trie analogue of [`SharedCharARTrie::enable_eviction`].
    ///
    /// Mirrors that construction: validate the config, share THIS trie's own
    /// `epoch_manager` with the coordinator (so a walk and the coordinator pin the
    /// same epoch), `start_char` the background loop, and `start_memory_monitor`
    /// (a no-op under `EvictionConfig::without_memory_monitor`).
    ///
    /// THE RECLAIM CALLBACK IS A NO-OP `(0, 0)`. This is faithful, not a cut: over
    /// a lock-free OVERLAY trie the owned `self.root` is `Empty` (the data lives in
    /// `lockfree_root`), so `evict_node_at_path` — which the production
    /// `SharedCharARTrie` callback (`evict_char_nodes`) walks — would unswizzle
    /// nothing and return `(0, 0)` ANYWAY (proven by the in-crate T1 correspondence
    /// test `immutable_eviction_checkpoint_reopens_losing_nothing`). Wiring the
    /// overlay into the owned eviction walk is the owner-gated Phase-E flip, out of
    /// scope. The benchmark measures the CHECKPOINT path (registry build +
    /// `update_disk_registry` publication — the eviction-ON cost being studied),
    /// which this enabler activates; the reclaim callback is never on the timed
    /// writer path.
    ///
    /// **Maintenance coupling (design §8 risk 5):** the coordinator construction
    /// duplicates `SharedCharARTrie::enable_eviction`'s shape (same crate; flagged).
    /// Deleting this method + `bench_immutable_checkpoint_with_eviction` + the
    /// `bench-internals` cfg disjunct on the publisher fully reverts the eviction-ON
    /// bench surface.
    ///
    /// `cfg(any(test, feature = "bench-internals"))`: widened from `bench-internals`-
    /// only so the in-crate OE1–OE4 overlay-eviction correspondence tests can
    /// install the coordinator (and thus publish a real overlay eviction registry)
    /// under the DEFAULT `cargo test`. The `bench-internals` benchmark path is
    /// unchanged.
    #[cfg(any(test, feature = "bench-internals"))]
    pub fn bench_enable_eviction(
        &mut self,
        config: crate::persistent_artrie::eviction::EvictionConfig,
    ) -> crate::persistent_artrie::error::Result<()> {
        use crate::persistent_artrie::error::PersistentARTrieError;

        config
            .validate()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        if self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .is_some()
        {
            return Err(PersistentARTrieError::internal("Eviction already enabled"));
        }

        // Share THIS trie's epoch manager with the coordinator (parity with
        // SharedCharARTrie::enable_eviction).
        let epoch_manager = Arc::clone(&self.epoch_manager);
        let coordinator = crate::persistent_artrie::eviction::EvictionCoordinator::new(
            config.clone(),
            epoch_manager,
        );

        // No-op reclaim callback: see the method doc — overlay eviction is a
        // structural no-op (owned self.root is Empty), so the production
        // `evict_char_nodes` callback would reclaim nothing here regardless. The
        // bench only measures the registry-publication CHECKPOINT path.
        coordinator
            .start_char(|_nodes_to_evict| (0usize, 0usize))
            .map_err(|e| PersistentARTrieError::internal(&e))?;
        coordinator
            .start_memory_monitor()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        *self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned") = Some(coordinator);
        Ok(())
    }

    /// **REVERSIBLE BENCH ACCESSOR — overlay COLD reclamation** (gated entirely
    /// behind `bench-internals`). Synchronously reclaim COLD overlay subtrees from
    /// the calling thread, returning the number of overlay nodes evicted.
    ///
    /// This is what makes the eviction-ON benchmark's TREATMENT arm perform REAL
    /// in-memory reclamation (vs the §E structural no-op). It reuses the
    /// coordinator's eviction SELECTION (coldest-first LRU, `min_eviction_depth`,
    /// `batch_size`, registry-validity gate — `force_eviction_char` refuses an
    /// invalidated registry, yielding 0 = liveness-not-safety), then filters the
    /// selected candidates to COLD paths (`cold_filter`, e.g.
    /// `|p| p.first() == Some(&'c')`) and reclaims them via the driver
    /// [`evict_overlay_nodes`]. ONLY COLD nodes are ever evicted (SF5(ii)
    /// `faultin_count == 0`): fault-in is absent, so a re-touchable LIVE node must
    /// never be evicted.
    ///
    /// Needs only `&self` (the overlay path is all `&self`), so the benchmark's
    /// checkpointer thread can call it synchronously after each checkpoint
    /// publishes the registry — deterministic, off the writer path. Returns 0 if
    /// eviction was not enabled (`bench_enable_eviction` not called).
    ///
    /// **Rollback (design §4):** delete this method (one edit); the driver +
    /// `OverlayEvictOutcome` + the §F bench arm + the TLA spec then revert
    /// independently. The write path, recovery, production eviction, and
    /// `checkpoint()` are untouched.
    #[cfg(feature = "bench-internals")]
    pub fn bench_evict_overlay_cold_nodes<F>(&self, budget_bytes: usize, cold_filter: F) -> usize
    where
        F: Fn(&[char]) -> bool,
    {
        // F4 (EC leaf): clone the coordinator Arc out under a brief lock; release
        // EC before `force_eviction_char` (its callback takes OR — order OR > EC).
        let coordinator = match self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
        {
            Some(c) => Arc::clone(c),
            None => return 0,
        };
        coordinator
            .force_eviction_char(budget_bytes, |cands| {
                // COLD-only: drop any selected candidate whose path is not cold, so
                // the evictor never touches a LIVE (re-touchable) subtree.
                let filtered: Vec<_> = cands
                    .into_iter()
                    .filter(|(_, p, _)| cold_filter(p))
                    .collect();
                evict_overlay_nodes(self, filtered, 4)
            })
            .0
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
            let trie = PersistentARTrieChar::<i32>::create(&path).expect("create");
            // L3.2/L3.3: the `DictionaryNode` faulting walk (`root().edges()`/`transition`)
            // reads the lock-free overlay (the owned tree is gone). Reopen rebuilds the
            // overlay from the dense on-disk image, so the walk descends it directly.
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

        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evict_traversal.artc");
        {
            let trie = PersistentARTrieChar::<i32>::create(&path).expect("create");
            // L3.2/L3.3: forced-eviction re-fault of the `DictionaryNode` walk over the
            // lock-free overlay (the owned tree is gone). Eviction unswizzles overlay nodes
            // to disk; the walk re-faults them on descent.
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

        let shared: SharedCharARTrie<i32> =
            Arc::new(PersistentARTrieChar::<i32>::open(&path).expect("open"));

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
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        assert!(trie.insert("hello").expect("insert failed"));
        assert!(trie.insert("world").expect("insert failed"));
        assert!(!trie.insert("hello").expect("insert failed")); // Duplicate
        assert_eq!(trie.len(), 2);
    }

    #[test]
    #[allow(deprecated)]
    fn test_insert_unicode() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        assert!(trie.insert("héllo").expect("insert failed")); // é is one character
        assert!(trie.insert("日本語").expect("insert failed")); // Japanese characters
        assert!(trie.insert("emoji😀").expect("insert failed")); // Emoji
        assert_eq!(trie.len(), 3);
    }

    #[test]
    #[allow(deprecated)]
    fn test_contains() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
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
        let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
        let _ = trie.insert_with_value("one", 1);
        let _ = trie.insert_with_value("two", 2);
        let _ = trie.insert_with_value("three", 3);

        assert_eq!(trie.get("one"), Some(1));
        assert_eq!(trie.get("two"), Some(2));
        assert_eq!(trie.get("three"), Some(3));
        assert_eq!(trie.get("four"), None);
    }

    #[test]
    #[allow(deprecated)]
    fn test_unicode_correctness() {
        // This test verifies that multi-byte characters are treated as single units
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
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
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
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
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
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
