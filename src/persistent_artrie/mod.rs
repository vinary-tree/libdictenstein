//! Persistent Adaptive Radix Trie (PART) Dictionary
//!
//! This module implements a disk-based dictionary using a hybrid of:
//! - **Adaptive Radix Tree (ART)**: For optimal trie traversal with adaptive node sizes
//! - **B-trie Buckets**: For efficient leaf storage with multiple strings per disk page
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    PersistentARTrie<V>                       │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Index Layer (ART)                                           │
//! │  - Node4: 1-4 children (linear scan)                        │
//! │  - Node16: 5-16 children (SIMD accelerated)                 │
//! │  - Node48: 17-48 children (index array)                     │
//! │  - Node256: 49-256 children (direct array)                  │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Leaf Layer (B-trie Buckets)                                 │
//! │  - StringBucket: 8KB pages with multiple strings            │
//! │  - Binary search within buckets                              │
//! │  - B-trie style splits when full                             │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Storage Layer                                               │
//! │  - BufferManager: LRU cache with Clock eviction             │
//! │  - DiskManager: Memory-mapped 256KB blocks                  │
//! │  - WAL: Write-ahead logging for crash recovery              │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Key Features
//!
//! - **Pointer Swizzling**: Lazy loading - nodes start as disk references,
//!   swizzled to memory pointers on access
//! - **Path Compression**: Up to 12 bytes of prefix stored inline
//! - **Adaptive Node Sizes**: Optimal memory/performance trade-off
//! - **SIMD Acceleration**: SSE4.1 for Node16 key lookup
//! - **Crash Recovery**: WAL with redo-only recovery
//!
//! # Usage
//!
//! ```text
//! use libdictenstein::persistent_artrie::PersistentARTrie;
//!
//! // Create a new persistent dictionary
//! let mut dict = PersistentARTrie::create("words.part")?;
//!
//! // Insert terms
//! dict.insert("hello", ())?;
//! dict.insert("world", ())?;
//!
//! // Query with Levenshtein automaton
//! let transducer = Transducer::new(&dict, Algorithm::Standard);
//! for result in transducer.query("helo", 1) {
//!     println!("{}: distance {}", result.term, result.distance);
//! }
//! ```
//!
//! # Feature Flag
//!
//! Enable with the `persistent-artrie` feature:
//!
//! ```toml
//! [dependencies]
//! liblevenshtein = { version = "0.8", features = ["persistent-artrie"] }
//! ```
//!
//! # References
//!
//! - [B-tries for disk-based string management](https://link.springer.com/article/10.1007/s00778-008-0094-1)
//!   (Askitis & Zobel, VLDBJ 2009)
//! - [The Adaptive Radix Tree](https://db.in.tum.de/~leis/papers/ART.pdf)
//!   (Leis et al., ICDE 2013)
//! - [Persistent Storage of ART in DuckDB](https://duckdb.org/2022/07/27/art-storage)
//!   (DuckDB, 2022)
//!
//! # ACID Guarantees
//!
//! This implementation provides ACID properties for reliable persistent storage:
//!
//! ## Atomicity
//!
//! - **Single Operations**: Individual insert/remove operations are atomic
//! - **Document Transactions**: Multi-term transactions via [`DocumentTransaction`] provide
//!   all-or-nothing semantics - either all terms are committed or none are
//! - **WAL Logging**: Operations are logged to WAL before application, ensuring atomicity
//!   across crashes
//!
//! ## Consistency
//!
//! - **Trie Invariants**: ART structure invariants (node types, child counts, path compression)
//!   are maintained after every operation
//! - **CRC32 Checksums**: WAL records include CRC32 checksums for integrity verification
//! - **Recovery Validation**: Crash recovery validates checksums and replays only valid records
//!
//! ## Isolation
//!
//! The implementation uses read-write locks for thread safety:
//!
//! | Isolation Level | Dirty Read | Non-Repeatable Read | Phantom Read |
//! |-----------------|------------|---------------------|--------------|
//! | RwLock (default)| No         | No                  | No           |
//! | MVCC-Lite       | No         | No                  | Possible*    |
//!
//! *MVCC-Lite uses epoch-based snapshots for reads, allowing concurrent writes.
//! Phantoms are possible if new terms are inserted between snapshot creation and read.
//!
//! ## Durability
//!
//! Durability is controlled by [`DurabilityPolicy`]:
//!
//! | Policy      | fsync Behavior      | Guarantee    | Use Case                        |
//! |-------------|---------------------|--------------|----------------------------------|
//! | `Immediate` | Before public mutation/commit acknowledgement | Full | ACID compliance (default) |
//! | `GroupCommit` | Batched by coordinator or blocking sync fallback | Full | High-throughput workloads |
//! | `Periodic`  | At checkpoints only | Bounded loss | Performance-critical            |
//! | `None`      | Never               | None         | Testing only                    |
//!
//! The default `Immediate` policy ensures that acknowledged public mutations and
//! committed transactions are durable on disk. The `GroupCommit` policy batches
//! fsync calls when a coordinator is installed and otherwise falls back to a
//! blocking sync to preserve full durability acknowledgements. `Periodic` trades
//! some durability for performance.

// Core modules (storage foundation)
//
// `error` has been relocated to `crate::persistent_artrie_core::error`; it is
// re-exported here under its original path so existing call-sites work
// unchanged after the core extraction.
pub use crate::persistent_artrie_core::error;
pub use crate::persistent_artrie_core::swizzled_ptr;

// Block storage abstraction + memory-mapped and io_uring backends (relocated to core).
pub use crate::persistent_artrie_core::block_storage;
pub use crate::persistent_artrie_core::disk_manager;

#[cfg(feature = "io-uring-backend")]
pub use crate::persistent_artrie_core::io_uring_disk_manager;

pub use crate::persistent_artrie_core::buffer_manager;

// Arena allocation for efficient node storage
pub mod arena;

pub mod arena_manager;

// Compact variable-width encoding
//
// Relocated to `crate::persistent_artrie_core::compact_encoding`; re-exported
// here for backward-compatible call-sites.
pub use crate::persistent_artrie_core::compact_encoding;

// ART node types
pub mod nodes;

// Path compression operations
pub mod path_compression;

// Compaction config/stats/progress (Phase-5 split out of dict_impl).
pub mod compaction;

// Document transaction data types (Phase-5 split out of dict_impl).
pub mod transactions;

// Parallel-merge extension trait (Phase-5 split out of dict_impl).
#[cfg(feature = "parallel-merge")]
pub mod parallel_merge;

// Lock-free CAS cluster (Phase-5 split out of dict_impl).
pub mod lockfree_cas;

// Thin production-write-path router for the lock-free overlay (the SOLE
// representation since L3.3). The byte seam impl of the shared
// `LockFreeOverlay<ByteKey, V, S>` trait lives here: `route_overlay()` + the
// overlay publishers (the owned tree is deleted).
pub(crate) mod overlay_write_mode;

// M2a byte LockFreeOverlay correspondence + reestablish round-trip (in-crate
// because the read-engine skins + the `LockFreeOverlay` trait are `pub(crate)`).
#[cfg(test)]
mod overlay_correspondence_tests;

// Byte read/write routing + reject correspondence tests (in-crate: the routed
// public reads/writes are exercised against the overlay, the SOLE representation —
// every constructor installs it, so route_overlay() is universally true).
#[cfg(test)]
mod overlay_routing_tests;

// Phase 5/6 byte overlay-eviction correspondence tests (OE3/OE5/OE8 twins of char's
// `overlay_eviction_driver_correspondence`). In-crate because they drive the lifted
// `pub(crate)` `OverlayEvictable` primitives + the `pub(crate)` `evict_overlay_nodes`
// byte driver + the `bench_*` eviction surface, and inspect the overlay-internal state
// (an OnDisk overlay child after eviction, the M-2a stamp 1c guard).
#[cfg(test)]
mod overlay_eviction_byte_tests;

// Per-monomorph value-route for the byte counter increment (increment_bytes) — the
// byte twin of char's `lockfree_value_route`. SAFE `Any` dispatch to the `<u64, S>`
// durable primitives; `None` for non-u64 `V` (caller runs the general overlay
// value-CAS path).
pub(crate) mod lockfree_value_route;

// Document-transaction execution methods (Phase-5 split out of dict_impl).
pub mod document_tx;

// Atomic read-modify-write operations (Phase-5 split out of dict_impl).
pub mod atomic_ops;

// ARTrie + EvictableARTrie trait impls for SharedARTrie<V> (Phase-5 split out of dict_impl).
pub mod shared_trait_impl;

// Public iteration API (iter / iter_strings / iter_prefix wrappers).
pub mod public_iter;

// IoUringDiskManager-specific constructors (Phase-5 split out of dict_impl).
#[cfg(feature = "io-uring-backend")]
pub mod io_uring_ctor;

// Dictionary / MappedDictionary / Debug trait impls (Phase-5 split out of dict_impl).
pub mod dictionary_traits;

// PersistentARTrie::compact (file-rewrite compaction) — Phase-5 split out of dict_impl.
pub mod compaction_impl;

// Persistence/durability/stats public API (Phase-5 split out of dict_impl).
pub mod persistence_api;

// Byte seam impl of the shared OverlayCheckpoint route-split (M2b). The overlay is
// the SOLE representation (`route_overlay()` universally true), so the route-split
// always runs the overlay arm.
pub(crate) mod overlay_checkpoint;

// Byte overlay fault-in primitive (`load_overlay_node_from_disk`) + the SAFE
// `OverlayFaulter` impl that lets the overlay-backed `DictionaryNode` resolve
// `Child::OnDisk` overlay children during a graph walk. F7 BLOCKER-1.
pub(crate) mod overlay_fault;

// MmapDiskManager-specific constructors (Phase-5 split out of dict_impl).
pub mod mmap_ctor;

// Public mutation API (insert/remove/batch wrappers) — Phase-5 split out of dict_impl.
pub mod mutation_api;

// Disk-loading helpers (load_root_from_disk + variants) — Phase-5 split out of dict_impl.
pub mod disk_load;

// Merge API (merge_from/merge_replace/merge_from_batched*) — Phase-5 split out of dict_impl.
pub mod merge_api;

// Disk-ref resolution + prefetch helpers — Phase-5 split out of dict_impl.
pub mod disk_resolve;

// On-disk serialization helpers (persist_to_disk + serialize_*) — Phase-5 split out of dict_impl.
pub mod serialize_impl;

// Arena-aware prefix iteration (navigate_to_prefix_with_arena, collect_terms_with_arena,
// iter_prefix_with_arena, iter_prefix_with_values_and_arena) — Phase-5 split out of dict_impl.
pub mod arena_iter;

// Cursor-based prefix iteration (iter_prefix_from_cursor) — Phase-5 split.
pub mod cursor_iter;

// F5 (Slice 3): the direct dense→overlay reopen loader — `load_root_immutable`
// (eager-load owned + iterative walk-converter owned→overlay). Gated OFF by default
// (`LockFreeOverlay::USE_F5_REOPEN_LOADER`); see `docs/design/slice3-f5-loader-impl.md`.
pub(crate) mod f5_loader;

// Page-aware prefix-iteration result types (Phase-5 split out of dict_impl).
pub mod prefix_term;

// DFS iterators (TermIterator + TermValueIterator + IterState).
pub mod iterators;

// Node serialization
pub mod serialization;

// B-trie string buckets
pub mod bucket;

// Bucket ↔ ART transitions
pub mod transitions;

// Dictionary node implementation
pub mod node_impl;

// Dictionary trait implementation
pub mod dict_impl;

// Zipper implementation
pub mod zipper;

// Write-ahead log for crash recovery (relocated to core)
pub use crate::persistent_artrie_core::wal;

// WAL management trait for shared WAL operations
pub use crate::persistent_artrie_core::wal_managed;

// Crash recovery (relocated to core)
pub use crate::persistent_artrie_core::recovery;

// Epoch-based checkpoint metadata/tracking (relocated to core)
pub use crate::persistent_artrie_core::epoch;

// Group commit for WAL batching (relocated to core)
#[cfg(feature = "group-commit")]
pub use crate::persistent_artrie_core::group_commit;

// Prefetching for DFS traversal
pub use crate::persistent_artrie_core::prefetch;

// Concurrency controls - optimistic lock coupling (relocated to core)
pub use crate::persistent_artrie_core::concurrency;

// Traversal context for block caching
pub mod traversal_context;

// Dirty tracking for incremental checkpoints (relocated to core)
pub use crate::persistent_artrie_core::dirty_tracker;

// Hash-based deduplication for space efficiency
pub mod dedup;

// Relative offset encoding for space-efficient child pointers
pub mod relative_encoding;

// Memory pressure monitoring for proactive eviction
pub use crate::persistent_artrie_core::memory_monitor;

// Memory pressure-driven node eviction (relocated to core)
pub use crate::persistent_artrie_core::eviction;

// Adaptive buffer pool sizing
pub use crate::persistent_artrie_core::adaptive_pool;

// Per-node logging for near-instant recovery
pub mod per_node_log;

// Version-based checkpoint management
pub use crate::persistent_artrie_core::version_checkpoint;

// MVCC-lite read transactions
pub mod mvcc;

// Version garbage collection
pub use crate::persistent_artrie_core::version_gc;

// Re-exports for convenience
pub use error::{PersistentARTrieError, Result, SwizzleError};
pub use path_compression::{PrefixMatchResult, SplitPrefix};
pub use swizzled_ptr::{DiskLocation, NodeType, SwizzledPtr};

// Bucket types
pub use bucket::{
    BucketError, BucketHeader, SplitByByteResult, SplitResult, StringBucket, StringEntry,
};

// Transition types
//
// L3.3c: the owned bucket↔ART transition surface (`art_node_to_bucket`,
// `bucket_to_art_node`, `should_convert_bucket_to_art`, `should_merge_art_to_bucket`,
// `ArtToBucketResult`, `BucketToArtResult`, `TransitionError`) was deleted with the
// owned tree. `ChildNode` (the disk-decode child pointer) survives.
pub use transitions::ChildNode;

// Node types
pub use node_impl::PersistentARTrieNode;

// Dictionary types
pub use dict_impl::{PersistentARTrie, TermIterator, TermValueIterator};

// Parallel merge extension trait
#[cfg(feature = "parallel-merge")]
pub use dict_impl::SharedARTrieParallelExt;

/// Thread-safe handle for `PersistentARTrie`.
///
/// **F4 (the lock collapse):** this is now a bare `Arc<PersistentARTrie<V,S>>` —
/// the outer `RwLock` is DELETED. Overlay reads AND writes are fully lock-free;
/// the only operations that still need mutual exclusion take dedicated inner locks
/// (`checkpoint_lock`, the wrapped `root` `RwLock` for the dormant owned path, the
/// `eviction_coordinator` `Mutex`, `merge_lock`) — never the handle. A
/// backward-compatible `.read()`/`.write()` API is preserved by
/// [`SharedTrieAccess`](crate::persistent_artrie_core::shared_access::SharedTrieAccess)
/// (both return a transparent guard that derefs to `&T`; there is no lock), so the
/// ~270 historical call sites and the `liblevenshtein-rust` sibling compile
/// unchanged against the collapsed type.
pub type SharedARTrie<V, S = MmapDiskManager> = std::sync::Arc<PersistentARTrie<V, S>>;

#[doc(inline)]
pub use crate::persistent_artrie_core::shared_access::SharedTrieAccess;

// F4: the concrete `.read()/.write()` shim impl on the collapsed byte handle.
// CONCRETE (not a blanket `Arc<T>`) so it never shadows the inherent
// `RwLock::{read,write}` on the crate's `Arc<RwLock<…>>` manager handles.
impl<V: crate::value::DictionaryValue, S: BlockStorage>
    crate::persistent_artrie_core::shared_access::SharedTrieAccess
    for std::sync::Arc<PersistentARTrie<V, S>>
{
    type Target = PersistentARTrie<V, S>;

    #[inline]
    fn read(
        &self,
    ) -> crate::persistent_artrie_core::shared_access::TrieAccessGuard<'_, PersistentARTrie<V, S>>
    {
        crate::persistent_artrie_core::shared_access::TrieAccessGuard::from_ref(self.as_ref())
    }

    #[inline]
    fn write(
        &self,
    ) -> crate::persistent_artrie_core::shared_access::TrieAccessGuard<'_, PersistentARTrie<V, S>>
    {
        crate::persistent_artrie_core::shared_access::TrieAccessGuard::from_ref(self.as_ref())
    }
}

// Arena-aware iteration types
pub use dict_impl::{PrefixTermWithArena, PrefixTermWithValueAndArena};

// Per-document transaction types
pub use dict_impl::{DocumentTransaction, DurabilityPolicy, TransactionState};

// Compaction types
pub use dict_impl::{CompactionConfig, CompactionProgress, CompactionStats};

// Zipper types
pub use zipper::PersistentARTrieZipper;

pub use block_storage::{AlignedBlock, BlockStorage};
pub use buffer_manager::{BufferManager, BufferPoolStats, PageReadGuard, PageWriteGuard};
pub use disk_manager::{DiskManager, FileHeader, MmapDiskManager, BLOCK_SIZE, MAX_BLOCK_COUNT};

#[cfg(feature = "io-uring-backend")]
pub use io_uring_disk_manager::IoUringDiskManager;

// Arena types
pub use arena::{
    ArenaHeader, ByteNodeArena, ByteNodeArenaV2, SlotEntry, VarintSlotEntry, ARENA_MAGIC,
    ARENA_MAGIC_V2, ARENA_VERSION, ARENA_VERSION_V2, FLAG_VARINT_DIRECTORY, HEADER_SIZE,
    MIN_FREE_SPACE, SLOT_SIZE,
};

pub use arena_manager::{
    ArenaManager, ArenaSlot, ArenaStats, FlushConfig, FlushStats, ReservedSlots,
};

// Compact encoding types
pub use compact_encoding::{
    compact_node_types, determine_ptr_width, read_varint_from_slice, varint_size,
    write_varint_to_vec, CompactHeader, DecodedCompactByteNode, COMPACT_HEADER_SIZE,
    VARINT_LEN_BIAS, VARINT_MAX_SINGLE_BYTE,
};

// WAL types
pub use wal::{Lsn, WalConfig, WalHeader, WalReader, WalRecord, WalRecordType, WalWriter};

// Async WAL types for concurrent writes during sync
pub use wal::{
    collect_all_segments, AsyncWalConfig, AsyncWalError, AsyncWalWriter, PendingSegment,
    SegmentSyncManager, StdFsync, SyncHandle, WalSyncBackend,
};

// io_uring-based WAL fsync backend (Linux-only, requires `io-uring-backend` feature)
#[cfg(feature = "io-uring-backend")]
pub use wal::IoUringFsync;

// WAL management trait for shared WAL operations
pub use wal_managed::{create_async_wal, open_async_wal, open_or_create_async_wal, WalManaged};

// Group commit types (opt-in feature for slower storage)
#[cfg(feature = "group-commit")]
pub use group_commit::{GroupCommitConfig, GroupCommitCoordinator, GroupCommitStats};

// Recovery types
pub use recovery::{
    collect_all_wal_segments, detect_corruption, find_wal_archive_segments,
    find_wal_pending_segments, get_segment_first_lsn, rebuild_from_wal_segments,
    recovered_operations_from_record, sort_segments_by_lsn, CorruptionType, IncrementalRecovery,
    RecoveredOperation, RecoveredState, RecoveryError, RecoveryManager, RecoveryMode,
    RecoveryReport, RecoveryStats,
};

// Epoch-based checkpointing types
pub use epoch::{
    CheckpointManager, CheckpointMeta, EpochConfig, EpochId, EpochMetadata, EpochState, EpochStats,
};

// Prefetch types
pub use prefetch::{
    AdaptivePrefetcher, PrefetchHint, PrefetchRequest, PrefetchStats, PrefetchStatsSnapshot,
    PrefetchStrategy, Prefetcher,
};

// Concurrency types
pub use concurrency::{
    EpochGuard, EpochManager, LockCoupling, MvccReadContext, OptimisticCell, OptimisticReadGuard,
    OptimisticVersion, RetryStats, TrieStats, TrieStatsSnapshot, WriteGuard,
};

// Traversal context types
pub use traversal_context::{LightweightTraversalContext, TraversalContext, TraversalStats};

// Dirty tracker types
pub use dirty_tracker::{BatchDirtyTracker, DirtyTracker, DirtyTrackerStats};

// Deduplication types
pub use dedup::{
    BatchDeduplicator, DeduplicatingArenaManager, DeduplicatorStats, NodeDeduplicator,
};

// Relative encoding types
pub use relative_encoding::{
    decode_child_pointer, decode_children, decode_sequential_siblings, encode_child_pointer,
    encode_children, encode_sequential_siblings, encoded_size, is_same_arena,
    try_decode_child_pointer, try_decode_children, try_decode_full, try_decode_relative,
    try_decode_sequential_siblings, try_encode_child_pointer, try_encode_children,
    try_encode_sequential_siblings, try_encoded_size, RelativeEncodingError,
    RelativeEncodingResult, CROSS_ARENA_SIZE, FLAG_CROSS_ARENA, FLAG_RELATIVE_OFFSETS,
    FLAG_SEQUENTIAL_SIBLINGS,
};

// Memory pressure monitoring types
pub use memory_monitor::{
    MemoryMonitorStats, MemoryPressureConfig, MemoryPressureLevel, MemoryPressureMonitor,
    MemoryStats, PressureCallback, PsiMetrics,
};

// Adaptive buffer pool sizing types
pub use adaptive_pool::{
    AdaptivePoolConfig, AdaptivePoolController, AdaptivePoolStats, CacheStats,
};

// Eviction types for bounded-memory operation
pub use eviction::{
    AccessTracker, DiskLocationRegistry, EvictionConfig, EvictionCoordinator, EvictionStats,
    EvictionUrgency, LruRegistry,
};

// Per-node logging types
pub use per_node_log::{
    DirtyNodeTracker, InlineLog, NodeId, NodeLogEntry, NodeRecoveryResult, PageId,
    PerNodeLogConfig, PerNodeLogStats, PerNodeLogStatsAtomic, RecoveryResult,
};

// Version checkpoint types (Phase 7)
pub use version_checkpoint::{VersionCheckpointManager, VersionCheckpointStats, VersionSnapshot};

// MVCC-lite read transaction types (Phase 8)
pub use mvcc::{MvccStats, MvccStatsTracker, ReadTransaction, TrieRoot};

// Version garbage collection types (Phase 9)
pub use version_gc::{GcCandidate, GcConfig, GcStats, ReaderGuard, VersionGcRegistry};

/// Maximum key length supported (64KB - 1)
pub const MAX_KEY_LENGTH: usize = 65535;

/// Maximum value size supported (limited by bucket size)
pub const MAX_VALUE_SIZE: usize = 8192;

/// Default buffer pool size (256 pages = 64MB)
pub const DEFAULT_BUFFER_POOL_SIZE: usize = 256;

/// B-trie bucket page size (8KB)
pub const BUCKET_PAGE_SIZE: usize = 8192;

/// Maximum entries per bucket before split
pub const MAX_BUCKET_ENTRIES: usize = 256;

/// Path compression prefix maximum length
pub const MAX_PREFIX_LENGTH: usize = 12;
