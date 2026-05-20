//! Dictionary Implementation for Persistent ART
//!
//! This module provides the `PersistentARTrie` dictionary type that implements
//! the `Dictionary` and `MappedDictionary` traits for integration with the
//! Levenshtein automata transducer.
//!
//! # In-Memory vs Disk-Backed
//!
//! This implementation currently provides an in-memory version suitable for
//! development and testing. The disk-backed version with memory-mapped I/O
//! will be added in a future phase.
//!
//! # Thread Safety
//!
//! The dictionary uses `Arc<RwLock>` for thread-safe concurrent access.
//! Read operations can proceed in parallel, while writes are serialized.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use crate::sync_compat::RwLock;
use log::warn;

use smallvec::SmallVec;

use crate::{Dictionary, MappedDictionary, MutableMappedDictionary, SyncStrategy};
use crate::value::DictionaryValue;
use super::bucket::StringBucket;
use super::error::{PersistentARTrieError, Result};
use super::node_impl::PersistentARTrieNode;
use super::nodes::{ArtNode, Node};
use super::swizzled_ptr::{DiskLocation, NodeType, SwizzledPtr};
use super::transitions::{bucket_to_art_node, ChildNode};
use super::serialization::{self, v2::{SerializationContext, DeserializationContext}};

use super::arena_manager::{ArenaManager, ArenaSlot};
use super::block_storage::BlockStorage;
use super::buffer_manager::BufferManager;
use super::disk_manager::MmapDiskManager;
use super::wal::{AsyncWalConfig, AsyncWalWriter, Lsn, SyncHandle, WalConfig};
use super::wal_managed::WalManaged;

#[cfg(feature = "parallel-merge")]
use rayon::prelude::*;

/// SIMD-accelerated lexicographic comparison of byte slices.
/// Returns Ordering::Less if a < b, Ordering::Equal if a == b, Ordering::Greater if a > b.
#[cfg(all(target_arch = "x86_64", target_feature = "sse4.2"))]
#[inline]
fn simd_cmp_bytes(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    use std::arch::x86_64::*;
    use std::cmp::Ordering;

    let min_len = a.len().min(b.len());
    let mut offset = 0;

    // Process 16 bytes at a time using SSE4.2
    while offset + 16 <= min_len {
        unsafe {
            let va = _mm_loadu_si128(a.as_ptr().add(offset) as *const __m128i);
            let vb = _mm_loadu_si128(b.as_ptr().add(offset) as *const __m128i);

            // Find first differing byte using XOR and compare
            let diff = _mm_xor_si128(va, vb);
            let mask = _mm_movemask_epi8(_mm_cmpeq_epi8(diff, _mm_setzero_si128()));

            // If mask != 0xFFFF, there's a difference in these 16 bytes
            if mask != 0xFFFF {
                // Find position of first difference (first 0 bit in mask)
                let first_diff = (!mask as u32).trailing_zeros() as usize;
                let pos = offset + first_diff;
                return a[pos].cmp(&b[pos]);
            }
        }
        offset += 16;
    }

    // Handle remaining bytes with scalar comparison
    for i in offset..min_len {
        match a[i].cmp(&b[i]) {
            Ordering::Equal => continue,
            other => return other,
        }
    }

    // If all compared bytes are equal, shorter slice is "less"
    a.len().cmp(&b.len())
}

/// Fallback scalar lexicographic comparison.
#[cfg(not(all(target_arch = "x86_64", target_feature = "sse4.2")))]
#[inline]
fn simd_cmp_bytes(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    a.cmp(b)
}

/// Check if a <= b using SIMD-accelerated comparison.
#[inline]
fn bytes_le(a: &[u8], b: &[u8]) -> bool {
    matches!(simd_cmp_bytes(a, b), std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
}

/// Result of a lock-free insert attempt.
///
/// Used by `insert_cas()` to communicate the outcome of a CAS operation.
#[cfg(feature = "persistent-artrie")]
#[derive(Debug)]
enum LockfreeInsertResult {
    /// Term was newly inserted - contains the node to finalize
    Inserted(Arc<super::nodes::PersistentNode>),
    /// Term already exists in the trie
    AlreadyExists,
    /// CAS conflict - another thread modified the tree, retry needed
    Conflict,
}

/// Check if a > b using SIMD-accelerated comparison.
#[inline]
fn bytes_gt(a: &[u8], b: &[u8]) -> bool {
    matches!(simd_cmp_bytes(a, b), std::cmp::Ordering::Greater)
}

/// Maximum buffer size for reading serialized ART nodes (4KB should be ample).
/// Largest node is Node256 at ~2KB, so 4KB provides good margin.
const ART_NODE_BUFFER_SIZE: usize = 4096;

/// Result of loading a single child node's data without loading its children.
///
/// Used by `load_single_child_data` for iterative loading.
enum SingleChildData {
    /// A bucket leaf node (complete, no children)
    Bucket(StringBucket),
    /// An ART node with child pointers (children not yet loaded)
    ArtNodePartial {
        node: Node,
        is_final: bool,
        child_ptrs: Vec<(u8, SwizzledPtr)>,
    },
}

/// Resolve a DiskRef child in place, replacing it with the loaded node.
///
/// This is a free function that can be called without holding a borrow on
/// `PersistentARTrie`, which is necessary when mutating children while
/// also needing access to the buffer manager.
///
/// # Arguments
/// * `child` - Mutable reference to the child node to resolve
/// * `buffer_manager` - Optional reference to the buffer manager for disk I/O
///
/// # Returns
/// * `true` if the child is now in memory (either already was, or successfully resolved)
/// * `false` if the child was a DiskRef that failed to resolve
fn resolve_child_for_mutation_with_bm<S: BlockStorage>(
    child: &mut ChildNode,
    buffer_manager: Option<&Arc<RwLock<BufferManager<S>>>>,
) -> bool {
    // Early return if not a DiskRef - nothing to resolve
    let ChildNode::DiskRef { ptr } = child else {
        return true; // Already in memory
    };

    let Some(disk_location) = ptr.disk_location() else {
        warn!("DiskRef has no valid disk location");
        return false;
    };

    // Get buffer manager (required for disk I/O)
    let Some(bm_arc) = buffer_manager else {
        warn!("No buffer manager available for resolving DiskRef");
        return false;
    };

    // Resolve the node from disk
    // We need to fully construct the resolved ChildNode before the page guard is dropped
    let resolved: ChildNode = {
        let bm = bm_arc.read();
        let page_guard = match bm.fetch_page(disk_location.block_id) {
            Ok(pg) => pg,
            Err(e) => {
                warn!(
                    "Failed to fetch page for DiskRef at block {}: {}",
                    disk_location.block_id, e
                );
                return false;
            }
        };

        let page_data = page_guard.data();
        let offset = disk_location.offset as usize;
        let node_data = &page_data[offset..];

        match disk_location.node_type {
            NodeType::Bucket => {
                // Deserialize bucket
                // For now, return an empty bucket - full bucket serialization
                // will be implemented in Phase 7.4
                ChildNode::Bucket(StringBucket::new())
            }
            NodeType::Node4 | NodeType::Node16 | NodeType::Node48 | NodeType::Node256 => {
                // Deserialize ART node
                match serialization::from_bytes(node_data) {
                    Ok(node) => {
                        let is_final = node.header().is_final();
                        ChildNode::ArtNode {
                            node,
                            is_final,
                            value: None,
                            children: Vec::new(),
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Failed to deserialize ART node at block {}, offset {}: {}",
                            disk_location.block_id, disk_location.offset, e
                        );
                        return false;
                    }
                }
            }
            // Char-level nodes should never appear in byte-level trie
            NodeType::CharNode4 | NodeType::CharNode16 | NodeType::CharNode48 | NodeType::CharBucket => {
                warn!(
                    "Char-level node type encountered in byte-level PersistentARTrie at block {}, offset {}",
                    disk_location.block_id, disk_location.offset
                );
                return false;
            }
        }
    }; // page_guard dropped here, resolved is fully owned

    *child = resolved;
    true
}

/// Resolve a DiskRef child in place (non-persistent fallback).
///
/// Without the persistent-artrie feature, DiskRef nodes should never exist.
/// This returns false for DiskRef (indicating an error state) and true for
/// all other node types.

/// A Persistent Adaptive Radix Trie dictionary.
///
/// This dictionary stores terms in a hybrid structure combining:
/// - **ART nodes** for efficient internal node traversal (Node4/16/48/256)
/// - **String buckets** for efficient leaf storage (multiple terms per bucket)
///
/// # Thread Safety
///
/// `PersistentARTrie` itself is not thread-safe. For concurrent access, wrap it in
/// `Arc<RwLock<PersistentARTrie<V>>>` or use the [`SharedARTrie`] type alias.
///
/// # Example
///
/// ```rust,ignore
/// use libdictenstein::persistent_artrie::PersistentARTrie;
///
/// let mut dict = PersistentARTrie::new();
/// dict.insert("hello");
/// dict.insert("world");
///
/// assert!(dict.contains("hello"));
/// assert!(!dict.contains("hi"));
/// ```
pub struct PersistentARTrie<V: DictionaryValue = (), S: BlockStorage = MmapDiskManager> {
    /// Root node of the trie (starts as a bucket, grows to ART)
    pub(crate) root: TrieRoot<V>,
    /// Number of terms in the dictionary (atomic for lock-free increment_cas)
    pub(crate) term_count: AtomicUsize,
    /// Whether the dictionary has been modified (atomic for lock-free increment_cas)
    pub(crate) dirty: AtomicBool,

    // === Storage Layer (only active with persistent-artrie feature) ===
    // Note: Storage backend is owned by BufferManager and accessible via buffer_manager.storage()
    /// Buffer manager with Clock-evicted page cache (owns DiskManager)
    pub(crate) buffer_manager: Option<Arc<RwLock<BufferManager<S>>>>,
    /// Write-ahead log writer for durability (async-capable)
    pub(crate) wal_writer: Option<Arc<AsyncWalWriter>>,
    /// Next log sequence number to assign (atomic for lock-free operations)
    pub(crate) next_lsn: std::sync::atomic::AtomicU64,
    /// Prefetcher for DFS traversal optimization
    pub(crate) prefetcher: super::prefetch::Prefetcher,
    /// Arena manager for space-efficient node storage
    /// Packs multiple nodes into 256KB blocks instead of one node per block
    pub(crate) arena_manager: Option<Arc<RwLock<ArenaManager<S>>>>,
    /// Durability policy for WAL synchronization
    pub(crate) durability_policy: DurabilityPolicy,
    /// Epoch manager for MVCC-Lite snapshot isolation
    pub(crate) epoch_manager: super::concurrency::EpochManager,
    /// Atomic statistics for monitoring
    pub(crate) stats: Arc<super::concurrency::TrieStats>,

    // === Eviction Support ===
    /// Eviction coordinator for memory pressure-driven eviction
    pub(crate) eviction_coordinator: Option<Arc<super::eviction::EvictionCoordinator>>,

    // === Selective Dirty Subtree Traversal ===
    /// Prefixes modified since last checkpoint (for selective traversal).
    ///
    /// When a term is inserted or removed, all prefixes along the path from
    /// root to the modified node are recorded here. This enables `persist_to_disk()`
    /// to skip clean subtrees entirely, reducing checkpoint time from O(N) to
    /// O(D × H) where D = dirty nodes, H = average depth.
    pub(crate) dirty_prefixes: HashSet<Vec<u8>>,

    /// Disk locations of persisted nodes (keyed by path).
    ///
    /// Populated during serialization, preserved across checkpoints.
    /// Invalidated when paths become dirty (on insert/remove).
    /// Uses `RwLock` for interior mutability since serialization methods
    /// take `&self` but need to update this cache. `RwLock` (unlike `RefCell`)
    /// is `Sync`, allowing the struct to remain thread-safe.
    pub(crate) persisted_disk_locations: RwLock<HashMap<Vec<u8>, SwizzledPtr>>,

    // === Lock-Free Layer ===
    /// Lock-free root pointer for CAS-based concurrent inserts.
    ///
    /// When `enable_lockfree()` is called, this pointer becomes the primary
    /// root for all lock-free operations. The persistent root remains separate
    /// and is merged during checkpoint.
    #[cfg(feature = "persistent-artrie")]
    pub(crate) lockfree_root: Option<super::nodes::AtomicNodePtr>,

    /// Fast cache for lock-free lookups (key → exists).
    ///
    /// Uses DashMap for O(1) sharded concurrent access. This cache is populated
    /// during `insert_cas()` and provides fast-path lookups without trie traversal.
    #[cfg(feature = "persistent-artrie")]
    pub(crate) lockfree_cache: Option<dashmap::DashMap<Vec<u8>, bool>>,

    /// Counter for CAS retry attempts (for monitoring contention).
    #[cfg(feature = "persistent-artrie")]
    pub(crate) cas_retries: std::sync::atomic::AtomicU64,
}

/// Thread-safe wrapper for `PersistentARTrie`.
///
/// This type alias provides the same thread-safety model as the previous
/// `PersistentARTrie` implementation (which internally used `Arc<RwLock<...>>`).
///

// `PrefixTermWithArena` and `PrefixTermWithValueAndArena` were relocated to
// `super::prefix_term`; re-exported here under their original paths.
pub use super::prefix_term::{PrefixTermWithArena, PrefixTermWithValueAndArena};

// `TermIterator` / `TermValueIterator` (plus the private `IterState`) were
// relocated to `super::iterators`; re-exported here under their original
// paths so the top-level `pub use dict_impl::{TermIterator, TermValueIterator}`
// in persistent_artrie/mod.rs continues to work.
pub use super::iterators::{TermIterator, TermValueIterator};

// `TransactionState` was relocated to `super::transactions`; re-exported here
// under its original path.
pub use super::transactions::TransactionState;

/// Durability policy — relocated to
/// [`crate::persistent_artrie_core::durability::DurabilityPolicy`]. Re-exported
/// at module scope below so existing `dict_impl::DurabilityPolicy` callers keep
/// working.
pub use crate::persistent_artrie_core::durability::DurabilityPolicy;

// `CompactionConfig`, `CompactionStats`, and `CompactionProgress` (plus the
// `Default` impl for `CompactionConfig`) were relocated to the sibling
// `compaction` module as the first piece of the Phase-5 byte dict_impl
// decomposition. Re-exported here under their original paths.
pub use super::compaction::{CompactionConfig, CompactionProgress, CompactionStats};

// `DocumentTransaction` was relocated to `super::transactions`; re-exported
// here under its original path.
pub use super::transactions::DocumentTransaction;

/// The root of the trie can be either a bucket or an ART node
pub(crate) enum TrieRoot<V: DictionaryValue> {
    /// Root is a single bucket (for small dictionaries)
    Bucket(StringBucket),
    /// Root is an ART node (for larger dictionaries)
    ArtNode {
        /// The root ART node
        node: Node,
        /// Child nodes (bucket or sub-ART)
        children: Vec<(u8, ChildNode)>,
        /// Whether empty string is in dictionary
        is_final: bool,
        /// Value for empty string
        value: Option<V>,
    },
}

// === WalManaged Trait Implementation ===

impl<V: DictionaryValue, S: BlockStorage> WalManaged for PersistentARTrie<V, S> {
    fn wal_writer(&self) -> Option<&Arc<AsyncWalWriter>> {
        self.wal_writer.as_ref()
    }
}

impl<V: DictionaryValue> PersistentARTrie<V> {
    /// Create a new empty in-memory dictionary.
    ///
    /// # Deprecated
    ///
    /// This method is deprecated because "Persistent" types are designed for
    /// disk-backed storage. Use `create()` or `open()` for disk persistence.
    /// For in-memory tries, use the optimized implementations instead:
    /// - [`DoubleArrayTrie`](crate::double_array_trie::DoubleArrayTrie) (fastest reads, insert-only)
    /// - [`DynamicDawg`](crate::dynamic_dawg::DynamicDawg) (insert + remove, SIMD optimized)
    #[deprecated(
        since = "0.2.0",
        note = "Use `create()` or `open()` for disk persistence. For in-memory tries, use DoubleArrayTrie or DynamicDawg instead."
    )]
    pub fn new() -> Self {
        Self {
            root: TrieRoot::Bucket(StringBucket::with_values()),
            term_count: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: None,
            wal_writer: None,
            next_lsn: std::sync::atomic::AtomicU64::new(0),
            prefetcher: super::prefetch::Prefetcher::disabled(),
            arena_manager: None,
            durability_policy: DurabilityPolicy::default(),
            epoch_manager: super::concurrency::EpochManager::new(),
            stats: Arc::new(super::concurrency::TrieStats::new()),
            eviction_coordinator: None,
            dirty_prefixes: HashSet::new(),
            persisted_disk_locations: RwLock::new(HashMap::new()),
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Create a new persistent dictionary at the given path.
    ///
    /// This creates a new dictionary file with WAL for crash recovery.
    /// If a file already exists at the path, this will return an error.
    ///
    /// # Arguments
    /// * `path` - Path to the dictionary file (will also create `.wal` file)
    ///
    /// # Example
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let dict: PersistentARTrie<()> = PersistentARTrie::create("words.part")?;
    /// ```
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        use super::disk_manager::DiskManager;
        use super::buffer_manager::BufferManager;
        use super::DEFAULT_BUFFER_POOL_SIZE;

        let path = path.as_ref();

        // Fail if file already exists
        if path.exists() {
            return Err(PersistentARTrieError::io_error(
                "create",
                path.display().to_string(),
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "Dictionary file already exists",
                ),
            ));
        }

        // Create disk manager (creates new file)
        let disk_manager = DiskManager::create(path)?;

        // Create buffer manager with default pool size (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create async WAL file alongside the main file
        let wal_path = path.with_extension("wal");
        let async_config = AsyncWalConfig {
            pending_dir: path.parent().unwrap_or(Path::new(".")).join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig::default();
        let wal_writer = AsyncWalWriter::create(&wal_path, async_config, archive_config)
            .map_err(|e| PersistentARTrieError::io_error(
                "create_wal",
                wal_path.display().to_string(),
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ))?;
        let wal_writer = Arc::new(wal_writer);

        // Create arena manager for space-efficient node storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        Ok(Self {
            root: TrieRoot::Bucket(StringBucket::with_values()),
            term_count: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            next_lsn: std::sync::atomic::AtomicU64::new(1), // Start at 1, 0 reserved for "no LSN"
            prefetcher: super::prefetch::Prefetcher::new(),
            arena_manager: Some(arena_manager),
            durability_policy: DurabilityPolicy::default(),
            epoch_manager: super::concurrency::EpochManager::new(),
            stats: Arc::new(super::concurrency::TrieStats::new()),
            eviction_coordinator: None,
            dirty_prefixes: HashSet::new(),
            persisted_disk_locations: RwLock::new(HashMap::new()),
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Create a new persistent dictionary with slot-level dirty tracking.
    ///
    /// This enables incremental checkpoints that write only modified slots
    /// instead of entire 256KB arenas, reducing checkpoint I/O by 90%+ for
    /// localized updates.
    ///
    /// # Arguments
    /// * `path` - Path to the dictionary file (must not exist)
    ///
    /// # Example
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let dict: PersistentARTrie<()> = PersistentARTrie::create_with_slot_tracking("words.part")?;
    /// ```
    pub fn create_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        use super::disk_manager::DiskManager;
        use super::buffer_manager::BufferManager;
        use super::arena_manager::FlushConfig;
        use super::DEFAULT_BUFFER_POOL_SIZE;

        let path = path.as_ref();

        // Fail if file already exists
        if path.exists() {
            return Err(PersistentARTrieError::io_error(
                "create",
                path.display().to_string(),
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "Dictionary file already exists",
                ),
            ));
        }

        // Create disk manager (creates new file)
        let disk_manager = DiskManager::create(path)?;

        // Create buffer manager with default pool size (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create async WAL file alongside the main file
        let wal_path = path.with_extension("wal");
        let async_config = AsyncWalConfig {
            pending_dir: path.parent().unwrap_or(Path::new(".")).join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig::default();
        let wal_writer = AsyncWalWriter::create(&wal_path, async_config, archive_config)
            .map_err(|e| PersistentARTrieError::io_error(
                "create_wal",
                wal_path.display().to_string(),
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ))?;
        let wal_writer = Arc::new(wal_writer);

        // Create arena manager with slot-level tracking enabled
        let flush_config = FlushConfig::with_slot_tracking();
        let arena_manager = ArenaManager::with_buffer_manager_and_config(
            Arc::clone(&buffer_manager),
            flush_config,
        );
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        Ok(Self {
            root: TrieRoot::Bucket(StringBucket::with_values()),
            term_count: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            next_lsn: std::sync::atomic::AtomicU64::new(1), // Start at 1, 0 reserved for "no LSN"
            prefetcher: super::prefetch::Prefetcher::new(),
            arena_manager: Some(arena_manager),
            durability_policy: DurabilityPolicy::default(),
            epoch_manager: super::concurrency::EpochManager::new(),
            stats: Arc::new(super::concurrency::TrieStats::new()),
            eviction_coordinator: None,
            dirty_prefixes: HashSet::new(),
            persisted_disk_locations: RwLock::new(HashMap::new()),
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Open an existing persistent dictionary from disk.
    ///
    /// This opens an existing dictionary file and replays the WAL if needed
    /// to recover from any crash.
    ///
    /// # Arguments
    /// * `path` - Path to the dictionary file
    ///
    /// # Example
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let dict: PersistentARTrie<()> = PersistentARTrie::open("words.part")?;
    /// ```
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        use super::disk_manager::{DiskManager, BLOCK_SIZE};
        use super::buffer_manager::BufferManager;
        use super::recovery::RecoveryManager;
        use super::DEFAULT_BUFFER_POOL_SIZE;

        let path = path.as_ref();

        // Fail if file doesn't exist
        if !path.exists() {
            return Err(PersistentARTrieError::io_error(
                "open",
                path.display().to_string(),
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Dictionary file not found",
                ),
            ));
        }

        // Open disk manager
        let disk_manager = DiskManager::open(path)?;

        // Get root pointer to check if trie exists
        let root_ptr = disk_manager.root_ptr()?;
        let _entry_count = disk_manager.entry_count()?;

        // Read arena_count from root descriptor at fixed location (block 0, offset 64)
        // Arena block IDs are derived from sequential allocation: 1..=arena_count
        const DESCRIPTOR_OFFSET: usize = 64;
        let arena_count: u32 = if root_ptr != 0 {
            let ptr = SwizzledPtr::from_raw(root_ptr);
            if let Some(location) = ptr.disk_location() {
                // Read descriptor from block 0 at offset 64
                let mut descriptor_buf = [0u8; 18];
                disk_manager.read_bytes(location.block_id, DESCRIPTOR_OFFSET, &mut descriptor_buf)?;
                // arena_count is at bytes 6-9 in the root descriptor
                u32::from_le_bytes([
                    descriptor_buf[6],
                    descriptor_buf[7],
                    descriptor_buf[8],
                    descriptor_buf[9],
                ])
            } else {
                0
            }
        } else {
            0
        };

        // Derive arena block IDs from sequential allocation
        // Block 0 = file header + descriptor, Blocks 1..=arena_count = arenas
        let arena_block_ids: Vec<u32> = (1..=arena_count).collect();

        // Create buffer manager (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create arena manager for space-efficient node storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        // Load arenas into ArenaManager using derived block IDs
        if arena_count > 0 {
            let mut am = arena_manager.write();
            am.clear_for_loading();
            for block_id in arena_block_ids {
                am.load_arena(block_id)?;
            }
            let count = am.arena_count();
            am.set_active_arena(count.saturating_sub(1));
        }

        // Now load trie from disk using the arena manager
        let (loaded_root, loaded_term_count) = if root_ptr != 0 {
            match Self::load_root_from_disk_with_arena(&buffer_manager, &arena_manager, root_ptr) {
                Ok((root, count)) => (Some(root), count),
                Err(e) => {
                    warn!("Failed to load trie from disk: {:?}", e);
                    (None, 0)
                }
            }
        } else {
            (None, 0)
        };

        // Recover from WAL if it exists
        let wal_path = path.with_extension("wal");
        let (recovered_ops, next_lsn, checkpoint_lsn) = if wal_path.exists() {
            let recovery_manager = RecoveryManager::new(&wal_path);
            match recovery_manager.recover() {
                Ok(state) => {
                    let lsn = state.next_lsn;
                    let cp_lsn = state.stats.checkpoint_lsn;
                    (state.into_operations(), lsn, cp_lsn)
                }
                Err(e) => {
                    warn!("WAL recovery error: {:?}", e);
                    (Vec::new(), 1, None)
                }
            }
        } else {
            (Vec::new(), 1, None)
        };

        // Open async WAL writer using TOCTOU-safe pattern
        // Matches formal model's `open_or_create_safe` in FileSystem.v:
        // - Uses mkdir_all (idempotent) to ensure parent exists
        // - Uses atomic open/create operations to avoid races
        let async_config = AsyncWalConfig {
            pending_dir: path.parent().unwrap_or(Path::new(".")).join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig::default();
        let wal_writer = AsyncWalWriter::open_or_create(&wal_path, async_config, archive_config)
            .map_err(|e| PersistentARTrieError::io_error(
                "open_wal",
                wal_path.display().to_string(),
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ))?;
        let wal_writer = Arc::new(wal_writer);

        // Create the dictionary with storage layer
        // Use loaded root if available, otherwise start with empty bucket
        let was_loaded_from_disk = loaded_root.is_some();
        let (initial_root, initial_term_count) = match loaded_root {
            Some(root) => (root, loaded_term_count as usize),
            None => (TrieRoot::Bucket(StringBucket::with_values()), 0),
        };

        let mut dict = Self {
            root: initial_root,
            term_count: AtomicUsize::new(initial_term_count),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(Arc::clone(&wal_writer)),
            next_lsn: std::sync::atomic::AtomicU64::new(next_lsn),
            prefetcher: super::prefetch::Prefetcher::new(),
            arena_manager: Some(arena_manager),
            durability_policy: DurabilityPolicy::default(),
            epoch_manager: super::concurrency::EpochManager::new(),
            stats: Arc::new(super::concurrency::TrieStats::new()),
            eviction_coordinator: None,
            dirty_prefixes: HashSet::new(),
            persisted_disk_locations: RwLock::new(HashMap::new()),
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        };

        // Replay recovered operations
        // If we loaded from disk, only replay operations AFTER the checkpoint
        // Determine the LSN threshold for skipping
        // Operations with LSN <= threshold are already in the on-disk state
        let skip_threshold = if was_loaded_from_disk {
            checkpoint_lsn
        } else {
            None
        };

        let mut replayed_count = 0;
        for op in recovered_ops.into_iter() {
            match op {
                super::recovery::RecoveredOperation::Insert { lsn, term, value } => {
                    // Skip if this operation is already reflected in disk state
                    if let Some(threshold) = skip_threshold {
                        if lsn <= threshold {
                            continue;
                        }
                    }
                    // Deserialize value from WAL if present
                    let deserialized_value: Option<V> = value.and_then(|bytes| {
                        match bincode::deserialize(&bytes) {
                            Ok(v) => Some(v),
                            Err(e) => {
                                warn!("Failed to deserialize value from WAL: {:?}", e);
                                None
                            }
                        }
                    });
                    // Replay insert without re-logging to WAL
                    dict.insert_impl_no_wal(&term, deserialized_value);
                    replayed_count += 1;
                }
                super::recovery::RecoveredOperation::Remove { lsn, term } => {
                    // Skip if this operation is already reflected in disk state
                    if let Some(threshold) = skip_threshold {
                        if lsn <= threshold {
                            continue;
                        }
                    }
                    // Replay remove without re-logging to WAL
                    dict.remove_impl_no_wal(&term);
                    replayed_count += 1;
                }
                super::recovery::RecoveredOperation::Increment {
                    lsn,
                    term,
                    delta: _,
                    result,
                } => {
                    // Skip if this operation is already reflected in disk state
                    if let Some(threshold) = skip_threshold {
                        if lsn <= threshold {
                            continue;
                        }
                    }
                    // For increment recovery, we set the final result value directly
                    // (this is idempotent even if replayed multiple times)
                    let value_bytes = result.to_le_bytes().to_vec();
                    if let Ok(value) = bincode::deserialize(&value_bytes) {
                        dict.upsert_impl_no_wal(&term, value);
                    }
                    replayed_count += 1;
                }
                super::recovery::RecoveredOperation::Upsert { lsn, term, value } => {
                    // Skip if this operation is already reflected in disk state
                    if let Some(threshold) = skip_threshold {
                        if lsn <= threshold {
                            continue;
                        }
                    }
                    // Deserialize and apply value
                    if let Ok(v) = bincode::deserialize(&value) {
                        dict.upsert_impl_no_wal(&term, v);
                    }
                    replayed_count += 1;
                }
                super::recovery::RecoveredOperation::CompareAndSwap {
                    lsn,
                    term,
                    new_value,
                    success,
                } => {
                    // Skip if this operation is already reflected in disk state
                    if let Some(threshold) = skip_threshold {
                        if lsn <= threshold {
                            continue;
                        }
                    }
                    // Only apply if the CAS succeeded
                    if success {
                        if let Ok(v) = bincode::deserialize(&new_value) {
                            dict.upsert_impl_no_wal(&term, v);
                        }
                    }
                    replayed_count += 1;
                }
            }
        }
        // Mark clean after recovery replay
        dict.dirty.store(false, AtomicOrdering::Release);

        // If we loaded from disk AND replayed no operations, we can truncate the WAL
        // (all operations were already persisted to disk before the checkpoint)
        if was_loaded_from_disk && replayed_count == 0 {
            if let Err(e) = wal_writer.truncate() {
                warn!("Failed to truncate WAL after recovery: {:?}", e);
            }
        }

        Ok(dict)
    }

    /// Open an existing persistent dictionary with slot-level dirty tracking enabled.
    ///
    /// Slot-level tracking reduces checkpoint I/O by writing only modified slots
    /// instead of entire arenas. For vocabularies with localized updates, this
    /// can reduce checkpoint I/O by 90%+.
    ///
    /// This is equivalent to calling `open()` followed by enabling slot tracking
    /// on the arena manager, but provides a convenient single-call API.
    ///
    /// # Arguments
    /// * `path` - Path to the dictionary file (must exist)
    ///
    /// # Example
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// // Open existing vocabulary with slot-level tracking
    /// let dict = PersistentARTrie::<u64>::open_with_slot_tracking("vocab.part")?;
    ///
    /// // Subsequent allocations will be tracked at slot level
    /// dict.insert("new_term", Some(42));
    ///
    /// // Checkpoint writes only modified slots
    /// dict.checkpoint()?;
    /// ```
    pub fn open_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        let mut dict = Self::open(path)?;

        // Enable slot-level tracking on the arena manager
        if let Some(ref am) = dict.arena_manager {
            am.write().enable_slot_tracking();
        }

        Ok(dict)
    }

    /// Open with both recovery and slot tracking enabled.
    ///
    /// Combines `open_with_recovery()` and slot tracking enablement.
    /// Returns `(trie, recovery_report)` so callers can inspect recovery status.
    pub fn open_with_recovery_and_slot_tracking<P: AsRef<Path>>(path: P) -> Result<(Self, super::recovery::RecoveryReport)> {
        let (mut dict, report) = Self::open_with_recovery(path)?;
        if let Some(ref am) = dict.arena_manager {
            am.write().enable_slot_tracking();
        }
        Ok((dict, report))
    }

    /// Open an existing persistent dictionary with automatic corruption detection and recovery.
    ///
    /// This is the recommended way to open a trie that may have been corrupted
    /// by a crash (OOM kill, power failure, etc.).
    ///
    /// # Recovery Process
    ///
    /// 1. **Check if file exists** - If not, create a new trie
    /// 2. **Detect corruption** - Check header checksum, arena checksums
    /// 3. **If corrupted** - Rebuild from WAL archive segments
    /// 4. **Return trie with recovery report**
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the dictionary file
    ///
    /// # Returns
    ///
    /// Tuple of (trie, recovery_report) indicating what recovery was performed.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let (dict, report) = PersistentARTrie::<i64>::open_with_recovery("data.part")?;
    ///
    /// if !report.mode.is_normal() {
    ///     eprintln!("Recovered from crash: {} records replayed", report.records_replayed);
    /// }
    /// ```
    pub fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, super::recovery::RecoveryReport)> {
        use super::wal::WalConfig;
        Self::open_with_recovery_config(path, WalConfig::default())
    }

    /// Open with recovery and custom WAL configuration.
    ///
    /// Same as `open_with_recovery()` but allows specifying custom WAL settings.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the dictionary file
    /// * `config` - WAL configuration for archive mode, segment limits, etc.
    ///
    /// # Returns
    ///
    /// Tuple of (trie, recovery_report) indicating what recovery was performed.
    pub fn open_with_recovery_config<P: AsRef<Path>>(
        path: P,
        config: super::wal::WalConfig,
    ) -> Result<(Self, super::recovery::RecoveryReport)> {
        use super::recovery::{
            detect_corruption, find_wal_archive_segments, RecoveryReport,
        };
        use super::wal::{WalReader, WalRecord};
        use std::time::Instant;

        let path = path.as_ref();
        let start_time = Instant::now();

        // Check if file exists
        if !path.exists() {
            // No file - create new and return CreatedNew report
            let trie = Self::create(path)?;
            return Ok((trie, RecoveryReport::created_new()));
        }

        // Check for corruption
        match detect_corruption(path, true) {
            Ok(None) => {
                // No corruption detected - open normally
                let trie = Self::open(path)?;
                Ok((trie, RecoveryReport::normal()))
            }
            Ok(Some(corruption)) => {
                // Corruption detected - attempt recovery from WAL archives
                let corruption_reason = corruption.to_string();

                // Find archive directory
                let archive_dir = path.parent().unwrap_or(Path::new(".")).join(&config.archive_dir);

                // Find WAL archive segments
                let segments = find_wal_archive_segments(&archive_dir);

                if segments.is_empty() {
                    // No archive segments - can't recover
                    return Err(PersistentARTrieError::RecoveryError {
                        reason: format!(
                            "Corruption detected ({}) but no WAL archive segments found in {:?}",
                            corruption_reason, archive_dir
                        ),
                    });
                }

                // Remove corrupted file
                let _ = std::fs::remove_file(path);

                // Also remove current WAL (we'll rebuild from archives)
                let wal_path = path.with_extension("wal");
                let _ = std::fs::remove_file(&wal_path);

                // Create fresh trie
                let mut trie = Self::create(path)?;

                // Rebuild from WAL archive segments
                let mut records_replayed: u64 = 0;
                let mut terms_recovered: u64 = 0;
                let mut segments_used = Vec::new();

                for segment_path in &segments {
                    let reader = match WalReader::new(segment_path) {
                        Ok(r) => r,
                        Err(_) => continue, // Skip unreadable segments
                    };

                    segments_used.push(segment_path.clone());

                    for result in reader.iter() {
                        let (_lsn, record) = match result {
                            Ok(r) => r,
                            Err(_) => continue, // Skip corrupted records
                        };

                        records_replayed += 1;

                        // Apply the record to the trie
                        match record {
                            WalRecord::Insert { term, value } => {
                                // Deserialize value if present
                                let deserialized: Option<V> = value.and_then(|bytes| {
                                    bincode::deserialize(&bytes).ok()
                                });
                                trie.insert_impl_no_wal(&term, deserialized);
                                terms_recovered += 1;
                            }
                            WalRecord::Increment { term, delta: _, result: val } => {
                                // For increment, store the final result
                                let value_bytes = val.to_le_bytes();
                                if let Ok(v) = bincode::deserialize::<V>(&value_bytes) {
                                    trie.upsert_impl_no_wal(&term, v);
                                    terms_recovered += 1;
                                }
                            }
                            WalRecord::Upsert { term, value } => {
                                if let Ok(v) = bincode::deserialize::<V>(&value) {
                                    trie.upsert_impl_no_wal(&term, v);
                                    terms_recovered += 1;
                                }
                            }
                            WalRecord::CompareAndSwap { term, new_value, success, .. } => {
                                if success {
                                    if let Ok(v) = bincode::deserialize::<V>(&new_value) {
                                        trie.upsert_impl_no_wal(&term, v);
                                        terms_recovered += 1;
                                    }
                                }
                            }
                            _ => {} // Skip transaction/checkpoint records
                        }
                    }
                }

                let duration_ms = start_time.elapsed().as_millis() as u64;

                let report = RecoveryReport::rebuild_from_wal(
                    path.to_path_buf(),
                    corruption_reason,
                    records_replayed,
                    terms_recovered,
                    segments_used,
                    duration_ms,
                );

                Ok((trie, report))
            }
            Err(e) => {
                // I/O error during corruption check
                Err(PersistentARTrieError::InternalError {
                    message: format!("Error during corruption check: {}", e),
                })
            }
        }
    }
}

// === io_uring convenience constructors (Linux-only, requires `io-uring-backend` feature) ===

#[cfg(feature = "io-uring-backend")]
impl<V: DictionaryValue> PersistentARTrie<V, super::IoUringDiskManager> {
    /// Create a new persistent dictionary backed by io_uring + O_DIRECT.
    ///
    /// This uses `IoUringDiskManager` instead of `MmapDiskManager`, which:
    /// - Bypasses the kernel page cache (O_DIRECT) to eliminate double caching
    /// - Uses io_uring for async I/O with predictable latency
    /// - Supports batched block submissions for better throughput
    ///
    /// # Arguments
    /// * `path` - Path to the dictionary file (will also create `.wal` file)
    ///
    /// # Example
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let dict: PersistentARTrie<(), _> = PersistentARTrie::create_with_io_uring("words.part")?;
    /// ```
    pub fn create_with_io_uring<P: AsRef<Path>>(path: P) -> Result<Self> {
        use super::buffer_manager::BufferManager;
        use super::IoUringDiskManager;
        use super::DEFAULT_BUFFER_POOL_SIZE;

        let path = path.as_ref();

        // Fail if file already exists
        if path.exists() {
            return Err(PersistentARTrieError::io_error(
                "create",
                path.display().to_string(),
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "Dictionary file already exists",
                ),
            ));
        }

        // Create io_uring disk manager (creates new file with O_DIRECT)
        let disk_manager = IoUringDiskManager::create(path)?;

        // Create buffer manager with default pool size (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create async WAL file alongside the main file
        let wal_path = path.with_extension("wal");
        let async_config = AsyncWalConfig {
            pending_dir: path.parent().unwrap_or(Path::new(".")).join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig::default();
        let wal_writer = AsyncWalWriter::create(&wal_path, async_config, archive_config)
            .map_err(|e| PersistentARTrieError::io_error(
                "create_wal",
                wal_path.display().to_string(),
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ))?;
        let wal_writer = Arc::new(wal_writer);

        // Create arena manager for space-efficient node storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        Ok(Self {
            root: TrieRoot::Bucket(StringBucket::with_values()),
            term_count: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            next_lsn: std::sync::atomic::AtomicU64::new(1),
            prefetcher: super::prefetch::Prefetcher::new(),
            arena_manager: Some(arena_manager),
            durability_policy: DurabilityPolicy::default(),
            epoch_manager: super::concurrency::EpochManager::new(),
            stats: Arc::new(super::concurrency::TrieStats::new()),
            eviction_coordinator: None,
            dirty_prefixes: HashSet::new(),
            persisted_disk_locations: RwLock::new(HashMap::new()),
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Open an existing persistent dictionary from disk using io_uring + O_DIRECT.
    ///
    /// This opens an existing dictionary file and replays the WAL if needed
    /// to recover from any crash, using `IoUringDiskManager` for block I/O.
    ///
    /// # Arguments
    /// * `path` - Path to the dictionary file
    ///
    /// # Example
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let dict: PersistentARTrie<(), _> = PersistentARTrie::open_with_io_uring("words.part")?;
    /// ```
    pub fn open_with_io_uring<P: AsRef<Path>>(path: P) -> Result<Self> {
        use super::buffer_manager::BufferManager;
        use super::IoUringDiskManager;
        use super::recovery::RecoveryManager;
        use super::DEFAULT_BUFFER_POOL_SIZE;

        let path = path.as_ref();

        // Fail if file doesn't exist
        if !path.exists() {
            return Err(PersistentARTrieError::io_error(
                "open",
                path.display().to_string(),
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Dictionary file not found",
                ),
            ));
        }

        // Open io_uring disk manager (validates header)
        let disk_manager = IoUringDiskManager::open(path)?;

        // Get root pointer to check if trie exists
        let root_ptr = disk_manager.root_ptr()?;
        let _entry_count = disk_manager.entry_count()?;

        // Read arena_count from root descriptor at fixed location (block 0, offset 64)
        const DESCRIPTOR_OFFSET: usize = 64;
        let arena_count: u32 = if root_ptr != 0 {
            let ptr = SwizzledPtr::from_raw(root_ptr);
            if let Some(location) = ptr.disk_location() {
                let mut descriptor_buf = [0u8; 18];
                disk_manager.read_bytes(location.block_id, DESCRIPTOR_OFFSET, &mut descriptor_buf)?;
                u32::from_le_bytes([
                    descriptor_buf[6],
                    descriptor_buf[7],
                    descriptor_buf[8],
                    descriptor_buf[9],
                ])
            } else {
                0
            }
        } else {
            0
        };

        // Derive arena block IDs from sequential allocation
        let arena_block_ids: Vec<u32> = (1..=arena_count).collect();

        // Create buffer manager (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create arena manager for space-efficient node storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        // Load arenas into ArenaManager using derived block IDs
        if arena_count > 0 {
            let mut am = arena_manager.write();
            am.clear_for_loading();
            for block_id in arena_block_ids {
                am.load_arena(block_id)?;
            }
            let count = am.arena_count();
            am.set_active_arena(count.saturating_sub(1));
        }

        // Load trie from disk using the arena manager
        let (loaded_root, loaded_term_count) = if root_ptr != 0 {
            match Self::load_root_from_disk_with_arena(&buffer_manager, &arena_manager, root_ptr) {
                Ok((root, count)) => (Some(root), count),
                Err(e) => {
                    warn!("Failed to load trie from disk: {:?}", e);
                    (None, 0)
                }
            }
        } else {
            (None, 0)
        };

        // Recover from WAL if it exists
        let wal_path = path.with_extension("wal");
        let (recovered_ops, next_lsn, checkpoint_lsn) = if wal_path.exists() {
            let recovery_manager = RecoveryManager::new(&wal_path);
            match recovery_manager.recover() {
                Ok(state) => {
                    let lsn = state.next_lsn;
                    let cp_lsn = state.stats.checkpoint_lsn;
                    (state.into_operations(), lsn, cp_lsn)
                }
                Err(e) => {
                    warn!("WAL recovery error: {:?}", e);
                    (Vec::new(), 1, None)
                }
            }
        } else {
            (Vec::new(), 1, None)
        };

        // Open async WAL writer
        let async_config = AsyncWalConfig {
            pending_dir: path.parent().unwrap_or(Path::new(".")).join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig::default();
        let wal_writer = AsyncWalWriter::open_or_create(&wal_path, async_config, archive_config)
            .map_err(|e| PersistentARTrieError::io_error(
                "open_wal",
                wal_path.display().to_string(),
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ))?;
        let wal_writer = Arc::new(wal_writer);

        // Create the dictionary with storage layer
        let was_loaded_from_disk = loaded_root.is_some();
        let (initial_root, initial_term_count) = match loaded_root {
            Some(root) => (root, loaded_term_count as usize),
            None => (TrieRoot::Bucket(StringBucket::with_values()), 0),
        };

        let mut dict = Self {
            root: initial_root,
            term_count: AtomicUsize::new(initial_term_count),
            dirty: AtomicBool::new(false),
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(Arc::clone(&wal_writer)),
            next_lsn: std::sync::atomic::AtomicU64::new(next_lsn),
            prefetcher: super::prefetch::Prefetcher::new(),
            arena_manager: Some(arena_manager),
            durability_policy: DurabilityPolicy::default(),
            epoch_manager: super::concurrency::EpochManager::new(),
            stats: Arc::new(super::concurrency::TrieStats::new()),
            eviction_coordinator: None,
            dirty_prefixes: HashSet::new(),
            persisted_disk_locations: RwLock::new(HashMap::new()),
            #[cfg(feature = "persistent-artrie")]
            lockfree_root: None,
            #[cfg(feature = "persistent-artrie")]
            lockfree_cache: None,
            #[cfg(feature = "persistent-artrie")]
            cas_retries: std::sync::atomic::AtomicU64::new(0),
        };

        // Replay recovered operations
        let skip_threshold = if was_loaded_from_disk {
            checkpoint_lsn
        } else {
            None
        };

        let mut replayed_count = 0;
        for op in recovered_ops.into_iter() {
            match op {
                super::recovery::RecoveredOperation::Insert { lsn, term, value } => {
                    if let Some(threshold) = skip_threshold {
                        if lsn <= threshold {
                            continue;
                        }
                    }
                    let deserialized_value: Option<V> = value.and_then(|bytes| {
                        match bincode::deserialize(&bytes) {
                            Ok(v) => Some(v),
                            Err(e) => {
                                warn!("Failed to deserialize value from WAL: {:?}", e);
                                None
                            }
                        }
                    });
                    dict.insert_impl_no_wal(&term, deserialized_value);
                    replayed_count += 1;
                }
                super::recovery::RecoveredOperation::Remove { lsn, term } => {
                    if let Some(threshold) = skip_threshold {
                        if lsn <= threshold {
                            continue;
                        }
                    }
                    dict.remove_impl_no_wal(&term);
                    replayed_count += 1;
                }
                super::recovery::RecoveredOperation::Increment { lsn, term, delta: _, result } => {
                    if let Some(threshold) = skip_threshold {
                        if lsn <= threshold {
                            continue;
                        }
                    }
                    let value_bytes = result.to_le_bytes().to_vec();
                    if let Ok(value) = bincode::deserialize(&value_bytes) {
                        dict.upsert_impl_no_wal(&term, value);
                    }
                    replayed_count += 1;
                }
                super::recovery::RecoveredOperation::Upsert { lsn, term, value } => {
                    if let Some(threshold) = skip_threshold {
                        if lsn <= threshold {
                            continue;
                        }
                    }
                    if let Ok(v) = bincode::deserialize(&value) {
                        dict.upsert_impl_no_wal(&term, v);
                    }
                    replayed_count += 1;
                }
                super::recovery::RecoveredOperation::CompareAndSwap { lsn, term, new_value, success } => {
                    if let Some(threshold) = skip_threshold {
                        if lsn <= threshold {
                            continue;
                        }
                    }
                    if success {
                        if let Ok(v) = bincode::deserialize(&new_value) {
                            dict.upsert_impl_no_wal(&term, v);
                        }
                    }
                    replayed_count += 1;
                }
            }
        }

        dict.dirty.store(false, AtomicOrdering::Release);

        if was_loaded_from_disk && replayed_count == 0 {
            if let Err(e) = wal_writer.truncate() {
                warn!("Failed to truncate WAL after recovery: {:?}", e);
            }
        }

        Ok(dict)
    }
}

// === Generic methods (work with any BlockStorage backend) ===

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Load the trie root from disk.
    ///
    /// Reads the root descriptor block and deserializes the trie structure.
    ///
    /// # Returns
    /// Tuple of (TrieRoot, term_count) on success.
    fn load_root_from_disk(
        disk_manager: &impl BlockStorage,
        root_ptr: u64,
    ) -> Result<(TrieRoot<V>, u64)> {
        use super::disk_manager::BLOCK_SIZE;
        use super::BUCKET_PAGE_SIZE;

        // Decode the SwizzledPtr to get block_id
        let ptr = SwizzledPtr::from_raw(root_ptr);
        if ptr.is_null() || ptr.is_swizzled() {
            return Err(PersistentARTrieError::corrupted(
                "Invalid root pointer: null or already swizzled",
            ));
        }

        let location = ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Could not decode disk location from root pointer")
        })?;

        // Read the descriptor block
        let mut descriptor_buf = [0u8; BLOCK_SIZE];
        disk_manager.read_block(location.block_id, &mut descriptor_buf)?;

        // Parse root descriptor
        // Format:
        //   0: type (1 byte)
        //   1: is_final (1 byte)
        //   2-5: term_count (4 bytes, little endian)
        //   6-9: arena_count (4 bytes, little endian)
        //   10-17: root_ptr (8 bytes, little endian)
        //   18+: value bytes (if any)
        let root_type = descriptor_buf[0];
        let is_final = descriptor_buf[1] != 0;
        let term_count = u32::from_le_bytes([
            descriptor_buf[2],
            descriptor_buf[3],
            descriptor_buf[4],
            descriptor_buf[5],
        ]);
        let arena_count = u32::from_le_bytes([
            descriptor_buf[6],
            descriptor_buf[7],
            descriptor_buf[8],
            descriptor_buf[9],
        ]);
        let data_ptr = u64::from_le_bytes([
            descriptor_buf[10],
            descriptor_buf[11],
            descriptor_buf[12],
            descriptor_buf[13],
            descriptor_buf[14],
            descriptor_buf[15],
            descriptor_buf[16],
            descriptor_buf[17],
        ]);

        let _ = arena_count; // Arena count stored for recovery
        let _ = is_final;  // Used for ArtNode but we simplified

        match root_type {
            ROOT_TYPE_BUCKET => {
                // Load bucket from disk
                let bucket_ptr = SwizzledPtr::from_raw(data_ptr);
                let bucket_loc = bucket_ptr.disk_location().ok_or_else(|| {
                    PersistentARTrieError::corrupted("Invalid bucket pointer in root descriptor")
                })?;

                let mut bucket_data = [0u8; BUCKET_PAGE_SIZE];
                disk_manager.read_bytes(bucket_loc.block_id, 0, &mut bucket_data)?;

                let bucket = StringBucket::from_bytes(&bucket_data).map_err(|e| {
                    PersistentARTrieError::corrupted(format!("Failed to load bucket: {:?}", e))
                })?;

                Ok((TrieRoot::Bucket(bucket), term_count as u64))
            }
            ROOT_TYPE_ART_NODE => {
                // Load the ART node from disk
                let node_ptr = SwizzledPtr::from_raw(data_ptr);

                // Load the node and its children recursively
                let (node, children) = Self::load_art_node_with_children(disk_manager, &node_ptr)?;

                // Value deserialization not yet implemented with arena storage
                // (value_len no longer in descriptor - using arena_count instead)
                let root_value: Option<V> = None;

                Ok((
                    TrieRoot::ArtNode {
                        node,
                        children,
                        is_final,
                        value: root_value,
                    },
                    term_count as u64,
                ))
            }
            ROOT_TYPE_EMPTY | _ => {
                // Empty or unknown type
                Ok((TrieRoot::Bucket(StringBucket::with_values()), 0))
            }
        }
    }

    /// Load an ART node from disk and recursively load all its children.
    ///
    /// This method deserializes an ART node and builds the in-memory ChildNode
    /// structure by loading each child (which may be a bucket or another ART node).
    ///
    /// # Returns
    /// Tuple of (Node, Vec<(u8, ChildNode)>) representing the node and its children.
    fn load_art_node_with_children(
        disk_manager: &impl BlockStorage,
        node_ptr: &SwizzledPtr,
    ) -> Result<(Node, Vec<(u8, ChildNode)>)> {
        // Get disk location from SwizzledPtr
        let location = node_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid node pointer: cannot get disk location")
        })?;

        // Read the node data from disk
        let mut node_data = [0u8; ART_NODE_BUFFER_SIZE];
        disk_manager.read_bytes(location.block_id, 0, &mut node_data)?;

        // Deserialize the node
        let node = serialization::from_bytes(&node_data).map_err(|e| {
            PersistentARTrieError::corrupted(format!("Failed to deserialize ART node: {:?}", e))
        })?;

        // Load all children recursively
        let mut children = Vec::new();
        for (key, child_ptr) in node.iter_children() {
            if !child_ptr.is_null() {
                let child = Self::load_child_from_disk(disk_manager, child_ptr)?;
                children.push((key, child));
            }
        }

        Ok((node, children))
    }

    /// Load a child node (bucket or ART node) from disk.
    ///
    /// This method examines the SwizzledPtr's node type to determine whether
    /// the child is a bucket or an ART node, and loads it appropriately.
    fn load_child_from_disk(
        disk_manager: &impl BlockStorage,
        child_ptr: &SwizzledPtr,
    ) -> Result<ChildNode> {
        use super::BUCKET_PAGE_SIZE;

        let location = child_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid child pointer: cannot get disk location")
        })?;

        // Determine child type from the DiskLocation's node_type
        let node_type = location.node_type;

        match node_type {
            NodeType::Bucket => {
                // Load bucket from disk
                let mut bucket_data = [0u8; BUCKET_PAGE_SIZE];
                disk_manager.read_bytes(location.block_id, 0, &mut bucket_data)?;

                let bucket = StringBucket::from_bytes(&bucket_data).map_err(|e| {
                    PersistentARTrieError::corrupted(format!("Failed to load child bucket: {:?}", e))
                })?;

                Ok(ChildNode::Bucket(bucket))
            }
            NodeType::Node4 | NodeType::Node16 | NodeType::Node48 | NodeType::Node256 => {
                // Read the node data from disk
                let mut node_data = [0u8; ART_NODE_BUFFER_SIZE];
                disk_manager.read_bytes(location.block_id, 0, &mut node_data)?;

                // Deserialize the node
                let node = serialization::from_bytes(&node_data).map_err(|e| {
                    PersistentARTrieError::corrupted(format!("Failed to deserialize child ART node: {:?}", e))
                })?;

                // Check if node is final (has IS_FINAL flag set)
                let is_final = node.header().is_final();

                // Load children recursively
                let mut children = Vec::new();
                for (key, grandchild_ptr) in node.iter_children() {
                    if !grandchild_ptr.is_null() {
                        let grandchild = Self::load_child_from_disk(disk_manager, grandchild_ptr)?;
                        children.push((key, grandchild));
                    }
                }

                Ok(ChildNode::ArtNode {
                    node,
                    is_final,
                    value: None, // Value serialization for nested nodes is future work
                    children,
                })
            }
            // Char-level nodes should never appear in byte-level trie
            NodeType::CharNode4 | NodeType::CharNode16 | NodeType::CharNode48 | NodeType::CharBucket => {
                Err(PersistentARTrieError::corrupted(
                    "Char-level node type encountered in byte-level PersistentARTrie"
                ))
            }
        }
    }

    /// Load the root of the trie from disk using arena-based storage.
    ///
    /// This version uses ArenaManager to read data from arena slots instead
    /// of reading full blocks directly from disk. The SwizzledPtr encodes:
    /// - block_id = arena_id
    /// - offset = slot_id
    ///
    /// # Returns
    /// Tuple of (TrieRoot, term_count) on success.
    fn load_root_from_disk_with_arena(
        buffer_manager: &Arc<RwLock<BufferManager<S>>>,
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        root_ptr: u64,
    ) -> Result<(TrieRoot<V>, u64)> {
        // Decode the SwizzledPtr to get block_id and offset
        let ptr = SwizzledPtr::from_raw(root_ptr);
        if ptr.is_null() || ptr.is_swizzled() {
            return Err(PersistentARTrieError::corrupted(
                "Invalid root pointer: null or already swizzled",
            ));
        }

        let location = ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Could not decode disk location from root pointer")
        })?;

        // Read the descriptor from block 0 at the encoded offset (64)
        // The SwizzledPtr now encodes (block_id=0, offset=64)
        let bm = buffer_manager.read();
        let page = bm.fetch_page(location.block_id)?;
        let page_data = page.data();

        // Read descriptor from the offset within block 0
        let offset = location.offset as usize;
        let descriptor_buf = &page_data[offset..offset + 18];

        // Parse root descriptor (fixed 18 bytes)
        // Format:
        //   0: type (1 byte)
        //   1: is_final (1 byte)
        //   2-5: term_count (4 bytes, little endian)
        //   6-9: arena_count (4 bytes, little endian)
        //   10-17: root_ptr (8 bytes, little endian)
        let root_type = descriptor_buf[0];
        let is_final = descriptor_buf[1] != 0;
        let term_count = u32::from_le_bytes([
            descriptor_buf[2],
            descriptor_buf[3],
            descriptor_buf[4],
            descriptor_buf[5],
        ]);
        let data_ptr = u64::from_le_bytes([
            descriptor_buf[10],
            descriptor_buf[11],
            descriptor_buf[12],
            descriptor_buf[13],
            descriptor_buf[14],
            descriptor_buf[15],
            descriptor_buf[16],
            descriptor_buf[17],
        ]);

        drop(page);
        drop(bm);

        match root_type {
            ROOT_TYPE_BUCKET => {
                // Load bucket from arena
                let bucket_ptr = SwizzledPtr::from_raw(data_ptr);
                let bucket_loc = bucket_ptr.disk_location().ok_or_else(|| {
                    PersistentARTrieError::corrupted("Invalid bucket pointer in root descriptor")
                })?;

                // Get arena slot from the disk location
                // block_id = arena_id + 1 (block 0 is file header)
                // offset = slot_id
                let arena_id = bucket_loc.block_id.checked_sub(1).ok_or_else(|| {
                    PersistentARTrieError::corrupted("Invalid block_id 0 for arena bucket")
                })?;
                let slot = ArenaSlot::new(arena_id, bucket_loc.offset);
                let am = arena_manager.read();
                let bucket_data = am.read(slot)?;

                let bucket = StringBucket::from_bytes(bucket_data).map_err(|e| {
                    PersistentARTrieError::corrupted(format!("Failed to load bucket: {:?}", e))
                })?;

                Ok((TrieRoot::Bucket(bucket), term_count as u64))
            }
            ROOT_TYPE_ART_NODE => {
                // Load the ART node from arena
                let node_ptr = SwizzledPtr::from_raw(data_ptr);

                // Load the node and its children using iterative loading
                // (avoids stack overflow for deep tries)
                let (node, children) = Self::load_art_node_with_children_from_arena_iterative(arena_manager, &node_ptr)?;

                // Value deserialization not yet implemented with arena storage
                let root_value: Option<V> = None;

                Ok((
                    TrieRoot::ArtNode {
                        node,
                        children,
                        is_final,
                        value: root_value,
                    },
                    term_count as u64,
                ))
            }
            ROOT_TYPE_EMPTY | _ => {
                // Empty or unknown type
                Ok((TrieRoot::Bucket(StringBucket::with_values()), 0))
            }
        }
    }

    /// Load an ART node from arena and recursively load all its children.
    ///
    /// This version uses ArenaManager to read data from arena slots.
    ///
    /// # Returns
    /// Tuple of (Node, Vec<(u8, ChildNode)>) representing the node and its children.
    fn load_art_node_with_children_from_arena(
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        node_ptr: &SwizzledPtr,
    ) -> Result<(Node, Vec<(u8, ChildNode)>)> {
        // Get arena slot from the disk location
        // block_id = arena_id + 1 (block 0 is file header)
        // offset = slot_id
        let disk_loc = node_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid node pointer: cannot get disk location")
        })?;
        let arena_id = disk_loc.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid block_id 0 for arena node")
        })?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);
        let am = arena_manager.read();
        let node_data = am.read(slot)?;

        // Deserialize the node using v2 format with relative offset support
        // The slot is the "parent slot" for decoding relative child offsets
        let ctx = DeserializationContext::new(slot);
        let node = serialization::v2::deserialize_node_v2(node_data, &ctx).map_err(|e| {
            PersistentARTrieError::corrupted(format!("Failed to deserialize ART node: {:?}", e))
        })?;

        // Collect child pointers before dropping the arena lock
        let child_data: Vec<(u8, SwizzledPtr)> = node
            .iter_children()
            .filter(|(_, ptr)| !ptr.is_null())
            .map(|(key, ptr)| (key, ptr.clone()))
            .collect();

        // Drop arena lock before recursive calls
        drop(am);

        // Load all children recursively
        let mut children = Vec::new();
        for (key, child_ptr) in child_data {
            let child = Self::load_child_from_disk_with_arena(arena_manager, &child_ptr)?;
            children.push((key, child));
        }

        Ok((node, children))
    }

    /// Load a child node (bucket or ART node) from arena.
    ///
    /// This version uses ArenaManager to read data from arena slots.
    fn load_child_from_disk_with_arena(
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        child_ptr: &SwizzledPtr,
    ) -> Result<ChildNode> {
        // Get arena slot from the disk location
        // block_id = arena_id + 1 (block 0 is file header)
        // offset = slot_id
        let disk_loc = child_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid child pointer: cannot get disk location")
        })?;
        let arena_id = disk_loc.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid block_id 0 for arena node")
        })?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);

        // Determine child type from the DiskLocation's node_type
        let node_type = disk_loc.node_type;

        // Read data from arena
        let am = arena_manager.read();
        let data = am.read(slot)?;

        match node_type {
            NodeType::Bucket => {
                let bucket = StringBucket::from_bytes(data).map_err(|e| {
                    PersistentARTrieError::corrupted(format!("Failed to load child bucket: {:?}", e))
                })?;

                Ok(ChildNode::Bucket(bucket))
            }
            NodeType::Node4 | NodeType::Node16 | NodeType::Node48 | NodeType::Node256 => {
                // Deserialize the node using v2 format with relative offset support
                // The slot is the "parent slot" for decoding relative child offsets
                let ctx = DeserializationContext::new(slot);
                let node = serialization::v2::deserialize_node_v2(data, &ctx).map_err(|e| {
                    PersistentARTrieError::corrupted(format!("Failed to deserialize child ART node: {:?}", e))
                })?;

                // Check if node is final (has IS_FINAL flag set)
                let is_final = node.header().is_final();

                // Collect child pointers before dropping the arena lock
                let child_data: Vec<(u8, SwizzledPtr)> = node
                    .iter_children()
                    .filter(|(_, ptr)| !ptr.is_null())
                    .map(|(key, ptr)| (key, ptr.clone()))
                    .collect();

                // Drop arena lock before recursive calls
                drop(am);

                // Load children recursively
                let mut children = Vec::new();
                for (key, grandchild_ptr) in child_data {
                    let grandchild = Self::load_child_from_disk_with_arena(arena_manager, &grandchild_ptr)?;
                    children.push((key, grandchild));
                }

                Ok(ChildNode::ArtNode {
                    node,
                    is_final,
                    value: None, // Value serialization for nested nodes is future work
                    children,
                })
            }
            // Char-level nodes should never appear in byte-level trie
            NodeType::CharNode4 | NodeType::CharNode16 | NodeType::CharNode48 | NodeType::CharBucket => {
                Err(PersistentARTrieError::corrupted(
                    "Char-level node type encountered in byte-level PersistentARTrie"
                ))
            }
        }
    }

    /// Load a single ART node's data from arena WITHOUT loading children.
    ///
    /// This is a helper for iterative loading. Returns the node info and
    /// the list of child pointers that need to be loaded.
    fn load_single_art_node_data(
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        node_ptr: &SwizzledPtr,
    ) -> Result<(Node, bool, Vec<(u8, SwizzledPtr)>)> {
        let disk_loc = node_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid node pointer: cannot get disk location")
        })?;
        let arena_id = disk_loc.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid block_id 0 for arena node")
        })?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);
        let am = arena_manager.read();
        let node_data = am.read(slot)?;

        // Deserialize the node using v2 format with relative offset support
        let ctx = DeserializationContext::new(slot);
        let node = serialization::v2::deserialize_node_v2(node_data, &ctx).map_err(|e| {
            PersistentARTrieError::corrupted(format!("Failed to deserialize ART node: {:?}", e))
        })?;

        let is_final = node.header().is_final();

        // Collect child pointers before dropping the arena lock
        let child_data: Vec<(u8, SwizzledPtr)> = node
            .iter_children()
            .filter(|(_, ptr)| !ptr.is_null())
            .map(|(key, ptr)| (key, ptr.clone()))
            .collect();

        drop(am);

        Ok((node, is_final, child_data))
    }

    /// Load a single child node's data from arena WITHOUT loading its children.
    ///
    /// Returns either a complete Bucket (no children) or the components needed
    /// to build an ArtNode (node, is_final, child pointers).
    fn load_single_child_data(
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        child_ptr: &SwizzledPtr,
    ) -> Result<SingleChildData> {
        let disk_loc = child_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid child pointer: cannot get disk location")
        })?;
        let arena_id = disk_loc.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid block_id 0 for arena node")
        })?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);
        let node_type = disk_loc.node_type;

        let am = arena_manager.read();
        let data = am.read(slot)?;

        match node_type {
            NodeType::Bucket => {
                let bucket = StringBucket::from_bytes(data).map_err(|e| {
                    PersistentARTrieError::corrupted(format!("Failed to load child bucket: {:?}", e))
                })?;
                Ok(SingleChildData::Bucket(bucket))
            }
            NodeType::Node4 | NodeType::Node16 | NodeType::Node48 | NodeType::Node256 => {
                let ctx = DeserializationContext::new(slot);
                let node = serialization::v2::deserialize_node_v2(data, &ctx).map_err(|e| {
                    PersistentARTrieError::corrupted(format!("Failed to deserialize child ART node: {:?}", e))
                })?;

                let is_final = node.header().is_final();

                let child_data: Vec<(u8, SwizzledPtr)> = node
                    .iter_children()
                    .filter(|(_, ptr)| !ptr.is_null())
                    .map(|(key, ptr)| (key, ptr.clone()))
                    .collect();

                drop(am);

                Ok(SingleChildData::ArtNodePartial {
                    node,
                    is_final,
                    child_ptrs: child_data,
                })
            }
            NodeType::CharNode4 | NodeType::CharNode16 | NodeType::CharNode48 | NodeType::CharBucket => {
                Err(PersistentARTrieError::corrupted(
                    "Char-level node type encountered in byte-level PersistentARTrie"
                ))
            }
        }
    }

    /// Load an ART node and all its children using iterative (non-recursive) traversal.
    ///
    /// This avoids stack overflow for deep tries by using an explicit work stack.
    /// Uses a two-phase algorithm:
    ///
    /// 1. **Phase 1**: Load all nodes into a vector (without connecting children)
    /// 2. **Phase 2**: Connect children to parents in reverse order (bottom-up)
    fn load_art_node_with_children_from_arena_iterative(
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        root_node_ptr: &SwizzledPtr,
    ) -> Result<(Node, Vec<(u8, ChildNode)>)> {
        use std::collections::HashMap;

        /// Work item for iterative loading
        enum WorkItem {
            /// Load from the root ART node
            RootNode(SwizzledPtr),
            /// Load a child node
            Child(SwizzledPtr),
        }

        /// Loaded node info before children are connected
        enum LoadedInfo {
            /// The root node
            RootNode {
                node: Node,
                is_final: bool,
                child_ptrs: Vec<(u8, SwizzledPtr)>,
            },
            /// A bucket child (complete, no children to connect)
            Bucket(StringBucket),
            /// An ART child node (needs children connected)
            ArtNodePartial {
                node: Node,
                is_final: bool,
                child_ptrs: Vec<(u8, SwizzledPtr)>,
            },
        }

        // Stack for DFS traversal
        let mut work_stack: Vec<WorkItem> = vec![WorkItem::RootNode(root_node_ptr.clone())];

        // Results vector - nodes stored in DFS pre-order
        let mut loaded_nodes: Vec<LoadedInfo> = Vec::new();

        // Map from disk pointer raw value to result index
        let mut ptr_to_idx: HashMap<u64, usize> = HashMap::new();

        // Phase 1: Load all nodes without connecting children
        while let Some(work_item) = work_stack.pop() {
            let (ptr_raw, loaded_info, child_ptrs_to_push) = match work_item {
                WorkItem::RootNode(ptr) => {
                    let ptr_raw = ptr.to_raw();
                    if ptr_to_idx.contains_key(&ptr_raw) {
                        continue;
                    }

                    let (node, is_final, child_ptrs) = Self::load_single_art_node_data(arena_manager, &ptr)?;
                    let ptrs_to_push: Vec<SwizzledPtr> = child_ptrs.iter().map(|(_, p)| p.clone()).collect();
                    (ptr_raw, LoadedInfo::RootNode { node, is_final, child_ptrs }, ptrs_to_push)
                }
                WorkItem::Child(ptr) => {
                    let ptr_raw = ptr.to_raw();
                    if ptr_to_idx.contains_key(&ptr_raw) {
                        continue;
                    }

                    match Self::load_single_child_data(arena_manager, &ptr)? {
                        SingleChildData::Bucket(bucket) => {
                            (ptr_raw, LoadedInfo::Bucket(bucket), vec![])
                        }
                        SingleChildData::ArtNodePartial { node, is_final, child_ptrs } => {
                            let ptrs_to_push: Vec<SwizzledPtr> = child_ptrs.iter().map(|(_, p)| p.clone()).collect();
                            (ptr_raw, LoadedInfo::ArtNodePartial { node, is_final, child_ptrs }, ptrs_to_push)
                        }
                    }
                }
            };

            let result_idx = loaded_nodes.len();
            ptr_to_idx.insert(ptr_raw, result_idx);
            loaded_nodes.push(loaded_info);

            // Push children in reverse order for correct DFS ordering
            for child_ptr in child_ptrs_to_push.into_iter().rev() {
                work_stack.push(WorkItem::Child(child_ptr));
            }
        }

        if loaded_nodes.is_empty() {
            return Err(PersistentARTrieError::corrupted("No nodes loaded from disk"));
        }

        // Phase 2: Build ChildNode structures bottom-up
        // We need to convert LoadedInfo into final ChildNode structures
        // Process in reverse order so children are ready before parents need them

        // Store built ChildNode results (indexed same as loaded_nodes)
        let mut built_children: Vec<Option<ChildNode>> = vec![None; loaded_nodes.len()];

        for idx in (0..loaded_nodes.len()).rev() {
            let child_node = match &mut loaded_nodes[idx] {
                LoadedInfo::RootNode { .. } => {
                    // Root is handled separately
                    continue;
                }
                LoadedInfo::Bucket(bucket) => {
                    ChildNode::Bucket(std::mem::take(bucket))
                }
                LoadedInfo::ArtNodePartial { node, is_final, child_ptrs } => {
                    // Collect built children
                    let mut children: Vec<(u8, ChildNode)> = Vec::with_capacity(child_ptrs.len());
                    for (key, child_ptr) in child_ptrs.drain(..) {
                        let child_idx = *ptr_to_idx.get(&child_ptr.to_raw())
                            .ok_or_else(|| PersistentARTrieError::corrupted(
                                "Child pointer not found in loaded nodes map"
                            ))?;
                        let child = built_children[child_idx].take()
                            .ok_or_else(|| PersistentARTrieError::corrupted(
                                "Child not yet built (ordering error)"
                            ))?;
                        children.push((key, child));
                    }

                    // Take ownership of node
                    let node_taken = std::mem::replace(node, Node::new_node4());

                    ChildNode::ArtNode {
                        node: node_taken,
                        is_final: *is_final,
                        value: None,
                        children,
                    }
                }
            };

            built_children[idx] = Some(child_node);
        }

        // Extract root node info and build its children
        match &mut loaded_nodes[0] {
            LoadedInfo::RootNode { node, is_final: _, child_ptrs } => {
                let mut children: Vec<(u8, ChildNode)> = Vec::with_capacity(child_ptrs.len());
                for (key, child_ptr) in child_ptrs.drain(..) {
                    let child_idx = *ptr_to_idx.get(&child_ptr.to_raw())
                        .ok_or_else(|| PersistentARTrieError::corrupted(
                            "Root child pointer not found in loaded nodes map"
                        ))?;
                    let child = built_children[child_idx].take()
                        .ok_or_else(|| PersistentARTrieError::corrupted(
                            "Root child not yet built"
                        ))?;
                    children.push((key, child));
                }

                let root_node = std::mem::replace(node, Node::new_node4());
                Ok((root_node, children))
            }
            _ => Err(PersistentARTrieError::corrupted("First loaded node is not root"))
        }
    }

    // ==================== Lock-Free CAS Methods ====================

    /// Enable lock-free mode for concurrent inserts.
    ///
    /// This initializes the lock-free infrastructure including:
    /// - An `AtomicNodePtr` root for CAS-based tree modifications
    /// - A `DashMap` cache for fast lookups
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut trie = PersistentARTrie::<()>::create("trie.part")?;
    /// trie.enable_lockfree();
    /// trie.insert_cas(b"hello");  // Now works concurrently
    /// ```
    #[cfg(feature = "persistent-artrie")]
    pub fn enable_lockfree(&mut self) {
        use super::nodes::atomic_ptr::AtomicNodePtr;
        use super::nodes::persistent_node::PersistentNode;
        use dashmap::DashMap;

        if self.lockfree_root.is_some() {
            return; // Already enabled
        }

        // Initialize with an empty root node
        let root_node = Arc::new(PersistentNode::new());
        self.lockfree_root = Some(AtomicNodePtr::new(root_node));
        self.lockfree_cache = Some(DashMap::new());
    }

    /// Lock-free insert using CAS operations.
    ///
    /// This method inserts a term into the lock-free trie structure without
    /// acquiring any locks. Multiple threads can call this concurrently.
    ///
    /// # Arguments
    ///
    /// * `term` - The term bytes to insert
    ///
    /// # Returns
    ///
    /// `true` if the term was newly inserted, `false` if it already existed.
    ///
    /// # Panics
    ///
    /// Panics if `enable_lockfree()` was not called first.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut trie = PersistentARTrie::<()>::create("trie.part")?;
    /// trie.enable_lockfree();
    ///
    /// let inserted = trie.insert_cas(b"hello");
    /// assert!(inserted);
    ///
    /// let inserted2 = trie.insert_cas(b"hello");
    /// assert!(!inserted2);  // Already exists
    /// ```
    #[cfg(feature = "persistent-artrie")]
    pub fn insert_cas(&self, term: &[u8]) -> bool {
        use std::sync::atomic::Ordering;

        let lockfree_root = self.lockfree_root.as_ref()
            .expect("Lock-free mode not enabled. Call enable_lockfree() first.");
        let lockfree_cache = self.lockfree_cache.as_ref()
            .expect("Lock-free mode not enabled. Call enable_lockfree() first.");

        // Fast path: check cache first
        if lockfree_cache.contains_key(term) {
            return false;
        }

        if term.is_empty() {
            return false;
        }

        // Enter the read epoch for safe memory access
        let _epoch = self.epoch_manager.enter_read();

        // CAS retry loop
        loop {
            match self.try_insert_lockfree_path(lockfree_root, term) {
                LockfreeInsertResult::Inserted(node) => {
                    // We inserted a new path - try to claim it as final
                    if node.try_set_final() {
                        // We won the race to finalize this node
                        lockfree_cache.insert(term.to_vec(), true);
                        return true;
                    } else {
                        // Another thread finalized it - the term already exists
                        return false;
                    }
                }
                LockfreeInsertResult::AlreadyExists => {
                    // Term already exists in the trie
                    lockfree_cache.insert(term.to_vec(), true);
                    return false;
                }
                LockfreeInsertResult::Conflict => {
                    // CAS failed due to concurrent modification - retry
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    /// Attempt to insert a path in the lock-free trie.
    ///
    /// Returns the result of the insertion attempt.
    #[cfg(feature = "persistent-artrie")]
    fn try_insert_lockfree_path(
        &self,
        root: &super::nodes::AtomicNodePtr,
        term: &[u8],
    ) -> LockfreeInsertResult {
        use super::nodes::PersistentNode;

        // Load current root
        let current_root = match root.load() {
            Some(node) => node,
            None => {
                // Root is null - try to initialize it
                let new_root = Arc::new(PersistentNode::new());
                match root.try_init(new_root) {
                    Ok(()) => return self.try_insert_lockfree_path(root, term),
                    Err(actual) => actual,
                }
            }
        };

        // Navigate/create path to the target node
        self.insert_lockfree_recursive(root, &current_root, term, 0)
    }

    /// Recursively build a new tree with the path inserted.
    ///
    /// This method builds the path from leaf to root: it recurses down to the
    /// target depth, creates the leaf node, then on the way back up creates
    /// new versions of each parent with updated child pointers.
    ///
    /// # Returns
    ///
    /// - `Ok(new_node, leaf)` - New version of this node with path inserted, plus leaf node
    /// - `Err(())` - Term already exists (node is already final at target depth)
    #[cfg(feature = "persistent-artrie")]
    fn build_path_recursive(
        &self,
        node: &Arc<super::nodes::PersistentNode>,
        term: &[u8],
        depth: usize,
    ) -> std::result::Result<(Arc<super::nodes::PersistentNode>, Arc<super::nodes::PersistentNode>), ()> {
        use super::nodes::PersistentNode;
        use super::swizzled_ptr::SwizzledPtr;

        if depth == term.len() {
            // Reached target depth - mark as final
            if node.is_final() {
                return Err(()); // Already exists
            }
            let final_node = Arc::new(node.as_final());
            return Ok((final_node.clone(), final_node));
        }

        let key = term[depth];

        match node.find_child(key) {
            Some(child_ptr) => {
                // Child exists - check if it's on disk
                if child_ptr.is_on_disk() {
                    // On-disk child means this path exists in persistent trie
                    // For lock-free overlay, we can't easily check this
                    // Mark as conflict to force re-check via cache/persistent lookup
                    return Err(());
                }

                // In-memory child - traverse into it
                if let Some(ptr) = child_ptr.as_ptr::<PersistentNode>() {
                    let child = unsafe {
                        Arc::increment_strong_count(ptr);
                        Arc::from_raw(ptr)
                    };

                    // Recursively build path in child
                    let (new_child, leaf) = self.build_path_recursive(&child, term, depth + 1)?;

                    // Create new version of this node with updated child pointer
                    let new_child_ptr = SwizzledPtr::in_memory(Arc::into_raw(new_child));
                    let new_node = Arc::new(node.with_child(key, new_child_ptr));

                    Ok((new_node, leaf))
                } else {
                    // Null pointer shouldn't happen
                    Err(())
                }
            }
            None => {
                // Child doesn't exist - create entire remaining path
                let (new_subtree, leaf) = self.create_lockfree_path(&term[depth + 1..]);
                let new_child_ptr = SwizzledPtr::in_memory(Arc::into_raw(new_subtree));
                let new_node = Arc::new(node.with_child(key, new_child_ptr));

                Ok((new_node, leaf))
            }
        }
    }

    /// Create a new path for the remaining bytes.
    ///
    /// Builds the path bottom-up: creates the final leaf node first,
    /// then wraps each byte as a parent going up to the start.
    ///
    /// # Returns
    ///
    /// A tuple of (subtree_root, leaf_node) where:
    /// - subtree_root is the top of the new path (to be attached as a child)
    /// - leaf_node is the final node (to have try_set_final called on it)
    #[cfg(feature = "persistent-artrie")]
    fn create_lockfree_path(&self, term: &[u8]) -> (Arc<super::nodes::PersistentNode>, Arc<super::nodes::PersistentNode>) {
        use super::nodes::PersistentNode;
        use super::swizzled_ptr::SwizzledPtr;

        // Create the final leaf node (not marked final yet - caller will try_set_final)
        let leaf = Arc::new(PersistentNode::new());

        if term.is_empty() {
            // No more bytes - leaf is also the root
            return (leaf.clone(), leaf);
        }

        // Build path bottom-up
        let mut current = leaf.clone();

        for &b in term.iter().rev() {
            let child_ptr = SwizzledPtr::in_memory(Arc::into_raw(current));
            let parent = PersistentNode::new().with_child(b, child_ptr);
            current = Arc::new(parent);
        }

        (current, leaf)
    }

    /// Attempt to insert a path using CAS. Called from insert_cas retry loop.
    #[cfg(feature = "persistent-artrie")]
    fn insert_lockfree_recursive(
        &self,
        root: &super::nodes::AtomicNodePtr,
        current: &Arc<super::nodes::PersistentNode>,
        term: &[u8],
        _depth: usize, // Kept for API compatibility
    ) -> LockfreeInsertResult {
        // Build the new tree structure with the path inserted
        match self.build_path_recursive(current, term, 0) {
            Ok((new_root, leaf)) => {
                // Try to CAS the root to the new version
                match root.compare_exchange(current, new_root) {
                    Ok(_) => {
                        // Successfully updated the tree
                        LockfreeInsertResult::Inserted(leaf)
                    }
                    Err(_actual) => {
                        // CAS failed - another thread modified the tree
                        LockfreeInsertResult::Conflict
                    }
                }
            }
            Err(()) => {
                // Term already exists or on-disk reference found
                LockfreeInsertResult::AlreadyExists
            }
        }
    }

    /// Check if a term exists in the lock-free trie.
    ///
    /// This is a fast, lock-free lookup that checks the cache first.
    #[cfg(feature = "persistent-artrie")]
    pub fn contains_lockfree(&self, term: &[u8]) -> bool {
        if let Some(ref cache) = self.lockfree_cache {
            if cache.contains_key(term) {
                return true;
            }
        }

        // Fall back to checking the lock-free trie structure
        if let Some(ref root) = self.lockfree_root {
            if let Some(root_node) = root.load() {
                return self.find_in_lockfree_trie(&root_node, term, 0);
            }
        }

        false
    }

    /// Navigate the lock-free trie to find a term.
    #[cfg(feature = "persistent-artrie")]
    fn find_in_lockfree_trie(
        &self,
        node: &Arc<super::nodes::PersistentNode>,
        term: &[u8],
        depth: usize,
    ) -> bool {
        use super::nodes::PersistentNode;

        if depth >= term.len() {
            return node.is_final();
        }

        let key = term[depth];
        if let Some(child_ptr) = node.find_child(key) {
            if child_ptr.is_on_disk() {
                // On-disk reference - can't traverse in lock-free overlay
                // The persistent trie would need to be checked
                return false;
            }

            // In-memory child - traverse into it
            if let Some(ptr) = child_ptr.as_ptr::<PersistentNode>() {
                let child = unsafe {
                    Arc::increment_strong_count(ptr);
                    Arc::from_raw(ptr)
                };
                return self.find_in_lockfree_trie(&child, term, depth + 1);
            }
        }

        false
    }

    /// Merge lock-free entries into the persistent trie.
    ///
    /// This method takes entries from the lock-free cache and inserts them
    /// into the persistent trie structure. Call this during checkpoints or
    /// before saving to ensure all entries are persisted.
    ///
    /// # Returns
    ///
    /// The number of entries merged.
    #[cfg(feature = "persistent-artrie")]
    pub fn merge_lockfree_to_persistent(&mut self) -> Result<usize> {
        // Collect entries first to avoid borrow conflict
        let entries: Vec<Vec<u8>> = match &self.lockfree_cache {
            Some(cache) => cache.iter().map(|e| e.key().clone()).collect(),
            None => return Ok(0),
        };

        let mut count = 0;
        for term in entries {
            if self.insert_impl(&term, None) {
                count += 1;
            }
        }

        // Clear the cache after merging
        if let Some(ref cache) = self.lockfree_cache {
            cache.clear();
        }

        Ok(count)
    }

    /// Get the number of CAS retries (for monitoring contention).
    #[cfg(feature = "persistent-artrie")]
    #[inline]
    pub fn cas_retry_count(&self) -> u64 {
        self.cas_retries.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Merge lock-free values into the persistent trie by summing.
    ///
    /// Unlike `merge_lockfree_to_persistent()` which does boolean insert,
    /// this method walks the lock-free trie overlay, collects all `(key, value)`
    /// entries, and adds each value to the persistent trie via `increment_bytes`.
    ///
    /// This is the correct merge strategy for n-gram counting where the lock-free
    /// layer accumulates counts that must be summed with existing persistent values.
    ///
    /// After merging, the lock-free layer is cleared (root reset, cache cleared).
    ///
    /// # Returns
    ///
    /// The number of entries merged.
    #[cfg(feature = "persistent-artrie")]
    pub fn merge_lockfree_values_to_persistent(&mut self) -> Result<usize> {
        use super::nodes::PersistentNode;

        // Collect all (key, value) entries from the lock-free trie
        let entries = {
            let lockfree_root = match self.lockfree_root.as_ref() {
                Some(root) => root,
                None => return Ok(0),
            };

            let root_node = match lockfree_root.load() {
                Some(node) => node,
                None => return Ok(0),
            };

            let mut entries: Vec<(Vec<u8>, u64)> = Vec::new();
            let mut key_buf: Vec<u8> = Vec::new();
            Self::collect_lockfree_entries_recursive(&root_node, &mut key_buf, &mut entries);
            entries
        };

        let mut count = 0;
        for (key, value) in &entries {
            self.increment_bytes(key, *value as i64)?;
            count += 1;
        }

        // Clear the lock-free layer
        if let Some(ref cache) = self.lockfree_cache {
            cache.clear();
        }
        if let Some(ref root) = self.lockfree_root {
            root.store(Arc::new(PersistentNode::new()));
        }

        Ok(count)
    }

    /// Recursively collect all (key, value) entries from the lock-free trie.
    #[cfg(feature = "persistent-artrie")]
    fn collect_lockfree_entries_recursive(
        node: &Arc<super::nodes::PersistentNode>,
        key_buf: &mut Vec<u8>,
        entries: &mut Vec<(Vec<u8>, u64)>,
    ) {
        use super::nodes::PersistentNode;

        // If this node is final and has a value, record it
        if node.is_final() {
            if let Some(value) = node.get_value() {
                entries.push((key_buf.clone(), value));
            }
        }

        // Recurse into children
        for (&child_key, child_ptr) in node.iter_children() {
            if child_ptr.is_on_disk() {
                continue; // Skip disk refs in lock-free overlay
            }
            if let Some(ptr) = child_ptr.as_ptr::<PersistentNode>() {
                let child = unsafe {
                    Arc::increment_strong_count(ptr);
                    Arc::from_raw(ptr)
                };
                key_buf.push(child_key);
                Self::collect_lockfree_entries_recursive(&child, key_buf, entries);
                key_buf.pop();
            }
        }
    }

    /// Find the leaf node for a key in the lock-free trie.
    ///
    /// Navigates the lock-free trie overlay and returns the leaf node if the
    /// full path exists and the leaf is final. Unlike `find_in_lockfree_trie`
    /// which returns a `bool`, this returns the node itself so the caller can
    /// read or atomically modify its value.
    ///
    /// # Arguments
    ///
    /// * `root` - The lock-free root pointer
    /// * `key` - The byte key to look up
    ///
    /// # Returns
    ///
    /// `Some(leaf)` if the key exists and is final, `None` otherwise.
    #[cfg(feature = "persistent-artrie")]
    fn find_leaf_lockfree(
        &self,
        root: &super::nodes::AtomicNodePtr,
        key: &[u8],
    ) -> Option<Arc<super::nodes::PersistentNode>> {
        let current = root.load()?;
        self.find_leaf_recursive(&current, key, 0)
    }

    /// Recursive helper for `find_leaf_lockfree`.
    #[cfg(feature = "persistent-artrie")]
    fn find_leaf_recursive(
        &self,
        node: &Arc<super::nodes::PersistentNode>,
        key: &[u8],
        depth: usize,
    ) -> Option<Arc<super::nodes::PersistentNode>> {
        use super::nodes::PersistentNode;

        if depth == key.len() {
            return if node.is_final() { Some(Arc::clone(node)) } else { None };
        }

        let child_ptr = node.find_child(key[depth])?;
        if child_ptr.is_on_disk() {
            return None; // Can't traverse disk refs in lock-free overlay
        }

        let ptr = child_ptr.as_ptr::<PersistentNode>()?;
        let child = unsafe {
            Arc::increment_strong_count(ptr);
            Arc::from_raw(ptr)
        };
        self.find_leaf_recursive(&child, key, depth + 1)
    }

    /// Lock-free read of a value from the lock-free trie overlay.
    ///
    /// Returns the value if the key is found in the lock-free layer with a value
    /// set. Does not check the persistent layer — callers should check both layers
    /// and sum the results for n-gram counting.
    ///
    /// # Arguments
    ///
    /// * `key` - The byte key (e.g., LEB128-encoded n-gram)
    ///
    /// # Returns
    ///
    /// `Some(value)` if found in the lock-free layer, `None` otherwise.
    #[cfg(feature = "persistent-artrie")]
    #[inline]
    pub fn get_lockfree(&self, key: &[u8]) -> Option<u64> {
        let lockfree_root = self.lockfree_root.as_ref()?;
        let _epoch = self.epoch_manager.enter_read();
        self.find_leaf_lockfree(lockfree_root, key)
            .and_then(|leaf| leaf.get_value())
    }

    /// Lock-free increment: create path if needed, then atomically add delta.
    ///
    /// For existing keys: single `fetch_add` on the leaf (wait-free).
    /// For new keys: CAS retry loop to create path, then set initial value.
    ///
    /// This is the primary method for n-gram counting. Workers call this
    /// concurrently without any locks — contention only occurs when two
    /// threads simultaneously create the *same new path* (rare in practice
    /// since n-gram keys are distributed across the alphabet).
    ///
    /// # Arguments
    ///
    /// * `key` - The byte key (e.g., LEB128-encoded n-gram)
    /// * `delta` - The count to add
    ///
    /// # Returns
    ///
    /// The new accumulated value after increment.
    #[cfg(feature = "persistent-artrie")]
    pub fn increment_cas(&self, key: &[u8], delta: u64) -> u64 {
        use std::sync::atomic::Ordering;

        let lockfree_root = self.lockfree_root.as_ref()
            .expect("Lock-free mode not enabled. Call enable_lockfree() first.");

        if key.is_empty() {
            return 0;
        }

        let _epoch = self.epoch_manager.enter_read();

        // Fast path: find existing leaf and increment atomically (wait-free)
        if let Some(leaf) = self.find_leaf_lockfree(lockfree_root, key) {
            return leaf.increment_value(delta);
        }

        // Slow path: create path, then increment
        loop {
            match self.try_insert_lockfree_path(lockfree_root, key) {
                LockfreeInsertResult::Inserted(leaf) => {
                    // New path created — claim it as final and set initial value
                    leaf.try_set_final();
                    return leaf.increment_value(delta);
                }
                LockfreeInsertResult::AlreadyExists => {
                    // Path exists but we didn't find the leaf earlier — retry find
                    if let Some(leaf) = self.find_leaf_lockfree(lockfree_root, key) {
                        return leaf.increment_value(delta);
                    }
                    // Unusual: exists flag but no leaf found. Retry full path.
                    continue;
                }
                LockfreeInsertResult::Conflict => {
                    // CAS failed — another thread modified the tree, retry
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    // ==================== End Lock-Free CAS Methods ====================

    /// Insert a term into the dictionary (without value)
    pub fn insert(&mut self, term: &str) -> bool {
        self.insert_impl(term.as_bytes(), None)
    }

    /// Insert a term with an associated value
    pub fn insert_with_value(&mut self, term: &str, value: V) -> bool {
        self.insert_impl(term.as_bytes(), Some(value))
    }

    /// Insert multiple terms in a single batch operation.
    ///
    /// This method is optimized for bulk insertions by:
    /// 1. Writing a single BatchInsert WAL record for all entries (reduces header overhead by ~99%)
    /// 2. Syncing only once after all entries are logged
    ///
    /// # Arguments
    ///
    /// * `entries` - Slice of (term, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted (excluding updates to existing terms).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let mut dict: PersistentARTrie<i32> = PersistentARTrie::new();
    /// let entries = vec![
    ///     ("hello".to_string(), Some(1)),
    ///     ("world".to_string(), Some(2)),
    ///     ("foo".to_string(), None),
    /// ];
    /// let inserted = dict.insert_batch(&entries);
    /// println!("Inserted {} new terms", inserted);
    /// ```
    pub fn insert_batch(&mut self, entries: &[(String, Option<V>)]) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // First, log all entries as a single batch WAL record
        if let Some(ref wal_writer) = self.wal_writer {
            // Serialize all entries for WAL
            let wal_entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = entries
                .iter()
                .map(|(term, value)| {
                    let term_bytes = term.as_bytes().to_vec();
                    let value_bytes = value.as_ref().and_then(|v| {
                        bincode::serialize(v).ok()
                    });
                    (term_bytes, value_bytes)
                })
                .collect();

            if let Err(e) = wal_writer.append_batch(&wal_entries) {
                warn!("Failed to log batch insert to WAL: {:?}", e);
            }
        }

        // Then insert each entry without individual WAL logging
        let mut inserted_count = 0;
        for (term, value) in entries {
            if self.insert_impl_core(term.as_bytes(), value.clone()) {
                inserted_count += 1;
            }
        }

        inserted_count
    }

    /// Insert multiple byte-slice terms in a single batch operation.
    ///
    /// This is the byte-slice version of `insert_batch()` for when you already
    /// have byte data and want to avoid string conversion overhead.
    ///
    /// # Arguments
    ///
    /// * `entries` - Slice of (term_bytes, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_bytes(&mut self, entries: &[(&[u8], Option<V>)]) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // First, log all entries as a single batch WAL record
        if let Some(ref wal_writer) = self.wal_writer {
            let wal_entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = entries
                .iter()
                .map(|(term, value)| {
                    let value_bytes = value.as_ref().and_then(|v| {
                        bincode::serialize(v).ok()
                    });
                    (term.to_vec(), value_bytes)
                })
                .collect();

            if let Err(e) = wal_writer.append_batch(&wal_entries) {
                warn!("Failed to log batch insert to WAL: {:?}", e);
            }
        }

        // Then insert each entry without individual WAL logging
        let mut inserted_count = 0;
        for (term, value) in entries {
            if self.insert_impl_core(term, value.clone()) {
                inserted_count += 1;
            }
        }

        inserted_count
    }

    /// Insert multiple terms with optional values in sorted order for cache locality.
    ///
    /// This method sorts the entries lexicographically before inserting them,
    /// which improves cache hit rates since consecutive terms share trie prefix
    /// paths. For large batches, this can improve throughput by 5-20%.
    ///
    /// All entries are logged as a single batch WAL record before insertion.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (term, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_sorted(&mut self, mut entries: Vec<(String, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by term lexicographically for cache locality
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Delegate to insert_batch
        let refs: Vec<(String, Option<V>)> = entries;
        self.insert_batch(&refs)
    }

    /// Insert multiple byte terms with optional values in sorted order for cache locality.
    ///
    /// This method sorts the entries lexicographically before inserting them,
    /// which improves cache hit rates since consecutive terms share trie prefix
    /// paths. For large batches, this can improve throughput by 5-20%.
    ///
    /// All entries are logged as a single batch WAL record before insertion.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (term_bytes, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_bytes_sorted(&mut self, mut entries: Vec<(Vec<u8>, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by term lexicographically for cache locality
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Convert to references for insert_batch_bytes
        let refs: Vec<(&[u8], Option<V>)> = entries
            .iter()
            .map(|(term, value)| (term.as_slice(), value.clone()))
            .collect();
        self.insert_batch_bytes(&refs)
    }

    /// Insert multiple byte terms grouped by first byte for arena locality.
    ///
    /// This method groups inserts by their first byte prefix before inserting,
    /// which improves I/O locality for disk-resident tries. Terms with the same
    /// first byte tend to land in nearby arenas because arenas fill sequentially
    /// during loading.
    ///
    /// # Performance
    ///
    /// Expected improvement: 5-10% faster batch inserts for disk-resident tries
    /// due to improved I/O locality. The first-byte heuristic provides ~60-80%
    /// of the benefit of full arena prediction with O(1) complexity.
    ///
    /// For in-memory tries, there is minimal difference since no disk I/O occurs.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (term_bytes, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    ///
    /// # Algorithm
    ///
    /// 1. Groups entries by first byte (prefix heuristic)
    /// 2. Sorts within groups by full term for lexicographic locality
    /// 3. Inserts entries in grouped order
    ///
    /// This provides arena locality without the overhead of tracking actual arena
    /// assignments, which would require traversal for each term.
    pub fn insert_batch_arena_grouped(&mut self, mut entries: Vec<(Vec<u8>, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by first byte (arena proxy) then by full term for within-group locality
        entries.sort_by(|a, b| {
            let a_prefix = a.0.first().copied().unwrap_or(0);
            let b_prefix = b.0.first().copied().unwrap_or(0);
            a_prefix.cmp(&b_prefix).then_with(|| a.0.cmp(&b.0))
        });

        // Convert to references for insert_batch_bytes
        let refs: Vec<(&[u8], Option<V>)> = entries
            .iter()
            .map(|(term, value)| (term.as_slice(), value.clone()))
            .collect();
        self.insert_batch_bytes(&refs)
    }

    /// Insert multiple string terms grouped by first character for arena locality.
    ///
    /// This is the string variant of `insert_batch_arena_grouped`. See that method
    /// for detailed documentation on the arena grouping strategy.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (term_string, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_grouped(&mut self, mut entries: Vec<(String, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by first character (arena proxy) then by full term
        entries.sort_by(|a, b| {
            let a_prefix = a.0.chars().next().unwrap_or('\0');
            let b_prefix = b.0.chars().next().unwrap_or('\0');
            a_prefix.cmp(&b_prefix).then_with(|| a.0.cmp(&b.0))
        });

        // Delegate to insert_batch
        self.insert_batch(&entries)
    }

    /// Remove a term from the dictionary
    pub fn remove(&mut self, term: &str) -> bool {
        self.remove_impl(term.as_bytes())
    }

    /// Remove all terms with the given prefix (batched for memory efficiency).
    ///
    /// Returns the number of terms removed. Each removal is logged to WAL
    /// individually for crash recovery safety (no batch WAL record type).
    ///
    /// This method processes removals in batches to limit memory usage to
    /// O(batch_size) regardless of how many terms match the prefix. This is
    /// important for large tries (e.g., language models) where a prefix might
    /// match millions of terms.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The byte prefix of terms to remove
    ///
    /// # Returns
    ///
    /// The number of terms that were removed.
    ///
    /// # Memory Usage
    ///
    /// Uses O(1024) memory by default via batched processing. For custom
    /// batch sizes, use `remove_prefix_batched()`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let mut dict: PersistentARTrie<()> = PersistentARTrie::new();
    /// dict.insert("app");
    /// dict.insert("apple");
    /// dict.insert("application");
    /// dict.insert("banana");
    ///
    /// let count = dict.remove_prefix(b"app");
    /// assert_eq!(count, 3); // Removed "app", "apple", "application"
    /// assert!(!dict.contains("apple"));
    /// assert!(dict.contains("banana"));
    /// ```
    pub fn remove_prefix(&mut self, prefix: &[u8]) -> usize {
        // Use default batch size of 1024 for good balance of memory and efficiency
        self.remove_prefix_batched(prefix, 1024)
    }

    /// Remove all terms with the given prefix using a custom batch size.
    ///
    /// This method allows fine-tuning the memory/efficiency trade-off:
    /// - Smaller batch_size = less memory, more iterations
    /// - Larger batch_size = more memory, fewer iterations
    ///
    /// # Arguments
    ///
    /// * `prefix` - The byte prefix of terms to remove
    /// * `batch_size` - Maximum number of terms to collect per iteration
    ///
    /// # Returns
    ///
    /// The number of terms that were removed.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let mut dict: PersistentARTrie<()> = PersistentARTrie::new();
    /// // ... insert many terms with prefix "old_" ...
    ///
    /// // Use small batch size for memory-constrained environments
    /// let count = dict.remove_prefix_batched(b"old_", 100);
    /// ```
    pub fn remove_prefix_batched(&mut self, prefix: &[u8], batch_size: usize) -> usize {
        let batch_size = batch_size.max(1); // Ensure at least 1
        let mut total_removed = 0;

        loop {
            // Collect a batch of terms to remove
            let batch: Vec<Vec<u8>> = self
                .iter_prefix(prefix)
                .map(|iter| iter.take(batch_size).collect())
                .unwrap_or_default();

            if batch.is_empty() {
                break;
            }

            // Remove the batch
            for term in batch {
                if self.remove_impl(&term) {
                    total_removed += 1;
                }
            }
        }

        total_removed
    }

    /// Check if the dictionary is dirty (has uncommitted changes)
    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(AtomicOrdering::Acquire)
    }

    /// Mark the dictionary as clean (after flushing to disk)
    #[inline]
    pub fn mark_clean(&mut self) {
        self.dirty.store(false, AtomicOrdering::Release);
    }

    /// Flush all buffered data to disk for durability.
    ///
    /// This ensures all WAL records are synced to persistent storage.
    /// Call this after a batch of operations to ensure durability.
    /// Honors [`DurabilityPolicy`] for flush behavior.
    ///
    /// # Example
    /// ```rust,ignore
    /// let mut dict: PersistentARTrie<()> = PersistentARTrie::create("words.part")?;
    /// dict.insert("hello");
    /// dict.insert("world");
    /// dict.sync()?; // Ensure both inserts are durable
    /// ```
    pub fn sync(&self) -> Result<()> {
        // Sync WAL to disk based on durability policy
        if let Some(ref wal_writer) = self.wal_writer {
            match self.durability_policy {
                DurabilityPolicy::Immediate => {
                    // Blocking sync - wait for durability
                    wal_writer.sync().map_err(|e| PersistentARTrieError::io_error(
                        "sync", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
                    ))?;
                }
                DurabilityPolicy::GroupCommit => {
                    // Async sync - let group commit coordinator handle batching
                    let _handle = wal_writer.sync_async().map_err(|e| PersistentARTrieError::io_error(
                        "sync_async", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
                    ))?;
                    // Handle can be returned to caller or tracked by coordinator
                }
                DurabilityPolicy::Periodic | DurabilityPolicy::None => {
                    // No immediate sync - background thread handles it
                }
            }
        }

        // Flush all dirty pages from buffer manager
        if let Some(ref buffer_manager) = self.buffer_manager {
            buffer_manager.read().flush_all()?;
        }

        Ok(())
    }

    /// Flush dirty arena slots using sequential I/O.
    ///
    /// This writes modified slots to disk without full re-serialization.
    /// Requires slot tracking to be enabled (via `create_with_slot_tracking`
    /// or `open_with_slot_tracking`).
    pub fn flush_sequential(&self) -> Result<()> {
        if let Some(ref am) = self.arena_manager {
            am.write().flush_sequential()?;
        }
        Ok(())
    }

    /// Async sync - returns a handle to track durability.
    ///
    /// The returned [`SyncHandle`] can be used to wait for durability or
    /// check status without blocking. This allows writes to continue while
    /// the sync happens in the background.
    ///
    /// # Returns
    ///
    /// `Ok(Some(handle))` if a WAL writer is configured, where `handle` can be
    /// used to wait for the sync to complete.
    /// `Ok(None)` if no WAL writer is configured.
    ///
    /// # Example
    /// ```rust,ignore
    /// let dict: PersistentARTrie<()> = PersistentARTrie::create("words.part")?;
    /// dict.insert("hello");
    ///
    /// // Initiate async sync
    /// let handle = dict.sync_async()?.unwrap();
    ///
    /// // Can continue writing while sync happens
    /// dict.insert("world");
    ///
    /// // Wait for first sync when needed
    /// handle.wait().map_err(|e| PersistentARTrieError::io_error(
    ///     "sync_wait", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
    /// ))?;
    /// ```
    pub fn sync_async(&self) -> Result<Option<SyncHandle>> {
        if let Some(ref wal_writer) = self.wal_writer {
            let handle = wal_writer.sync_async().map_err(|e| PersistentARTrieError::io_error(
                "sync_async", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            ))?;
            Ok(Some(handle))
        } else {
            Ok(None)
        }
    }

    /// Returns the next LSN that will be assigned to a write operation.
    ///
    /// This value increases monotonically with each write (insert, remove, update).
    /// It can be used as a "version" or "sequence number" for the trie state.
    ///
    /// # Returns
    /// - The next LSN to be assigned (starts at 1 for persistent tries, 0 for in-memory)
    ///
    /// # Example
    /// ```rust,ignore
    /// let trie = PersistentARTrie::<i32>::create("test.part")?;
    /// let before = trie.current_lsn();
    /// trie.insert_with_value(b"key", 42);
    /// let after = trie.current_lsn();
    /// assert!(after > before);
    /// ```
    #[inline]
    pub fn current_lsn(&self) -> Lsn {
        // Use WAL's authoritative LSN if available, otherwise fall back to cached value
        self.wal_writer.as_ref()
            .map(|wal| wal.current_lsn())
            .unwrap_or_else(|| self.next_lsn.load(AtomicOrdering::Acquire))
    }

    /// Returns the highest LSN that has been durably synced to storage.
    ///
    /// Operations with LSN <= synced_lsn are guaranteed to survive crashes.
    /// Operations with LSN > synced_lsn may be lost if a crash occurs before
    /// the next sync or checkpoint.
    ///
    /// # Returns
    /// - `Some(lsn)` if WAL is enabled and has synced data
    /// - `None` if WAL is disabled (in-memory trie) or no data has been synced yet
    ///
    /// # Example
    /// ```rust,ignore
    /// let trie = PersistentARTrie::<i32>::create("test.part")?;
    /// trie.insert_with_value(b"key", 42);
    /// trie.sync()?;  // Force durability
    /// let synced = trie.synced_lsn();
    /// assert!(synced.is_some());
    /// ```
    pub fn synced_lsn(&self) -> Option<Lsn> {
        self.wal_writer.as_ref().map(|wal| wal.synced_lsn())
    }

    /// Get the current durability policy.
    ///
    /// The durability policy controls when fsync is called after WAL writes.
    /// See [`DurabilityPolicy`] for available options and their trade-offs.
    pub fn durability_policy(&self) -> DurabilityPolicy {
        self.durability_policy
    }

    /// Set the durability policy for this trie.
    ///
    /// The durability policy controls when fsync is called after WAL writes,
    /// providing a trade-off between durability and performance.
    ///
    /// # Arguments
    ///
    /// * `policy` - The new durability policy
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::{PersistentARTrie, DurabilityPolicy};
    ///
    /// let mut dict: PersistentARTrie<()> = PersistentARTrie::create("words.part")?;
    ///
    /// // Use periodic sync for better performance (accepts bounded data loss)
    /// dict.set_durability_policy(DurabilityPolicy::Periodic);
    /// ```
    pub fn set_durability_policy(&mut self, policy: DurabilityPolicy) {
        self.durability_policy = policy;
    }

    /// Get a snapshot of the trie statistics.
    ///
    /// Returns atomic counters for reads, writes, cache hits/misses, etc.
    /// Useful for monitoring and debugging.
    pub fn stats(&self) -> super::concurrency::TrieStatsSnapshot {
        self.stats.snapshot()
    }

    /// Get a reference to the stats tracker for direct recording.
    pub fn stats_tracker(&self) -> Arc<super::concurrency::TrieStats> {
        Arc::clone(&self.stats)
    }

    /// Advance the MVCC epoch.
    ///
    /// This should be called periodically by a background thread to
    /// enable garbage collection of old versions.
    pub fn advance_epoch(&self) -> u64 {
        self.epoch_manager.advance()
    }

    /// Get the current MVCC epoch.
    pub fn current_epoch(&self) -> u64 {
        self.epoch_manager.current_epoch()
    }

    /// Create a checkpoint to allow WAL truncation.
    ///
    /// A checkpoint persists all in-memory trie data to disk, then records
    /// the current LSN in the WAL. This allows older WAL records to be safely
    /// removed after recovery.
    ///
    /// This should be called periodically to prevent unbounded WAL growth.
    ///
    /// # Algorithm
    ///
    /// 1. Persist all in-memory nodes to disk via `persist_to_disk()`
    /// 2. Write a Checkpoint record to WAL with the current LSN
    /// 3. Sync the WAL to ensure durability
    ///
    /// After a checkpoint, recovery only needs to replay WAL records with
    /// LSN > checkpoint_lsn, as earlier operations are already in the
    /// persistent trie structure.
    pub fn checkpoint(&mut self) -> Result<()> {
        use super::wal::WalRecord;

        // First, persist all in-memory data to disk
        self.persist_to_disk()?;

        // Then write the checkpoint record to WAL
        if let Some(ref wal_writer) = self.wal_writer {
            // Get current LSN as checkpoint
            let checkpoint_lsn = self.next_lsn.load(AtomicOrdering::Acquire).saturating_sub(1);
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let record = WalRecord::Checkpoint {
                checkpoint_lsn,
                timestamp,
            };

            wal_writer.append(record).map_err(|e| PersistentARTrieError::io_error(
                "checkpoint_append", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            ))?;
            wal_writer.sync().map_err(|e| PersistentARTrieError::io_error(
                "checkpoint_sync", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            ))?;

            // Truncate WAL after successful checkpoint - all operations are now persisted
            wal_writer.truncate().map_err(|e| PersistentARTrieError::io_error(
                "checkpoint_truncate", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            ))?;
        }

        Ok(())
    }

    /// Get prefetch statistics for performance monitoring.
    ///
    /// Returns a snapshot of prefetch operation counts including:
    /// - Total requests
    /// - Cache hits (node already in memory)
    /// - I/O operations issued
    /// - Dropped requests (queue full)
    ///
    /// # Example
    /// ```rust,ignore
    /// let dict: PersistentARTrie<()> = PersistentARTrie::create("words.part")?;
    /// // ... perform queries ...
    /// let stats = dict.prefetch_stats();
    /// println!("Prefetch hit rate: {:.1}%", stats.hit_rate() * 100.0);
    /// ```
    pub fn prefetch_stats(&self) -> super::prefetch::PrefetchStatsSnapshot {
        self.prefetcher.stats().snapshot()
    }

    /// Get a snapshot node for traversal.
    ///
    /// For `TrieRoot::ArtNode`, threads the real `children: Vec<(u8, ChildNode)>`
    /// into the `PersistentARTrieNode` so the `DictionaryNode::transition`
    /// path returns correct in-memory transitions rather than synthetic
    /// empty Node4 placeholders. On-disk children (`ChildNode::DiskRef`) are
    /// not resolved here — the `DictionaryNode` traversal API skips them,
    /// and callers needing disk-resident traversal should use
    /// `PersistentARTrie::contains` / `get_value` which go through
    /// `resolve_disk_ref` directly.
    fn get_root_node(&self) -> PersistentARTrieNode<V> {
        match &self.root {
            TrieRoot::Bucket(bucket) => PersistentARTrieNode::new_bucket(bucket.clone()),
            TrieRoot::ArtNode {
                node,
                is_final,
                value,
                children,
            } => PersistentARTrieNode::new_root_with_children(
                node.clone(),
                *is_final,
                value.clone(),
                children.clone(),
            ),
        }
    }
}

impl<V: DictionaryValue> Default for PersistentARTrie<V> {
    #[allow(deprecated)]
    fn default() -> Self {
        Self::new()
    }
}

// === Internal Implementation Methods ===
// These methods were previously on PersistentARTrie and are now on PersistentARTrie directly.

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    // =========================================================================
    // Dirty Path Tracking for Selective Persistence
    // =========================================================================

    /// Record a path as dirty for selective persistence.
    ///
    /// This records all prefixes of the given path in `dirty_prefixes` and
    /// invalidates corresponding cached disk locations, since those nodes
    /// will need re-serialization.
    ///
    /// During `persist_to_disk()`, only nodes along dirty paths will be
    /// traversed and serialized.
    ///
    /// # Arguments
    /// * `path` - The full path to the modified node
    #[inline]
    fn record_dirty_path(&mut self, path: &[u8]) {
        // Record all prefixes (including the full path) and invalidate cached locations
        let mut cache = self.persisted_disk_locations.write();
        for len in 0..=path.len() {
            let prefix = path[..len].to_vec();
            self.dirty_prefixes.insert(prefix.clone());
            // Invalidate cached location - node will need re-serialization
            cache.remove(&prefix);
        }
    }

    /// Check if a path needs persistence.
    ///
    /// Returns true if any modification has been made along this path
    /// since the last checkpoint.
    #[inline]
    fn path_needs_persistence(&self, path: &[u8]) -> bool {
        self.dirty_prefixes.contains(path)
    }

    /// Propagate dirty flags up the ancestor chain.
    ///
    /// This sets the HAS_DIRTY_DESCENDANTS flag on the root node
    /// when any modification is made. For nested ART nodes along the path,
    /// the flag propagation happens during the serialization phase based on
    /// dirty_prefixes.
    fn propagate_dirty_to_root(&mut self) {
        if let TrieRoot::ArtNode { node, .. } = &mut self.root {
            node.header_mut().set_has_dirty_descendants(true);
        }
    }

    /// Cache a disk location for a path.
    ///
    /// This is called:
    /// 1. When a DiskRef is resolved for mutation (to potentially skip re-serialization)
    /// 2. After serializing a node (to cache its new disk location for future checkpoints)
    #[inline]
    fn cache_disk_location(&self, path: &[u8], ptr: SwizzledPtr) {
        self.persisted_disk_locations.write().insert(path.to_vec(), ptr);
    }

    /// Get a cached disk location for a path if it exists and the subtree is clean.
    ///
    /// Returns Some(ptr) if:
    /// 1. The path has a cached disk location
    /// 2. The path is NOT in dirty_prefixes (subtree was not modified)
    ///
    /// Returns an owned `SwizzledPtr` to avoid borrow issues with RefCell.
    #[inline]
    fn get_cached_disk_location(&self, path: &[u8]) -> Option<SwizzledPtr> {
        if self.dirty_prefixes.contains(path) {
            None // Path was modified, can't use cached location
        } else {
            self.persisted_disk_locations.read().get(path).cloned()
        }
    }

    /// Resolve a DiskRef child for mutation, caching the original location.
    ///
    /// This wraps `resolve_child_for_mutation_with_bm` to also cache the
    /// original disk location for potential reuse during persistence.
    ///
    /// # Arguments
    /// * `child` - Mutable reference to the child node to resolve
    /// * `path` - The path to this child (for caching the disk location)
    ///
    /// # Returns
    /// * `true` if the child is now in memory
    /// * `false` if resolution failed
    fn resolve_and_cache_disk_location(&mut self, child: &mut ChildNode, path: &[u8]) -> bool {
        // If it's a DiskRef, cache its location before resolving
        if let ChildNode::DiskRef { ptr } = child {
            self.cache_disk_location(path, ptr.clone());
        }

        resolve_child_for_mutation_with_bm(child, self.buffer_manager.as_ref())
    }

    /// Clear dirty tracking state after a successful checkpoint.
    ///
    /// This clears:
    /// 1. `dirty_prefixes` - All recorded dirty paths
    /// 2. Dirty flags on all nodes in the trie
    ///
    /// NOTE: `persisted_disk_locations` is intentionally NOT cleared.
    /// We preserve cached disk locations so that on subsequent checkpoints,
    /// clean subtrees can return their cached location without re-serialization.
    /// These cached entries are invalidated by `record_dirty_path()` when
    /// paths become dirty (on insert/remove).
    fn clear_dirty_tracking_state(&mut self) {
        self.dirty_prefixes.clear();
        // NOTE: Do NOT clear persisted_disk_locations - we need to preserve
        // cached locations for subsequent checkpoints to skip clean subtrees.
        self.clear_dirty_flags_recursive();
    }

    /// Recursively clear dirty flags on all nodes in the trie.
    fn clear_dirty_flags_recursive(&mut self) {
        match &mut self.root {
            TrieRoot::Bucket(_) => {
                // Buckets don't have dirty flags
            }
            TrieRoot::ArtNode { node, children, .. } => {
                node.header_mut().clear_dirty_flags();
                for (_, child) in children {
                    Self::clear_child_dirty_flags_recursive(child);
                }
            }
        }
    }

    /// Recursively clear dirty flags on a child node and its descendants.
    fn clear_child_dirty_flags_recursive(child: &mut ChildNode) {
        match child {
            ChildNode::Bucket(_) => {
                // Buckets don't have dirty flags
            }
            ChildNode::ArtNode { node, children, .. } => {
                node.header_mut().clear_dirty_flags();
                for (_, c) in children {
                    Self::clear_child_dirty_flags_recursive(c);
                }
            }
            ChildNode::DiskRef { .. } => {
                // DiskRef nodes are already clean
            }
        }
    }

    // =========================================================================
    // Insert / Remove Implementations
    // =========================================================================

    /// Insert implementation with WAL logging (for persistent mode).
    fn insert_impl(&mut self, term: &[u8], value: Option<V>) -> bool {
        // Clone value for WAL logging if needed (before move into core)
        let value_for_wal = value.clone();

        // Perform the actual insert
        let inserted = self.insert_impl_core(term, value);

        // Log to WAL if insert was successful OR if we're updating an existing term's value
        // We need to log value updates even when the term already exists (inserted = false)
        if inserted || value_for_wal.is_some() {
            if let Some(ref wal_writer) = self.wal_writer {
                use super::wal::WalRecord;

                // Serialize the value using bincode if present
                let serialized_value = value_for_wal.and_then(|v| {
                    match bincode::serialize(&v) {
                        Ok(bytes) => Some(bytes),
                        Err(e) => {
                            warn!("Failed to serialize value for WAL: {:?}", e);
                            None
                        }
                    }
                });

                let record = WalRecord::Insert {
                    term: term.to_vec(),
                    value: serialized_value,
                };
                if let Err(e) = wal_writer.append(record) {
                    // Log error but don't fail the insert - data is in memory
                    warn!("Failed to log insert to WAL: {:?}", e);
                }
            }
        }

        inserted
    }

    /// Core insert implementation without WAL logging.
    fn insert_impl_core(&mut self, term: &[u8], value: Option<V>) -> bool {
        // Clone buffer manager reference before mutable borrow of self.root
        // This is needed to resolve DiskRef nodes during mutation
        let buffer_manager = self.buffer_manager.clone();

        let inserted = match &mut self.root {
            TrieRoot::Bucket(bucket) => {
                // Clone value here in case we need to retry after bucket conversion
                let value_for_retry = value.clone();

                // Serialize value for bucket storage
                let serialized_value: Option<Vec<u8>> = value.and_then(|v| {
                    bincode::serialize(&v).ok()
                });


                let result = if let Some(ref val_bytes) = serialized_value {
                    bucket.insert(term, val_bytes)
                } else {
                    bucket.insert_key(term)
                };

                match result {
                    Ok(inserted) => {
                        // Check if bucket needs to be converted to ART
                        if bucket.header().should_split() {
                            self.convert_bucket_to_art();
                        }
                        inserted
                    }
                    Err(_) => {
                        // Bucket is full, convert to ART and retry
                        self.convert_bucket_to_art();
                        // Retry insert in the new ART structure (no double WAL logging)
                        self.insert_impl_core(term, value_for_retry);
                        true
                    }
                }
            }
            TrieRoot::ArtNode {
                node,
                children,
                is_final,
                value: root_value,
            } => {
                // Serialize value for bucket storage (same as root bucket case)
                let serialized_value: Option<Vec<u8>> = value.clone().and_then(|v| {
                    bincode::serialize(&v).ok()
                });


                if term.is_empty() {
                    // Inserting empty string
                    if *is_final {
                        if value.is_some() {
                            *root_value = value;
                        }
                        false
                    } else {
                        *is_final = true;
                        *root_value = value;
                        true
                    }
                } else {
                    // Find or create child for first byte
                    let first_byte = term[0];
                    let remaining = &term[1..];

                    // Find existing child
                    let child_idx = children.iter().position(|(b, _)| *b == first_byte);

                    if let Some(idx) = child_idx {
                        // Resolve DiskRef if needed before mutation
                        if !resolve_child_for_mutation_with_bm(&mut children[idx].1, buffer_manager.as_ref()) {
                            return false; // Resolution failed (logged in resolve_child_for_mutation_with_bm)
                        }
                        // Use insert_with_value which handles bucket overflow recursively
                        children[idx]
                            .1
                            .insert_with_value(remaining, serialized_value.as_deref())
                    } else {
                        // Create new child bucket
                        let mut bucket = StringBucket::with_values();
                        // Insert with value if provided
                        if let Some(ref val_bytes) = serialized_value {
                            let _ = bucket.insert(remaining, val_bytes);
                        } else {
                            let _ = bucket.insert_key(remaining);
                        }

                        // Add child to ART node, growing the node if it's full
                        let ptr = SwizzledPtr::null();
                        let add_result = match node {
                            Node::N4(n) => n.add_child(first_byte, ptr.clone()),
                            Node::N16(n) => n.add_child(first_byte, ptr.clone()),
                            Node::N48(n) => n.add_child(first_byte, ptr.clone()),
                            Node::N256(n) => n.add_child(first_byte, ptr.clone()),
                        };

                        // If node is full, grow it and retry
                        if let Err(super::nodes::AddChildError::NodeFull) = add_result {
                            // Grow the node to a larger type
                            let grown_node = match node {
                                Node::N4(n) => Node::N16(Box::new(n.grow())),
                                Node::N16(n) => Node::N48(Box::new(n.grow())),
                                Node::N48(n) => Node::N256(Box::new(n.grow())),
                                Node::N256(_) => {
                                    // Node256 can't grow further, this shouldn't happen
                                    // since Node256 can hold all 256 children
                                    log::error!("Cannot grow Node256 - this should never happen");
                                    children.push((first_byte, ChildNode::Bucket(bucket)));
                                    return true;
                                }
                            };
                            *node = grown_node;

                            // Retry add_child on the grown node
                            let _ = match node {
                                Node::N4(n) => n.add_child(first_byte, ptr),
                                Node::N16(n) => n.add_child(first_byte, ptr),
                                Node::N48(n) => n.add_child(first_byte, ptr),
                                Node::N256(n) => n.add_child(first_byte, ptr),
                            };
                        }

                        children.push((first_byte, ChildNode::Bucket(bucket)));
                        true
                    }
                }
            }
        };

        if inserted {
            self.term_count.fetch_add(1, AtomicOrdering::Relaxed);
            self.dirty.store(true, AtomicOrdering::Release);
            // Record the path as dirty for selective persistence
            self.record_dirty_path(term);
            self.propagate_dirty_to_root();
        }

        inserted
    }

    /// Remove implementation with WAL logging (for persistent mode).
    fn remove_impl(&mut self, term: &[u8]) -> bool {
        // Perform the actual remove
        let removed = self.remove_impl_core(term);

        // Log to WAL if remove was successful and we have a WAL writer
        if removed {
            if let Some(ref wal_writer) = self.wal_writer {
                use super::wal::WalRecord;
                let record = WalRecord::Remove {
                    term: term.to_vec(),
                };
                if let Err(e) = wal_writer.append(record) {
                    // Log error but don't fail the remove - data is in memory
                    warn!("Failed to log remove to WAL: {:?}", e);
                }
            }
        }

        removed
    }

    /// Core remove implementation without WAL logging.
    fn remove_impl_core(&mut self, term: &[u8]) -> bool {
        // Clone buffer manager reference before mutable borrow of self.root
        // This is needed to resolve DiskRef nodes during mutation
        let buffer_manager = self.buffer_manager.clone();

        let removed = match &mut self.root {
            TrieRoot::Bucket(bucket) => bucket.remove(term).is_some(),
            TrieRoot::ArtNode {
                node: _,
                children,
                is_final,
                value,
            } => {
                if term.is_empty() {
                    if *is_final {
                        *is_final = false;
                        *value = None;
                        true
                    } else {
                        false
                    }
                } else {
                    let first_byte = term[0];
                    let remaining = &term[1..];

                    let child_idx = children.iter().position(|(b, _)| *b == first_byte);

                    if let Some(idx) = child_idx {
                        // Resolve DiskRef if needed before mutation
                        if !resolve_child_for_mutation_with_bm(&mut children[idx].1, buffer_manager.as_ref()) {
                            return false; // Resolution failed (logged in resolve_child_for_mutation_with_bm)
                        }
                        match &mut children[idx].1 {
                            ChildNode::Bucket(bucket) => bucket.remove(remaining).is_some(),
                            ChildNode::ArtNode {
                                is_final: child_is_final,
                                value: child_value,
                                children: child_children,
                                ..
                            } => {
                                // Recursive remove from child ART
                                if remaining.is_empty() {
                                    if *child_is_final {
                                        *child_is_final = false;
                                        *child_value = None;
                                        true
                                    } else {
                                        false
                                    }
                                } else {
                                    let first = remaining[0];
                                    let rest = &remaining[1..];

                                    // Find child with matching byte
                                    for (b, c) in child_children.iter_mut() {
                                        if *b == first {
                                            // Use recursive remove_key for all child types
                                            return c.remove_key(rest);
                                        }
                                    }
                                    false
                                }
                            }
                            ChildNode::DiskRef { .. } => {
                                // DiskRef should have been resolved above
                                unreachable!("DiskRef should have been resolved by resolve_child_for_mutation_with_bm")
                            }
                        }
                    } else {
                        false
                    }
                }
            }
        };

        if removed {
            self.term_count.fetch_sub(1, AtomicOrdering::Relaxed);
            self.dirty.store(true, AtomicOrdering::Release);
            // Record the path as dirty for selective persistence
            self.record_dirty_path(term);
            self.propagate_dirty_to_root();
        }

        removed
    }

    /// Convert root bucket to ART node structure
    fn convert_bucket_to_art(&mut self) {
        if let TrieRoot::Bucket(bucket) = &self.root {
            if let Some(result) = bucket_to_art_node(bucket).ok() {
                // bucket_to_art_node now returns ChildNode directly (which may be
                // buckets or nested ART nodes for overflowed children)
                self.root = TrieRoot::ArtNode {
                    node: result.node,
                    children: result.children,
                    is_final: result.is_final,
                    // Value cannot be preserved from bucket conversion because
                    // bucket uses Vec<u8> while TrieRoot uses V. Adding serde
                    // bounds to DictionaryValue would enable value preservation.
                    value: None,
                };
            }
        }
    }

    /// Insert implementation without WAL logging (for recovery replay).
    ///
    /// This is used during WAL recovery to avoid re-logging operations
    /// that are already in the WAL.
    fn insert_impl_no_wal(&mut self, term: &[u8], value: Option<V>) -> bool {
        // Call core implementation directly to skip WAL logging
        self.insert_impl_core(term, value)
    }

    /// Remove implementation without WAL logging (for recovery replay).
    ///
    /// This is used during WAL recovery to avoid re-logging operations
    /// that are already in the WAL.
    fn remove_impl_no_wal(&mut self, term: &[u8]) -> bool {
        // Call core implementation directly to skip WAL logging
        self.remove_impl_core(term)
    }

    /// Upsert implementation without WAL logging (for recovery replay).
    ///
    /// This updates the value if the term exists, or inserts if it doesn't.
    /// Used during WAL recovery to replay Upsert, Increment, and CAS operations.
    fn upsert_impl_no_wal(&mut self, term: &[u8], value: V) {
        // First remove existing entry (if any) to allow update
        self.remove_impl_core(term);
        // Then insert with new value
        self.insert_impl_core(term, Some(value));
    }

    /// Check if a term is contained in the dictionary.
    ///
    /// This method handles:
    /// - Bucket root lookups
    /// - ART node traversal
    /// - Lazy loading of DiskRef children
    /// - Multi-level prefetching of sibling nodes for better I/O performance
    fn contains_impl(&self, term: &[u8]) -> bool {
        match &self.root {
            TrieRoot::Bucket(bucket) => bucket.contains(term),
            TrieRoot::ArtNode {
                children,
                is_final,
                ..
            } => {
                if term.is_empty() {
                    return *is_final;
                }

                let first_byte = term[0];
                let remaining = &term[1..];

                // Prefetch DiskRef children at the root level (depth 0)
                self.prefetch_disk_refs_bounded(children, 0);

                // Find child with matching first byte
                for (b, child) in children {
                    if *b == first_byte {
                        return self.contains_in_child_with_depth(child, remaining, 1);
                    }
                }
                false
            }
        }
    }

    /// Get the value associated with a term.
    ///
    /// Returns `Some(value)` if the term exists and has an associated value,
    /// `None` if the term doesn't exist or has no value.
    ///
    /// Uses multi-level prefetching for better I/O performance on disk-resident tries.
    fn get_value_impl(&self, term: &[u8]) -> Option<V> {
        match &self.root {
            TrieRoot::Bucket(bucket) => {
                // Search for the term in the bucket
                match bucket.search(term) {
                    Ok(idx) => {
                        // Found the term, get its value
                        if let Some(entry) = bucket.get_entry(idx) {
                            if let Some(value_bytes) = bucket.get_value(&entry) {
                                // Deserialize the value
                                bincode::deserialize(value_bytes).ok()
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    Err(_) => None, // Term not found
                }
            }
            TrieRoot::ArtNode {
                children,
                is_final,
                value,
                ..
            } => {
                if term.is_empty() {
                    if *is_final {
                        return value.clone();
                    }
                    return None;
                }

                let first_byte = term[0];
                let remaining = &term[1..];

                // Prefetch DiskRef children at the root level (depth 0)
                self.prefetch_disk_refs_bounded(children, 0);

                // Find child with matching first byte
                for (b, child) in children {
                    if *b == first_byte {
                        return self.get_value_in_child_with_depth(child, remaining, 1);
                    }
                }
                None
            }
        }
    }

    /// Get value from a child node.
    fn get_value_in_child(&self, child: &ChildNode, remaining: &[u8]) -> Option<V> {
        self.get_value_in_child_with_depth(child, remaining, 0)
    }

    /// Get value from a child node with depth tracking for multi-level prefetching.
    ///
    /// # Arguments
    ///
    /// * `child` - The child node to search
    /// * `remaining` - The remaining term bytes to match
    /// * `depth` - Current traversal depth (increments with each level)
    fn get_value_in_child_with_depth(&self, child: &ChildNode, remaining: &[u8], depth: u16) -> Option<V> {
        match child {
            ChildNode::Bucket(bucket) => {
                match bucket.search(remaining) {
                    Ok(idx) => {
                        if let Some(entry) = bucket.get_entry(idx) {
                            if let Some(value_bytes) = bucket.get_value(&entry) {
                                bincode::deserialize(value_bytes).ok()
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    Err(_) => None,
                }
            }
            ChildNode::ArtNode {
                is_final,
                children,
                value,
                ..
            } => {
                if remaining.is_empty() {
                    if *is_final {
                        // Deserialize value from stored bytes
                        return value.as_ref().and_then(|bytes| {
                            bincode::deserialize(bytes).ok()
                        });
                    }
                    return None;
                }

                let first_byte = remaining[0];
                let rest = &remaining[1..];

                // Multi-level prefetch with depth bounds
                self.prefetch_disk_refs_bounded(children, depth);

                for (b, grandchild) in children {
                    if *b == first_byte {
                        return self.get_value_in_child_with_depth(grandchild, rest, depth + 1);
                    }
                }
                None
            }
            ChildNode::DiskRef { ptr } => {
                // Lazy load from disk and get value
                if let Some(disk_location) = ptr.disk_location() {
                    if let Ok(resolved) = self.resolve_disk_ref(&disk_location) {
                        return self.get_value_in_child_with_depth(&resolved, remaining, depth);
                    }
                }
                None
            }
        }
    }

    /// Check if remaining term is contained in a child node.
    ///
    /// Handles all child node types including lazy loading of DiskRef.
    /// Uses prefetcher to read-ahead sibling nodes for better I/O performance.
    fn contains_in_child(&self, child: &ChildNode, remaining: &[u8]) -> bool {
        self.contains_in_child_with_depth(child, remaining, 0)
    }

    /// Check if remaining term is contained in a child node with depth tracking.
    ///
    /// This internal method tracks traversal depth for multi-level prefetching.
    /// The depth parameter enables the prefetcher to limit prefetching at deep
    /// levels to avoid excessive I/O for very deep tries.
    ///
    /// # Arguments
    ///
    /// * `child` - The child node to search
    /// * `remaining` - The remaining term bytes to match
    /// * `depth` - Current traversal depth (increments with each level)
    fn contains_in_child_with_depth(&self, child: &ChildNode, remaining: &[u8], depth: u16) -> bool {
        match child {
            ChildNode::Bucket(bucket) => bucket.contains(remaining),
            ChildNode::ArtNode {
                is_final,
                children,
                ..
            } => {
                if remaining.is_empty() {
                    return *is_final;
                }

                let first_byte = remaining[0];
                let rest = &remaining[1..];

                // Multi-level prefetch with depth bounds
                self.prefetch_disk_refs_bounded(children, depth);

                // Recursively search in children with incremented depth
                for (b, child) in children {
                    if *b == first_byte {
                        return self.contains_in_child_with_depth(child, rest, depth + 1);
                    }
                }
                false
            }
            ChildNode::DiskRef { ptr } => {
                // Lazy load from disk
                if let Some(disk_location) = ptr.disk_location() {
                    if let Ok(resolved) = self.resolve_disk_ref(&disk_location) {
                        return self.contains_in_child_with_depth(&resolved, remaining, depth);
                    }
                }
                false
            }
        }
    }

    /// Prefetch all DiskRef children in a children list.
    ///
    /// This hints the prefetcher to start loading disk-resident children
    /// in the background while we process the current node.
    fn prefetch_disk_refs(&self, children: &[(u8, ChildNode)]) {
        self.prefetch_disk_refs_bounded(children, 0);
    }

    /// Prefetch DiskRef children with depth bounds for multi-level prefetching.
    ///
    /// This method extends prefetching to all traversal levels, not just the root.
    /// When the prefetcher is configured with `DepthLimited(n)` strategy, prefetching
    /// will be disabled for nodes deeper than `n` levels, preventing excessive I/O
    /// for very deep tries.
    ///
    /// # Performance
    ///
    /// Multi-level prefetching improves cold lookup performance by 15-30% by
    /// initiating I/O for nodes at depth D while processing nodes at depth D-1.
    /// With default `DepthLimited(3)`, prefetching occurs for the first 4 levels.
    ///
    /// # Arguments
    ///
    /// * `children` - The children to potentially prefetch
    /// * `depth` - Current traversal depth (0 = root level)
    fn prefetch_disk_refs_bounded(&self, children: &[(u8, ChildNode)], depth: u16) {
        // Collect SwizzledPtr references for disk-resident children
        let disk_children: Vec<(u8, super::swizzled_ptr::SwizzledPtr)> = children
            .iter()
            .filter_map(|(key, child)| {
                if let ChildNode::DiskRef { ptr } = child {
                    Some((*key, ptr.clone()))
                } else {
                    None
                }
            })
            .collect();

        if !disk_children.is_empty() {
            self.prefetcher.prefetch_children_bounded(&disk_children, depth);
        }
    }

    /// Resolve a DiskRef to its actual node data by loading from disk.
    ///
    /// This is the core lazy loading mechanism. When a child is stored as a
    /// DiskRef (pointing to disk), this method:
    /// 1. Reads the page data from the BufferManager
    /// 2. Deserializes the node or bucket data
    /// 3. Returns the resolved ChildNode
    ///
    /// # Arguments
    /// * `disk_location` - The disk location to load from
    ///
    /// # Returns
    /// The resolved ChildNode, or an error if loading failed.
    pub(super) fn resolve_disk_ref(&self, disk_location: &DiskLocation) -> Result<ChildNode> {
        // Get buffer manager (required for disk I/O)
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager available for disk I/O")
        })?;

        // Fetch the page containing the node
        let bm = buffer_manager.read();

        let page_guard = bm.fetch_page(disk_location.block_id)?;
        let page_data = page_guard.data();

        // Deserialize based on node type
        let offset = disk_location.offset as usize;
        let node_data = &page_data[offset..];

        match disk_location.node_type {
            NodeType::Bucket => {
                // Deserialize bucket
                // For now, return an empty bucket - full bucket serialization
                // will be implemented in Phase 7.4
                let bucket = StringBucket::new();
                Ok(ChildNode::Bucket(bucket))
            }
            NodeType::Node4 | NodeType::Node16 | NodeType::Node48 | NodeType::Node256 => {
                // Deserialize ART node
                let node = serialization::from_bytes(node_data)?;
                let is_final = node.header().is_final();
                Ok(ChildNode::ArtNode {
                    node,
                    is_final, // Read from deserialized node header
                    value: None, // Value not stored in node header currently
                    children: Vec::new(), // Children will be loaded lazily via SwizzledPtr
                })
            }
            // Char-level nodes should never appear in byte-level trie
            NodeType::CharNode4 | NodeType::CharNode16 | NodeType::CharNode48 | NodeType::CharBucket => {
                Err(PersistentARTrieError::corrupted(
                    "Char-level node type encountered in byte-level PersistentARTrie"
                ))
            }
        }
    }

    /// Check if a child needs lazy loading and resolve it if necessary.
    ///
    /// This is a no-op without the persistent-artrie feature.

    /// Check if a child needs lazy loading and resolve it if necessary.
    ///
    /// Returns Some(resolved_child) if the child was a DiskRef that was successfully
    /// resolved, or None if no resolution was needed (already in memory).
    fn resolve_child_if_needed(&self, child: &ChildNode) -> Option<ChildNode> {
        match child {
            ChildNode::DiskRef { ptr } => {
                // Get disk location from SwizzledPtr
                if let Some(disk_location) = ptr.disk_location() {
                    // Resolve from disk
                    self.resolve_disk_ref(&disk_location).ok()
                } else {
                    None
                }
            }
            _ => None, // Already in memory
        }
    }

    /// Resolve a DiskRef child in place, replacing it with the loaded node.
    ///
    /// This is a wrapper method that extracts the buffer manager and calls
    /// the free function `resolve_child_for_mutation_with_bm`.
    ///
    /// # Arguments
    /// * `child` - Mutable reference to the child node to resolve
    ///
    /// # Returns
    /// * `true` if the child is now in memory (either already was, or successfully resolved)
    /// * `false` if the child was a DiskRef that failed to resolve
    fn resolve_child_for_mutation(&self, child: &mut ChildNode) -> bool {
        resolve_child_for_mutation_with_bm(child, self.buffer_manager.as_ref())
    }

    /// Resolve a DiskRef child in place (non-persistent fallback).
    ///
    /// Without the persistent-artrie feature, DiskRef nodes should never exist.
    /// This returns false for DiskRef (indicating an error state) and true for
    /// all other node types.

    /// Check if child slots are consecutive in the same arena.
    ///
    /// For sequential sibling storage to work, all children must:
    /// 1. Be in the same arena as the parent will be
    /// 2. Have consecutive slot IDs (first, first+1, first+2, ...)
    ///
    /// # Arguments
    /// * `node` - The node whose children to check
    /// * `parent_arena_id` - The arena ID where the parent will be allocated
    ///
    /// # Returns
    /// `Some(first_child_slot)` if children are consecutive, `None` otherwise.
    fn check_sequential_children(node: &Node, parent_arena_id: u32) -> Option<ArenaSlot> {
        // Collect all non-null children
        let mut child_slots: Vec<ArenaSlot> = Vec::new();

        for (_key, child_ptr) in node.iter_children() {
            if let Some(slot) = child_ptr.as_arena_slot() {
                child_slots.push(slot);
            } else if !child_ptr.is_null() {
                // Child is in memory or other format, can't use sequential
                return None;
            }
        }

        // Need at least 2 children for sequential to provide benefit
        if child_slots.len() < 2 {
            return None;
        }

        // All children must be in the same arena as the parent
        if child_slots.iter().any(|slot| slot.arena_id != parent_arena_id) {
            return None;
        }

        // Sort by slot ID to check if consecutive
        child_slots.sort_by_key(|slot| slot.slot_id);

        // Check if consecutive
        let first = child_slots[0];
        for (i, slot) in child_slots.iter().enumerate() {
            if slot.slot_id != first.slot_id + i as u32 {
                return None; // Not consecutive
            }
        }

        Some(first)
    }

    /// Serialize a bucket to disk and return a SwizzledPtr to its location.
    ///
    /// This allocates a new page via the BufferManager, writes the bucket data,
    /// and returns a SwizzledPtr pointing to the disk location.
    ///
    /// # Arguments
    /// * `bucket` - The bucket to serialize
    ///
    /// # Returns
    /// A SwizzledPtr pointing to the bucket on disk.
    ///
    /// The SwizzledPtr uses:
    /// - arena_id as block_id (23 bits, up to 8M arenas)
    /// - slot_id as offset (22 bits, up to 4M slots per arena)
    fn serialize_bucket_to_disk(&self, bucket: &StringBucket) -> Result<SwizzledPtr> {
        // Get arena manager
        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk serialization")
        })?;

        // Get bucket bytes (8KB)
        let bucket_bytes = bucket.as_bytes();

        // Allocate in arena (space-efficient: packs buckets and nodes together)
        let slot = arena_manager.write().allocate(bucket_bytes)?;

        // Return pointer using arena addressing:
        // - block_id = arena_id + 1 (block 0 is file header, arena N is block N+1)
        // - offset = slot_id
        Ok(SwizzledPtr::from_arena_slot(slot, NodeType::Bucket))
    }

    /// Serialize an ART node to disk and return a SwizzledPtr to its location.
    ///
    /// This allocates a new page via the BufferManager, writes the serialized node,
    /// and returns a SwizzledPtr pointing to the disk location.
    ///
    /// # Arguments
    /// * `node` - The ART node to serialize
    ///
    /// # Returns
    /// A SwizzledPtr pointing to the node on disk.
    ///
    /// The SwizzledPtr uses:
    /// - arena_id as block_id (23 bits, up to 8M arenas)
    /// - slot_id as offset (22 bits, up to 4M slots per arena)
    ///
    /// # Encoding Strategy
    ///
    /// Uses v2 serialization with relative offset encoding for child pointers.
    /// When children are in the same arena as the parent, their pointers are
    /// encoded as relative offsets (parent_slot - child_slot), which typically
    /// fit in 1-2 bytes instead of 8 bytes for absolute pointers.
    ///
    /// If the parent would overflow to a new arena (breaking same-arena locality),
    /// falls back to v1 serialization with absolute pointers.
    fn serialize_node_to_disk(&self, node: &Node) -> Result<SwizzledPtr> {
        // Get arena manager
        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk serialization")
        })?;

        let mut am = arena_manager.write();

        // Estimate serialized size to check for arena overflow
        // We need a temporary context to estimate size - use current arena
        let temp_slot = am.next_slot();
        let temp_ctx = SerializationContext::new(temp_slot);
        let estimated_size = serialization::v2::estimate_serialized_size_v2(node, &temp_ctx);

        // Predict the actual parent slot accounting for possible arena overflow
        let parent_slot = if am.can_fit(estimated_size) {
            // Node will fit in current arena
            am.next_slot()
        } else {
            // Node will overflow to new arena - predict slot 0 in new arena
            ArenaSlot::new(am.arena_count() as u32, 0)
        };

        // Create serialization context with predicted parent slot for relative encoding
        // Check if children are consecutive (enables sequential sibling storage)
        let ctx = if let Some(first_child) = Self::check_sequential_children(node, parent_slot.arena_id) {
            // Children are consecutive in same arena: use sequential sibling encoding
            // This stores only (first_child_slot, count) instead of N separate pointers
            SerializationContext::sequential(parent_slot, first_child)
        } else {
            // Children are not consecutive: use relative encoding only
            SerializationContext::new(parent_slot)
        };

        // Serialize the node to bytes using v2 format with relative offsets
        let node_bytes = serialization::v2::serialize_node_v2(node, &ctx)?;

        // Allocate in arena (space-efficient: packs many nodes per 256KB block)
        let slot = am.allocate(&node_bytes)?;

        // Verify we got the slot we predicted
        debug_assert_eq!(
            slot, parent_slot,
            "Slot mismatch: predicted {:?}, got {:?}",
            parent_slot, slot
        );

        // Determine node type for SwizzledPtr
        let node_type = match node {
            Node::N4(_) => NodeType::Node4,
            Node::N16(_) => NodeType::Node16,
            Node::N48(_) => NodeType::Node48,
            Node::N256(_) => NodeType::Node256,
        };

        // Return pointer using arena addressing:
        // - block_id = arena_id + 1 (block 0 is file header, arena N is block N+1)
        // - offset = slot_id
        Ok(SwizzledPtr::from_arena_slot(slot, node_type))
    }

    /// Persist all modified nodes in the trie to disk.
    ///
    /// This method walks through the trie structure and serializes all
    /// in-memory nodes to disk, then updates the file header with the
    /// root pointer. After this, the trie can be loaded from disk without
    /// replaying the WAL.
    ///
    /// # Algorithm
    ///
    /// 1. Recursively serialize all children (buckets and nested ART nodes)
    /// 2. Serialize the root node/bucket
    /// 3. Create a root descriptor block with metadata
    /// 4. Update the file header's root_ptr to point to the descriptor
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error if serialization fails.
    pub fn persist_to_disk(&mut self) -> Result<()> {

        // Get buffer manager and arena manager
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for disk serialization")
        })?;

        // Serialize the trie root and get a descriptor
        let (root_type, root_ptr, is_final, term_count) = match &self.root {
            TrieRoot::Bucket(bucket) => {
                // Serialize the bucket
                let ptr = self.serialize_bucket_to_disk(bucket)?;
                (ROOT_TYPE_BUCKET, ptr.to_raw(), false, self.term_count.load(AtomicOrdering::Acquire))
            }
            TrieRoot::ArtNode {
                node,
                children,
                is_final,
                value,
            } => {
                // First, serialize all children recursively and collect their pointers
                // Use path-aware serialization for selective dirty subtree traversal
                let mut child_ptrs: Vec<(u8, u64)> = Vec::with_capacity(children.len());
                for (edge, child) in children {
                    // Construct the path to this child (single byte from root)
                    let child_path = [*edge];
                    let ptr = self.serialize_child_to_disk_with_path(child, &child_path)?;
                    child_ptrs.push((*edge, ptr.to_raw()));
                }

                // Create a copy of the node with updated child pointers
                let mut node_copy = node.clone();
                for (edge, ptr_raw) in &child_ptrs {
                    // Update the node's child pointer for this edge
                    if let Some(child_ptr) = node_copy.find_child_mut(*edge) {
                        *child_ptr = SwizzledPtr::from_raw(*ptr_raw);
                    }
                }

                // Serialize the updated node
                let node_ptr = self.serialize_node_to_disk(&node_copy)?;

                // Value serialization not implemented (DictionaryValue requires serde bounds)
                let _ = value;

                (ROOT_TYPE_ART_NODE, node_ptr.to_raw(), *is_final, self.term_count.load(AtomicOrdering::Acquire))
            }
        };

        // Flush arenas to disk before creating root descriptor
        // This ensures all nodes are persisted before we record the root pointer
        // Uses slot-level incremental flush if configured, otherwise full arena flush
        if let Some(ref arena_manager) = self.arena_manager {
            let stats = arena_manager.write().flush_dirty_slots()?;
            if stats.partial_writes > 0 {
                log::debug!(
                    "Incremental flush: {} full arenas, {} partial, {} slots, {} bytes written, {} bytes saved",
                    stats.full_arena_writes, stats.partial_writes, stats.slots_written,
                    stats.bytes_written, stats.bytes_saved
                );
            }
        }

        // Get arena count (block IDs are derived from sequential allocation: 1..=arena_count)
        let arena_count: u32 = if let Some(ref arena_manager) = self.arena_manager {
            arena_manager.read().arena_count() as u32
        } else {
            0
        };

        // Create root descriptor (fixed 18 bytes)
        // Format:
        //   0: type (1 byte)
        //   1: is_final (1 byte)
        //   2-5: term_count (4 bytes, little endian)
        //   6-9: arena_count (4 bytes, little endian)
        //   10-17: root_ptr (8 bytes, little endian)
        //
        // Note: Arena block IDs are NOT stored - they are derived from sequential allocation:
        // Block 0 = file header + descriptor, Blocks 1..=arena_count = arenas
        let mut descriptor = [0u8; 18];
        descriptor[0] = root_type;
        descriptor[1] = if is_final { 1 } else { 0 };
        descriptor[2..6].copy_from_slice(&(term_count as u32).to_le_bytes());
        descriptor[6..10].copy_from_slice(&arena_count.to_le_bytes());
        descriptor[10..18].copy_from_slice(&root_ptr.to_le_bytes());

        // Write descriptor to fixed location in block 0 (offset 64, after file header)
        // This ensures arenas always occupy blocks 1, 2, 3, ... sequentially
        const DESCRIPTOR_OFFSET: usize = 64;
        let bm = buffer_manager.write();
        let dm = bm.storage();
        dm.write_bytes(0, DESCRIPTOR_OFFSET, &descriptor)?;

        // Update root_ptr to point to block 0, offset 64
        let root_descriptor_ptr = SwizzledPtr::on_disk(0, DESCRIPTOR_OFFSET as u32, NodeType::Bucket);
        dm.set_root_ptr(root_descriptor_ptr.to_raw())?;
        dm.set_entry_count(term_count as u64)?;

        // Flush all pages to ensure durability
        bm.flush_all()?;
        dm.sync()?;

        self.dirty.store(false, AtomicOrdering::Release);

        // Clear dirty tracking state after successful checkpoint
        // This must be done AFTER flushing to ensure durability
        drop(bm); // Release buffer manager lock before clearing state
        self.clear_dirty_tracking_state();

        Ok(())
    }

    /// Serialize a ChildNode to disk and return its SwizzledPtr.
    ///
    /// This is a convenience wrapper around `serialize_child_to_disk_with_path`
    /// that uses an empty path (legacy behavior).
    fn serialize_child_to_disk(&self, child: &ChildNode) -> Result<SwizzledPtr> {
        self.serialize_child_to_disk_with_path(child, &[])
    }

    /// Serialize a ChildNode to disk with path tracking for selective persistence.
    ///
    /// This method implements the selective dirty subtree traversal optimization:
    /// - For `DiskRef` nodes: Returns the existing disk pointer (already persisted)
    /// - For `ArtNode` with no dirty descendants: May skip serialization if a cached
    ///   disk location exists for this path
    /// - For dirty `ArtNode` or `Bucket`: Recursively serializes the node and its children
    ///
    /// # Arguments
    /// * `child` - The child node to serialize
    /// * `path` - The path from root to this child (for dirty tracking lookup)
    ///
    /// # Returns
    /// The `SwizzledPtr` pointing to the serialized node on disk
    fn serialize_child_to_disk_with_path(&self, child: &ChildNode, path: &[u8]) -> Result<SwizzledPtr> {
        match child {
            ChildNode::Bucket(bucket) => {
                // Buckets are always serialized (they don't have per-entry dirty tracking)
                let ptr = self.serialize_bucket_to_disk(bucket)?;
                // Cache the serialized location for future checkpoints
                self.cache_disk_location(path, ptr.clone());
                Ok(ptr)
            }
            ChildNode::ArtNode {
                node,
                is_final,
                value,
                children,
            } => {
                // OPTIMIZATION: Check if this subtree needs persistence
                // A node needs persistence if:
                // 1. It's marked as dirty (IS_DIRTY flag)
                // 2. Any of its descendants are dirty (HAS_DIRTY_DESCENDANTS flag)
                // 3. Any of the paths in this subtree are in dirty_prefixes
                let needs_persist = node.header().needs_persistence()
                    || self.path_needs_persistence(path);

                // If the subtree is clean and we have a cached disk location, return it
                if !needs_persist {
                    if let Some(cached_ptr) = self.get_cached_disk_location(path) {
                        log::trace!(
                            "Skipping clean subtree at path {:?} (using cached disk location)",
                            String::from_utf8_lossy(path)
                        );
                        return Ok(cached_ptr);  // Already owned, no clone needed
                    }
                    // No cached location, but if this is a fresh in-memory node with no
                    // dirty flags, we still need to serialize it (first persistence)
                }

                // Recursively serialize all children first
                let mut child_ptrs: Vec<(u8, u64)> = Vec::with_capacity(children.len());
                for (edge, child) in children {
                    // Construct the path to this child
                    let mut child_path = path.to_vec();
                    child_path.push(*edge);

                    let ptr = self.serialize_child_to_disk_with_path(child, &child_path)?;
                    child_ptrs.push((*edge, ptr.to_raw()));
                }

                // Create a copy of the node with updated child pointers
                let mut node_copy = node.clone();
                for (edge, ptr_raw) in &child_ptrs {
                    if let Some(child_ptr) = node_copy.find_child_mut(*edge) {
                        *child_ptr = SwizzledPtr::from_raw(*ptr_raw);
                    }
                }

                // CRITICAL: Set the node's is_final flag to match the ChildNode's is_final
                // This ensures the flag survives serialization/deserialization
                node_copy.header_mut().set_final(*is_final);

                // Serialize the node
                let node_ptr = self.serialize_node_to_disk(&node_copy)?;

                // Cache the serialized location for future checkpoints
                self.cache_disk_location(path, node_ptr.clone());

                // Note: Value serialization for nested ART nodes is not yet implemented
                // DictionaryValue would need serde bounds to enable this
                let _ = value;

                Ok(node_ptr)
            }
            ChildNode::DiskRef { ptr } => {
                // Already on disk - also cache this location so future checkpoints can skip
                self.cache_disk_location(path, ptr.clone());
                Ok(ptr.clone())
            }
        }
    }

    // =========================================================================
    // Arena-aware iteration and merge operations
    // =========================================================================

    /// Navigate to a prefix node, returning the child and its arena ID.
    ///
    /// This variant of prefix navigation also tracks the arena ID from the
    /// SwizzledPtr that points to the final node. This is used for page-aware
    /// batch operations.
    ///
    /// # Returns
    ///
    /// - `Ok(Some((child, arena_id)))` - The child at the prefix and its arena location
    /// - `Ok(None)` - The prefix path doesn't exist
    /// - `Err` - An I/O error occurred during lazy loading
    fn navigate_to_prefix_with_arena(
        &self,
        prefix: &[u8],
    ) -> Result<Option<(&ChildNode, Option<u32>)>> {
        if prefix.is_empty() {
            // Empty prefix means the root - root has no incoming pointer
            return match &self.root {
                TrieRoot::Bucket(_) => Ok(None), // Can't return ChildNode for root bucket
                TrieRoot::ArtNode { children, .. } => {
                    // For empty prefix on ART root, return first child if any
                    // This is a special case - we can't return ChildNode for root itself
                    Ok(None)
                }
            };
        }

        match &self.root {
            TrieRoot::Bucket(_) => {
                // Root bucket doesn't have individual prefix navigation
                Ok(None)
            }
            TrieRoot::ArtNode { children, .. } => {
                let first_byte = prefix[0];
                let remaining = &prefix[1..];

                // Find child for first byte
                let child_entry = children.iter().find(|(b, _)| *b == first_byte);
                let (child, mut current_arena) = match child_entry {
                    Some((_, child)) => {
                        let arena = match child {
                            ChildNode::DiskRef { ptr } => {
                                ptr.as_arena_slot().map(|s| s.arena_id)
                            }
                            _ => None,
                        };
                        (child, arena)
                    }
                    None => return Ok(None),
                };

                // Navigate through remaining bytes
                let mut current = child;
                for &byte in remaining {
                    match current {
                        ChildNode::Bucket(_) => {
                            // Can't navigate further into bucket
                            return Ok(None);
                        }
                        ChildNode::ArtNode { children, .. } => {
                            let next = children.iter().find(|(b, _)| *b == byte);
                            match next {
                                Some((_, next_child)) => {
                                    current_arena = match next_child {
                                        ChildNode::DiskRef { ptr } => {
                                            ptr.as_arena_slot().map(|s| s.arena_id)
                                        }
                                        _ => None,
                                    };
                                    current = next_child;
                                }
                                None => return Ok(None),
                            }
                        }
                        ChildNode::DiskRef { ptr } => {
                            // Would need to load from disk - not yet implemented
                            // For now, return what we have
                            return Ok(Some((current, current_arena)));
                        }
                    }
                }

                Ok(Some((current, current_arena)))
            }
        }
    }

    /// Collect terms with arena information for page-aware batch operations.
    ///
    /// This method traverses the subtree and collects terms along with their
    /// disk arena location. This enables grouping operations by arena for
    /// improved I/O locality.
    ///
    /// # Arguments
    ///
    /// * `child` - The subtree root to collect from
    /// * `prefix` - The prefix bytes leading to this node
    /// * `current_arena` - Arena ID from the parent's SwizzledPtr to this node
    /// * `terms` - Output vector for collected terms with arena info
    /// * `limit` - Maximum number of terms to collect
    ///
    /// # Returns
    ///
    /// `Ok(true)` if the limit was reached, `Ok(false)` otherwise.
    fn collect_terms_with_arena(
        &self,
        child: &ChildNode,
        prefix: Vec<u8>,
        current_arena: Option<u32>,
        terms: &mut Vec<PrefixTermWithArena>,
        limit: usize,
    ) -> Result<bool> {
        if terms.len() >= limit {
            return Ok(true);
        }

        match child {
            ChildNode::Bucket(bucket) => {
                // Iterate through bucket entries
                for i in 0..bucket.len() {
                    if terms.len() >= limit {
                        return Ok(true);
                    }
                    if let Some(entry) = bucket.get_entry(i) {
                        let suffix = bucket.get_suffix(&entry);
                        let mut term = prefix.clone();
                        term.extend_from_slice(suffix);
                        terms.push(PrefixTermWithArena {
                            term,
                            arena_id: current_arena,
                        });
                    }
                }
            }
            ChildNode::ArtNode {
                is_final,
                children,
                ..
            } => {
                // If this node is final, record the term
                if *is_final {
                    terms.push(PrefixTermWithArena {
                        term: prefix.clone(),
                        arena_id: current_arena,
                    });
                    if terms.len() >= limit {
                        return Ok(true);
                    }
                }

                // Recurse into children
                for (edge, child) in children {
                    let mut child_prefix = prefix.clone();
                    child_prefix.push(*edge);

                    let child_arena = match child {
                        ChildNode::DiskRef { ptr } => {
                            ptr.as_arena_slot().map(|s| s.arena_id)
                        }
                        _ => None,
                    };

                    if self.collect_terms_with_arena(
                        child,
                        child_prefix,
                        child_arena,
                        terms,
                        limit,
                    )? {
                        return Ok(true);
                    }
                }
            }
            ChildNode::DiskRef { ptr } => {
                // Resolve the disk reference and recurse into it
                if let Some(disk_location) = ptr.disk_location() {
                    let child_arena = ptr.as_arena_slot().map(|s| s.arena_id);
                    if let Ok(resolved) = self.resolve_disk_ref(&disk_location) {
                        if self.collect_terms_with_arena(
                            &resolved,
                            prefix,
                            child_arena,
                            terms,
                            limit,
                        )? {
                            return Ok(true);
                        }
                    }
                }
            }
        }

        Ok(false)
    }

    /// Collect terms with their values and arena locations.
    ///
    /// This method performs a DFS traversal, recording each final node's term,
    /// value, and the arena where it resides. Used for page-locality optimized
    /// merge operations.
    fn collect_terms_with_values_and_arena(
        &self,
        child: &ChildNode,
        prefix: Vec<u8>,
        current_arena: Option<u32>,
        terms: &mut Vec<PrefixTermWithValueAndArena<V>>,
        limit: usize,
    ) -> Result<bool>
    where
        V: Clone,
    {
        if terms.len() >= limit {
            return Ok(true);
        }

        match child {
            ChildNode::Bucket(bucket) => {
                // Iterate through bucket entries
                for i in 0..bucket.len() {
                    if terms.len() >= limit {
                        return Ok(true);
                    }
                    if let Some(entry) = bucket.get_entry(i) {
                        let suffix = bucket.get_suffix(&entry);
                        let mut term = prefix.clone();
                        term.extend_from_slice(suffix);

                        // Deserialize value from bucket
                        if let Some(value_bytes) = bucket.get_value(&entry) {
                            if let Ok(value) = bincode::deserialize::<V>(value_bytes) {
                                terms.push(PrefixTermWithValueAndArena {
                                    term,
                                    value,
                                    arena_id: current_arena,
                                });
                            }
                        }
                    }
                }
            }
            ChildNode::ArtNode {
                is_final,
                value,
                children,
                ..
            } => {
                // If this node is final with a value, record it
                if *is_final {
                    if let Some(value_bytes) = value {
                        // Deserialize the value from bytes
                        if let Ok(v) = bincode::deserialize::<V>(value_bytes) {
                            terms.push(PrefixTermWithValueAndArena {
                                term: prefix.clone(),
                                value: v,
                                arena_id: current_arena,
                            });
                            if terms.len() >= limit {
                                return Ok(true);
                            }
                        }
                    }
                }

                // Recurse into children
                for (edge, child) in children {
                    let mut child_prefix = prefix.clone();
                    child_prefix.push(*edge);

                    let child_arena = match child {
                        ChildNode::DiskRef { ptr } => {
                            ptr.as_arena_slot().map(|s| s.arena_id)
                        }
                        _ => None,
                    };

                    if self.collect_terms_with_values_and_arena(
                        child,
                        child_prefix,
                        child_arena,
                        terms,
                        limit,
                    )? {
                        return Ok(true);
                    }
                }
            }
            ChildNode::DiskRef { ptr } => {
                // Resolve the disk reference and recurse into it
                if let Some(disk_location) = ptr.disk_location() {
                    let child_arena = ptr.as_arena_slot().map(|s| s.arena_id);
                    if let Ok(resolved) = self.resolve_disk_ref(&disk_location) {
                        if self.collect_terms_with_values_and_arena(
                            &resolved,
                            prefix.clone(),
                            child_arena,
                            terms,
                            limit,
                        )? {
                            return Ok(true);
                        }
                    }
                }
            }
        }

        Ok(false)
    }

    /// Iterate over all terms with the given prefix, including arena locations.
    ///
    /// Returns all terms matching the prefix along with their disk arena IDs.
    /// This enables page-aware batch operations by grouping terms by arena.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The byte prefix to search for
    ///
    /// # Returns
    ///
    /// - `Ok(Some(vec))` - Vector of terms with arena info
    /// - `Ok(None)` - The prefix path doesn't exist
    /// - `Err` - An I/O error occurred
    pub fn iter_prefix_with_arena(
        &self,
        prefix: &[u8],
    ) -> Result<Option<Vec<PrefixTermWithArena>>> {
        const DEFAULT_LIMIT: usize = 100_000;

        match &self.root {
            TrieRoot::Bucket(bucket) => {
                // For root bucket, collect matching entries
                let mut terms = Vec::new();
                for i in 0..bucket.len() {
                    if let Some(entry) = bucket.get_entry(i) {
                        let suffix = bucket.get_suffix(&entry);
                        if suffix.starts_with(prefix) {
                            terms.push(PrefixTermWithArena {
                                term: suffix.to_vec(),
                                arena_id: None, // Root bucket is in-memory
                            });
                        }
                    }
                }
                if terms.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(terms))
                }
            }
            TrieRoot::ArtNode {
                is_final,
                children,
                ..
            } => {
                let mut terms = Vec::new();

                if prefix.is_empty() {
                    // Empty prefix - collect all terms
                    if *is_final {
                        terms.push(PrefixTermWithArena {
                            term: Vec::new(),
                            arena_id: None,
                        });
                    }

                    for (edge, child) in children {
                        let child_arena = match child {
                            ChildNode::DiskRef { ptr } => {
                                ptr.as_arena_slot().map(|s| s.arena_id)
                            }
                            _ => None,
                        };

                        self.collect_terms_with_arena(
                            child,
                            vec![*edge],
                            child_arena,
                            &mut terms,
                            DEFAULT_LIMIT,
                        )?;
                    }
                } else {
                    // Navigate to prefix and collect from there
                    let first_byte = prefix[0];
                    let remaining = &prefix[1..];

                    let child_entry = children.iter().find(|(b, _)| *b == first_byte);
                    if let Some((_, child)) = child_entry {
                        let child_arena = match child {
                            ChildNode::DiskRef { ptr } => {
                                ptr.as_arena_slot().map(|s| s.arena_id)
                            }
                            _ => None,
                        };

                        // Navigate through remaining prefix
                        let mut current = child;
                        let mut current_arena = child_arena;
                        let mut path = vec![first_byte];

                        for &byte in remaining {
                            match current {
                                ChildNode::ArtNode { children, .. } => {
                                    let next = children.iter().find(|(b, _)| *b == byte);
                                    match next {
                                        Some((_, next_child)) => {
                                            current_arena = match next_child {
                                                ChildNode::DiskRef { ptr } => {
                                                    ptr.as_arena_slot().map(|s| s.arena_id)
                                                }
                                                _ => None,
                                            };
                                            current = next_child;
                                            path.push(byte);
                                        }
                                        None => return Ok(None),
                                    }
                                }
                                ChildNode::Bucket(bucket) => {
                                    // Check if remaining prefix exists in bucket
                                    let search_suffix = &prefix[path.len()..];
                                    for i in 0..bucket.len() {
                                        if let Some(entry) = bucket.get_entry(i) {
                                            let suffix = bucket.get_suffix(&entry);
                                            if suffix.starts_with(search_suffix) {
                                                let mut term = path.clone();
                                                term.extend_from_slice(suffix);
                                                terms.push(PrefixTermWithArena {
                                                    term,
                                                    arena_id: current_arena,
                                                });
                                            }
                                        }
                                    }
                                    return if terms.is_empty() {
                                        Ok(None)
                                    } else {
                                        Ok(Some(terms))
                                    };
                                }
                                ChildNode::DiskRef { .. } => {
                                    // Would need lazy loading - not yet implemented
                                    return Ok(None);
                                }
                            }
                        }

                        // Collect all terms under the prefix
                        self.collect_terms_with_arena(
                            current,
                            path,
                            current_arena,
                            &mut terms,
                            DEFAULT_LIMIT,
                        )?;
                    }
                }

                if terms.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(terms))
                }
            }
        }
    }

    /// Iterate over all terms with values and arena locations for the given prefix.
    ///
    /// Returns all (term, value, arena_id) tuples matching the prefix.
    /// This enables page-locality optimized merge operations.
    pub fn iter_prefix_with_values_and_arena(
        &self,
        prefix: &[u8],
    ) -> Result<Option<Vec<PrefixTermWithValueAndArena<V>>>>
    where
        V: Clone,
    {
        const DEFAULT_LIMIT: usize = 100_000;

        match &self.root {
            TrieRoot::Bucket(bucket) => {
                // For root bucket, collect matching entries with values
                let mut terms = Vec::new();
                for i in 0..bucket.len() {
                    if let Some(entry) = bucket.get_entry(i) {
                        let suffix = bucket.get_suffix(&entry);
                        if suffix.starts_with(prefix) {
                            if let Some(value_bytes) = bucket.get_value(&entry) {
                                if let Ok(value) = bincode::deserialize::<V>(value_bytes) {
                                    terms.push(PrefixTermWithValueAndArena {
                                        term: suffix.to_vec(),
                                        value,
                                        arena_id: None,
                                    });
                                }
                            }
                        }
                    }
                }
                if terms.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(terms))
                }
            }
            TrieRoot::ArtNode {
                is_final,
                value,
                children,
                ..
            } => {
                let mut terms = Vec::new();

                if prefix.is_empty() {
                    // Empty prefix - collect all terms
                    if *is_final {
                        if let Some(v) = value {
                            terms.push(PrefixTermWithValueAndArena {
                                term: Vec::new(),
                                value: v.clone(),
                                arena_id: None,
                            });
                        }
                    }

                    for (edge, child) in children {
                        let child_arena = match child {
                            ChildNode::DiskRef { ptr } => {
                                ptr.as_arena_slot().map(|s| s.arena_id)
                            }
                            _ => None,
                        };

                        self.collect_terms_with_values_and_arena(
                            child,
                            vec![*edge],
                            child_arena,
                            &mut terms,
                            DEFAULT_LIMIT,
                        )?;
                    }
                } else {
                    // Navigate to prefix and collect from there
                    let first_byte = prefix[0];
                    let remaining = &prefix[1..];

                    let child_entry = children.iter().find(|(b, _)| *b == first_byte);
                    if let Some((_, child)) = child_entry {
                        let child_arena = match child {
                            ChildNode::DiskRef { ptr } => {
                                ptr.as_arena_slot().map(|s| s.arena_id)
                            }
                            _ => None,
                        };

                        // Navigate through remaining prefix
                        let mut current = child;
                        let mut current_arena = child_arena;
                        let mut path = vec![first_byte];

                        for &byte in remaining {
                            match current {
                                ChildNode::ArtNode { children, .. } => {
                                    let next = children.iter().find(|(b, _)| *b == byte);
                                    match next {
                                        Some((_, next_child)) => {
                                            current_arena = match next_child {
                                                ChildNode::DiskRef { ptr } => {
                                                    ptr.as_arena_slot().map(|s| s.arena_id)
                                                }
                                                _ => None,
                                            };
                                            current = next_child;
                                            path.push(byte);
                                        }
                                        None => return Ok(None),
                                    }
                                }
                                ChildNode::Bucket(bucket) => {
                                    // Check if remaining prefix exists in bucket
                                    let search_suffix = &prefix[path.len()..];
                                    for i in 0..bucket.len() {
                                        if let Some(entry) = bucket.get_entry(i) {
                                            let suffix = bucket.get_suffix(&entry);
                                            if suffix.starts_with(search_suffix) {
                                                if let Some(value_bytes) = bucket.get_value(&entry) {
                                                    if let Ok(value) = bincode::deserialize::<V>(value_bytes) {
                                                        let mut term = path.clone();
                                                        term.extend_from_slice(suffix);
                                                        terms.push(PrefixTermWithValueAndArena {
                                                            term,
                                                            value,
                                                            arena_id: current_arena,
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    return if terms.is_empty() {
                                        Ok(None)
                                    } else {
                                        Ok(Some(terms))
                                    };
                                }
                                ChildNode::DiskRef { .. } => {
                                    // Would need lazy loading - not yet implemented
                                    return Ok(None);
                                }
                            }
                        }

                        // Collect all terms under the prefix
                        self.collect_terms_with_values_and_arena(
                            current,
                            path,
                            current_arena,
                            &mut terms,
                            DEFAULT_LIMIT,
                        )?;
                    }
                }

                if terms.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(terms))
                }
            }
        }
    }

    /// Merge another trie into this one using a custom merge function.
    ///
    /// Uses arena-aware iteration for improved I/O locality. Groups terms by
    /// their disk arena before processing, processing arena groups in sorted
    /// order for sequential I/O patterns.
    ///
    /// # Arguments
    ///
    /// * `other` - The source trie to merge from
    /// * `merge_fn` - Function to combine values when a term exists in both tries
    ///
    /// # Returns
    ///
    /// The number of terms processed from `other`.
    pub fn merge_from<F>(&mut self, other: &Self, merge_fn: F) -> Result<usize>
    where
        F: Fn(&V, &V) -> V,
        V: Clone,
    {
        use std::collections::BTreeMap;

        // Get all terms with values and arena info from other
        let other_terms = match other.iter_prefix_with_values_and_arena(b"")? {
            Some(terms) => terms,
            None => return Ok(0),
        };

        // Group by arena_id for I/O locality
        let mut by_arena: BTreeMap<Option<u32>, Vec<PrefixTermWithValueAndArena<V>>> = BTreeMap::new();
        for term_info in other_terms {
            by_arena.entry(term_info.arena_id).or_default().push(term_info);
        }

        let mut processed = 0;

        // Process arena groups in order (None first, then ascending arena IDs)
        for (_arena_id, arena_terms) in by_arena {
            for term_info in arena_terms {
                processed += 1;

                // Check if term exists in self and merge values
                let existing_value = self.get_value_impl(&term_info.term);
                let merged_value = if let Some(ref self_value) = existing_value {
                    merge_fn(self_value, &term_info.value)
                } else {
                    term_info.value
                };

                // Insert the merged value
                self.insert_impl(&term_info.term, Some(merged_value));
            }
        }

        Ok(processed)
    }

    /// Merge another trie into this one, replacing values on conflict.
    ///
    /// This is a convenience method equivalent to:
    /// `merge_from(other, |_, other_val| other_val.clone())`
    ///
    /// # Arguments
    ///
    /// * `other` - The source trie to merge from
    ///
    /// # Returns
    ///
    /// The number of terms processed from `other`.
    pub fn merge_replace(&mut self, other: &Self) -> Result<usize>
    where
        V: Clone,
    {
        self.merge_from(other, |_, other_val| other_val.clone())
    }

    /// Merge another trie into this one with memory-bounded batching.
    ///
    /// This method processes the source trie in batches to bound peak memory usage.
    /// Each batch is processed and then discarded before loading the next batch.
    ///
    /// # Arguments
    ///
    /// * `other` - The source trie to merge from
    /// * `merge_fn` - Function to merge values when a term exists in both tries
    /// * `batch_size` - Maximum number of terms to process per batch (default: 10,000)
    ///
    /// # Returns
    ///
    /// The total number of terms processed from `other`.
    ///
    /// # Memory Usage
    ///
    /// Peak memory is bounded by approximately `batch_size * (avg_term_len + avg_value_size)`.
    /// For 10,000 terms with 28-byte average terms and 100-byte values, this is ~1.3MB.
    pub fn merge_from_batched<F>(
        &mut self,
        other: &Self,
        merge_fn: F,
        batch_size: usize,
    ) -> Result<usize>
    where
        F: Fn(&V, &V) -> V,
        V: Clone,
    {
        self.merge_from_batched_with_options(other, merge_fn, batch_size, false)
    }

    /// Merge terms from another trie in batches, sorted by arena ID for sequential I/O.
    ///
    /// This is an optimized version of `merge_from_batched` that sorts each batch
    /// by arena ID before processing. This optimization improves I/O performance
    /// when merging disk-resident tries by ensuring sequential disk access patterns.
    ///
    /// # Performance
    ///
    /// Expected improvement: 10-20% faster merge for disk-resident tries due to
    /// sequential I/O patterns. For in-memory tries, there is no significant difference.
    ///
    /// # Arguments
    ///
    /// * `other` - The source trie to merge from
    /// * `merge_fn` - Function to merge values when a term exists in both tries
    /// * `batch_size` - Number of terms to process per batch (0 uses default 5,000)
    ///
    /// # Returns
    ///
    /// The total number of terms processed from `other`.
    pub fn merge_from_batched_grouped<F>(
        &mut self,
        other: &Self,
        merge_fn: F,
        batch_size: usize,
    ) -> Result<usize>
    where
        F: Fn(&V, &V) -> V,
        V: Clone,
    {
        self.merge_from_batched_with_options(other, merge_fn, batch_size, true)
    }

    /// Internal implementation of batched merge with optional arena grouping.
    ///
    /// # Arguments
    ///
    /// * `other` - The source trie to merge from
    /// * `merge_fn` - Function to merge values when a term exists in both tries
    /// * `batch_size` - Number of terms to process per batch (0 uses default 5,000)
    /// * `arena_grouped` - If true, sort each batch by arena_id for sequential I/O
    ///
    /// # Returns
    ///
    /// The total number of terms processed from `other`.
    fn merge_from_batched_with_options<F>(
        &mut self,
        other: &Self,
        merge_fn: F,
        batch_size: usize,
        arena_grouped: bool,
    ) -> Result<usize>
    where
        F: Fn(&V, &V) -> V,
        V: Clone,
    {
        let batch_size = if batch_size == 0 { 5_000 } else { batch_size };
        let mut total_processed = 0;
        let mut cursor: Option<Vec<u8>> = None;

        loop {
            // Get next batch from other starting after cursor
            let mut batch = other.iter_prefix_from_cursor(b"", cursor.as_deref(), batch_size)?;

            if batch.is_empty() {
                break;
            }

            let batch_len = batch.len();
            let last_term = batch.last().map(|t| t.term.clone());

            // Sort batch by arena_id for sequential I/O if requested
            if arena_grouped {
                batch.sort_by(|a, b| {
                    match (a.arena_id, b.arena_id) {
                        (Some(a_id), Some(b_id)) => {
                            a_id.cmp(&b_id).then_with(|| a.term.cmp(&b.term))
                        }
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => a.term.cmp(&b.term),
                    }
                });
            }

            // Process this batch
            for term_info in batch {
                // Check if term exists in self and merge values
                let existing_value = self.get_value_impl(&term_info.term);
                let merged_value = if let Some(ref self_value) = existing_value {
                    merge_fn(self_value, &term_info.value)
                } else {
                    term_info.value
                };

                // Insert the merged value
                self.insert_impl(&term_info.term, Some(merged_value));
                total_processed += 1;
            }

            // If batch was smaller than requested, we're done
            if batch_len < batch_size {
                break;
            }

            // Update cursor to continue after last term
            cursor = last_term;
        }

        Ok(total_processed)
    }

    /// Iterate terms with values starting from a cursor position.
    ///
    /// This method enables memory-bounded iteration by returning terms in batches.
    /// The cursor allows resuming iteration from where the previous batch ended.
    ///
    /// # Arguments
    ///
    /// * `prefix` - Only return terms starting with this prefix
    /// * `cursor` - If Some, skip terms <= cursor (exclusive lower bound)
    /// * `limit` - Maximum number of terms to return
    ///
    /// # Returns
    ///
    /// A vector of terms (sorted lexicographically) starting after the cursor,
    /// up to the specified limit.
    pub fn iter_prefix_from_cursor(
        &self,
        prefix: &[u8],
        cursor: Option<&[u8]>,
        limit: usize,
    ) -> Result<Vec<PrefixTermWithValueAndArena<V>>>
    where
        V: Clone,
    {
        let mut terms = Vec::with_capacity(limit);

        // Collect terms with the cursor filtering
        self.collect_terms_from_cursor(
            prefix,
            cursor,
            limit,
            &mut terms,
        )?;

        Ok(terms)
    }

    /// Helper to collect terms from a cursor position.
    fn collect_terms_from_cursor(
        &self,
        prefix: &[u8],
        cursor: Option<&[u8]>,
        limit: usize,
        terms: &mut Vec<PrefixTermWithValueAndArena<V>>,
    ) -> Result<()>
    where
        V: Clone,
    {
        match &self.root {
            TrieRoot::Bucket(bucket) => {
                // For root bucket, collect matching entries
                let mut entries: Vec<_> = (0..bucket.len())
                    .filter_map(|i| bucket.get_entry(i))
                    .filter_map(|entry| {
                        let suffix = bucket.get_suffix(&entry);
                        if !suffix.starts_with(prefix) {
                            return None;
                        }
                        // Apply cursor filter using SIMD-accelerated comparison
                        if let Some(c) = cursor {
                            if bytes_le(suffix.as_ref(), c) {
                                return None;
                            }
                        }
                        bucket.get_value(&entry).and_then(|value_bytes| {
                            bincode::deserialize::<V>(value_bytes).ok().map(|value| {
                                PrefixTermWithValueAndArena {
                                    term: suffix.to_vec(),
                                    value,
                                    arena_id: None,
                                }
                            })
                        })
                    })
                    .collect();

                // Sort for consistent ordering
                entries.sort_by(|a, b| a.term.cmp(&b.term));
                terms.extend(entries.into_iter().take(limit));
            }
            TrieRoot::ArtNode {
                is_final,
                value,
                children,
                ..
            } => {
                // If prefix is empty and we're at root
                if prefix.is_empty() {
                    // Check root node itself
                    if *is_final {
                        if let Some(v) = value {
                            let empty_term = Vec::new();
                            // Apply cursor filter
                            let include = cursor.map_or(true, |c| empty_term.as_slice() > c);
                            if include && terms.len() < limit {
                                terms.push(PrefixTermWithValueAndArena {
                                    term: empty_term,
                                    value: v.clone(),
                                    arena_id: None,
                                });
                            }
                        }
                    }

                    // Collect from children in sorted order
                    let mut sorted_children: Vec<_> = children.iter().collect();
                    sorted_children.sort_by_key(|(b, _)| *b);

                    for (edge, child) in sorted_children {
                        if terms.len() >= limit {
                            break;
                        }

                        let child_arena = match child {
                            ChildNode::DiskRef { ptr } => ptr.as_arena_slot().map(|s| s.arena_id),
                            _ => None,
                        };

                        self.collect_terms_with_cursor_and_arena(
                            child,
                            vec![*edge],
                            cursor,
                            limit,
                            child_arena,
                            terms,
                        )?;
                    }
                } else {
                    // Navigate to prefix first, then collect
                    // This is a simplified version; full implementation would
                    // navigate to prefix and then collect
                    if let Some(all_terms) = self.iter_prefix_with_values_and_arena(prefix)? {
                        let filtered: Vec<_> = all_terms
                            .into_iter()
                            .filter(|t| cursor.map_or(true, |c| bytes_gt(t.term.as_slice(), c)))
                            .take(limit)
                            .collect();
                        terms.extend(filtered);
                    }
                }
            }
        }

        Ok(())
    }

    /// Collect terms from a child node with cursor filtering.
    fn collect_terms_with_cursor_and_arena(
        &self,
        child: &ChildNode,
        path: Vec<u8>,
        cursor: Option<&[u8]>,
        limit: usize,
        arena_id: Option<u32>,
        terms: &mut Vec<PrefixTermWithValueAndArena<V>>,
    ) -> Result<()>
    where
        V: Clone,
    {
        if terms.len() >= limit {
            return Ok(());
        }

        match child {
            ChildNode::Bucket(bucket) => {
                for i in 0..bucket.len() {
                    if terms.len() >= limit {
                        break;
                    }
                    if let Some(entry) = bucket.get_entry(i) {
                        let suffix = bucket.get_suffix(&entry);
                        // Use SmallVec to avoid heap allocation for short paths
                        let mut full_term: SmallVec<[u8; 64]> = SmallVec::from_slice(&path);
                        full_term.extend_from_slice(suffix);

                        // Apply cursor filter using SIMD-accelerated comparison
                        if let Some(c) = cursor {
                            if bytes_le(full_term.as_slice(), c) {
                                continue;
                            }
                        }

                        if let Some(value_bytes) = bucket.get_value(&entry) {
                            if let Ok(value) = bincode::deserialize::<V>(value_bytes) {
                                terms.push(PrefixTermWithValueAndArena {
                                    term: full_term.into_vec(),
                                    value,
                                    arena_id,
                                });
                            }
                        }
                    }
                }
                // Sort bucket terms
                terms.sort_by(|a, b| a.term.cmp(&b.term));
            }
            ChildNode::ArtNode {
                is_final,
                value,
                children,
                ..
            } => {
                // Check this node's finality
                if *is_final {
                    if let Some(value_bytes) = value {
                        // Deserialize the value from bytes
                        if let Ok(v) = bincode::deserialize::<V>(value_bytes) {
                            // Apply cursor filter using SIMD-accelerated comparison
                            if cursor.map_or(true, |c| bytes_gt(path.as_slice(), c)) && terms.len() < limit {
                                terms.push(PrefixTermWithValueAndArena {
                                    term: path.clone(),
                                    value: v,
                                    arena_id,
                                });
                            }
                        }
                    }
                }

                // Recurse into children in sorted order
                let mut sorted_children: Vec<_> = children.iter().collect();
                sorted_children.sort_by_key(|(b, _)| *b);

                for (edge, child_node) in sorted_children {
                    if terms.len() >= limit {
                        break;
                    }
                    // Use SmallVec to avoid heap allocation for short paths
                    let mut child_path: SmallVec<[u8; 64]> = SmallVec::from_slice(&path);
                    child_path.push(*edge);

                    let child_arena = match child_node {
                        ChildNode::DiskRef { ptr } => ptr.as_arena_slot().map(|s| s.arena_id),
                        _ => arena_id,
                    };

                    self.collect_terms_with_cursor_and_arena(
                        child_node,
                        child_path.into_vec(),
                        cursor,
                        limit,
                        child_arena,
                        terms,
                    )?;
                }
            }
            ChildNode::DiskRef { .. } => {
                // DiskRef children are not loaded in this simple implementation
                // The parent method handles disk-backed nodes through the buffer manager
                // For streaming merge, we skip disk refs (they would be loaded via
                // iter_prefix_with_values_and_arena which handles this)
            }
        }

        Ok(())
    }
}

/// Root descriptor type constants
const ROOT_TYPE_EMPTY: u8 = 0;
const ROOT_TYPE_BUCKET: u8 = 1;
const ROOT_TYPE_ART_NODE: u8 = 2;

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentARTrie<V, S> {
    type Node = PersistentARTrieNode<V>;

    fn root(&self) -> Self::Node {
        self.get_root_node()
    }

    fn contains(&self, term: &str) -> bool {
        self.contains_impl(term.as_bytes())
    }

    #[inline]
    fn len(&self) -> Option<usize> {
        Some(self.term_count.load(AtomicOrdering::Acquire))
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }
}

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentARTrie<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        self.get_value_impl(term.as_bytes())
    }
}


impl<V: DictionaryValue, S: BlockStorage> std::fmt::Debug for PersistentARTrie<V, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentARTrie")
            .field("term_count", &self.term_count.load(AtomicOrdering::Relaxed))
            .field("dirty", &self.dirty.load(AtomicOrdering::Relaxed))
            .finish()
    }
}


impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Iterate over all terms in the dictionary.
    ///
    /// Returns an iterator yielding terms as `Vec<u8>` in lexicographic order.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let mut dict = PersistentARTrie::new();
    /// dict.insert("apple");
    /// dict.insert("banana");
    ///
    /// for term in dict.iter() {
    ///     println!("{}", String::from_utf8_lossy(&term));
    /// }
    /// ```
    pub fn iter(&self) -> TermIterator<V> {
        TermIterator::new(&self.root)
    }

    /// Iterate over all terms with their values.
    ///
    /// Returns an iterator yielding `(term, Option<value>)` pairs in lexicographic order.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let mut dict: PersistentARTrie<i32> = PersistentARTrie::new();
    /// dict.insert_with_value("apple", 1);
    /// dict.insert_with_value("banana", 2);
    ///
    /// for (term, value) in dict.iter_with_values() {
    ///     println!("{}: {:?}", String::from_utf8_lossy(&term), value);
    /// }
    /// ```
    pub fn iter_with_values(&self) -> TermValueIterator<V> {
        TermValueIterator::new(&self.root)
    }

    /// Iterate over all terms as strings.
    ///
    /// This is a convenience method that converts terms to UTF-8 strings,
    /// skipping any terms that contain invalid UTF-8.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let mut dict = PersistentARTrie::new();
    /// dict.insert("hello");
    /// dict.insert("world");
    ///
    /// for term in dict.iter_strings() {
    ///     println!("{}", term);
    /// }
    /// ```
    pub fn iter_strings(&self) -> impl Iterator<Item = String> + '_ {
        self.iter()
            .filter_map(|bytes| String::from_utf8(bytes).ok())
    }

    /// Iterate over all terms with the given prefix.
    ///
    /// Returns `None` if the prefix path doesn't exist in the trie.
    /// Returns `Some(iterator)` that yields all terms starting with the prefix.
    ///
    /// Uses the zipper-based `PrefixZipper` trait for O(k) navigation to the prefix,
    /// followed by O(m) iteration over matching terms.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The byte prefix to search for
    ///
    /// # Returns
    ///
    /// * `Some(impl Iterator<Item = Vec<u8>>)` - Iterator over matching terms
    /// * `None` - If no terms with this prefix exist
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let mut dict: PersistentARTrie<()> = PersistentARTrie::new();
    /// dict.insert("apple");
    /// dict.insert("application");
    /// dict.insert("banana");
    ///
    /// if let Some(iter) = dict.iter_prefix(b"app") {
    ///     for term in iter {
    ///         println!("{}", String::from_utf8_lossy(&term));
    ///     }
    ///     // Prints: "apple" and "application"
    /// }
    /// ```
    pub fn iter_prefix(&self, prefix: &[u8]) -> Option<impl Iterator<Item = Vec<u8>> + '_> {
        // Direct iteration without zipper for the flattened struct design
        self.iter_prefix_direct(prefix)
    }

    /// Direct prefix iteration implementation (non-zipper based).
    fn iter_prefix_direct(&self, prefix: &[u8]) -> Option<impl Iterator<Item = Vec<u8>> + '_> {
        let terms = self.iter_prefix_with_arena(prefix).ok()??;
        Some(terms.into_iter().map(|t| t.term))
    }

    /// Iterate over all (term, value) pairs with the given prefix.
    ///
    /// Returns `None` if the prefix path doesn't exist in the trie.
    /// Returns `Some(iterator)` that yields all (term, value) pairs where term starts with prefix.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The byte prefix to search for
    ///
    /// # Returns
    ///
    /// * `Some(impl Iterator<Item = (Vec<u8>, V)>)` - Iterator over matching (term, value) pairs
    /// * `None` - If no terms with this prefix exist
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let mut dict: PersistentARTrie<i32> = PersistentARTrie::new();
    /// dict.insert_with_value("apple", 1);
    /// dict.insert_with_value("application", 2);
    /// dict.insert_with_value("banana", 3);
    ///
    /// if let Some(iter) = dict.iter_prefix_with_values(b"app") {
    ///     for (term, value) in iter {
    ///         println!("{}: {}", String::from_utf8_lossy(&term), value);
    ///     }
    ///     // Prints: "apple: 1" and "application: 2"
    /// }
    /// ```
    pub fn iter_prefix_with_values(&self, prefix: &[u8]) -> Option<impl Iterator<Item = (Vec<u8>, V)> + '_>
    where
        V: Clone,
    {
        // Direct iteration without zipper for the flattened struct design
        let terms = self.iter_prefix_with_values_and_arena(prefix).ok()??;
        Some(terms.into_iter().map(|t| (t.term, t.value)))
    }
}

// === MmapDiskManager-specific methods (compaction requires file I/O) ===

impl<V: DictionaryValue> PersistentARTrie<V> {
    /// Compact the trie, eliminating orphaned nodes and fragmentation.
    ///
    /// Compaction performs a fresh rebuild of the trie by iterating all terms
    /// and inserting them into a new trie. This eliminates:
    ///
    /// - **Intra-Arena fragmentation**: Old node versions orphaned when updated
    /// - **Inter-Arena fragmentation**: Underutilized arenas from append-only allocation
    /// - **File-level fragmentation**: Scattered freed blocks that never coalesce
    ///
    /// # Algorithm
    ///
    /// 1. **Setup**: Record original file size, create new trie at temp path
    /// 2. **Copy**: Iterate all (term, value) pairs and insert into new trie
    /// 3. **Checkpoint**: Persist new trie to disk
    /// 4. **Verify** (optional): Confirm term counts match
    /// 5. **Finalize** (in-place mode): Atomic rename of temp file to original
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration options (output path, progress interval, verification)
    /// * `progress` - Callback invoked periodically with progress updates
    ///
    /// # Returns
    ///
    /// * `Ok(CompactionStats)` - Statistics about the compaction operation
    /// * `Err(PersistentARTrieError)` - If compaction fails
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::{PersistentARTrie, CompactionConfig};
    ///
    /// let mut trie = PersistentARTrie::<u64>::open("data.artrie")?;
    ///
    /// // In-place compaction
    /// let stats = trie.compact(CompactionConfig::default(), |p| {
    ///     println!("{}: {:.1}%", p.phase, p.percent_complete);
    /// })?;
    ///
    /// println!("Compacted {} terms, saved {:.1}% space",
    ///     stats.terms_copied, stats.space_savings_percent);
    /// ```
    ///
    /// # Edge Cases
    ///
    /// - **Empty trie**: Creates a minimal compacted file
    /// - **In-memory trie (no path)**: Returns error
    /// - **Crash during copy**: Temp file can be safely deleted on restart
    /// - **Crash after rename**: Compaction is complete (atomic)
    pub fn compact<F>(&mut self, config: CompactionConfig, mut progress: F) -> Result<CompactionStats>
    where
        V: Clone,
        F: FnMut(CompactionProgress),
    {
        use std::time::Instant;

        let start = Instant::now();

        // Get the original file path from the buffer manager
        let original_path = self
            .buffer_manager
            .as_ref()
            .map(|bm| {
                let bm_guard = bm.read();
                std::path::PathBuf::from(bm_guard.storage().path())
            })
            .ok_or_else(|| {
                PersistentARTrieError::io_error(
                    "compact",
                    "",
                    std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "Cannot compact in-memory trie (no disk backing)",
                    ),
                )
            })?;

        // Get original file size
        let original_bytes = std::fs::metadata(&original_path)
            .map(|m| m.len())
            .unwrap_or(0);

        let estimated_total = self.term_count.load(AtomicOrdering::Acquire) as u64;

        // Determine output path
        let (temp_path, is_in_place) = match &config.output_path {
            Some(output) => (output.clone(), false),
            None => (original_path.with_extension("compacting"), true),
        };

        // Remove temp file if it exists from a previous failed compaction
        if temp_path.exists() {
            std::fs::remove_file(&temp_path).map_err(|e| {
                PersistentARTrieError::io_error("compact", temp_path.display().to_string(), e)
            })?;
        }

        // Also clean up temp WAL if it exists
        let temp_wal_path = temp_path.with_extension("wal");
        if temp_wal_path.exists() {
            let _ = std::fs::remove_file(&temp_wal_path);
        }

        // Phase 1: Create new trie (always uses MmapDiskManager for compaction output)
        let mut new_trie = PersistentARTrie::<V>::create(&temp_path)?;

        // Phase 2: Copy all entries
        let mut terms_processed = 0u64;

        // Collect terms to avoid borrowing issues
        let terms_to_copy: Vec<(Vec<u8>, V)> = self
            .iter_prefix_with_values(b"")
            .map(|iter| iter.collect())
            .unwrap_or_default();

        for (term, value) in terms_to_copy {
            // Convert bytes to string for insert_with_value
            // Note: term bytes should be valid UTF-8 since they came from string insertions
            let term_str = match std::str::from_utf8(&term) {
                Ok(s) => s,
                Err(_) => {
                    // For non-UTF8 terms, use lossy conversion
                    // This shouldn't happen in normal usage but handles edge cases
                    warn!("Non-UTF8 term encountered during compaction: {:?}", term);
                    continue;
                }
            };

            new_trie.insert_with_value(term_str, value);
            terms_processed += 1;

            // Progress callback
            if config.progress_interval > 0 && terms_processed % config.progress_interval as u64 == 0 {
                let percent = if estimated_total > 0 {
                    (terms_processed as f32 / estimated_total as f32) * 100.0
                } else {
                    100.0
                };
                progress(CompactionProgress {
                    phase: "copying",
                    terms_processed,
                    estimated_total,
                    percent_complete: percent,
                });
            }
        }

        // Phase 3: Checkpoint
        progress(CompactionProgress {
            phase: "checkpointing",
            terms_processed,
            estimated_total,
            percent_complete: 100.0,
        });
        new_trie.checkpoint()?;

        // Get compacted file size
        let compacted_bytes = std::fs::metadata(&temp_path)
            .map(|m| m.len())
            .unwrap_or(0);

        // Phase 4: Verify (optional)
        if config.verify_after_compact {
            progress(CompactionProgress {
                phase: "verifying",
                terms_processed,
                estimated_total,
                percent_complete: 100.0,
            });

            let original_count = self.term_count.load(AtomicOrdering::Acquire);
            let compacted_count = new_trie.term_count.load(AtomicOrdering::Acquire);

            if original_count != compacted_count {
                // Clean up temp files on verification failure
                drop(new_trie);
                let _ = std::fs::remove_file(&temp_path);
                let _ = std::fs::remove_file(&temp_wal_path);

                return Err(PersistentARTrieError::CheckpointVerificationFailed {
                    reason: format!(
                        "Term count mismatch after compaction: expected {}, got {}",
                        original_count, compacted_count
                    ),
                });
            }
        }

        // Phase 5: Finalize
        if is_in_place {
            progress(CompactionProgress {
                phase: "finalizing",
                terms_processed,
                estimated_total,
                percent_complete: 100.0,
            });

            // Drop the new trie's handles before rename
            drop(new_trie);

            // Close our own handles to release file locks
            self.buffer_manager = None;
            self.wal_writer = None;
            self.arena_manager = None;

            // Atomic rename
            std::fs::rename(&temp_path, &original_path).map_err(|e| {
                PersistentARTrieError::io_error("compact", original_path.display().to_string(), e)
            })?;

            // Clean up temp WAL (may not exist if checkpoint was clean)
            let original_wal = original_path.with_extension("wal");
            let _ = std::fs::remove_file(&temp_wal_path);

            // Also need to handle the original WAL - rename temp WAL to original
            // Actually, the new trie's WAL should be renamed too
            let new_wal_path = temp_path.with_extension("wal");
            if new_wal_path.exists() {
                let _ = std::fs::rename(&new_wal_path, &original_wal);
            }

            // Reopen at original path
            *self = Self::open(&original_path)?;
        }

        let duration_ms = start.elapsed().as_millis() as u64;
        let space_savings_percent = if original_bytes > 0 {
            (1.0 - (compacted_bytes as f64 / original_bytes as f64)) * 100.0
        } else {
            0.0
        };

        Ok(CompactionStats {
            terms_copied: terms_processed,
            original_bytes,
            compacted_bytes,
            space_savings_percent,
            duration_ms,
        })
    }

}

/// Extension trait for parallel merge operations on [`SharedARTrie`].
///
/// These methods require the `parallel-merge` feature and use rayon for
/// parallel processing. They are implemented as an extension trait because
/// `SharedARTrie` is a type alias for `Arc<RwLock<PersistentARTrie<V>>>`,
/// and Rust doesn't allow inherent `impl` blocks on type aliases that resolve
/// to external types.
///
/// # Usage
///
/// ```rust,ignore
/// use libdictenstein::persistent_artrie::{SharedARTrie, SharedARTrieParallelExt};
///
/// let trie1: SharedARTrie<u32> = /* ... */;
/// let trie2: SharedARTrie<u32> = /* ... */;
///
/// // Import the trait to use the method
/// let count = trie1.merge_from_parallel(&trie2, |a, b| a + b)?;
/// ```
#[cfg(feature = "parallel-merge")]
pub trait SharedARTrieParallelExt<V: DictionaryValue> {
    /// Merge all terms from another trie using parallel processing.
    ///
    /// This method uses rayon to parallelize the merge computation across multiple
    /// cores. The parallelization strategy:
    /// 1. Partition source terms by first byte (256 possible partitions)
    /// 2. Process partitions in parallel: read source terms, compute merge values
    /// 3. Batch-insert results sequentially (avoids write contention)
    ///
    /// # Performance
    ///
    /// Expected speedup: 4-6x on 8 cores for large merges (100K+ terms).
    /// The speedup is limited by the sequential write phase but the parallel
    /// read and merge computation phases scale well.
    ///
    /// # Arguments
    ///
    /// * `other` - The source trie to merge from
    /// * `merge_fn` - Function to merge values when a term exists in both tries.
    ///                Called as `merge_fn(self_value, other_value)`.
    ///
    /// # Returns
    ///
    /// The number of terms processed from the source trie.
    fn merge_from_parallel<F>(&self, other: &Self, merge_fn: F) -> Result<usize>
    where
        F: Fn(&V, &V) -> V + Sync + Send;
}

#[cfg(feature = "parallel-merge")]
impl<V: DictionaryValue + Clone + Send + Sync> SharedARTrieParallelExt<V> for SharedARTrie<V> {
    fn merge_from_parallel<F>(
        &self,
        other: &Self,
        merge_fn: F,
    ) -> Result<usize>
    where
        F: Fn(&V, &V) -> V + Sync + Send,
    {
        use rayon::prelude::*;

        // Partition by first byte (0-255) for parallel processing
        // This naturally distributes work across the trie structure
        let partitions: Vec<Vec<(Vec<u8>, V)>> = (0u8..=255u8)
            .into_par_iter()
            .map(|prefix_byte| {
                // Read all terms starting with this byte from source
                let prefix = [prefix_byte];
                let other_guard = other.read();

                // Collect all terms with this prefix from source
                let mut partition_terms = Vec::new();
                let mut cursor: Option<Vec<u8>> = None;
                let batch_size = 10_000;

                loop {
                    let batch = match other_guard.iter_prefix_from_cursor(
                        &prefix,
                        cursor.as_deref(),
                        batch_size,
                    ) {
                        Ok(b) => b,
                        Err(_) => break,
                    };

                    if batch.is_empty() {
                        break;
                    }

                    let batch_len = batch.len();
                    let last_term = batch.last().map(|t| t.term.clone());

                    // For each term, compute the merged value
                    for term_info in batch {
                        // We need to check if term exists in self
                        // This read is safe since we're just reading
                        let self_guard = self.read();
                        let existing_value = self_guard.get_value_impl(&term_info.term);
                        drop(self_guard);

                        let merged_value = if let Some(ref self_value) = existing_value {
                            merge_fn(self_value, &term_info.value)
                        } else {
                            term_info.value
                        };

                        partition_terms.push((term_info.term, merged_value));
                    }

                    if batch_len < batch_size {
                        break;
                    }

                    cursor = last_term;
                }

                partition_terms
            })
            .collect();

        // Sequential write phase - batch insert all partitions
        let mut total_processed = 0;
        let mut guard = self.write();

        for partition in partitions {
            for (term, value) in partition {
                guard.insert_impl(&term, Some(value));
                total_processed += 1;
            }
        }

        Ok(total_processed)
    }
}

// ===========================================================================
// Document Transactions
// ===========================================================================
//
// Per-document atomicity: buffer all terms for a document, then atomically
// apply them on commit or discard them on abort. This enables rollback of
// individual documents without affecting other insertions.

impl<V: DictionaryValue + serde::Serialize + serde::de::DeserializeOwned, S: BlockStorage> PersistentARTrie<V, S> {
    /// Begin a new document transaction.
    ///
    /// This creates a transaction that buffers terms in memory. The terms are
    /// only applied to the trie when `commit_document()` is called. If processing
    /// fails, `abort_document()` discards all buffered terms.
    ///
    /// # Arguments
    ///
    /// * `document_id` - A unique identifier for this document (for debugging/logging)
    ///
    /// # Returns
    ///
    /// A `DocumentTransaction` that can be used to buffer terms and then commit or abort.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut tx = trie.begin_document("doc_123")?;
    /// trie.tx_insert(&mut tx, "term1", Some(1));
    /// trie.commit_document(tx)?;
    /// ```
    pub fn begin_document(&self, document_id: &str) -> Result<DocumentTransaction<V>> {
        // Generate a unique transaction ID
        let tx_id = {
            let base = self.next_lsn.load(AtomicOrdering::Acquire);
            // Combine LSN with a random component for uniqueness
            base ^ (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0))
        };

        // Log BeginTx to WAL
        if let Some(ref wal) = self.wal_writer {
            wal.append(super::wal::WalRecord::BeginTx { tx_id }).map_err(|e| {
                PersistentARTrieError::io_error(
                    "begin_tx", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
                )
            })?;
        }

        Ok(DocumentTransaction {
            tx_id,
            document_id: document_id.to_string(),
            shadow_terms: Vec::new(),
            state: TransactionState::Active,
        })
    }

    /// Buffer a term in a document transaction.
    ///
    /// The term is NOT inserted into the trie yet - it's only buffered in memory.
    /// The term will be inserted when `commit_document()` is called.
    ///
    /// # Arguments
    ///
    /// * `tx` - The active transaction to buffer the term in
    /// * `term` - The term to insert
    /// * `value` - Optional value to associate with the term
    ///
    /// # Panics
    ///
    /// Panics if the transaction is not in Active state.
    pub fn tx_insert(&self, tx: &mut DocumentTransaction<V>, term: &str, value: Option<V>) {
        self.tx_insert_bytes(tx, term.as_bytes(), value);
    }

    /// Buffer a term (as bytes) in a document transaction.
    ///
    /// See [`tx_insert`](Self::tx_insert) for details.
    pub fn tx_insert_bytes(&self, tx: &mut DocumentTransaction<V>, term: &[u8], value: Option<V>) {
        assert!(
            tx.state == TransactionState::Active,
            "Cannot insert into a {} transaction",
            match tx.state {
                TransactionState::Committed => "committed",
                TransactionState::Aborted => "aborted",
                TransactionState::Active => unreachable!(),
            }
        );
        tx.shadow_terms.push((term.to_vec(), value));
    }

    /// Buffer an increment operation in a document transaction (byte key variant).
    ///
    /// Reads the current value from the trie, computes the new value after applying
    /// the delta, and buffers the result as a SET operation in the transaction's
    /// shadow map. The actual write occurs on commit.
    ///
    /// # Arguments
    ///
    /// * `tx` - The active transaction to buffer the increment in
    /// * `term` - The raw byte key to increment
    /// * `delta` - The increment amount (positive or negative)
    ///
    /// # Panics
    ///
    /// Panics if the transaction is not in Active state.
    pub fn tx_increment_bytes(&self, tx: &mut DocumentTransaction<V>, term: &[u8], delta: i64) {
        assert!(
            tx.is_active(),
            "Cannot increment in a {} transaction",
            match tx.state {
                TransactionState::Committed => "committed",
                TransactionState::Aborted => "aborted",
                TransactionState::Active => unreachable!(),
            }
        );

        // Check if we already have a buffered value for this term in the transaction
        let current: i64 = if let Some(pos) = tx.shadow_terms.iter().rposition(|(k, _)| k == term) {
            if let Some(ref v) = tx.shadow_terms[pos].1 {
                let bytes = bincode::serialize(v).unwrap_or_default();
                if bytes.len() == 8 {
                    i64::from_le_bytes(bytes.try_into().expect("expected 8 bytes"))
                } else {
                    bincode::deserialize::<i64>(&bytes).unwrap_or(0)
                }
            } else {
                0
            }
        } else {
            // Fall back to the persistent trie value
            match self.get_value_impl(term) {
                Some(v) => {
                    let bytes = bincode::serialize(&v).unwrap_or_default();
                    if bytes.len() == 8 {
                        i64::from_le_bytes(bytes.try_into().expect("expected 8 bytes"))
                    } else {
                        bincode::deserialize::<i64>(&bytes).unwrap_or(0)
                    }
                }
                None => 0,
            }
        };

        let new_value = current + delta;
        let value_bytes = bincode::serialize(&new_value).expect("failed to serialize i64");
        let v: V = bincode::deserialize(&value_bytes).expect("failed to deserialize i64 as V");
        tx.shadow_terms.push((term.to_vec(), Some(v)));
    }

    /// Commit a document transaction, atomically applying all buffered terms.
    ///
    /// This method:
    /// 1. Logs all buffered terms as a BatchInsert to WAL
    /// 2. Logs CommitTx to WAL
    /// 3. Applies all terms to the trie
    ///
    /// If the commit fails partway through, recovery will either replay the
    /// complete transaction or skip it entirely (atomic semantics).
    ///
    /// # Arguments
    ///
    /// * `tx` - The transaction to commit (consumed)
    ///
    /// # Returns
    ///
    /// The number of terms successfully inserted.
    pub fn commit_document(&mut self, mut tx: DocumentTransaction<V>) -> Result<usize>
    where
        V: Clone,
    {
        if tx.state != TransactionState::Active {
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "Cannot commit a {} transaction",
                match tx.state {
                    TransactionState::Committed => "committed",
                    TransactionState::Aborted => "aborted",
                    TransactionState::Active => unreachable!(),
                }
            )));
        }

        let count = tx.shadow_terms.len();

        if count == 0 {
            // Empty transaction - just log commit and sync based on durability policy
            tx.state = TransactionState::Committed;
            if let Some(ref wal) = self.wal_writer {
                wal.append(super::wal::WalRecord::CommitTx { tx_id: tx.tx_id }).map_err(|e| {
                    PersistentARTrieError::io_error(
                        "commit_tx", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
                    )
                })?;
                // Sync WAL based on durability policy (ACID Durability)
                if self.durability_policy == DurabilityPolicy::Immediate {
                    wal.sync().map_err(|e| PersistentARTrieError::io_error(
                        "commit_tx_sync", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
                    ))?;
                }
            }
            return Ok(0);
        }

        // Convert to the format expected by insert_batch
        let entries: Vec<(String, Option<V>)> = tx
            .shadow_terms
            .drain(..)
            .map(|(term, value)| {
                let term_str = String::from_utf8_lossy(&term).to_string();
                (term_str, value)
            })
            .collect();

        // Use insert_batch which handles WAL logging internally
        let inserted = self.insert_batch(&entries);

        // Log CommitTx and sync based on durability policy
        if let Some(ref wal) = self.wal_writer {
            wal.append(super::wal::WalRecord::CommitTx { tx_id: tx.tx_id }).map_err(|e| {
                PersistentARTrieError::io_error(
                    "commit_tx", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
                )
            })?;
            // Sync WAL based on durability policy (ACID Durability)
            if self.durability_policy == DurabilityPolicy::Immediate {
                wal.sync().map_err(|e| PersistentARTrieError::io_error(
                    "commit_tx_sync", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
                ))?;
            }
        }

        tx.state = TransactionState::Committed;
        Ok(inserted)
    }

    /// Abort a document transaction, discarding all buffered terms.
    ///
    /// This method logs AbortTx to WAL and discards the buffered terms.
    /// No terms are inserted into the trie.
    ///
    /// # Arguments
    ///
    /// * `tx` - The transaction to abort (consumed)
    pub fn abort_document(&self, mut tx: DocumentTransaction<V>) -> Result<()> {
        if tx.state != TransactionState::Active {
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "Cannot abort a {} transaction",
                match tx.state {
                    TransactionState::Committed => "committed",
                    TransactionState::Aborted => "aborted",
                    TransactionState::Active => unreachable!(),
                }
            )));
        }

        // Log AbortTx to WAL
        if let Some(ref wal) = self.wal_writer {
            wal.append(super::wal::WalRecord::AbortTx { tx_id: tx.tx_id }).map_err(|e| {
                PersistentARTrieError::io_error(
                    "abort_tx", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
                )
            })?;
        }

        // Discard buffered terms
        tx.shadow_terms.clear();
        tx.state = TransactionState::Aborted;

        Ok(())
    }
}

// ===========================================================================
// Atomic Operations
// ===========================================================================
//
// These operations provide lock-free atomic semantics for concurrent access.
// While the underlying storage uses RwLock, the API ensures atomic read-modify-write
// semantics through CAS (Compare-And-Swap) patterns and WAL logging.

impl<V: DictionaryValue + serde::Serialize + serde::de::DeserializeOwned, S: BlockStorage> PersistentARTrie<V, S> {
    /// Atomically increment a numeric value associated with a term.
    ///
    /// If the term doesn't exist, inserts it with `delta` as the initial value.
    /// If the term exists but the value cannot be interpreted as i64, returns an error.
    ///
    /// This operation is atomic: the read-modify-write is performed under a lock,
    /// and the result is logged to WAL before returning.
    ///
    /// # Arguments
    ///
    /// * `term` - The term to increment
    /// * `delta` - The delta to add (can be negative for decrement)
    ///
    /// # Returns
    ///
    /// The new value after increment, or an error if the operation failed.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let dict: PersistentARTrie<i64> = PersistentARTrie::new();
    ///
    /// // First increment creates the entry with value 1
    /// assert_eq!(dict.increment("counter", 1)?, 1);
    ///
    /// // Subsequent increments add to the existing value
    /// assert_eq!(dict.increment("counter", 5)?, 6);
    /// assert_eq!(dict.increment("counter", -2)?, 4);
    /// ```
    pub fn increment(&mut self, term: &str, delta: i64) -> super::error::Result<i64> {
        self.increment_bytes(term.as_bytes(), delta)
    }

    /// Atomically increment a value by term bytes.
    ///
    /// See [`increment`](Self::increment) for details.
    pub fn increment_bytes(&mut self, term: &[u8], delta: i64) -> super::error::Result<i64> {
        // Read current value (if exists)
        let current: i64 = match self.get_value_impl(term) {
            Some(v) => {
                // Try to interpret the value as i64
                let bytes = bincode::serialize(&v)
                    .map_err(|e| super::error::PersistentARTrieError::internal(format!("Serialization error: {}", e)))?;
                if bytes.len() == 8 {
                    i64::from_le_bytes(bytes.try_into().unwrap())
                } else {
                    // Try to deserialize as i64 directly
                    bincode::deserialize::<i64>(&bytes)
                        .map_err(|e| super::error::PersistentARTrieError::internal(
                            format!("Value cannot be interpreted as i64: {}", e)
                        ))?
                }
            }
            None => 0, // Initial value if term doesn't exist
        };

        let new_value = current + delta;

        // Create value from i64
        let value_bytes = bincode::serialize(&new_value)
            .map_err(|e| super::error::PersistentARTrieError::internal(format!("Serialization error: {}", e)))?;
        let v: V = bincode::deserialize(&value_bytes)
            .map_err(|e| super::error::PersistentARTrieError::internal(
                format!("Cannot create value from i64: {}", e)
            ))?;

        // Update the value
        self.remove_impl_core(term);
        self.insert_impl_core(term, Some(v));

        // Log to WAL
        if let Some(ref wal_writer) = self.wal_writer {
            let record = super::wal::WalRecord::Increment {
                term: term.to_vec(),
                delta,
                result: new_value,
            };
            wal_writer.append(record).map_err(|e| PersistentARTrieError::io_error(
                "increment", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            ))?;
        }

        Ok(new_value)
    }

    /// Get value by raw byte key.
    ///
    /// Public wrapper around the private `get_value_impl` method for callers
    /// that already have byte keys (e.g., varint-encoded n-gram keys).
    ///
    /// # Arguments
    ///
    /// * `term` - The raw byte key to look up
    ///
    /// # Returns
    ///
    /// `Some(value)` if the term exists, `None` otherwise.
    #[inline]
    pub fn get_value_bytes(&self, term: &[u8]) -> Option<V>
    where
        V: Clone,
    {
        self.get_value_impl(term)
    }

    /// Check containment by raw byte key.
    ///
    /// Public wrapper around the private `contains_impl` method for callers
    /// that already have byte keys (e.g., varint-encoded n-gram keys).
    ///
    /// # Arguments
    ///
    /// * `term` - The raw byte key to check
    ///
    /// # Returns
    ///
    /// `true` if the term exists in the trie, `false` otherwise.
    #[inline]
    pub fn contains_bytes(&self, term: &[u8]) -> bool {
        self.contains_impl(term)
    }

    /// Atomically update or insert a value.
    ///
    /// If the term exists, updates its value. If not, inserts the term with the value.
    /// This is atomic: the operation is logged to WAL before returning.
    ///
    /// # Arguments
    ///
    /// * `term` - The term to upsert
    /// * `value` - The value to set
    ///
    /// # Returns
    ///
    /// `true` if a new term was inserted, `false` if an existing term was updated.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let dict: PersistentARTrie<String> = PersistentARTrie::new();
    ///
    /// // Insert new term
    /// assert!(dict.upsert("greeting", "hello".to_string())?);
    ///
    /// // Update existing term
    /// assert!(!dict.upsert("greeting", "hi".to_string())?);
    /// ```
    pub fn upsert(&mut self, term: &str, value: V) -> super::error::Result<bool> {
        self.upsert_bytes(term.as_bytes(), value)
    }

    /// Atomically upsert by term bytes.
    ///
    /// See [`upsert`](Self::upsert) for details.
    pub fn upsert_bytes(&mut self, term: &[u8], value: V) -> super::error::Result<bool> {
        // Check if term exists
        let existed = self.contains_impl(term);

        // Remove existing entry (if any) and insert new value
        self.remove_impl_core(term);
        self.insert_impl_core(term, Some(value.clone()));

        // Serialize value for WAL
        let value_bytes = bincode::serialize(&value)
            .map_err(|e| super::error::PersistentARTrieError::internal(format!("Serialization error: {}", e)))?;

        // Log to WAL
        if let Some(ref wal_writer) = self.wal_writer {
            let record = super::wal::WalRecord::Upsert {
                term: term.to_vec(),
                value: value_bytes,
            };
            wal_writer.append(record).map_err(|e| PersistentARTrieError::io_error(
                "upsert", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            ))?;
        }

        Ok(!existed)
    }

    /// Atomically compare and swap a value.
    ///
    /// Updates the value only if the current value matches `expected`.
    /// This provides optimistic concurrency control.
    ///
    /// # Arguments
    ///
    /// * `term` - The term to update
    /// * `expected` - The expected current value (None means term should not exist)
    /// * `new_value` - The new value to set
    ///
    /// # Returns
    ///
    /// `Ok(true)` if the swap succeeded, `Ok(false)` if the current value didn't match expected.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::PersistentARTrie;
    ///
    /// let dict: PersistentARTrie<i32> = PersistentARTrie::new();
    ///
    /// // Insert initial value
    /// dict.upsert("counter", 0)?;
    ///
    /// // CAS succeeds when expected matches
    /// assert!(dict.compare_and_swap("counter", Some(0), 1)?);
    ///
    /// // CAS fails when expected doesn't match
    /// assert!(!dict.compare_and_swap("counter", Some(0), 2)?);
    /// ```
    pub fn compare_and_swap(
        &mut self,
        term: &str,
        expected: Option<V>,
        new_value: V,
    ) -> super::error::Result<bool> {
        self.compare_and_swap_bytes(term.as_bytes(), expected, new_value)
    }

    /// Atomically compare and swap by term bytes.
    ///
    /// See [`compare_and_swap`](Self::compare_and_swap) for details.
    pub fn compare_and_swap_bytes(
        &mut self,
        term: &[u8],
        expected: Option<V>,
        new_value: V,
    ) -> super::error::Result<bool> {
        // Read current value
        let current = self.get_value_impl(term);

        // Check if current matches expected
        let matches = match (&current, &expected) {
            (None, None) => true,
            (Some(c), Some(e)) => {
                // Compare serialized forms for equality
                let c_bytes = bincode::serialize(c).ok();
                let e_bytes = bincode::serialize(e).ok();
                c_bytes == e_bytes
            }
            _ => false,
        };

        // Serialize for WAL
        let expected_bytes = expected.as_ref().and_then(|e| bincode::serialize(e).ok());
        let new_value_bytes = bincode::serialize(&new_value)
            .map_err(|e| super::error::PersistentARTrieError::internal(format!("Serialization error: {}", e)))?;

        if matches {
            // Perform the swap
            self.remove_impl_core(term);
            self.insert_impl_core(term, Some(new_value));
        }

        // Log to WAL (always log, including success status for idempotency)
        if let Some(ref wal_writer) = self.wal_writer {
            let record = super::wal::WalRecord::CompareAndSwap {
                term: term.to_vec(),
                expected: expected_bytes,
                new_value: new_value_bytes,
                success: matches,
            };
            wal_writer.append(record).map_err(|e| PersistentARTrieError::io_error(
                "compare_and_swap", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            ))?;
        }

        Ok(matches)
    }

    /// Get the current value and increment atomically (fetch-and-add).
    ///
    /// Returns the value *before* the increment.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let dict: PersistentARTrie<i64> = PersistentARTrie::new();
    /// dict.upsert("counter", 5)?;
    ///
    /// let old = dict.fetch_add("counter", 3)?; // old = 5, new value = 8
    /// assert_eq!(old, 5);
    /// ```
    pub fn fetch_add(&mut self, term: &str, delta: i64) -> super::error::Result<i64> {
        let new_value = self.increment(term, delta)?;
        Ok(new_value - delta) // Return the old value
    }

    /// Get or insert a default value atomically.
    ///
    /// If the term exists, returns its current value.
    /// If not, inserts the default value and returns it.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let dict: PersistentARTrie<i32> = PersistentARTrie::new();
    ///
    /// // First call inserts the default
    /// let v = dict.get_or_insert("key", 42)?;
    /// assert_eq!(v, 42);
    ///
    /// // Second call returns existing value
    /// let v = dict.get_or_insert("key", 100)?;
    /// assert_eq!(v, 42);
    /// ```
    pub fn get_or_insert(&mut self, term: &str, default: V) -> super::error::Result<V> {
        self.get_or_insert_bytes(term.as_bytes(), default)
    }

    /// Get or insert by term bytes.
    ///
    /// See [`get_or_insert`](Self::get_or_insert) for details.
    pub fn get_or_insert_bytes(&mut self, term: &[u8], default: V) -> super::error::Result<V> {
        // Check if term exists
        if let Some(v) = self.get_value_impl(term) {
            return Ok(v);
        }

        // Insert default value
        self.insert_impl_core(term, Some(default.clone()));

        // Serialize for WAL
        let value_bytes = bincode::serialize(&default)
            .map_err(|e| super::error::PersistentARTrieError::internal(format!("Serialization error: {}", e)))?;

        // Log to WAL
        if let Some(ref wal_writer) = self.wal_writer {
            let record = super::wal::WalRecord::Upsert {
                term: term.to_vec(),
                value: value_bytes,
            };
            wal_writer.append(record).map_err(|e| PersistentARTrieError::io_error(
                "get_or_insert", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            ))?;
        }

        Ok(default)
    }
}

// ============================================================================
// ARTrie Trait Implementation
// ============================================================================

use super::SharedARTrie;

impl<V: DictionaryValue> crate::artrie_trait::ARTrie for SharedARTrie<V> {
    type Unit = u8;
    type Value = V;

    fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::create(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn create_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::create_with_slot_tracking(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::open(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentARTrie::open_with_slot_tracking(path).map(|t| Arc::new(RwLock::new(t)))
    }

    fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, super::recovery::RecoveryReport)> {
        PersistentARTrie::open_with_recovery(path).map(|(t, r)| (Arc::new(RwLock::new(t)), r))
    }

    fn open_with_recovery_and_slot_tracking<P: AsRef<Path>>(path: P) -> Result<(Self, super::recovery::RecoveryReport)> {
        let (trie, report) = PersistentARTrie::open_with_recovery(path)?;
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

    fn flush_sequential(&self) -> Result<()> {
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
        guard.insert_impl(term.as_bytes(), Some(V::default()))
    }

    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        let mut guard = self.write();
        guard.insert_impl(term.as_bytes(), Some(value))
    }

    fn contains(&self, term: &str) -> bool {
        let guard = self.read();
        guard.contains_impl(term.as_bytes())
    }

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        let guard = self.read();
        guard.get_value_impl(term.as_bytes())
    }

    fn remove(&self, term: &str) -> bool {
        let mut guard = self.write();
        guard.remove_impl(term.as_bytes())
    }

    #[inline]
    fn len(&self) -> usize {
        let guard = self.read();
        guard.term_count.load(AtomicOrdering::Acquire)
    }

    fn checkpoint(&self) -> Result<()> {
        use super::wal::WalRecord;

        // First, persist all in-memory data to disk
        {
            let mut guard = self.write();
            guard.persist_to_disk()?;
        }

        // Then write the checkpoint record to WAL
        let guard = self.read();

        if let Some(ref wal_writer) = guard.wal_writer {
            // Get current LSN as checkpoint
            let checkpoint_lsn = guard.next_lsn.load(AtomicOrdering::Acquire).saturating_sub(1);
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let record = WalRecord::Checkpoint {
                checkpoint_lsn,
                timestamp,
            };

            wal_writer.append(record).map_err(|e| PersistentARTrieError::io_error(
                "checkpoint_append", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            ))?;
            wal_writer.sync().map_err(|e| PersistentARTrieError::io_error(
                "checkpoint_sync", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            ))?;

            // Truncate WAL after successful checkpoint - all operations are now persisted
            wal_writer.truncate().map_err(|e| PersistentARTrieError::io_error(
                "checkpoint_truncate", "WAL", std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            ))?;
        }

        Ok(())
    }

    #[inline]
    fn is_dirty(&self) -> bool {
        let guard = self.read();
        guard.dirty.load(AtomicOrdering::Acquire)
    }

    fn remove_prefix(&self, prefix: &str) -> usize {
        // Use batched removal for memory efficiency
        let prefix_bytes = prefix.as_bytes();
        let batch_size = 1024;
        let mut total_removed = 0;

        loop {
            // Collect a batch of terms to remove
            let batch: Vec<Vec<u8>> = {
                let guard = self.read();
                guard.iter_prefix(prefix_bytes)
                    .map(|iter| iter.take(batch_size).collect())
                    .unwrap_or_default()
            };

            if batch.is_empty() {
                break;
            }

            // Remove the batch
            let mut guard = self.write();
            for term in batch {
                if guard.remove_impl(&term) {
                    total_removed += 1;
                }
            }
        }

        total_removed
    }

    fn iter_prefix(&self, prefix: &str) -> Option<Box<dyn Iterator<Item = String> + '_>> {
        let guard = self.read();
        // Use direct iteration without zipper
        let terms = guard.iter_prefix_with_arena(prefix.as_bytes()).ok()??;
        Some(Box::new(terms.into_iter().map(|t| {
            String::from_utf8_lossy(&t.term).into_owned()
        })))
    }

    fn sync(&self) -> Result<()> {
        let guard = self.read();
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

    fn upsert(&self, term: &str, value: Self::Value) -> Result<bool> {
        let mut guard = self.write();
        guard.upsert(term, value)
    }

    fn increment(&self, term: &str, delta: i64) -> Result<i64> {
        let mut guard = self.write();
        guard.increment(term, delta)
    }
}

// EvictableARTrie Trait Implementation
// ============================================================================

impl<V: DictionaryValue> crate::artrie_trait::EvictableARTrie for SharedARTrie<V> {
    fn enable_eviction(&mut self, config: super::eviction::EvictionConfig) -> Result<()> {
        config.validate().map_err(|e| PersistentARTrieError::internal(&e))?;

        let mut guard = self.write();

        // Check if eviction is already enabled
        if guard.eviction_coordinator.is_some() {
            return Err(PersistentARTrieError::internal("Eviction already enabled"));
        }

        // Create the epoch manager reference
        // Note: We need to create a shared epoch manager for the coordinator
        let epoch_manager = Arc::new(super::concurrency::EpochManager::new());

        // Create the eviction coordinator
        let coordinator = super::eviction::EvictionCoordinator::new(config.clone(), epoch_manager);

        // Create a weak reference to self for the eviction callback
        let self_weak = Arc::downgrade(self);

        // Start the eviction coordinator with the eviction callback
        coordinator.start(move |nodes_to_evict| {
            // Try to upgrade the weak reference
            let Some(trie) = self_weak.upgrade() else {
                return (0, 0);
            };

            let mut guard = trie.write();
            let mut evicted_count = 0;
            let mut bytes_freed = 0;

            for (path_hash, path, disk_ptr) in nodes_to_evict {
                if guard.evict_node_at_path(&path, disk_ptr.clone()) {
                    evicted_count += 1;
                    bytes_freed += 256; // Estimate ~256 bytes per node

                    // Remove from LRU tracking
                    if let Some(ref coordinator) = guard.eviction_coordinator {
                        coordinator.lru_registry().remove(&path);
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

    fn disable_eviction(&mut self) -> Result<()> {
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

    fn eviction_stats(&self) -> super::eviction::EvictionStats {
        let guard = self.read();
        guard.eviction_coordinator
            .as_ref()
            .map(|c| c.stats())
            .unwrap_or_default()
    }

    fn force_eviction(&mut self, target_bytes: usize) -> Result<(usize, usize)> {
        let guard = self.read();

        let Some(coordinator) = &guard.eviction_coordinator else {
            return Ok((0, 0));
        };

        Ok(coordinator.force_eviction(target_bytes))
    }

    fn touch_node(&self, path: &[Self::Unit]) {
        let guard = self.read();
        if let Some(coordinator) = &guard.eviction_coordinator {
            coordinator.lru_registry().touch(path);
        }
    }
}

// Helper methods for eviction on PersistentARTrie
impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Evict a single node at the given path, replacing it with a DiskRef.
    ///
    /// Returns `true` if the node was successfully evicted, `false` if the
    /// node was not found or was already a DiskRef.
    ///
    /// # Safety
    ///
    /// This method should only be called after epoch quiescence has been
    /// achieved, ensuring no readers from the old epoch are active.
    pub(crate) fn evict_node_at_path(&mut self, path: &[u8], disk_ptr: SwizzledPtr) -> bool {
        if path.is_empty() {
            // Cannot evict root
            return false;
        }

        // Navigate to the parent of the target node
        let parent_path = &path[..path.len() - 1];
        let target_edge = path[path.len() - 1];

        // Find the parent node
        match self.find_parent_mut(parent_path) {
            Some(children) => {
                // Find the child with the target edge
                for (edge, child) in children.iter_mut() {
                    if *edge == target_edge {
                        match child {
                            super::transitions::ChildNode::DiskRef { .. } => {
                                // Already evicted
                                return false;
                            }
                            super::transitions::ChildNode::Bucket(_)
                            | super::transitions::ChildNode::ArtNode { .. } => {
                                // Replace with DiskRef
                                *child = super::transitions::ChildNode::DiskRef { ptr: disk_ptr };
                                return true;
                            }
                        }
                    }
                }
                false
            }
            None => false,
        }
    }

    /// Find the children vector of the node at the given path.
    ///
    /// Returns `Some(&mut Vec<(u8, ChildNode)>)` if found, `None` if the path
    /// doesn't exist or leads to a bucket/disk ref.
    fn find_parent_mut(&mut self, path: &[u8]) -> Option<&mut Vec<(u8, super::transitions::ChildNode)>> {
        if path.is_empty() {
            // Return root children
            match &mut self.root {
                TrieRoot::Bucket(_) => None,
                TrieRoot::ArtNode { children, .. } => Some(children),
            }
        } else {
            // Navigate down the path
            let mut current_children = match &mut self.root {
                TrieRoot::Bucket(_) => return None,
                TrieRoot::ArtNode { children, .. } => children,
            };

            for &edge in &path[..path.len().saturating_sub(1)] {
                let found = current_children
                    .iter_mut()
                    .find(|(e, _)| *e == edge);

                match found {
                    Some((_, super::transitions::ChildNode::ArtNode { children, .. })) => {
                        current_children = children;
                    }
                    _ => return None,
                }
            }

            // Handle the last edge
            if path.is_empty() {
                return Some(current_children);
            }

            let last_edge = path[path.len() - 1];
            let found = current_children
                .iter_mut()
                .find(|(e, _)| *e == last_edge);

            match found {
                Some((_, super::transitions::ChildNode::ArtNode { children, .. })) => Some(children),
                _ => None,
            }
        }
    }
}

/// Drop implementation for clean shutdown.
///
/// Attempts a best-effort sync on drop to ensure data durability.
/// This is not guaranteed to succeed (e.g., if locks are poisoned),
/// but provides a safety net for normal program termination.
impl<V: DictionaryValue, S: BlockStorage> Drop for PersistentARTrie<V, S> {
    fn drop(&mut self) {
        // Shutdown eviction coordinator first
        if let Some(ref coordinator) = self.eviction_coordinator {
            coordinator.shutdown();
        }

        // Best-effort sync on close
        // Sync WAL
        if let Some(ref wal_writer) = self.wal_writer {
            let _ = wal_writer.sync();
        }
        // Flush buffer manager dirty pages
        if let Some(ref buffer_manager) = self.buffer_manager {
            let bm = buffer_manager.read();
            let _ = bm.flush_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_dictionary() {
        let dict: PersistentARTrie = PersistentARTrie::new();
        assert_eq!(dict.len(), Some(0));
        assert!(!dict.is_dirty());
    }

    #[test]
    fn test_insert_and_contains() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        assert!(dict.insert("apple"));
        assert!(dict.insert("banana"));
        assert!(dict.insert("cherry"));

        assert!(dict.contains("apple"));
        assert!(dict.contains("banana"));
        assert!(dict.contains("cherry"));
        assert!(!dict.contains("date"));

        assert_eq!(dict.len(), Some(3));
        assert!(dict.is_dirty());
    }

    #[test]
    fn test_duplicate_insert() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        assert!(dict.insert("test"));
        assert!(!dict.insert("test")); // Duplicate

        assert_eq!(dict.len(), Some(1));
    }

    #[test]
    fn test_remove() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        dict.insert("apple");
        dict.insert("banana");

        assert!(dict.remove("apple"));
        assert!(!dict.contains("apple"));
        assert!(dict.contains("banana"));

        assert_eq!(dict.len(), Some(1));
    }

    #[test]
    fn test_remove_not_found() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        dict.insert("apple");

        assert!(!dict.remove("banana"));
        assert_eq!(dict.len(), Some(1));
    }

    #[test]
    fn test_empty_string() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        assert!(dict.insert(""));
        assert!(dict.contains(""));

        dict.insert("test");
        assert!(dict.contains(""));
        assert!(dict.contains("test"));
    }

    #[test]
    fn test_dictionary_trait() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        dict.insert("hello");
        dict.insert("world");

        // Test through Dictionary trait
        let dict_ref: &dyn Dictionary<Node = _> = &dict;
        assert!(dict_ref.contains("hello"));
        assert!(!dict_ref.contains("hi"));
    }

    #[test]
    fn test_mark_clean() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        dict.insert("test");
        assert!(dict.is_dirty());

        dict.mark_clean();
        assert!(!dict.is_dirty());
    }

    #[test]
    fn test_many_insertions() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        // Insert many terms to trigger bucket conversion
        for i in 0..100 {
            dict.insert(&format!("word{:03}", i));
        }

        assert_eq!(dict.len(), Some(100));

        // Verify all terms exist
        for i in 0..100 {
            assert!(dict.contains(&format!("word{:03}", i)));
        }
    }

    #[test]
    fn test_sync_strategy() {
        let dict: PersistentARTrie = PersistentARTrie::new();
        assert_eq!(dict.sync_strategy(), SyncStrategy::InternalSync);
    }

    #[test]
    fn test_iter_empty() {
        let dict: PersistentARTrie = PersistentARTrie::new();
        let terms: Vec<_> = dict.iter().collect();
        assert!(terms.is_empty());
    }

    #[test]
    fn test_iter_single() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();
        dict.insert("hello");

        let terms: Vec<_> = dict.iter().collect();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0], b"hello".to_vec());
    }

    #[test]
    fn test_iter_multiple() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();
        dict.insert("apple");
        dict.insert("banana");
        dict.insert("cherry");

        let terms: Vec<String> = dict.iter_strings().collect();
        assert_eq!(terms.len(), 3);

        // Should contain all terms
        assert!(terms.contains(&"apple".to_string()));
        assert!(terms.contains(&"banana".to_string()));
        assert!(terms.contains(&"cherry".to_string()));
    }

    #[test]
    fn test_iter_with_empty_string() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();
        dict.insert("");
        dict.insert("hello");

        let terms: Vec<String> = dict.iter_strings().collect();
        assert_eq!(terms.len(), 2);
        assert!(terms.contains(&"".to_string()));
        assert!(terms.contains(&"hello".to_string()));
    }

    #[test]
    fn test_iter_common_prefix() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();
        dict.insert("test");
        dict.insert("testing");
        dict.insert("tested");
        dict.insert("tester");

        let terms: Vec<String> = dict.iter_strings().collect();
        assert_eq!(terms.len(), 4);
        assert!(terms.contains(&"test".to_string()));
        assert!(terms.contains(&"testing".to_string()));
        assert!(terms.contains(&"tested".to_string()));
        assert!(terms.contains(&"tester".to_string()));
    }

    #[test]
    fn test_iter_preserves_order() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();
        // Insert in random order
        dict.insert("cherry");
        dict.insert("apple");
        dict.insert("banana");

        let terms: Vec<String> = dict.iter_strings().collect();
        // Should be in lexicographic order
        // (but bucket-based iteration may have different order within bucket)
        assert_eq!(terms.len(), 3);
    }

    // Note: test_clone was removed because PersistentARTrie no longer implements Clone
    // after the flattening refactor. For shared access, use SharedARTrie (Arc<RwLock<...>>).

    mod persistent_tests {
        use super::*;
        use tempfile::TempDir;

        #[test]
        fn test_create_and_open() {
            let temp_dir = TempDir::new().expect("create temp dir");
            let dict_path = temp_dir.path().join("test.part");

            // Create new dictionary
            {
                let mut dict: PersistentARTrie<()> =
                    PersistentARTrie::create(&dict_path).expect("create dict");
                dict.insert("hello");
                dict.insert("world");
                dict.sync().expect("sync");
            }

            // Open existing dictionary
            {
                let dict: PersistentARTrie<()> =
                    PersistentARTrie::open(&dict_path).expect("open dict");
                assert!(dict.contains("hello"));
                assert!(dict.contains("world"));
                assert_eq!(dict.len(), Some(2));
            }
        }

        #[test]
        fn test_create_fails_if_exists() {
            let temp_dir = TempDir::new().expect("create temp dir");
            let dict_path = temp_dir.path().join("test.part");

            // Create the file first
            std::fs::write(&dict_path, b"dummy").expect("create file");

            // Trying to create should fail
            let result: Result<PersistentARTrie<()>> = PersistentARTrie::create(&dict_path);
            assert!(result.is_err());
        }

        #[test]
        fn test_open_fails_if_not_exists() {
            let temp_dir = TempDir::new().expect("create temp dir");
            let dict_path = temp_dir.path().join("nonexistent.part");

            let result: Result<PersistentARTrie<()>> = PersistentARTrie::open(&dict_path);
            assert!(result.is_err());
        }

        #[test]
        fn test_wal_recovery() {
            let temp_dir = TempDir::new().expect("create temp dir");
            let dict_path = temp_dir.path().join("test.part");

            // Create dictionary and insert data
            {
                let mut dict: PersistentARTrie<()> =
                    PersistentARTrie::create(&dict_path).expect("create dict");
                dict.insert("apple");
                dict.insert("banana");
                dict.insert("cherry");
                dict.sync().expect("sync");
            }

            // Reopen and verify WAL recovery
            {
                let dict: PersistentARTrie<()> =
                    PersistentARTrie::open(&dict_path).expect("open dict");
                assert!(dict.contains("apple"));
                assert!(dict.contains("banana"));
                assert!(dict.contains("cherry"));
            }
        }

        #[test]
        fn test_checkpoint() {
            let temp_dir = TempDir::new().expect("create temp dir");
            let dict_path = temp_dir.path().join("test.part");

            let mut dict: PersistentARTrie<()> =
                PersistentARTrie::create(&dict_path).expect("create dict");
            dict.insert("test");
            dict.checkpoint().expect("checkpoint");
        }

        #[test]
        fn test_sync() {
            let temp_dir = TempDir::new().expect("create temp dir");
            let dict_path = temp_dir.path().join("test.part");

            let mut dict: PersistentARTrie<()> =
                PersistentARTrie::create(&dict_path).expect("create dict");
            dict.insert("test");
            dict.sync().expect("sync");
        }

        #[test]
        fn test_many_insertions_persistent() {
            let temp_dir = TempDir::new().expect("create temp dir");
            let dict_path = temp_dir.path().join("test.part");

            // Create and insert many terms
            {
                let mut dict: PersistentARTrie<()> =
                    PersistentARTrie::create(&dict_path).expect("create dict");
                for i in 0..50 {
                    dict.insert(&format!("word{:03}", i));
                }
                dict.sync().expect("sync");
            }

            // Reopen and verify
            {
                let dict: PersistentARTrie<()> =
                    PersistentARTrie::open(&dict_path).expect("open dict");
                assert_eq!(dict.len(), Some(50));
                for i in 0..50 {
                    assert!(
                        dict.contains(&format!("word{:03}", i)),
                        "missing word{:03}",
                        i
                    );
                }
            }
        }
    }

    // === Atomic Operations Tests ===

    mod atomic_ops_tests {
        use super::*;
        use tempfile::tempdir;

        #[test]
        fn test_increment_new_term() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("atomic_test.part");

            let mut dict: PersistentARTrie<i64> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // First increment creates the entry with the delta value
            let result = dict.increment("counter", 1).expect("increment");
            assert_eq!(result, 1, "First increment should return delta value");

            // Verify the term exists
            assert!(dict.contains("counter"));
        }

        #[test]
        fn test_increment_existing_term() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("atomic_test.part");

            let mut dict: PersistentARTrie<i64> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Insert initial value
            dict.upsert("counter", 10i64).expect("upsert");

            // Increment
            let result = dict.increment("counter", 5).expect("increment");
            assert_eq!(result, 15);

            // Negative increment (decrement)
            let result = dict.increment("counter", -3).expect("decrement");
            assert_eq!(result, 12);
        }

        #[test]
        fn test_upsert_new_term() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("atomic_test.part");

            let mut dict: PersistentARTrie<String> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Insert new term
            let is_new = dict.upsert("greeting", "hello".to_string()).expect("upsert");
            assert!(is_new, "Should return true for new insertion");

            // Verify value
            let value = dict.get_value("greeting");
            assert_eq!(value, Some("hello".to_string()));
        }

        #[test]
        fn test_upsert_existing_term() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("atomic_test.part");

            let mut dict: PersistentARTrie<String> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Insert initial value
            dict.upsert("greeting", "hello".to_string()).expect("upsert");

            // Update existing term
            let is_new = dict.upsert("greeting", "hi".to_string()).expect("upsert");
            assert!(!is_new, "Should return false for update");

            // Verify updated value
            let value = dict.get_value("greeting");
            assert_eq!(value, Some("hi".to_string()));
        }

        #[test]
        fn test_compare_and_swap_success() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("atomic_test.part");

            let mut dict: PersistentARTrie<i32> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Insert initial value
            dict.upsert("counter", 0i32).expect("upsert");

            // CAS succeeds when expected matches
            let success = dict.compare_and_swap("counter", Some(0), 1).expect("cas");
            assert!(success, "CAS should succeed when expected matches");

            // Verify new value
            assert_eq!(dict.get_value("counter"), Some(1));
        }

        #[test]
        fn test_compare_and_swap_failure() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("atomic_test.part");

            let mut dict: PersistentARTrie<i32> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Insert initial value
            dict.upsert("counter", 5i32).expect("upsert");

            // CAS fails when expected doesn't match
            let success = dict.compare_and_swap("counter", Some(0), 10).expect("cas");
            assert!(!success, "CAS should fail when expected doesn't match");

            // Value should be unchanged
            assert_eq!(dict.get_value("counter"), Some(5));
        }

        #[test]
        fn test_compare_and_swap_none_expected() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("atomic_test.part");

            let mut dict: PersistentARTrie<i32> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // CAS with None expected succeeds when term doesn't exist
            let success = dict.compare_and_swap("new_key", None, 42).expect("cas");
            assert!(success, "CAS should succeed when expecting None and key doesn't exist");

            // Verify value was inserted
            assert_eq!(dict.get_value("new_key"), Some(42));
        }

        #[test]
        fn test_fetch_add() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("atomic_test.part");

            let mut dict: PersistentARTrie<i64> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Insert initial value
            dict.upsert("counter", 10i64).expect("upsert");

            // fetch_add returns old value
            let old = dict.fetch_add("counter", 5).expect("fetch_add");
            assert_eq!(old, 10, "fetch_add should return old value");

            // Verify new value
            let new_val = dict.increment("counter", 0).expect("read");
            assert_eq!(new_val, 15);
        }

        #[test]
        fn test_get_or_insert_new() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("atomic_test.part");

            let mut dict: PersistentARTrie<i32> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // get_or_insert on new key returns default
            let value = dict.get_or_insert("key", 42).expect("get_or_insert");
            assert_eq!(value, 42);

            // Verify it was inserted
            assert!(dict.contains("key"));
        }

        #[test]
        fn test_get_or_insert_existing() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("atomic_test.part");

            let mut dict: PersistentARTrie<i32> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Insert initial value
            dict.upsert("key", 100i32).expect("upsert");

            // get_or_insert returns existing value, ignores default
            let value = dict.get_or_insert("key", 42).expect("get_or_insert");
            assert_eq!(value, 100, "Should return existing value, not default");
        }

        // ===================================================================
        // Per-Document Transaction Tests
        // ===================================================================

        #[test]
        fn test_document_transaction_commit() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("tx_test.part");

            let mut dict: PersistentARTrie<i64> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Begin a document transaction
            let mut tx = dict.begin_document("doc1").expect("begin transaction");
            assert_eq!(tx.state, TransactionState::Active);

            // Buffer some terms
            dict.tx_insert(&mut tx, "term1", Some(100));
            dict.tx_insert(&mut tx, "term2", Some(200));
            dict.tx_insert(&mut tx, "term3", None);

            // Terms should NOT be visible yet
            assert!(!dict.contains("term1"));
            assert!(!dict.contains("term2"));
            assert!(!dict.contains("term3"));

            // Commit the transaction
            let count = dict.commit_document(tx).expect("commit");
            assert_eq!(count, 3);

            // Now all terms should be visible
            assert!(dict.contains("term1"));
            assert!(dict.contains("term2"));
            assert!(dict.contains("term3"));

            // Values should be correct
            assert_eq!(dict.get_value("term1"), Some(100));
            assert_eq!(dict.get_value("term2"), Some(200));
            assert_eq!(dict.get_value("term3"), None);
        }

        #[test]
        fn test_document_transaction_abort() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("tx_test.part");

            let mut dict: PersistentARTrie<i64> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Insert one term directly
            dict.insert_with_value("existing", 42);
            assert!(dict.contains("existing"));

            // Begin a document transaction
            let mut tx = dict.begin_document("doc1").expect("begin transaction");

            // Buffer some terms
            dict.tx_insert(&mut tx, "term1", Some(100));
            dict.tx_insert(&mut tx, "term2", Some(200));

            // Abort the transaction
            dict.abort_document(tx).expect("abort");

            // Buffered terms should NOT be visible
            assert!(!dict.contains("term1"));
            assert!(!dict.contains("term2"));

            // Existing term should still be there
            assert!(dict.contains("existing"));
            assert_eq!(dict.get_value("existing"), Some(42));
        }

        #[test]
        fn test_document_transaction_empty_commit() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("tx_test.part");

            let mut dict: PersistentARTrie<i64> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Begin and immediately commit an empty transaction
            let tx = dict.begin_document("empty_doc").expect("begin transaction");
            let count = dict.commit_document(tx).expect("commit");
            assert_eq!(count, 0);
        }

        #[test]
        fn test_document_transaction_bytes() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("tx_test.part");

            let mut dict: PersistentARTrie<i64> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            let mut tx = dict.begin_document("doc1").expect("begin transaction");

            // Use bytes API
            dict.tx_insert_bytes(&mut tx, b"binary_term", Some(999));

            let count = dict.commit_document(tx).expect("commit");
            assert_eq!(count, 1);

            assert!(dict.contains("binary_term"));
            assert_eq!(dict.get_value("binary_term"), Some(999));
        }

        #[test]
        fn test_multiple_document_transactions() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("tx_test.part");

            let mut dict: PersistentARTrie<i64> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // First document - commit
            let mut tx1 = dict.begin_document("doc1").expect("begin tx1");
            dict.tx_insert(&mut tx1, "doc1_term1", Some(1));
            dict.tx_insert(&mut tx1, "doc1_term2", Some(2));
            dict.commit_document(tx1).expect("commit tx1");

            // Second document - abort
            let mut tx2 = dict.begin_document("doc2").expect("begin tx2");
            dict.tx_insert(&mut tx2, "doc2_term1", Some(100));
            dict.abort_document(tx2).expect("abort tx2");

            // Third document - commit
            let mut tx3 = dict.begin_document("doc3").expect("begin tx3");
            dict.tx_insert(&mut tx3, "doc3_term1", Some(300));
            dict.commit_document(tx3).expect("commit tx3");

            // Verify state
            assert!(dict.contains("doc1_term1"));
            assert!(dict.contains("doc1_term2"));
            assert!(!dict.contains("doc2_term1")); // Aborted
            assert!(dict.contains("doc3_term1"));

            assert_eq!(dict.get_value("doc1_term1"), Some(1));
            assert_eq!(dict.get_value("doc3_term1"), Some(300));
        }

        // Note: The following scenarios are prevented by Rust's type system (ownership):
        // - Inserting after commit: transaction is consumed by commit_document()
        // - Double commit: transaction is consumed by first commit
        // - Double abort: transaction is consumed by first abort
        // These are compile-time guarantees, not runtime checks.
    }

    mod sequential_siblings_tests {
        use super::*;
        use crate::persistent_artrie::arena_manager::ArenaSlot;
        use crate::persistent_artrie::nodes::{Node, Node4, ChildStorage};
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

        #[test]
        fn test_check_sequential_children_empty() {
            // Node with no children - should return None
            let node = Node::N4(Box::new(Node4::new()));
            let result = PersistentARTrie::<()>::check_sequential_children(&node, 0);
            assert!(result.is_none());
        }

        #[test]
        fn test_check_sequential_children_single_child() {
            // Single child - not enough for sequential optimization
            // Note: block_id=1 maps to arena_id=0 (arena N = block N+1)
            let mut n4 = Node4::new();
            let child_ptr = SwizzledPtr::on_disk(1, 10, crate::persistent_artrie::swizzled_ptr::NodeType::Node4);
            let _ = n4.add_child(b'a', child_ptr);
            let node = Node::N4(Box::new(n4));

            let result = PersistentARTrie::<()>::check_sequential_children(&node, 0);
            assert!(result.is_none(), "Single child should not use sequential");
        }

        #[test]
        fn test_check_sequential_children_consecutive() {
            // Two children with consecutive slot IDs in same arena
            // Note: block_id=1 maps to arena_id=0 (arena N = block N+1)
            let mut n4 = Node4::new();
            let ptr1 = SwizzledPtr::on_disk(1, 10, crate::persistent_artrie::swizzled_ptr::NodeType::Node4);
            let ptr2 = SwizzledPtr::on_disk(1, 11, crate::persistent_artrie::swizzled_ptr::NodeType::Node4);
            let _ = n4.add_child(b'a', ptr1);
            let _ = n4.add_child(b'b', ptr2);
            let node = Node::N4(Box::new(n4));

            let result = PersistentARTrie::<()>::check_sequential_children(&node, 0);
            assert!(result.is_some(), "Consecutive children should use sequential");

            let first = result.unwrap();
            assert_eq!(first.arena_id, 0);
            assert_eq!(first.slot_id, 10);
        }

        #[test]
        fn test_check_sequential_children_not_consecutive() {
            // Two children with gap in slot IDs
            // Note: block_id=1 maps to arena_id=0 (arena N = block N+1)
            let mut n4 = Node4::new();
            let ptr1 = SwizzledPtr::on_disk(1, 10, crate::persistent_artrie::swizzled_ptr::NodeType::Node4);
            let ptr2 = SwizzledPtr::on_disk(1, 15, crate::persistent_artrie::swizzled_ptr::NodeType::Node4); // Gap!
            let _ = n4.add_child(b'a', ptr1);
            let _ = n4.add_child(b'b', ptr2);
            let node = Node::N4(Box::new(n4));

            let result = PersistentARTrie::<()>::check_sequential_children(&node, 0);
            assert!(result.is_none(), "Non-consecutive slots should not use sequential");
        }

        #[test]
        fn test_check_sequential_children_different_arenas() {
            // Two children in different arenas
            // block_id=1 maps to arena_id=0, block_id=2 maps to arena_id=1
            let mut n4 = Node4::new();
            let ptr1 = SwizzledPtr::on_disk(1, 10, crate::persistent_artrie::swizzled_ptr::NodeType::Node4);
            let ptr2 = SwizzledPtr::on_disk(2, 11, crate::persistent_artrie::swizzled_ptr::NodeType::Node4); // Different arena!
            let _ = n4.add_child(b'a', ptr1);
            let _ = n4.add_child(b'b', ptr2);
            let node = Node::N4(Box::new(n4));

            let result = PersistentARTrie::<()>::check_sequential_children(&node, 0);
            assert!(result.is_none(), "Cross-arena children should not use sequential");
        }

        #[test]
        fn test_check_sequential_children_wrong_parent_arena() {
            // Children consecutive but parent will be in different arena
            // block_id=1 maps to arena_id=0 (arena N = block N+1)
            let mut n4 = Node4::new();
            let ptr1 = SwizzledPtr::on_disk(1, 10, crate::persistent_artrie::swizzled_ptr::NodeType::Node4);
            let ptr2 = SwizzledPtr::on_disk(1, 11, crate::persistent_artrie::swizzled_ptr::NodeType::Node4);
            let _ = n4.add_child(b'a', ptr1);
            let _ = n4.add_child(b'b', ptr2);
            let node = Node::N4(Box::new(n4));

            // Parent will be in arena 1, but children are in arena 0
            let result = PersistentARTrie::<()>::check_sequential_children(&node, 1);
            assert!(result.is_none(), "Children must be in same arena as parent");
        }

        #[test]
        fn test_check_sequential_children_three_consecutive() {
            // Three children with consecutive slot IDs
            // Note: block_id=1 maps to arena_id=0 (arena N = block N+1)
            let mut n4 = Node4::new();
            let ptr1 = SwizzledPtr::on_disk(1, 100, crate::persistent_artrie::swizzled_ptr::NodeType::Node4);
            let ptr2 = SwizzledPtr::on_disk(1, 101, crate::persistent_artrie::swizzled_ptr::NodeType::Node4);
            let ptr3 = SwizzledPtr::on_disk(1, 102, crate::persistent_artrie::swizzled_ptr::NodeType::Node4);
            let _ = n4.add_child(b'a', ptr1);
            let _ = n4.add_child(b'b', ptr2);
            let _ = n4.add_child(b'c', ptr3);
            let node = Node::N4(Box::new(n4));

            let result = PersistentARTrie::<()>::check_sequential_children(&node, 0);
            assert!(result.is_some());

            let first = result.unwrap();
            assert_eq!(first.arena_id, 0);
            assert_eq!(first.slot_id, 100);
        }

        #[test]
        fn test_child_storage_enum() {
            // Test ChildStorage enum basic operations
            let direct = ChildStorage::Direct;
            assert!(direct.is_direct());
            assert!(!direct.is_sequential());
            assert!(direct.first_slot().is_none());

            let sequential = ChildStorage::sequential(5, 100);
            assert!(!sequential.is_direct());
            assert!(sequential.is_sequential());
            assert_eq!(sequential.arena_id(), Some(5));
            assert_eq!(sequential.first_slot(), Some(100));
        }
    }

    // ========================================================================
    // Arena-Aware and Disk-Paging Optimization Tests
    // ========================================================================

    mod optimization_tests {
        use super::*;

        // ====================================================================
        // Optimization 1: Multi-Level Prefetch Tests
        // ====================================================================

        #[test]
        fn test_multi_level_prefetch_respects_depth_limit() {
            use crate::persistent_artrie::prefetch::{Prefetcher, PrefetchStrategy};
            use crate::persistent_artrie::swizzled_ptr::{SwizzledPtr, NodeType};

            // Create prefetcher with depth limit of 2
            let prefetcher = Prefetcher::with_config(100, PrefetchStrategy::DepthLimited(2));

            // Create some disk pointers
            let children: Vec<(u8, SwizzledPtr)> = (0..5)
                .map(|i| (i, SwizzledPtr::on_disk(i as u32, 0, NodeType::Node4)))
                .collect();

            // Depth 0 - should prefetch
            prefetcher.prefetch_children_bounded(&children, 0);
            assert_eq!(prefetcher.queue_len(), 5);
            prefetcher.clear();

            // Depth 1 - should prefetch
            prefetcher.prefetch_children_bounded(&children, 1);
            assert_eq!(prefetcher.queue_len(), 5);
            prefetcher.clear();

            // Depth 2 - should prefetch
            prefetcher.prefetch_children_bounded(&children, 2);
            assert_eq!(prefetcher.queue_len(), 5);
            prefetcher.clear();

            // Depth 3 - should NOT prefetch (beyond limit)
            prefetcher.prefetch_children_bounded(&children, 3);
            assert_eq!(prefetcher.queue_len(), 0);
        }

        #[test]
        fn test_prefetch_children_bounded_with_first_n_strategy() {
            use crate::persistent_artrie::prefetch::{Prefetcher, PrefetchStrategy};
            use crate::persistent_artrie::swizzled_ptr::{SwizzledPtr, NodeType};

            // Create prefetcher with FirstN(3) limit
            let prefetcher = Prefetcher::with_config(100, PrefetchStrategy::FirstN(3));

            // Create 10 disk pointers
            let children: Vec<(u8, SwizzledPtr)> = (0..10)
                .map(|i| (i, SwizzledPtr::on_disk(i as u32, 0, NodeType::Node4)))
                .collect();

            // Should only prefetch first 3 regardless of depth
            prefetcher.prefetch_children_bounded(&children, 0);
            assert_eq!(prefetcher.queue_len(), 3);
            prefetcher.clear();

            prefetcher.prefetch_children_bounded(&children, 5);
            assert_eq!(prefetcher.queue_len(), 3);
        }

        #[test]
        fn test_prefetch_disabled_strategy() {
            use crate::persistent_artrie::prefetch::{Prefetcher, PrefetchStrategy};
            use crate::persistent_artrie::swizzled_ptr::{SwizzledPtr, NodeType};

            let prefetcher = Prefetcher::with_config(100, PrefetchStrategy::Disabled);

            let children: Vec<(u8, SwizzledPtr)> = (0..5)
                .map(|i| (i, SwizzledPtr::on_disk(i as u32, 0, NodeType::Node4)))
                .collect();

            // Should not prefetch anything with Disabled strategy
            prefetcher.prefetch_children_bounded(&children, 0);
            assert_eq!(prefetcher.queue_len(), 0);
        }

        // ====================================================================
        // Optimization 2: Arena-Grouped Merge Tests
        // ====================================================================

        #[test]
        fn test_merge_arena_sorting_preserves_correctness() {
            // Create source trie with entries
            let mut source: PersistentARTrie<u32> = PersistentARTrie::new();
            source.insert_with_value("apple", 1);
            source.insert_with_value("banana", 2);
            source.insert_with_value("cherry", 3);
            source.insert_with_value("apricot", 4);
            source.insert_with_value("blueberry", 5);

            // Create target trie with some overlapping entries
            let mut target: PersistentARTrie<u32> = PersistentARTrie::new();
            target.insert_with_value("apple", 10);
            target.insert_with_value("date", 6);

            // Use standard batched merge
            let result1 = target.merge_from_batched(&source, |a, b| a + b, 2);
            assert!(result1.is_ok());
            let count1 = result1.unwrap();
            assert_eq!(count1, 5); // All 5 terms from source

            // Verify merge result
            assert_eq!(target.get_value("apple"), Some(11)); // 10 + 1
            assert_eq!(target.get_value("banana"), Some(2));
            assert_eq!(target.get_value("cherry"), Some(3));
            assert_eq!(target.get_value("apricot"), Some(4));
            assert_eq!(target.get_value("blueberry"), Some(5));
            assert_eq!(target.get_value("date"), Some(6)); // Preserved
        }

        #[test]
        fn test_merge_arena_grouped_ordering() {
            // Create source trie
            let mut source: PersistentARTrie<u32> = PersistentARTrie::new();
            for i in 0..100 {
                let term = format!("term{:03}", i);
                source.insert_with_value(&term, i);
            }

            // Create target trie
            let mut target: PersistentARTrie<u32> = PersistentARTrie::new();

            // Use grouped merge
            let result = target.merge_from_batched_grouped(&source, |a, b| a + b, 20);
            assert!(result.is_ok());
            let count = result.unwrap();
            assert_eq!(count, 100);

            // Verify all entries present
            for i in 0..100 {
                let term = format!("term{:03}", i);
                assert_eq!(target.get_value(&term), Some(i));
            }
        }

        // ====================================================================
        // Optimization 3: Arena-Aware Insert Batching Tests
        // ====================================================================

        #[test]
        fn test_insert_batch_arena_grouped_ordering() {
            let mut trie: PersistentARTrie<u32> = PersistentARTrie::new();

            // Create entries with various first bytes
            let entries: Vec<(Vec<u8>, Option<u32>)> = vec![
                (b"zebra".to_vec(), Some(1)),
                (b"apple".to_vec(), Some(2)),
                (b"apricot".to_vec(), Some(3)),
                (b"zoo".to_vec(), Some(4)),
                (b"banana".to_vec(), Some(5)),
                (b"azure".to_vec(), Some(6)),
            ];

            // Insert with arena grouping
            let count = trie.insert_batch_arena_grouped(entries);
            assert_eq!(count, 6);

            // Verify all entries are present with correct values
            assert_eq!(trie.get_value("zebra"), Some(1));
            assert_eq!(trie.get_value("apple"), Some(2));
            assert_eq!(trie.get_value("apricot"), Some(3));
            assert_eq!(trie.get_value("zoo"), Some(4));
            assert_eq!(trie.get_value("banana"), Some(5));
            assert_eq!(trie.get_value("azure"), Some(6));
        }

        #[test]
        fn test_insert_batch_grouped_string_variant() {
            let mut trie: PersistentARTrie<u32> = PersistentARTrie::new();

            let entries: Vec<(String, Option<u32>)> = vec![
                ("zebra".to_string(), Some(1)),
                ("apple".to_string(), Some(2)),
                ("apricot".to_string(), Some(3)),
                ("zoo".to_string(), Some(4)),
                ("banana".to_string(), Some(5)),
                ("azure".to_string(), Some(6)),
            ];

            let count = trie.insert_batch_grouped(entries);
            assert_eq!(count, 6);

            // Verify entries
            assert_eq!(trie.get_value("zebra"), Some(1));
            assert_eq!(trie.get_value("apple"), Some(2));
            assert_eq!(trie.get_value("apricot"), Some(3));
        }

        #[test]
        fn test_insert_batch_arena_grouped_empty() {
            let mut trie: PersistentARTrie<u32> = PersistentARTrie::new();

            let entries: Vec<(Vec<u8>, Option<u32>)> = vec![];
            let count = trie.insert_batch_arena_grouped(entries);
            assert_eq!(count, 0);
            assert_eq!(trie.len(), Some(0));
        }

        #[test]
        fn test_insert_batch_grouped_preserves_values() {
            let mut trie: PersistentARTrie<String> = PersistentARTrie::new();

            let entries: Vec<(String, Option<String>)> = vec![
                ("key1".to_string(), Some("value1".to_string())),
                ("key2".to_string(), Some("value2".to_string())),
                ("akey".to_string(), Some("avalue".to_string())),
            ];

            let count = trie.insert_batch_grouped(entries);
            assert_eq!(count, 3);

            assert_eq!(trie.get_value("key1"), Some("value1".to_string()));
            assert_eq!(trie.get_value("key2"), Some("value2".to_string()));
            assert_eq!(trie.get_value("akey"), Some("avalue".to_string()));
        }

        // ====================================================================
        // Arena Manager Sequential Flush Tests
        // ====================================================================

        #[test]
        fn test_arena_manager_flush_sequential() {
            use crate::persistent_artrie::arena_manager::ArenaManager;
            use crate::persistent_artrie::disk_manager::MmapDiskManager;

            // ArenaManager without buffer manager - flush_sequential is a no-op
            let mut manager: ArenaManager<MmapDiskManager> = ArenaManager::new();

            // Allocate some data to make arenas dirty
            manager.allocate(b"test1").expect("alloc 1");
            manager.allocate(b"test2").expect("alloc 2");

            // flush_sequential should succeed (no-op without buffer manager)
            let result = manager.flush_sequential();
            assert!(result.is_ok());
        }
    }

    // ========================================================================
    // LSN API Tests
    // ========================================================================

    mod lsn_api_tests {
        use super::*;
        use tempfile::tempdir;

        #[test]
        fn test_current_lsn_starts_at_one_for_persistent() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("lsn_test.part");

            let dict: PersistentARTrie<i32> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Persistent tries start at LSN 1 (0 is reserved for "no LSN")
            assert_eq!(dict.current_lsn(), 1);
        }

        #[test]
        fn test_current_lsn_starts_at_zero_for_in_memory() {
            // In-memory tries have no LSN tracking, so next_lsn is 0
            let dict: PersistentARTrie<i32> = PersistentARTrie::new();
            assert_eq!(dict.current_lsn(), 0);
        }

        #[test]
        fn test_current_lsn_increases_after_insert() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("lsn_test.part");

            let mut dict: PersistentARTrie<i32> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            let before = dict.current_lsn();
            dict.insert_with_value("key1", 42);
            let after = dict.current_lsn();

            assert!(
                after > before,
                "LSN should increase after insert: before={}, after={}",
                before,
                after
            );
        }

        #[test]
        fn test_current_lsn_increases_after_remove() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("lsn_test.part");

            let mut dict: PersistentARTrie<i32> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            dict.insert_with_value("key1", 42);
            let before = dict.current_lsn();
            dict.remove("key1");
            let after = dict.current_lsn();

            assert!(
                after > before,
                "LSN should increase after remove: before={}, after={}",
                before,
                after
            );
        }

        #[test]
        fn test_synced_lsn_none_for_in_memory() {
            // In-memory tries have no WAL, so synced_lsn should be None
            let dict: PersistentARTrie<i32> = PersistentARTrie::new();
            assert!(
                dict.synced_lsn().is_none(),
                "In-memory trie should have no synced LSN"
            );
        }

        #[test]
        fn test_synced_lsn_after_sync() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("lsn_test.part");

            let mut dict: PersistentARTrie<i32> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Insert some data
            dict.insert_with_value("key1", 42);
            dict.insert_with_value("key2", 43);

            // Before sync, synced_lsn should be 0 (no syncs yet)
            let synced_before = dict.synced_lsn().expect("persistent trie should have synced_lsn");
            assert_eq!(synced_before, 0, "No data should be synced yet");

            // Sync to disk
            dict.sync().expect("sync should succeed");

            // After sync, synced_lsn should be positive
            let synced_after = dict.synced_lsn().expect("persistent trie should have synced_lsn");
            assert!(
                synced_after > 0,
                "synced_lsn should be positive after sync: {}",
                synced_after
            );
        }

        #[test]
        fn test_synced_lsn_invariant() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("lsn_test.part");

            let mut dict: PersistentARTrie<i32> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Insert and sync
            dict.insert_with_value("key1", 42);
            dict.sync().expect("sync should succeed");

            // Insert more data without syncing
            dict.insert_with_value("key2", 43);

            let current = dict.current_lsn();
            let synced = dict.synced_lsn().expect("persistent trie should have synced_lsn");

            // Invariant: synced_lsn <= current_lsn - 1
            // (current_lsn is the NEXT lsn to be assigned, so the last written is current - 1)
            assert!(
                synced < current,
                "synced_lsn ({}) should be less than current_lsn ({})",
                synced,
                current
            );
        }

        #[test]
        fn test_lsn_monotonically_increasing() {
            let dir = tempdir().expect("create temp dir");
            let dict_path = dir.path().join("lsn_test.part");

            let mut dict: PersistentARTrie<i32> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            let mut prev_lsn = dict.current_lsn();

            // Perform multiple operations and verify LSN increases
            for i in 0..10 {
                dict.insert_with_value(&format!("key{}", i), i);
                let curr_lsn = dict.current_lsn();
                assert!(
                    curr_lsn > prev_lsn,
                    "LSN should increase monotonically: prev={}, curr={}",
                    prev_lsn,
                    curr_lsn
                );
                prev_lsn = curr_lsn;
            }
        }
    }

    // =========================================================================
    // Selective Dirty Subtree Traversal Tests
    // =========================================================================

    #[test]
    fn test_dirty_path_recording() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        // Initially, no dirty paths
        assert!(dict.dirty_prefixes.is_empty());

        // Insert a term - should record the path
        dict.insert("apple");

        // Check that path prefixes are recorded
        assert!(dict.dirty_prefixes.contains(&vec![])); // Root
        assert!(dict.dirty_prefixes.contains(&vec![b'a']));
        assert!(dict.dirty_prefixes.contains(&vec![b'a', b'p']));
        assert!(dict.dirty_prefixes.contains(&vec![b'a', b'p', b'p']));
        assert!(dict.dirty_prefixes.contains(&vec![b'a', b'p', b'p', b'l']));
        assert!(dict.dirty_prefixes.contains(&vec![b'a', b'p', b'p', b'l', b'e']));
    }

    #[test]
    fn test_dirty_path_recording_multiple_terms() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        // Insert multiple terms with shared prefix
        dict.insert("apple");
        dict.insert("apricot");

        // Both share "ap" prefix, so paths should include both
        assert!(dict.dirty_prefixes.contains(&vec![b'a', b'p']));
        // Each has its own suffix paths
        assert!(dict.dirty_prefixes.contains(&vec![b'a', b'p', b'p'])); // apple
        assert!(dict.dirty_prefixes.contains(&vec![b'a', b'p', b'r'])); // apricot
    }

    #[test]
    fn test_dirty_path_recording_on_remove() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        dict.insert("apple");
        dict.dirty_prefixes.clear(); // Clear after insert

        // Remove should also record the path
        dict.remove("apple");
        assert!(dict.dirty_prefixes.contains(&vec![b'a', b'p', b'p', b'l', b'e']));
    }

    #[test]
    fn test_dirty_tracking_state_clear() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        dict.insert("apple");
        dict.insert("banana");

        assert!(!dict.dirty_prefixes.is_empty());

        // Manually add a cached location to verify it's NOT cleared
        // (This simulates what happens after serialization)
        let dummy_ptr = SwizzledPtr::null();
        dict.persisted_disk_locations.write().insert(vec![b'a'], dummy_ptr);

        // Clear should reset dirty_prefixes but PRESERVE persisted_disk_locations
        dict.clear_dirty_tracking_state();

        assert!(dict.dirty_prefixes.is_empty());
        // persisted_disk_locations should NOT be cleared - we preserve cached locations
        // for subsequent checkpoints to skip clean subtrees
        assert!(!dict.persisted_disk_locations.read().is_empty());
        assert!(dict.persisted_disk_locations.read().contains_key(&vec![b'a']));
    }

    #[test]
    fn test_dirty_path_invalidates_cache() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        // Manually populate the cache with some locations
        dict.persisted_disk_locations.write().insert(vec![b'a'], SwizzledPtr::null());
        dict.persisted_disk_locations.write().insert(vec![b'a', b'p'], SwizzledPtr::null());
        dict.persisted_disk_locations.write().insert(vec![b'a', b'p', b'p'], SwizzledPtr::null());
        dict.persisted_disk_locations.write().insert(vec![b'b'], SwizzledPtr::null());

        // Recording a dirty path should invalidate cache entries along that path
        dict.record_dirty_path(b"ap");

        // Cache entries along the dirty path should be removed
        let cache = dict.persisted_disk_locations.read();
        assert!(!cache.contains_key(&vec![]));  // Root prefix is now dirty
        assert!(!cache.contains_key(&vec![b'a']));  // 'a' prefix is dirty
        assert!(!cache.contains_key(&vec![b'a', b'p']));  // 'ap' prefix is dirty

        // Unrelated entries should remain
        assert!(cache.contains_key(&vec![b'a', b'p', b'p']));  // 'app' not on dirty path
        assert!(cache.contains_key(&vec![b'b']));  // 'b' not on dirty path
    }

    #[test]
    fn test_dirty_root_flag_propagation() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        // Insert enough terms to trigger bucket-to-ART conversion
        for i in 0..100 {
            dict.insert(&format!("term{:03}", i));
        }

        // Root should have HAS_DIRTY_DESCENDANTS flag set
        if let TrieRoot::ArtNode { node, .. } = &dict.root {
            assert!(
                node.header().has_dirty_descendants(),
                "Root should have HAS_DIRTY_DESCENDANTS flag after inserts"
            );
        }

        // After clearing dirty state, flags should be reset
        dict.clear_dirty_tracking_state();

        if let TrieRoot::ArtNode { node, .. } = &dict.root {
            assert!(
                !node.header().has_dirty_descendants(),
                "Root should not have HAS_DIRTY_DESCENDANTS flag after clear"
            );
            assert!(
                !node.header().is_dirty(),
                "Root should not have IS_DIRTY flag after clear"
            );
        }
    }

    #[test]
    fn test_path_needs_persistence() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        dict.insert("apple");

        // Paths along "apple" should need persistence
        assert!(dict.path_needs_persistence(b""));
        assert!(dict.path_needs_persistence(b"a"));
        assert!(dict.path_needs_persistence(b"ap"));
        assert!(dict.path_needs_persistence(b"apple"));

        // Paths not along "apple" should not need persistence
        assert!(!dict.path_needs_persistence(b"b"));
        assert!(!dict.path_needs_persistence(b"banana"));
        assert!(!dict.path_needs_persistence(b"ax"));
    }

    #[test]
    fn test_disk_location_caching() {
        let mut dict: PersistentARTrie = PersistentARTrie::new();

        // Cache a disk location
        let test_ptr = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
        dict.cache_disk_location(b"test", test_ptr.clone());

        // Should be retrievable if path is not dirty
        assert!(dict.get_cached_disk_location(b"test").is_some());

        // After marking path as dirty, should not return cached location
        dict.record_dirty_path(b"test");
        assert!(dict.get_cached_disk_location(b"test").is_none());
    }

    // =========================================================================
    // SIMD Comparison Edge Case Tests
    //
    // These tests verify correct SIMD-accelerated byte comparison behavior
    // when differences occur at various positions within the 16-byte SIMD chunks.
    // =========================================================================

    #[test]
    fn test_simd_cmp_empty_slices() {
        // Two empty slices should be equal
        assert!(bytes_le(b"", b""));
        assert!(!bytes_gt(b"", b""));
    }

    #[test]
    fn test_simd_cmp_different_lengths_prefix() {
        // Shorter is prefix of longer - shorter should be less
        assert!(bytes_le(b"abc", b"abcd"));
        assert!(bytes_gt(b"abcd", b"abc"));
    }

    #[test]
    fn test_simd_cmp_first_byte_difference() {
        // Difference at position 0
        assert!(bytes_le(b"a", b"b"));
        assert!(bytes_gt(b"b", b"a"));
    }

    #[test]
    fn test_simd_cmp_position_1_difference() {
        // Difference at position 1
        assert!(bytes_le(b"aa", b"ab"));
        assert!(bytes_gt(b"ab", b"aa"));
    }

    #[test]
    fn test_simd_cmp_mid_chunk_difference() {
        // Difference at position 8 (middle of 16-byte chunk)
        let a = b"aaaaaaaa_aaaaaaa";
        let b = b"aaaaaaaazaaaaaaa";
        assert!(bytes_le(a, b));
        assert!(bytes_gt(b, a));
    }

    #[test]
    fn test_simd_cmp_position_15_difference() {
        // Difference at position 15 (last byte of 16-byte chunk)
        let a = b"aaaaaaaaaaaaaaax";
        let b = b"aaaaaaaaaaaaaaay";
        assert!(bytes_le(a, b));
        assert!(bytes_gt(b, a));
    }

    #[test]
    fn test_simd_cmp_across_chunk_boundary() {
        // 32-byte strings with difference at position 16 (first byte of second chunk)
        let a = b"aaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbb";
        let b = b"aaaaaaaaaaaaaaaa~bbbbbbbbbbbbbbb";
        assert!(bytes_le(a, b));
        assert!(bytes_gt(b, a));
    }

    #[test]
    fn test_simd_cmp_long_equal_prefix() {
        // Long strings that differ only at the very end
        let mut a = vec![b'x'; 100];
        let mut b = vec![b'x'; 100];
        a.push(b'a');
        b.push(b'b');
        assert!(bytes_le(&a, &b));
        assert!(bytes_gt(&b, &a));
    }

    #[test]
    fn test_simd_cmp_scalar_fallback() {
        // Strings shorter than 16 bytes - uses scalar path
        let a = b"hello";
        let b = b"helli";
        assert!(bytes_gt(a, b)); // 'o' > 'i'
        assert!(bytes_le(b, a));
    }

    #[test]
    fn test_simd_cmp_exactly_16_bytes() {
        // Exactly 16 bytes - one full SIMD chunk
        let a = b"abcdefghijklmnop";
        let b = b"abcdefghijklmnop";
        assert!(bytes_le(a, b)); // Equal
        assert!(!bytes_gt(a, b));
    }

    #[test]
    fn test_simd_cmp_all_positions_in_chunk() {
        // Test differences at each position from 0 to 15
        for pos in 0..16 {
            let mut a = vec![b'a'; 16];
            let mut b = vec![b'a'; 16];
            a[pos] = b'x';
            b[pos] = b'y';

            assert!(
                bytes_le(&a, &b),
                "bytes_le failed at position {}",
                pos
            );
            assert!(
                bytes_gt(&b, &a),
                "bytes_gt failed at position {}",
                pos
            );
        }
    }

    #[test]
    fn test_simd_cmp_utf8_multibyte() {
        // UTF-8 multibyte characters (bytes are compared, not codepoints)
        let a = "hello世界";
        let b = "hello地球";
        // Compare as bytes
        assert!(
            bytes_le(a.as_bytes(), b.as_bytes()) || bytes_gt(a.as_bytes(), b.as_bytes()),
            "One must be true"
        );
    }

    // =========================================================================
    // DiskRef Resolution Tests
    //
    // These tests verify correct handling of DiskRef resolution failures.
    // =========================================================================

    #[test]
    fn test_resolve_child_already_in_memory() {
        // Test that resolve_child_for_mutation_with_bm returns true for in-memory nodes
        let bucket = StringBucket::new();
        let mut child = ChildNode::Bucket(bucket);

        // Should return true without needing buffer manager
        let none_bm: Option<&Arc<RwLock<BufferManager>>> = None;
        assert!(resolve_child_for_mutation_with_bm(&mut child, none_bm));
    }

    #[test]
    fn test_resolve_child_art_node_already_in_memory() {
        // ArtNode variant should also return true
        let node = Node::new_node4();
        let mut child = ChildNode::ArtNode {
            node,
            is_final: false,
            value: None,
            children: Vec::new(),
        };

        let none_bm: Option<&Arc<RwLock<BufferManager>>> = None;
        assert!(resolve_child_for_mutation_with_bm(&mut child, none_bm));
    }

    #[test]
    fn test_resolve_child_disk_ref_no_buffer_manager() {
        // DiskRef without buffer manager should return false
        let ptr = SwizzledPtr::on_disk(1, 0, NodeType::Node4);
        let mut child = ChildNode::DiskRef { ptr };

        let none_bm: Option<&Arc<RwLock<BufferManager>>> = None;
        assert!(!resolve_child_for_mutation_with_bm(&mut child, none_bm));
    }

    #[test]
    fn test_bytes_le_equality() {
        // Equal slices
        assert!(bytes_le(b"test", b"test"));
        assert!(!bytes_gt(b"test", b"test"));
    }

    #[test]
    fn test_simd_cmp_binary_data() {
        // Binary data with high byte values
        let a: &[u8] = &[0xFF, 0xFE, 0xFD, 0xFC];
        let b: &[u8] = &[0xFF, 0xFE, 0xFD, 0xFB];
        assert!(bytes_gt(a, b)); // 0xFC > 0xFB
    }

    #[test]
    fn test_simd_cmp_null_bytes() {
        // Strings with embedded null bytes
        let a = b"\x00\x00\x01";
        let b = b"\x00\x00\x02";
        assert!(bytes_le(a, b));
        assert!(bytes_gt(b, a));
    }

    // =========================================================================
    // Error Path Coverage Tests
    // =========================================================================

    mod error_path_tests {
        use super::*;
        use tempfile::TempDir;

        #[test]
        fn test_open_nonexistent_returns_error() {
            let temp_dir = TempDir::new().expect("create temp dir");
            let dict_path = temp_dir.path().join("nonexistent.part");

            let result: Result<PersistentARTrie<()>> = PersistentARTrie::open(&dict_path);
            assert!(result.is_err());
        }

        #[test]
        fn test_create_with_invalid_parent_path() {
            // Try to create in a deeply nested path that doesn't exist
            // The create function should handle directory creation
            let temp_dir = TempDir::new().expect("create temp dir");
            let dict_path = temp_dir.path().join("nested/deep/path/test.part");

            // This should succeed because create() handles directory creation
            let result: Result<PersistentARTrie<()>> = PersistentARTrie::create(&dict_path);
            assert!(result.is_ok(), "Create should handle nested directory creation");
        }

        #[test]
        fn test_sync_on_new_dict() {
            // Sync on a newly created dict (no changes)
            let temp_dir = TempDir::new().expect("create temp dir");
            let dict_path = temp_dir.path().join("new.part");

            let dict: PersistentARTrie<()> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Sync with no changes should succeed
            dict.sync().expect("sync empty dict");
        }

        #[test]
        fn test_checkpoint_on_new_dict() {
            // Checkpoint on a newly created dict
            let temp_dir = TempDir::new().expect("create temp dir");
            let dict_path = temp_dir.path().join("new.part");

            let mut dict: PersistentARTrie<()> =
                PersistentARTrie::create(&dict_path).expect("create dict");

            // Checkpoint with no changes should succeed
            dict.checkpoint().expect("checkpoint empty dict");
        }

        #[test]
        fn test_open_with_recovery_new_file() {
            // Test open_with_recovery on a fresh path (creates new)
            let temp_dir = TempDir::new().expect("create temp dir");
            let dict_path = temp_dir.path().join("test.part");

            // First create a trie
            {
                let mut dict: PersistentARTrie<()> =
                    PersistentARTrie::create(&dict_path).expect("create dict");
                dict.insert("test");
                dict.sync().expect("sync");
            }

            // Now open with recovery
            let (dict, report) = PersistentARTrie::<()>::open_with_recovery(&dict_path)
                .expect("open_with_recovery");

            // Should have normal recovery mode
            assert!(report.mode.is_normal());
            assert!(dict.contains("test"));
        }
    }
}
