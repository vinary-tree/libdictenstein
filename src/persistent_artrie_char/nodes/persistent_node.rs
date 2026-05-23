//! Lock-Free Persistent Character Node
//!
//! This module provides a lock-free node implementation using persistent (immutable)
//! data structures. Modifications create new versions of the node rather than
//! mutating in place, enabling lock-free concurrent access.
//!
//! # Design
//!
//! Child storage uses a tiered `ChildStore` enum:
//!
//! ```text
//! ChildStore::Inline  (0-4 children, ~85% of nodes)
//!   → Zero heap allocation. Clone is pure memcpy.
//!   → Linear scan for lookups (faster than binary search at this size).
//!
//! ChildStore::Heap    (5+ children, ~15% of nodes)
//!   → Owned Vec<u32> + Vec<SwizzledPtr>. Clone is flat contiguous memcpy.
//!   → Binary search for lookups.
//! ```
//!
//! This replaces the previous `im::Vector`-based design which used Arc-based
//! structural sharing (RRB-tree), causing ~7.2 GB peak memory from COW cloning
//! and ~45% CPU time in `__mprotect` syscalls from glibc mmap for Arc allocations.
//!
//! For lock-free concurrent updates, we use CAS on a pointer to the node:
//!
//! ```text
//! Thread 1                    Thread 2
//! --------                    --------
//! Load current node           Load current node
//! Create new version          Create new version
//! CAS(old → new)              CAS(old → new)
//!   ↓                           ↓
//! Success!                    Fail (retry with new node)
//! ```
//!
//! # Memory Management
//!
//! Nodes are wrapped in `Arc` for shared ownership. Old versions are reclaimed
//! when their reference count drops to zero.
//!
//! # Example
//!
//! ```text
//! use libdictenstein::persistent_artrie_char::nodes::persistent_node::PersistentCharNode;
//!
//! let node = PersistentCharNode::new();
//! let child_ptr = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);
//!
//! // Create a new version with the child - original is unchanged
//! let node2 = node.with_child('a' as u32, child_ptr);
//!
//! assert_eq!(node.num_children(), 0);  // Original unchanged
//! assert_eq!(node2.num_children(), 1); // New version has child
//! ```

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

/// Node flags (same as existing char node flags for compatibility)
pub mod flags {
    /// Node represents a valid dictionary entry (is_final)
    pub const IS_FINAL: u8 = 0b0000_0001;
    /// Node has been modified (dirty)
    pub const IS_DIRTY: u8 = 0b0000_0010;
    /// Node is a leaf (bucket pointer)
    pub const IS_LEAF: u8 = 0b0000_0100;
    /// Node has a value assigned
    pub const HAS_VALUE: u8 = 0b0000_1000;
}

/// Maximum prefix length in characters (u32 values)
pub const MAX_PREFIX_LEN: usize = 6;

/// Maximum number of children in the inline storage tier.
///
/// Nodes with 0-4 children use fully inline storage (zero heap allocation).
/// Adding a 5th child promotes to the Heap tier.
///
/// Threshold of 4 is optimal because:
/// - 4 u32 keys (16 bytes) + 4 SwizzledPtr children (32 bytes) fit in a cache line pair
/// - Linear scan of 4 elements is faster than binary search
/// - ~85% of trie nodes have ≤4 children (empirical from vocabulary tries)
const INLINE_CAPACITY: usize = 4;

// ============================================================================
// ChildStore: Tiered child storage for PersistentCharNode
// ============================================================================

/// Tiered child storage that eliminates heap allocation for 85% of nodes.
///
/// # Variants
///
/// - **Inline**: 0-4 children stored directly in the struct. Zero heap allocation.
///   Clone is pure value copy. Linear scan for lookups.
///
/// - **Heap**: 5+ children in owned `Vec`s. Contiguous memory layout for
///   cache-friendly iteration. Binary search for lookups.
///
/// # Why Not `im::Vector`?
///
/// The previous design used `im::Vector<u32>` + `im::Vector<SwizzledPtr>` for
/// structural sharing via RRB-trees. Profiling showed this caused:
///
/// - **7.22 GB peak memory** from `Arc::make_mut` COW cloning during `with_child`
/// - **4.23 GB** from `Arc::clone_from_ref_in` for lock-free node references
/// - **~45% CPU** in `__mprotect` syscalls from glibc mmap for Arc allocations
///
/// The new tiered design eliminates all Arc/RRB-tree overhead:
/// - Inline: pure memcpy, no allocation
/// - Heap: flat Vec clone (single contiguous memcpy per Vec)
#[derive(Debug)]
enum ChildStore {
    /// 0-4 children stored inline (no heap allocation).
    ///
    /// Keys are sorted in ascending order. Unused slots contain
    /// uninitialized values (only `keys[..count]` and `children[..count]`
    /// are valid).
    Inline {
        /// Number of valid children (0-4).
        count: u8,
        /// Sorted child keys (Unicode code points). Only `[..count]` is valid.
        keys: [u32; INLINE_CAPACITY],
        /// Child pointers corresponding to keys. Only `[..count]` is valid.
        children: [SwizzledPtr; INLINE_CAPACITY],
    },

    /// 5+ children in owned Vecs.
    ///
    /// Keys are sorted in ascending order. Both Vecs always have the same length.
    Heap {
        /// Sorted child keys (Unicode code points).
        keys: Vec<u32>,
        /// Child pointers corresponding to keys.
        children: Vec<SwizzledPtr>,
    },
}

impl ChildStore {
    /// Create an empty inline child store.
    #[inline]
    fn new() -> Self {
        ChildStore::Inline {
            count: 0,
            keys: [0u32; INLINE_CAPACITY],
            children: [
                SwizzledPtr::null(),
                SwizzledPtr::null(),
                SwizzledPtr::null(),
                SwizzledPtr::null(),
            ],
        }
    }

    /// Number of children.
    #[inline]
    fn len(&self) -> usize {
        match self {
            ChildStore::Inline { count, .. } => *count as usize,
            ChildStore::Heap { keys, .. } => keys.len(),
        }
    }

    /// Check if empty.
    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Find a child by key.
    ///
    /// Uses linear scan for Inline (optimal for ≤4 elements),
    /// binary search for Heap.
    #[inline]
    fn find_child(&self, key: u32) -> Option<&SwizzledPtr> {
        match self {
            ChildStore::Inline {
                count,
                keys,
                children,
            } => {
                let n = *count as usize;
                // Linear scan — faster than binary search for ≤4 elements
                for i in 0..n {
                    if keys[i] == key {
                        return Some(&children[i]);
                    }
                    // Keys are sorted; early exit if we've passed the target
                    if keys[i] > key {
                        return None;
                    }
                }
                None
            }
            ChildStore::Heap { keys, children } => match keys.binary_search(&key) {
                Ok(idx) => Some(&children[idx]),
                Err(_) => None,
            },
        }
    }

    /// Check if a child exists for the given key.
    #[inline]
    fn has_child(&self, key: u32) -> bool {
        self.find_child(key).is_some()
    }

    /// Get the child at a specific index.
    #[inline]
    fn child_at(&self, index: usize) -> Option<(&u32, &SwizzledPtr)> {
        match self {
            ChildStore::Inline {
                count,
                keys,
                children,
            } => {
                if index < *count as usize {
                    Some((&keys[index], &children[index]))
                } else {
                    None
                }
            }
            ChildStore::Heap { keys, children } => {
                if index < keys.len() {
                    Some((&keys[index], &children[index]))
                } else {
                    None
                }
            }
        }
    }

    /// Get key and child slices for iteration.
    #[inline]
    fn slices(&self) -> (&[u32], &[SwizzledPtr]) {
        match self {
            ChildStore::Inline {
                count,
                keys,
                children,
            } => {
                let n = *count as usize;
                (&keys[..n], &children[..n])
            }
            ChildStore::Heap { keys, children } => (keys.as_slice(), children.as_slice()),
        }
    }

    /// Create a new ChildStore with a child added (or replaced if key exists).
    ///
    /// Maintains sorted key order. Promotes from Inline to Heap when adding
    /// a 5th child.
    fn with_child(&self, key: u32, child: SwizzledPtr) -> Self {
        match self {
            ChildStore::Inline {
                count,
                keys,
                children,
            } => {
                let n = *count as usize;

                // Find insertion point or existing key
                let mut insert_pos = n;
                for i in 0..n {
                    if keys[i] == key {
                        // Key exists — replace the child
                        let new_keys = *keys;
                        let mut new_children = clone_swizzled_array(children);
                        new_children[i] = child;
                        return ChildStore::Inline {
                            count: *count,
                            keys: new_keys,
                            children: new_children,
                        };
                    }
                    if keys[i] > key {
                        insert_pos = i;
                        break;
                    }
                }

                if n < INLINE_CAPACITY {
                    // Room in inline — shift right and insert
                    let mut new_keys = *keys;
                    let mut new_children = clone_swizzled_array(children);

                    // Shift elements right from insert_pos
                    for i in (insert_pos..n).rev() {
                        new_keys[i + 1] = new_keys[i];
                        new_children[i + 1] = new_children[i].clone();
                    }
                    new_keys[insert_pos] = key;
                    new_children[insert_pos] = child;

                    ChildStore::Inline {
                        count: *count + 1,
                        keys: new_keys,
                        children: new_children,
                    }
                } else {
                    // Promote to Heap: copy 4 existing + insert 1 new = 5
                    let mut new_keys = Vec::with_capacity(n + 1);
                    let mut new_children = Vec::with_capacity(n + 1);

                    for i in 0..insert_pos {
                        new_keys.push(keys[i]);
                        new_children.push(children[i].clone());
                    }
                    new_keys.push(key);
                    new_children.push(child);
                    for i in insert_pos..n {
                        new_keys.push(keys[i]);
                        new_children.push(children[i].clone());
                    }

                    ChildStore::Heap {
                        keys: new_keys,
                        children: new_children,
                    }
                }
            }
            ChildStore::Heap { keys, children } => {
                match keys.binary_search(&key) {
                    Ok(idx) => {
                        // Key exists — replace the child
                        let mut new_children = clone_swizzled_vec(children);
                        new_children[idx] = child;
                        ChildStore::Heap {
                            keys: keys.clone(),
                            children: new_children,
                        }
                    }
                    Err(idx) => {
                        // Insert at sorted position
                        let mut new_keys = keys.clone();
                        let mut new_children = clone_swizzled_vec(children);
                        new_keys.insert(idx, key);
                        new_children.insert(idx, child);
                        ChildStore::Heap {
                            keys: new_keys,
                            children: new_children,
                        }
                    }
                }
            }
        }
    }

    /// Create a new ChildStore with a child removed.
    ///
    /// Returns `None` if the key doesn't exist. Demotes from Heap to Inline
    /// when the child count drops to INLINE_CAPACITY.
    fn without_child(&self, key: u32) -> Option<Self> {
        match self {
            ChildStore::Inline {
                count,
                keys,
                children,
            } => {
                let n = *count as usize;

                // Find the key
                let mut found_pos = None;
                for i in 0..n {
                    if keys[i] == key {
                        found_pos = Some(i);
                        break;
                    }
                    if keys[i] > key {
                        return None; // Keys are sorted; not found
                    }
                }

                let pos = found_pos?;

                // Shift elements left
                let mut new_keys = *keys;
                let mut new_children = clone_swizzled_array(children);

                for i in pos..n - 1 {
                    new_keys[i] = new_keys[i + 1];
                    new_children[i] = new_children[i + 1].clone();
                }
                // Clear the now-unused last slot
                new_keys[n - 1] = 0;
                new_children[n - 1] = SwizzledPtr::null();

                Some(ChildStore::Inline {
                    count: *count - 1,
                    keys: new_keys,
                    children: new_children,
                })
            }
            ChildStore::Heap { keys, children } => {
                let idx = keys.binary_search(&key).ok()?;

                let new_len = keys.len() - 1;

                if new_len <= INLINE_CAPACITY {
                    // Demote to Inline
                    let mut new_keys = [0u32; INLINE_CAPACITY];
                    let mut new_children = [
                        SwizzledPtr::null(),
                        SwizzledPtr::null(),
                        SwizzledPtr::null(),
                        SwizzledPtr::null(),
                    ];

                    let mut j = 0;
                    for i in 0..keys.len() {
                        if i != idx {
                            new_keys[j] = keys[i];
                            new_children[j] = children[i].clone();
                            j += 1;
                        }
                    }

                    Some(ChildStore::Inline {
                        count: new_len as u8,
                        keys: new_keys,
                        children: new_children,
                    })
                } else {
                    // Stay Heap
                    let mut new_keys = keys.clone();
                    let mut new_children = clone_swizzled_vec(children);
                    new_keys.remove(idx);
                    new_children.remove(idx);
                    Some(ChildStore::Heap {
                        keys: new_keys,
                        children: new_children,
                    })
                }
            }
        }
    }

    /// Estimated memory usage in bytes.
    fn memory_usage(&self) -> usize {
        match self {
            ChildStore::Inline { count, .. } => {
                // The inline arrays are part of the struct — no heap allocation.
                // Report the logical usage (valid elements only).
                let n = *count as usize;
                n * (std::mem::size_of::<u32>() + std::mem::size_of::<SwizzledPtr>())
            }
            ChildStore::Heap { keys, children } => {
                keys.capacity() * std::mem::size_of::<u32>()
                    + children.capacity() * std::mem::size_of::<SwizzledPtr>()
            }
        }
    }
}

impl Clone for ChildStore {
    fn clone(&self) -> Self {
        match self {
            ChildStore::Inline {
                count,
                keys,
                children,
            } => ChildStore::Inline {
                count: *count,
                keys: *keys,
                children: clone_swizzled_array(children),
            },
            ChildStore::Heap { keys, children } => ChildStore::Heap {
                keys: keys.clone(),
                children: clone_swizzled_vec(children),
            },
        }
    }
}

/// Clone a fixed-size array of SwizzledPtrs.
///
/// SwizzledPtr wraps AtomicU64 which doesn't implement Copy,
/// so we clone each element individually.
#[inline]
fn clone_swizzled_array(src: &[SwizzledPtr; INLINE_CAPACITY]) -> [SwizzledPtr; INLINE_CAPACITY] {
    [
        src[0].clone(),
        src[1].clone(),
        src[2].clone(),
        src[3].clone(),
    ]
}

/// Clone a Vec of SwizzledPtrs.
#[inline]
fn clone_swizzled_vec(src: &[SwizzledPtr]) -> Vec<SwizzledPtr> {
    src.iter().map(|p| p.clone()).collect()
}

// ============================================================================
// PersistentCharNode
// ============================================================================

/// A lock-free persistent character node using tiered child storage.
///
/// This node type uses `ChildStore` for keys and children, enabling efficient
/// zero-allocation storage for the 85% of nodes with ≤4 children. All
/// modifications return a new node rather than mutating in place.
///
/// # Thread Safety
///
/// Individual nodes are immutable after creation (except for atomic flags/value).
/// Thread-safe concurrent access is achieved by CAS-swapping pointers to nodes
/// using `AtomicNodePtr`.
///
/// # Memory Layout
///
/// The node stores:
/// - `version`: Monotonic version counter for detecting modifications
/// - `store`: Tiered child storage (Inline for 0-4, Heap for 5+)
/// - `flags`: Atomic flags (IS_FINAL, IS_DIRTY, etc.)
/// - `value`: Atomic value for final nodes (vocabulary index)
/// - `prefix`: Compressed path prefix for path compression
#[derive(Debug)]
pub struct PersistentCharNode {
    /// Monotonic version counter (incremented on each modification)
    version: AtomicU64,

    /// Tiered child storage (Inline for 0-4 children, Heap for 5+)
    store: ChildStore,

    /// Node flags (IS_FINAL, IS_DIRTY, IS_LEAF, HAS_VALUE)
    /// Atomic to allow setting final flag during concurrent insert race
    flags: AtomicU8,

    /// Value for final nodes (e.g., vocabulary index)
    /// Atomic to allow setting value during concurrent insert race
    value: AtomicU64,

    /// Compressed prefix for path compression (up to 6 chars)
    prefix: Arc<[u32]>,

    /// Length of the valid prefix (may be less than prefix.len())
    prefix_len: u8,
}

impl PersistentCharNode {
    /// Create a new empty node.
    pub fn new() -> Self {
        Self {
            version: AtomicU64::new(0),
            store: ChildStore::new(),
            flags: AtomicU8::new(0),
            value: AtomicU64::new(0),
            prefix: Arc::new([]),
            prefix_len: 0,
        }
    }

    /// Create a new node with a prefix.
    pub fn with_prefix(prefix: &[u32]) -> Self {
        let prefix_len = prefix.len().min(MAX_PREFIX_LEN) as u8;
        let prefix_data: Arc<[u32]> = prefix[..prefix_len as usize].into();

        Self {
            version: AtomicU64::new(0),
            store: ChildStore::new(),
            flags: AtomicU8::new(0),
            value: AtomicU64::new(0),
            prefix: prefix_data,
            prefix_len,
        }
    }

    /// Get the current version number.
    #[inline]
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Get the number of children.
    #[inline]
    pub fn num_children(&self) -> usize {
        self.store.len()
    }

    /// Check if the node is empty (no children).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Get the prefix as a slice.
    #[inline]
    pub fn prefix(&self) -> &[u32] {
        &self.prefix[..self.prefix_len as usize]
    }

    /// Get the prefix length.
    #[inline]
    pub fn prefix_len(&self) -> usize {
        self.prefix_len as usize
    }

    /// Check if this node is final (represents a complete word).
    #[inline]
    pub fn is_final(&self) -> bool {
        self.flags.load(Ordering::Acquire) & flags::IS_FINAL != 0
    }

    /// Check if this node has a value assigned.
    #[inline]
    pub fn has_value(&self) -> bool {
        self.flags.load(Ordering::Acquire) & flags::HAS_VALUE != 0
    }

    /// Get the node value (vocabulary index for final nodes).
    #[inline]
    pub fn get_value(&self) -> Option<u64> {
        if self.has_value() {
            Some(self.value.load(Ordering::Acquire))
        } else {
            None
        }
    }

    /// Atomically try to set the final flag.
    ///
    /// This is used during concurrent insertion when multiple threads
    /// race to finalize the same node. Only one thread will succeed.
    ///
    /// # Returns
    ///
    /// - `true` if this call set the flag (winner of the race)
    /// - `false` if the flag was already set (lost the race)
    #[inline]
    pub fn try_set_final(&self) -> bool {
        let old = self.flags.fetch_or(flags::IS_FINAL, Ordering::AcqRel);
        (old & flags::IS_FINAL) == 0
    }

    /// Atomically try to set the value.
    ///
    /// This uses compare-and-swap to ensure only one thread sets the value.
    /// Should only be called after `try_set_final()` succeeds.
    ///
    /// # Arguments
    ///
    /// * `value` - The value to set (e.g., vocabulary index)
    ///
    /// # Returns
    ///
    /// - `true` if this call set the value
    /// - `false` if value was already set
    #[inline]
    pub fn try_set_value(&self, value: u64) -> bool {
        // First try to set the HAS_VALUE flag
        let old_flags = self.flags.fetch_or(flags::HAS_VALUE, Ordering::AcqRel);
        if (old_flags & flags::HAS_VALUE) != 0 {
            // Value already set by another thread
            return false;
        }

        // We won the race to set the flag, now set the value
        self.value.store(value, Ordering::Release);
        true
    }

    /// Atomically increment the value by delta.
    ///
    /// Sets HAS_VALUE flag if not already set, then adds delta to the current value.
    /// Multiple threads can safely increment concurrently (wait-free for existing values).
    ///
    /// # Arguments
    ///
    /// * `delta` - The amount to add to the current value
    ///
    /// # Returns
    ///
    /// The new value after increment.
    #[inline]
    pub fn increment_value(&self, delta: u64) -> u64 {
        self.flags.fetch_or(flags::HAS_VALUE, Ordering::AcqRel);
        self.value.fetch_add(delta, Ordering::Relaxed) + delta
    }

    /// Find a child by key (lock-free read).
    ///
    /// Uses linear scan for Inline nodes (≤4 children) and binary search
    /// for Heap nodes (5+ children).
    ///
    /// # Arguments
    ///
    /// * `key` - The child key (Unicode code point)
    #[inline]
    pub fn find_child(&self, key: u32) -> Option<&SwizzledPtr> {
        self.store.find_child(key)
    }

    /// Check if a child exists for the given key.
    #[inline]
    pub fn has_child(&self, key: u32) -> bool {
        self.store.has_child(key)
    }

    /// Get the child at a specific index.
    #[inline]
    pub fn child_at(&self, index: usize) -> Option<(&u32, &SwizzledPtr)> {
        self.store.child_at(index)
    }

    /// Iterate over all (key, child) pairs.
    pub fn iter_children(&self) -> impl Iterator<Item = (&u32, &SwizzledPtr)> {
        let (keys, children) = self.store.slices();
        keys.iter().zip(children.iter())
    }

    /// Create a new version of this node with an added child.
    ///
    /// This does NOT modify the current node - it returns a new node
    /// with the child added. For Inline nodes (≤4 children), this is
    /// a pure value copy with zero heap allocation.
    ///
    /// # Arguments
    ///
    /// * `key` - The key for the new child
    /// * `child` - The child pointer
    ///
    /// # Returns
    ///
    /// A new node with the child added (or replaced if key exists).
    pub fn with_child(&self, key: u32, child: SwizzledPtr) -> Self {
        Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire) + 1),
            store: self.store.with_child(key, child),
            flags: AtomicU8::new(self.flags.load(Ordering::Acquire)),
            value: AtomicU64::new(self.value.load(Ordering::Acquire)),
            prefix: self.prefix.clone(),
            prefix_len: self.prefix_len,
        }
    }

    /// Create a new version with a child removed.
    ///
    /// # Arguments
    ///
    /// * `key` - The key of the child to remove
    ///
    /// # Returns
    ///
    /// - `Some(new_node)` if the key existed and was removed
    /// - `None` if the key didn't exist
    pub fn without_child(&self, key: u32) -> Option<Self> {
        self.store.without_child(key).map(|new_store| Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire) + 1),
            store: new_store,
            flags: AtomicU8::new(self.flags.load(Ordering::Acquire)),
            value: AtomicU64::new(self.value.load(Ordering::Acquire)),
            prefix: self.prefix.clone(),
            prefix_len: self.prefix_len,
        })
    }

    /// Create a new version with a different prefix.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The new prefix
    pub fn with_prefix_replaced(&self, prefix: &[u32]) -> Self {
        let prefix_len = prefix.len().min(MAX_PREFIX_LEN) as u8;
        let prefix_data: Arc<[u32]> = prefix[..prefix_len as usize].into();

        Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire) + 1),
            store: self.store.clone(),
            flags: AtomicU8::new(self.flags.load(Ordering::Acquire)),
            value: AtomicU64::new(self.value.load(Ordering::Acquire)),
            prefix: prefix_data,
            prefix_len,
        }
    }

    /// Create a new version marked as final.
    pub fn as_final(&self) -> Self {
        Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire) + 1),
            store: self.store.clone(),
            flags: AtomicU8::new(self.flags.load(Ordering::Acquire) | flags::IS_FINAL),
            value: AtomicU64::new(self.value.load(Ordering::Acquire)),
            prefix: self.prefix.clone(),
            prefix_len: self.prefix_len,
        }
    }

    /// Create a new version with a value set.
    pub fn with_value(&self, value: u64) -> Self {
        Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire) + 1),
            store: self.store.clone(),
            flags: AtomicU8::new(self.flags.load(Ordering::Acquire) | flags::HAS_VALUE),
            value: AtomicU64::new(value),
            prefix: self.prefix.clone(),
            prefix_len: self.prefix_len,
        }
    }

    /// Match this node's prefix against a key slice.
    ///
    /// Returns the number of matching characters (0 to prefix_len).
    pub fn match_prefix(&self, key: &[u32]) -> usize {
        let prefix = self.prefix();
        let check_len = prefix.len().min(key.len());

        for i in 0..check_len {
            if prefix[i] != key[i] {
                return i;
            }
        }
        check_len
    }

    /// Check if this node's prefix fully matches the beginning of the key.
    #[inline]
    pub fn prefix_matches(&self, key: &[u32]) -> bool {
        self.match_prefix(key) == self.prefix_len()
    }

    /// Get estimated memory usage in bytes.
    pub fn memory_usage(&self) -> usize {
        // Base struct size
        let base = std::mem::size_of::<Self>();

        // Child store (heap portion only — inline is part of base)
        let store_heap = self.store.memory_usage();

        // Prefix Arc
        let prefix_size = self.prefix.len() * std::mem::size_of::<u32>();

        base + store_heap + prefix_size
    }
}

impl Default for PersistentCharNode {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for PersistentCharNode {
    fn clone(&self) -> Self {
        Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire)),
            store: self.store.clone(),
            flags: AtomicU8::new(self.flags.load(Ordering::Acquire)),
            value: AtomicU64::new(self.value.load(Ordering::Acquire)),
            prefix: self.prefix.clone(),
            prefix_len: self.prefix_len,
        }
    }
}

// SAFETY: PersistentCharNode is shared through Arc in the lock-free overlay.
// Child storage and prefixes are path-copy data and are never mutated through
// shared references after publication. The remaining mutable fields are
// atomics, and child publication uses SwizzledPtr's atomic raw value.
unsafe impl Send for PersistentCharNode {}
unsafe impl Sync for PersistentCharNode {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_artrie::NodeType;

    #[test]
    fn test_new_node() {
        let node = PersistentCharNode::new();
        assert_eq!(node.num_children(), 0);
        assert!(node.is_empty());
        assert!(!node.is_final());
        assert!(!node.has_value());
        assert_eq!(node.version(), 0);
    }

    #[test]
    fn test_with_prefix() {
        let prefix: Vec<u32> = "hello".chars().map(|c| c as u32).collect();
        let node = PersistentCharNode::with_prefix(&prefix);

        assert_eq!(node.prefix_len(), 5);
        let stored_prefix: Vec<char> = node
            .prefix()
            .iter()
            .filter_map(|&c| char::from_u32(c))
            .collect();
        assert_eq!(stored_prefix, vec!['h', 'e', 'l', 'l', 'o']);
    }

    #[test]
    fn test_prefix_max_length() {
        // Prefix longer than MAX_PREFIX_LEN should be truncated
        let prefix: Vec<u32> = "abcdefghij".chars().map(|c| c as u32).collect();
        let node = PersistentCharNode::with_prefix(&prefix);

        assert_eq!(node.prefix_len(), MAX_PREFIX_LEN);
    }

    #[test]
    fn test_with_child_immutability() {
        let node = PersistentCharNode::new();
        let child = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);

        let node2 = node.with_child('a' as u32, child);

        // Original should be unchanged
        assert_eq!(node.num_children(), 0);

        // New version should have the child
        assert_eq!(node2.num_children(), 1);
        assert!(node2.has_child('a' as u32));
    }

    #[test]
    fn test_with_child_sorted_order() {
        let mut node = PersistentCharNode::new();

        // Add children in random order
        let keys = ['z', 'a', 'm', 'f'];
        for &k in &keys {
            let child = SwizzledPtr::on_disk(k as u32, 0, NodeType::CharNode4);
            node = node.with_child(k as u32, child);
        }

        assert_eq!(node.num_children(), 4);

        // Verify sorted order
        let collected_keys: Vec<u32> = node.iter_children().map(|(&k, _)| k).collect();
        assert_eq!(
            collected_keys,
            vec!['a' as u32, 'f' as u32, 'm' as u32, 'z' as u32]
        );
    }

    #[test]
    fn test_with_child_replace() {
        let child1 = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);
        let child2 = SwizzledPtr::on_disk(2, 200, NodeType::CharNode4);
        let child2_raw = child2.to_raw();

        let node = PersistentCharNode::new().with_child('a' as u32, child1);

        assert_eq!(node.num_children(), 1);

        // Replace existing child
        let node2 = node.with_child('a' as u32, child2);

        // Should still have only 1 child
        assert_eq!(node2.num_children(), 1);

        // Check the child was replaced (different raw value)
        let found = node2.find_child('a' as u32).expect("should find child");
        assert_eq!(found.to_raw(), child2_raw);
    }

    #[test]
    fn test_without_child() {
        let child = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);
        let node = PersistentCharNode::new()
            .with_child('a' as u32, child.clone())
            .with_child('b' as u32, child.clone())
            .with_child('c' as u32, child);

        assert_eq!(node.num_children(), 3);

        // Remove middle child
        let node2 = node.without_child('b' as u32).expect("should remove");
        assert_eq!(node2.num_children(), 2);
        assert!(node2.has_child('a' as u32));
        assert!(!node2.has_child('b' as u32));
        assert!(node2.has_child('c' as u32));

        // Original unchanged
        assert_eq!(node.num_children(), 3);
    }

    #[test]
    fn test_without_child_not_found() {
        let node = PersistentCharNode::new();
        assert!(node.without_child('x' as u32).is_none());
    }

    #[test]
    fn test_find_child() {
        let child = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);
        let node = PersistentCharNode::new()
            .with_child('a' as u32, child.clone())
            .with_child('b' as u32, child);

        assert!(node.find_child('a' as u32).is_some());
        assert!(node.find_child('b' as u32).is_some());
        assert!(node.find_child('c' as u32).is_none());
    }

    #[test]
    fn test_try_set_final() {
        let node = PersistentCharNode::new();

        // First call should succeed
        assert!(node.try_set_final());
        assert!(node.is_final());

        // Second call should fail
        assert!(!node.try_set_final());
        assert!(node.is_final());
    }

    #[test]
    fn test_try_set_value() {
        let node = PersistentCharNode::new();

        // First call should succeed
        assert!(node.try_set_value(42));
        assert!(node.has_value());
        assert_eq!(node.get_value(), Some(42));

        // Second call should fail
        assert!(!node.try_set_value(100));
        assert_eq!(node.get_value(), Some(42)); // Value unchanged
    }

    #[test]
    fn test_version_increment() {
        let node = PersistentCharNode::new();
        assert_eq!(node.version(), 0);

        let child = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);
        let node2 = node.with_child('a' as u32, child);
        assert_eq!(node2.version(), 1);

        let node3 = node2.as_final();
        assert_eq!(node3.version(), 2);
    }

    #[test]
    fn test_prefix_matching() {
        let prefix: Vec<u32> = "hello".chars().map(|c| c as u32).collect();
        let node = PersistentCharNode::with_prefix(&prefix);

        // Full match
        let key: Vec<u32> = "helloworld".chars().map(|c| c as u32).collect();
        assert_eq!(node.match_prefix(&key), 5);
        assert!(node.prefix_matches(&key));

        // Partial match
        let key: Vec<u32> = "help".chars().map(|c| c as u32).collect();
        assert_eq!(node.match_prefix(&key), 3);
        assert!(!node.prefix_matches(&key));

        // No match
        let key: Vec<u32> = "world".chars().map(|c| c as u32).collect();
        assert_eq!(node.match_prefix(&key), 0);
        assert!(!node.prefix_matches(&key));
    }

    #[test]
    fn test_unicode_children() {
        let mut node = PersistentCharNode::new();

        // Add Unicode characters (emoji, CJK)
        let keys = ['a', 'z', '日', '本', '🎉', 'α'];
        for &k in &keys {
            let child = SwizzledPtr::on_disk(k as u32, 0, NodeType::CharNode4);
            node = node.with_child(k as u32, child);
        }

        assert_eq!(node.num_children(), 6);

        // All should be findable
        for &k in &keys {
            assert!(node.has_child(k as u32), "should find '{}'", k);
        }

        // Keys should be sorted by code point value
        let collected: Vec<u32> = node.iter_children().map(|(&k, _)| k).collect();
        let mut expected: Vec<u32> = keys.iter().map(|&c| c as u32).collect();
        expected.sort();
        assert_eq!(collected, expected);
    }

    #[test]
    fn test_clone() {
        let node = PersistentCharNode::new();
        let child = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);
        let node = node.with_child('a' as u32, child);
        node.try_set_final();
        node.try_set_value(42);

        let cloned = node.clone();

        assert_eq!(cloned.num_children(), 1);
        assert!(cloned.is_final());
        assert_eq!(cloned.get_value(), Some(42));
        assert_eq!(cloned.version(), node.version());
    }

    #[test]
    fn test_as_final() {
        let node = PersistentCharNode::new();
        assert!(!node.is_final());

        let final_node = node.as_final();
        assert!(final_node.is_final());

        // Original unchanged
        assert!(!node.is_final());
    }

    #[test]
    fn test_with_value() {
        let node = PersistentCharNode::new();
        assert!(!node.has_value());

        let valued_node = node.with_value(123);
        assert!(valued_node.has_value());
        assert_eq!(valued_node.get_value(), Some(123));

        // Original unchanged
        assert!(!node.has_value());
    }

    #[test]
    fn test_iter_children() {
        let child = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);
        let node = PersistentCharNode::new()
            .with_child('c' as u32, child.clone())
            .with_child('a' as u32, child.clone())
            .with_child('b' as u32, child);

        let pairs: Vec<(u32, u64)> = node
            .iter_children()
            .map(|(&k, c)| (k, c.to_raw()))
            .collect();

        assert_eq!(pairs.len(), 3);
        // Should be sorted
        assert_eq!(pairs[0].0, 'a' as u32);
        assert_eq!(pairs[1].0, 'b' as u32);
        assert_eq!(pairs[2].0, 'c' as u32);
    }

    // =========================================================================
    // ChildStore tier transition tests
    // =========================================================================

    #[test]
    fn test_inline_to_heap_promotion() {
        let mut node = PersistentCharNode::new();

        // Add 4 children — should stay Inline
        for i in 0..4u32 {
            let child = SwizzledPtr::on_disk(i, i * 100, NodeType::CharNode4);
            node = node.with_child(i + 100, child);
        }
        assert_eq!(node.num_children(), 4);
        assert!(matches!(node.store, ChildStore::Inline { .. }));

        // Add 5th child — should promote to Heap
        let child = SwizzledPtr::on_disk(5, 500, NodeType::CharNode4);
        node = node.with_child(104, child);
        assert_eq!(node.num_children(), 5);
        assert!(matches!(node.store, ChildStore::Heap { .. }));

        // Verify all 5 children are present and sorted
        let keys: Vec<u32> = node.iter_children().map(|(&k, _)| k).collect();
        assert_eq!(keys, vec![100, 101, 102, 103, 104]);
    }

    #[test]
    fn test_heap_to_inline_demotion() {
        let mut node = PersistentCharNode::new();

        // Build a node with 5 children (Heap)
        for i in 0..5u32 {
            let child = SwizzledPtr::on_disk(i, i * 100, NodeType::CharNode4);
            node = node.with_child(i + 100, child);
        }
        assert!(matches!(node.store, ChildStore::Heap { .. }));

        // Remove one — should demote to Inline (4 remaining ≤ INLINE_CAPACITY)
        let node2 = node.without_child(102).expect("should remove");
        assert_eq!(node2.num_children(), 4);
        assert!(matches!(node2.store, ChildStore::Inline { .. }));

        // Verify remaining children are correct and sorted
        let keys: Vec<u32> = node2.iter_children().map(|(&k, _)| k).collect();
        assert_eq!(keys, vec![100, 101, 103, 104]);
    }

    #[test]
    fn test_heap_stays_heap_above_threshold() {
        let mut node = PersistentCharNode::new();

        // Build a node with 6 children (Heap)
        for i in 0..6u32 {
            let child = SwizzledPtr::on_disk(i, i * 100, NodeType::CharNode4);
            node = node.with_child(i + 100, child);
        }
        assert!(matches!(node.store, ChildStore::Heap { .. }));

        // Remove one — should stay Heap (5 remaining > INLINE_CAPACITY)
        let node2 = node.without_child(102).expect("should remove");
        assert_eq!(node2.num_children(), 5);
        assert!(matches!(node2.store, ChildStore::Heap { .. }));
    }

    #[test]
    fn test_many_children() {
        let mut node = PersistentCharNode::new();

        // Add 256 children (all ASCII values)
        for i in 0..256u32 {
            let child = SwizzledPtr::on_disk(i, i, NodeType::CharNode4);
            node = node.with_child(i, child);
        }

        assert_eq!(node.num_children(), 256);
        assert!(matches!(node.store, ChildStore::Heap { .. }));

        // Verify all children findable
        for i in 0..256u32 {
            assert!(node.has_child(i), "should find child {}", i);
        }

        // Verify sorted order
        let keys: Vec<u32> = node.iter_children().map(|(&k, _)| k).collect();
        let expected: Vec<u32> = (0..256).collect();
        assert_eq!(keys, expected);
    }

    #[test]
    fn test_child_at() {
        let child = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);
        let node = PersistentCharNode::new()
            .with_child('b' as u32, child.clone())
            .with_child('a' as u32, child);

        // Index 0 should be 'a' (sorted)
        let (k, _) = node.child_at(0).expect("should exist");
        assert_eq!(*k, 'a' as u32);

        // Index 1 should be 'b'
        let (k, _) = node.child_at(1).expect("should exist");
        assert_eq!(*k, 'b' as u32);

        // Index 2 should not exist
        assert!(node.child_at(2).is_none());
    }

    #[test]
    fn test_inline_replace_preserves_count() {
        let child1 = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);
        let child2 = SwizzledPtr::on_disk(2, 200, NodeType::CharNode4);

        let node = PersistentCharNode::new()
            .with_child('a' as u32, child1.clone())
            .with_child('b' as u32, child1);

        assert_eq!(node.num_children(), 2);
        assert!(matches!(node.store, ChildStore::Inline { .. }));

        // Replace 'a' — should stay Inline with same count
        let node2 = node.with_child('a' as u32, child2);
        assert_eq!(node2.num_children(), 2);
        assert!(matches!(node2.store, ChildStore::Inline { .. }));
    }
}
