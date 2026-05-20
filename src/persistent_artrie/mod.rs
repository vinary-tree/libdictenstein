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
//! ```rust,ignore
//! use libdictenstein::persistent_artrie::PersistentARTrie;
//!
//! // Create a new persistent dictionary
//! let dict = PersistentARTrie::create("words.part")?;
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
//! | `Immediate` | Every CommitTx      | Full         | ACID compliance (default)       |
//! | `GroupCommit`| Batched by coordinator| Full      | High-throughput workloads       |
//! | `Periodic`  | At checkpoints only | Bounded loss | Performance-critical            |
//! | `None`      | Never               | None         | Testing only                    |
//!
//! The default `Immediate` policy ensures that committed transactions are immediately
//! durable on disk. The `GroupCommit` policy batches fsync calls for better throughput
//! while maintaining full durability. `Periodic` trades some durability for performance.

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

// Epoch-based automatic checkpointing (relocated to core)
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
pub use bucket::{BucketError, BucketHeader, SplitByByteResult, SplitResult, StringBucket, StringEntry};

// Transition types
pub use transitions::{
    art_node_to_bucket, bucket_to_art_node, should_convert_bucket_to_art,
    should_merge_art_to_bucket, ArtToBucketResult, BucketToArtResult, ChildNode, TransitionError,
};

// Node types
pub use node_impl::PersistentARTrieNode;

// Dictionary types
pub use dict_impl::{PersistentARTrie, TermIterator, TermValueIterator};

// Parallel merge extension trait
#[cfg(feature = "parallel-merge")]
pub use dict_impl::SharedARTrieParallelExt;

/// Thread-safe wrapper for `PersistentARTrie`.
///
/// This type alias provides `Arc<RwLock<...>>` semantics for concurrent access
/// to the disk-backed byte-level trie.
pub type SharedARTrie<V, S = MmapDiskManager> = std::sync::Arc<parking_lot::RwLock<PersistentARTrie<V, S>>>;

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
pub use disk_manager::{DiskManager, MmapDiskManager, FileHeader, BLOCK_SIZE, MAX_BLOCK_COUNT};

#[cfg(feature = "io-uring-backend")]
pub use io_uring_disk_manager::IoUringDiskManager;

// Arena types
pub use arena::{
    ArenaHeader, ByteNodeArena, ByteNodeArenaV2, SlotEntry, VarintSlotEntry,
    ARENA_MAGIC, ARENA_MAGIC_V2, ARENA_VERSION, ARENA_VERSION_V2,
    FLAG_VARINT_DIRECTORY, HEADER_SIZE, MIN_FREE_SPACE, SLOT_SIZE,
};

pub use arena_manager::{ArenaManager, ArenaSlot, ArenaStats, FlushConfig, FlushStats, ReservedSlots};

// Compact encoding types
pub use compact_encoding::{
    CompactHeader, DecodedCompactByteNode, COMPACT_HEADER_SIZE, VARINT_LEN_BIAS, VARINT_MAX_SINGLE_BYTE,
    compact_node_types, determine_ptr_width, read_varint_from_slice, write_varint_to_vec, varint_size,
};

// WAL types
pub use wal::{Lsn, WalConfig, WalHeader, WalReader, WalRecord, WalRecordType, WalWriter};

// Async WAL types for concurrent writes during sync
pub use wal::{
    AsyncWalConfig, AsyncWalError, AsyncWalWriter, PendingSegment, SegmentSyncManager, SyncHandle,
    WalSyncBackend, StdFsync,
    collect_all_segments,
};

// io_uring-based WAL fsync backend (Linux-only, requires `io-uring-backend` feature)
#[cfg(feature = "io-uring-backend")]
pub use wal::IoUringFsync;

// WAL management trait for shared WAL operations
pub use wal_managed::{
    WalManaged, create_async_wal, open_async_wal, open_or_create_async_wal,
};

// Group commit types (opt-in feature for slower storage)
#[cfg(feature = "group-commit")]
pub use group_commit::{GroupCommitConfig, GroupCommitCoordinator, GroupCommitStats};

// Recovery types
pub use recovery::{
    CorruptionType, IncrementalRecovery, RecoveredOperation, RecoveredState, RecoveryError,
    RecoveryManager, RecoveryMode, RecoveryReport, RecoveryStats,
    detect_corruption, find_wal_archive_segments, find_wal_pending_segments,
    collect_all_wal_segments, get_segment_first_lsn, sort_segments_by_lsn,
    rebuild_from_wal_segments,
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
pub use traversal_context::{
    LightweightTraversalContext, TraversalContext, TraversalStats,
};

// Dirty tracker types
pub use dirty_tracker::{
    BatchDirtyTracker, DirtyTracker, DirtyTrackerStats,
};

// Deduplication types
pub use dedup::{
    BatchDeduplicator, DeduplicatingArenaManager, DeduplicatorStats, NodeDeduplicator,
};

// Relative encoding types
pub use relative_encoding::{
    encode_child_pointer, decode_child_pointer, encode_children, decode_children,
    encode_sequential_siblings, decode_sequential_siblings, encoded_size, is_same_arena,
    FLAG_CROSS_ARENA, FLAG_RELATIVE_OFFSETS, FLAG_SEQUENTIAL_SIBLINGS, CROSS_ARENA_SIZE,
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
    AccessTracker, DiskLocationRegistry, EvictionConfig, EvictionCoordinator,
    EvictionStats, EvictionUrgency, LruRegistry,
};

// Per-node logging types
pub use per_node_log::{
    DirtyNodeTracker, InlineLog, NodeId, NodeLogEntry, NodeRecoveryResult,
    PageId, PerNodeLogConfig, PerNodeLogStats, PerNodeLogStatsAtomic, RecoveryResult,
};

// Version checkpoint types (Phase 7)
pub use version_checkpoint::{
    VersionCheckpointManager, VersionCheckpointStats, VersionSnapshot,
};

// MVCC-lite read transaction types (Phase 8)
pub use mvcc::{
    MvccStats, MvccStatsTracker, ReadTransaction, TrieRoot,
};

// Version garbage collection types (Phase 9)
pub use version_gc::{
    GcCandidate, GcConfig, GcStats, ReaderGuard, VersionGcRegistry,
};

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
