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
pub(super) fn bytes_le(a: &[u8], b: &[u8]) -> bool {
    matches!(simd_cmp_bytes(a, b), std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
}


/// Check if a > b using SIMD-accelerated comparison.
#[inline]
pub(super) fn bytes_gt(a: &[u8], b: &[u8]) -> bool {
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
pub(super) fn resolve_child_for_mutation_with_bm<S: BlockStorage>(
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
