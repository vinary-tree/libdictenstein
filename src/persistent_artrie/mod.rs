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
pub mod error;
pub mod swizzled_ptr;

pub mod disk_manager;

pub mod buffer_manager;

// Arena allocation for efficient node storage
pub mod arena;

pub mod arena_manager;

// Compact variable-width encoding
pub mod compact_encoding;

// ART node types
pub mod nodes;

// Path compression operations
pub mod path_compression;

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

// Write-ahead log for crash recovery
pub mod wal;

// Crash recovery
pub mod recovery;

// Epoch-based automatic checkpointing
pub mod epoch;

// Group commit for WAL batching (opt-in feature for slower storage)
#[cfg(feature = "group-commit")]
pub mod group_commit;

// Prefetching for DFS traversal
pub mod prefetch;

// Concurrency controls - optimistic lock coupling
pub mod concurrency;

// Traversal context for block caching
pub mod traversal_context;

// Dirty tracking for incremental checkpoints
pub mod dirty_tracker;

// Hash-based deduplication for space efficiency
pub mod dedup;

// Relative offset encoding for space-efficient child pointers
pub mod relative_encoding;

// Memory pressure monitoring for proactive eviction
pub mod memory_monitor;

// Adaptive buffer pool sizing
pub mod adaptive_pool;

// Per-node logging for near-instant recovery
pub mod per_node_log;

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

// Arena-aware iteration types
pub use dict_impl::{PrefixTermWithArena, PrefixTermWithValueAndArena};

// Per-document transaction types
pub use dict_impl::{DocumentTransaction, DurabilityPolicy, TransactionState};

// Zipper types
pub use zipper::PersistentARTrieZipper;

pub use buffer_manager::{BufferManager, BufferPoolStats, PageReadGuard, PageWriteGuard};
pub use disk_manager::{DiskManager, FileHeader, BLOCK_SIZE, MAX_BLOCK_COUNT};

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
pub use wal::{GroupCommit, Lsn, WalConfig, WalHeader, WalReader, WalRecord, WalRecordType, WalWriter};

// Group commit types (opt-in feature for slower storage)
#[cfg(feature = "group-commit")]
pub use group_commit::{GroupCommitConfig, GroupCommitCoordinator, GroupCommitStats};

// Recovery types
pub use recovery::{
    CorruptionType, IncrementalRecovery, RecoveredOperation, RecoveredState, RecoveryError,
    RecoveryManager, RecoveryMode, RecoveryReport, RecoveryStats,
    detect_corruption, find_wal_archive_segments, rebuild_from_wal_segments,
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

// Per-node logging types
pub use per_node_log::{
    DirtyNodeTracker, InlineLog, NodeId, NodeLogEntry, NodeRecoveryResult,
    PageId, PerNodeLogConfig, PerNodeLogStats, PerNodeLogStatsAtomic, RecoveryResult,
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
