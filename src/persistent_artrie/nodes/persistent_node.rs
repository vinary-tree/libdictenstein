//! Lock-Free Persistent Node (u8/ASCII variant)
//!
//! This module provides a lock-free node implementation using persistent (immutable)
//! data structures from the `im` crate. Modifications create new versions of the node
//! rather than mutating in place, enabling lock-free concurrent access.
//!
//! This is the u8/byte key variant, suitable for ASCII keys and arbitrary byte sequences.
//! For UTF-8/Unicode keys using u32 code points, see `persistent_artrie_char::PersistentCharNode`.
//!
//! # Design
//!
//! The key insight is that persistent data structures use structural sharing:
//! when you "modify" an `im::Vector`, you get a new vector that shares most of
//! its structure with the original. This is O(log n) for insertions and lookups.
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
//! when their reference count drops to zero. The epoch-based reclamation system
//! (already implemented in libdictenstein) protects against use-after-free.
//!
//! # Example
//!
//! ```rust,ignore
//! use libdictenstein::persistent_artrie::nodes::persistent_node::PersistentNode;
//!
//! let node = PersistentNode::new();
//! let child_ptr = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
//!
//! // Create a new version with the child - original is unchanged
//! let node2 = node.with_child(b'a', child_ptr);
//!
//! assert_eq!(node.num_children(), 0);  // Original unchanged
//! assert_eq!(node2.num_children(), 1); // New version has child
//! ```

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use im::Vector;

use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

/// Node flags (same as existing node flags for compatibility)
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

/// Maximum prefix length in bytes
pub const MAX_PREFIX_LEN: usize = 12;

/// A lock-free persistent node using immutable data structures (u8/byte keys).
///
/// This node type uses `im::Vector` for keys and children, enabling efficient
/// structural sharing when creating modified versions. All modifications return
/// a new node rather than mutating in place.
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
/// - `keys`: Sorted vector of child keys (u8 bytes)
/// - `children`: Vector of child pointers corresponding to keys
/// - `flags`: Atomic flags (IS_FINAL, IS_DIRTY, etc.)
/// - `value`: Atomic value for final nodes (vocabulary index)
/// - `prefix`: Compressed path prefix for path compression
#[derive(Debug)]
pub struct PersistentNode {
    /// Monotonic version counter (incremented on each modification)
    version: AtomicU64,

    /// Sorted keys for children (byte values)
    /// Using im::Vector for O(log n) structural sharing on modifications
    keys: Vector<u8>,

    /// Child pointers corresponding to keys
    /// Must maintain same length as keys
    children: Vector<SwizzledPtr>,

    /// Node flags (IS_FINAL, IS_DIRTY, IS_LEAF, HAS_VALUE)
    /// Atomic to allow setting final flag during concurrent insert race
    flags: AtomicU8,

    /// Value for final nodes (e.g., vocabulary index)
    /// Atomic to allow setting value during concurrent insert race
    value: AtomicU64,

    /// Compressed prefix for path compression (up to 12 bytes)
    prefix: Arc<[u8]>,

    /// Length of the valid prefix (may be less than prefix.len())
    prefix_len: u8,
}

impl PersistentNode {
    /// Create a new empty node.
    pub fn new() -> Self {
        Self {
            version: AtomicU64::new(0),
            keys: Vector::new(),
            children: Vector::new(),
            flags: AtomicU8::new(0),
            value: AtomicU64::new(0),
            prefix: Arc::new([]),
            prefix_len: 0,
        }
    }

    /// Create a new node with a prefix.
    pub fn with_prefix(prefix: &[u8]) -> Self {
        let prefix_len = prefix.len().min(MAX_PREFIX_LEN) as u8;
        let prefix_data: Arc<[u8]> = prefix[..prefix_len as usize].into();

        Self {
            version: AtomicU64::new(0),
            keys: Vector::new(),
            children: Vector::new(),
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
        self.keys.len()
    }

    /// Check if the node is empty (no children).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Get the prefix as a slice.
    #[inline]
    pub fn prefix(&self) -> &[u8] {
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
    /// Uses binary search on the sorted keys vector.
    /// Returns the child pointer if found, None otherwise.
    ///
    /// # Arguments
    ///
    /// * `key` - The child key (byte value)
    #[inline]
    pub fn find_child(&self, key: u8) -> Option<&SwizzledPtr> {
        match self.keys.binary_search(&key) {
            Ok(idx) => self.children.get(idx),
            Err(_) => None,
        }
    }

    /// Check if a child exists for the given key.
    #[inline]
    pub fn has_child(&self, key: u8) -> bool {
        self.keys.binary_search(&key).is_ok()
    }

    /// Get the child at a specific index.
    #[inline]
    pub fn child_at(&self, index: usize) -> Option<(&u8, &SwizzledPtr)> {
        match (self.keys.get(index), self.children.get(index)) {
            (Some(k), Some(c)) => Some((k, c)),
            _ => None,
        }
    }

    /// Iterate over all (key, child) pairs.
    pub fn iter_children(&self) -> impl Iterator<Item = (&u8, &SwizzledPtr)> {
        self.keys.iter().zip(self.children.iter())
    }

    /// Create a new version of this node with an added child.
    ///
    /// This does NOT modify the current node - it returns a new node
    /// with the child added. The new node shares structure with this one.
    ///
    /// # Arguments
    ///
    /// * `key` - The key for the new child
    /// * `child` - The child pointer
    ///
    /// # Returns
    ///
    /// A new node with the child added (or replaced if key exists).
    pub fn with_child(&self, key: u8, child: SwizzledPtr) -> Self {
        let (new_keys, new_children) = match self.keys.binary_search(&key) {
            Ok(idx) => {
                // Key exists - replace the child
                let mut new_children = self.children.clone();
                new_children.set(idx, child);
                (self.keys.clone(), new_children)
            }
            Err(idx) => {
                // Key doesn't exist - insert at sorted position
                let mut new_keys = self.keys.clone();
                let mut new_children = self.children.clone();
                new_keys.insert(idx, key);
                new_children.insert(idx, child);
                (new_keys, new_children)
            }
        };

        Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire) + 1),
            keys: new_keys,
            children: new_children,
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
    pub fn without_child(&self, key: u8) -> Option<Self> {
        match self.keys.binary_search(&key) {
            Ok(idx) => {
                let mut new_keys = self.keys.clone();
                let mut new_children = self.children.clone();
                new_keys.remove(idx);
                new_children.remove(idx);

                Some(Self {
                    version: AtomicU64::new(self.version.load(Ordering::Acquire) + 1),
                    keys: new_keys,
                    children: new_children,
                    flags: AtomicU8::new(self.flags.load(Ordering::Acquire)),
                    value: AtomicU64::new(self.value.load(Ordering::Acquire)),
                    prefix: self.prefix.clone(),
                    prefix_len: self.prefix_len,
                })
            }
            Err(_) => None,
        }
    }

    /// Create a new version with a different prefix.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The new prefix
    pub fn with_prefix_replaced(&self, prefix: &[u8]) -> Self {
        let prefix_len = prefix.len().min(MAX_PREFIX_LEN) as u8;
        let prefix_data: Arc<[u8]> = prefix[..prefix_len as usize].into();

        Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire) + 1),
            keys: self.keys.clone(),
            children: self.children.clone(),
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
            keys: self.keys.clone(),
            children: self.children.clone(),
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
            keys: self.keys.clone(),
            children: self.children.clone(),
            flags: AtomicU8::new(self.flags.load(Ordering::Acquire) | flags::HAS_VALUE),
            value: AtomicU64::new(value),
            prefix: self.prefix.clone(),
            prefix_len: self.prefix_len,
        }
    }

    /// Match this node's prefix against a key slice.
    ///
    /// Returns the number of matching bytes (0 to prefix_len).
    pub fn match_prefix(&self, key: &[u8]) -> usize {
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
    pub fn prefix_matches(&self, key: &[u8]) -> bool {
        self.match_prefix(key) == self.prefix_len()
    }

    /// Get estimated memory usage in bytes.
    pub fn memory_usage(&self) -> usize {
        // Base struct size
        let base = std::mem::size_of::<Self>();

        // Keys vector (im::Vector has some overhead)
        let keys_size = self.keys.len() * std::mem::size_of::<u8>();

        // Children vector
        let children_size = self.children.len() * std::mem::size_of::<SwizzledPtr>();

        // Prefix Arc
        let prefix_size = self.prefix.len() * std::mem::size_of::<u8>();

        base + keys_size + children_size + prefix_size
    }
}

impl Default for PersistentNode {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for PersistentNode {
    fn clone(&self) -> Self {
        Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire)),
            keys: self.keys.clone(),
            children: self.children.clone(),
            flags: AtomicU8::new(self.flags.load(Ordering::Acquire)),
            value: AtomicU64::new(self.value.load(Ordering::Acquire)),
            prefix: self.prefix.clone(),
            prefix_len: self.prefix_len,
        }
    }
}

// Safety: PersistentNode uses atomic operations for all mutable state
unsafe impl Send for PersistentNode {}
unsafe impl Sync for PersistentNode {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_artrie::NodeType;

    #[test]
    fn test_new_node() {
        let node = PersistentNode::new();
        assert_eq!(node.num_children(), 0);
        assert!(node.is_empty());
        assert!(!node.is_final());
        assert!(!node.has_value());
        assert_eq!(node.version(), 0);
    }

    #[test]
    fn test_with_prefix() {
        let prefix = b"hello";
        let node = PersistentNode::with_prefix(prefix);

        assert_eq!(node.prefix_len(), 5);
        assert_eq!(node.prefix(), b"hello");
    }

    #[test]
    fn test_prefix_max_length() {
        // Prefix longer than MAX_PREFIX_LEN should be truncated
        let prefix = b"abcdefghijklmnop"; // 16 bytes > 12
        let node = PersistentNode::with_prefix(prefix);

        assert_eq!(node.prefix_len(), MAX_PREFIX_LEN);
        assert_eq!(node.prefix(), b"abcdefghijkl");
    }

    #[test]
    fn test_with_child_immutability() {
        let node = PersistentNode::new();
        let child = SwizzledPtr::on_disk(1, 100, NodeType::Node4);

        let node2 = node.with_child(b'a', child);

        // Original should be unchanged
        assert_eq!(node.num_children(), 0);

        // New version should have the child
        assert_eq!(node2.num_children(), 1);
        assert!(node2.has_child(b'a'));
    }

    #[test]
    fn test_with_child_sorted_order() {
        let mut node = PersistentNode::new();

        // Add children in random order
        let keys = [b'z', b'a', b'm', b'f'];
        for &k in &keys {
            let child = SwizzledPtr::on_disk(k as u32, 0, NodeType::Node4);
            node = node.with_child(k, child);
        }

        assert_eq!(node.num_children(), 4);

        // Verify sorted order
        let collected_keys: Vec<u8> = node.iter_children().map(|(&k, _)| k).collect();
        assert_eq!(collected_keys, vec![b'a', b'f', b'm', b'z']);
    }

    #[test]
    fn test_with_child_replace() {
        let child1 = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
        let child2 = SwizzledPtr::on_disk(2, 200, NodeType::Node4);
        let child2_raw = child2.to_raw();

        let node = PersistentNode::new().with_child(b'a', child1);

        assert_eq!(node.num_children(), 1);

        // Replace existing child
        let node2 = node.with_child(b'a', child2);

        // Should still have only 1 child
        assert_eq!(node2.num_children(), 1);

        // Check the child was replaced (different raw value)
        let found = node2.find_child(b'a').expect("should find child");
        assert_eq!(found.to_raw(), child2_raw);
    }

    #[test]
    fn test_without_child() {
        let child = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
        let node = PersistentNode::new()
            .with_child(b'a', child.clone())
            .with_child(b'b', child.clone())
            .with_child(b'c', child);

        assert_eq!(node.num_children(), 3);

        // Remove middle child
        let node2 = node.without_child(b'b').expect("should remove");
        assert_eq!(node2.num_children(), 2);
        assert!(node2.has_child(b'a'));
        assert!(!node2.has_child(b'b'));
        assert!(node2.has_child(b'c'));

        // Original unchanged
        assert_eq!(node.num_children(), 3);
    }

    #[test]
    fn test_without_child_not_found() {
        let node = PersistentNode::new();
        assert!(node.without_child(b'x').is_none());
    }

    #[test]
    fn test_find_child() {
        let child = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
        let node = PersistentNode::new()
            .with_child(b'a', child.clone())
            .with_child(b'b', child);

        assert!(node.find_child(b'a').is_some());
        assert!(node.find_child(b'b').is_some());
        assert!(node.find_child(b'c').is_none());
    }

    #[test]
    fn test_try_set_final() {
        let node = PersistentNode::new();

        // First call should succeed
        assert!(node.try_set_final());
        assert!(node.is_final());

        // Second call should fail
        assert!(!node.try_set_final());
        assert!(node.is_final());
    }

    #[test]
    fn test_try_set_value() {
        let node = PersistentNode::new();

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
        let node = PersistentNode::new();
        assert_eq!(node.version(), 0);

        let child = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
        let node2 = node.with_child(b'a', child);
        assert_eq!(node2.version(), 1);

        let node3 = node2.as_final();
        assert_eq!(node3.version(), 2);
    }

    #[test]
    fn test_prefix_matching() {
        let prefix = b"hello";
        let node = PersistentNode::with_prefix(prefix);

        // Full match
        assert_eq!(node.match_prefix(b"helloworld"), 5);
        assert!(node.prefix_matches(b"helloworld"));

        // Partial match
        assert_eq!(node.match_prefix(b"help"), 3);
        assert!(!node.prefix_matches(b"help"));

        // No match
        assert_eq!(node.match_prefix(b"world"), 0);
        assert!(!node.prefix_matches(b"world"));
    }

    #[test]
    fn test_clone() {
        let node = PersistentNode::new();
        let child = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
        let node = node.with_child(b'a', child);
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
        let node = PersistentNode::new();
        assert!(!node.is_final());

        let final_node = node.as_final();
        assert!(final_node.is_final());

        // Original unchanged
        assert!(!node.is_final());
    }

    #[test]
    fn test_with_value() {
        let node = PersistentNode::new();
        assert!(!node.has_value());

        let valued_node = node.with_value(123);
        assert!(valued_node.has_value());
        assert_eq!(valued_node.get_value(), Some(123));

        // Original unchanged
        assert!(!node.has_value());
    }

    #[test]
    fn test_iter_children() {
        let child = SwizzledPtr::on_disk(1, 100, NodeType::Node4);
        let node = PersistentNode::new()
            .with_child(b'c', child.clone())
            .with_child(b'a', child.clone())
            .with_child(b'b', child);

        let pairs: Vec<(u8, u64)> = node
            .iter_children()
            .map(|(&k, c)| (k, c.to_raw()))
            .collect();

        assert_eq!(pairs.len(), 3);
        // Should be sorted
        assert_eq!(pairs[0].0, b'a');
        assert_eq!(pairs[1].0, b'b');
        assert_eq!(pairs[2].0, b'c');
    }

    #[test]
    fn test_all_byte_values() {
        // Test that we can handle all byte values 0-255
        let mut node = PersistentNode::new();
        let child = SwizzledPtr::on_disk(1, 100, NodeType::Node4);

        for key in 0u8..=255 {
            node = node.with_child(key, child.clone());
        }

        assert_eq!(node.num_children(), 256);

        // All should be findable
        for key in 0u8..=255 {
            assert!(node.has_child(key), "should find key {}", key);
        }

        // Keys should be sorted by byte value
        let collected: Vec<u8> = node.iter_children().map(|(&k, _)| k).collect();
        let mut expected: Vec<u8> = (0u8..=255).collect();
        expected.sort();
        assert_eq!(collected, expected);
    }
}
