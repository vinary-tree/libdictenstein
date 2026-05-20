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


/// Check if a > b using SIMD-accelerated comparison.
#[inline]
fn bytes_gt(a: &[u8], b: &[u8]) -> bool {
    matches!(simd_cmp_bytes(a, b), std::cmp::Ordering::Greater)
}

/// Maximum buffer size for reading serialized ART nodes (4KB should be ample).
/// Largest node is Node256 at ~2KB, so 4KB provides good margin.
pub(super) const ART_NODE_BUFFER_SIZE: usize = 4096;

/// Result of loading a single child node's data without loading its children.
///
/// Used by `load_single_child_data` for iterative loading.
pub(super) enum SingleChildData {
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


// === io_uring convenience constructors (Linux-only, requires `io-uring-backend` feature) ===


// === Generic methods (work with any BlockStorage backend) ===

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {

    /// Insert a term into the dictionary (without value)


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
    pub(super) fn get_root_node(&self) -> PersistentARTrieNode<V> {
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
    ///
    /// `pub(super)` so the parallel-merge extension trait in
    /// `crate::persistent_artrie::parallel_merge` (gated on the
    /// `parallel-merge` feature) can call it during the
    /// sequential-write phase of `merge_from_parallel`.
    pub(super) fn insert_impl(&mut self, term: &[u8], value: Option<V>) -> bool {
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
    pub(super) fn insert_impl_core(&mut self, term: &[u8], value: Option<V>) -> bool {
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
    pub(super) fn remove_impl(&mut self, term: &[u8]) -> bool {
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
    pub(super) fn remove_impl_core(&mut self, term: &[u8]) -> bool {
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
    pub(super) fn insert_impl_no_wal(&mut self, term: &[u8], value: Option<V>) -> bool {
        // Call core implementation directly to skip WAL logging
        self.insert_impl_core(term, value)
    }

    /// Remove implementation without WAL logging (for recovery replay).
    ///
    /// This is used during WAL recovery to avoid re-logging operations
    /// that are already in the WAL.
    pub(super) fn remove_impl_no_wal(&mut self, term: &[u8]) -> bool {
        // Call core implementation directly to skip WAL logging
        self.remove_impl_core(term)
    }

    /// Upsert implementation without WAL logging (for recovery replay).
    ///
    /// This updates the value if the term exists, or inserts if it doesn't.
    /// Used during WAL recovery to replay Upsert, Increment, and CAS operations.
    pub(super) fn upsert_impl_no_wal(&mut self, term: &[u8], value: V) {
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
    pub(super) fn contains_impl(&self, term: &[u8]) -> bool {
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
    ///
    /// `pub(super)` for the parallel-merge extension trait (see also
    /// `insert_impl` above).
    pub(super) fn get_value_impl(&self, term: &[u8]) -> Option<V> {
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
pub(super) const ROOT_TYPE_EMPTY: u8 = 0;
pub(super) const ROOT_TYPE_BUCKET: u8 = 1;
pub(super) const ROOT_TYPE_ART_NODE: u8 = 2;




// `SharedARTrieParallelExt` trait + its blanket impl on `SharedARTrie<V>`
// (feature-gated on `parallel-merge`) were relocated to the sibling
// `super::parallel_merge` module; re-exported here under their original
// paths.
#[cfg(feature = "parallel-merge")]
pub use super::parallel_merge::SharedARTrieParallelExt;

// Document-transaction methods (begin_document / tx_insert* /
// commit_document / abort_document) moved to sibling
// `super::document_tx` module in Phase-5 decomposition. Data
// carriers (`DocumentTransaction` / `TransactionState`) live in
// `super::transactions`.



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
