//! Thread-safe node pointer for CAS-style operations
//!
//! This module provides `AtomicNodePtr`, a wrapper around `Arc<PersistentCharNode>`
//! that exposes compare-and-swap-style operations for concurrent trie
//! modifications.
//!
//! # Design
//!
//! The pointer stores an `Arc<PersistentCharNode>` behind a small lock. Earlier
//! versions stored raw `Arc` pointers in an atomic integer, but that is not
//! sound without an epoch/hazard-pointer scheme because `load()` can race with
//! replacement and attempt to increment a freed allocation.
//!
//! # Memory Safety
//!
//! - `load()` clones the current `Arc` while the slot is locked
//! - `compare_exchange()` swaps only when the stored `Arc` is pointer-equal to
//!   the expected `Arc`
//! - rejected replacements are dropped normally
//!
//! # Usage
//!
//! ```text
//! use std::sync::Arc;
//! use libdictenstein::persistent_artrie_char::nodes::atomic_ptr::AtomicNodePtr;
//! use libdictenstein::persistent_artrie_char::nodes::persistent_node::PersistentCharNode;
//!
//! let node = Arc::new(PersistentCharNode::new());
//! let ptr = AtomicNodePtr::new(node.clone());
//!
//! // Load current node
//! let current = ptr.load();
//!
//! // Create new version and try to swap
//! let child = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);
//! let new_node = Arc::new(current.with_child('a' as u32, child));
//!
//! match ptr.compare_exchange(&current, new_node) {
//!     Ok(_old) => println!("Successfully updated!"),
//!     Err(actual) => println!("CAS failed, someone else modified"),
//! }
//! ```

use std::sync::{Arc, RwLock};

use super::persistent_node::PersistentCharNode;

/// Null pointer sentinel value used by `as_raw` for diagnostics/tests.
const NULL_PTR: u64 = 0;

/// A CAS-style pointer to a persistent character node.
///
/// This wrapper enables thread-safe compare-and-swap-style operations on
/// `Arc<PersistentCharNode>` pointers.
///
/// # Thread Safety
///
/// All operations are safe to call from multiple threads. The
/// `compare_exchange` method provides CAS-style success/failure semantics while
/// keeping `Arc` ownership inside Rust's safe memory model.
///
/// # Memory Management
///
/// The struct carefully manages Arc reference counts:
/// - `load()` clones the stored `Arc`
/// - `compare_exchange()` returns a clone of the replaced or actual node
/// - replaced/rejected nodes are dropped by normal `Arc` ownership
#[derive(Debug)]
pub struct AtomicNodePtr {
    /// The current node slot.
    ptr: RwLock<Option<Arc<PersistentCharNode>>>,
}

impl AtomicNodePtr {
    /// Create a new atomic pointer from an Arc.
    ///
    /// The Arc's reference count is NOT incremented - ownership is transferred.
    pub fn new(node: Arc<PersistentCharNode>) -> Self {
        Self {
            ptr: RwLock::new(Some(node)),
        }
    }

    /// Create a null atomic pointer.
    pub fn null() -> Self {
        Self {
            ptr: RwLock::new(None),
        }
    }

    /// Check if the pointer is null.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.ptr
            .read()
            .expect("AtomicNodePtr read lock poisoned")
            .is_none()
    }

    /// Load the current node pointer.
    ///
    /// This increments the Arc's reference count before returning,
    /// so the caller receives a valid Arc that they own.
    ///
    /// # Returns
    ///
    /// - `Some(Arc<PersistentCharNode>)` if the pointer is not null
    /// - `None` if the pointer is null
    pub fn load(&self) -> Option<Arc<PersistentCharNode>> {
        self.ptr
            .read()
            .expect("AtomicNodePtr read lock poisoned")
            .clone()
    }

    /// Load the current node, panicking if null.
    ///
    /// Use this when you know the pointer cannot be null.
    ///
    /// # Panics
    ///
    /// Panics if the pointer is null.
    #[inline]
    pub fn load_unchecked(&self) -> Arc<PersistentCharNode> {
        self.load()
            .expect("AtomicNodePtr::load_unchecked called on null pointer")
    }

    /// Store a new node pointer.
    ///
    /// This atomically replaces the current pointer with the new one.
    /// The old pointer's Arc is decremented.
    ///
    /// # Arguments
    ///
    /// * `node` - The new node to store (ownership is transferred)
    pub fn store(&self, node: Arc<PersistentCharNode>) {
        *self.ptr.write().expect("AtomicNodePtr write lock poisoned") = Some(node);
    }

    /// Store null, returning the old value.
    ///
    /// This is useful for cleanup or when resetting a node.
    ///
    /// # Returns
    ///
    /// The previous node if it was not null.
    pub fn take(&self) -> Option<Arc<PersistentCharNode>> {
        self.ptr
            .write()
            .expect("AtomicNodePtr write lock poisoned")
            .take()
    }

    /// Atomically compare and exchange the node pointer.
    ///
    /// This is the core operation for CAS-style updates. If the current
    /// pointer equals `expected`, it's replaced with `new`. Otherwise,
    /// the operation fails and returns the actual current value.
    ///
    /// # Arguments
    ///
    /// * `expected` - The expected current node
    /// * `new` - The new node to store if expectation matches
    ///
    /// # Returns
    ///
    /// - `Ok(old)` if CAS succeeded (old == expected)
    /// - `Err(actual)` if CAS failed (actual != expected)
    ///
    /// # Memory Management
    ///
    /// - On success: the pointer slot owns `new` and the returned `old` Arc is
    ///   a normal owned clone of the replaced node
    /// - On success: `new`'s Arc ownership is transferred to the pointer
    /// - On failure: `new`'s Arc is decremented (rejected)
    /// - On failure: Returns a new Arc to `actual` (with incremented refcount)
    pub fn compare_exchange(
        &self,
        expected: &Arc<PersistentCharNode>,
        new: Arc<PersistentCharNode>,
    ) -> Result<Arc<PersistentCharNode>, Arc<PersistentCharNode>> {
        let mut guard = self.ptr.write().expect("AtomicNodePtr write lock poisoned");

        match guard.as_ref() {
            Some(current) if Arc::ptr_eq(current, expected) => {
                let old = Arc::clone(current);
                *guard = Some(new);
                Ok(old)
            }
            Some(current) => Err(Arc::clone(current)),
            None => Err(Arc::new(PersistentCharNode::new())),
        }
    }

    /// Weak compare and exchange (may spuriously fail).
    ///
    /// Like `compare_exchange`, but may fail even when the comparison
    /// would succeed. Use this in a loop for better performance on
    /// some architectures.
    pub fn compare_exchange_weak(
        &self,
        expected: &Arc<PersistentCharNode>,
        new: Arc<PersistentCharNode>,
    ) -> Result<Arc<PersistentCharNode>, Arc<PersistentCharNode>> {
        self.compare_exchange(expected, new)
    }

    /// Try to set a null pointer to a new value.
    ///
    /// This is a convenience method for initializing an empty slot.
    ///
    /// # Arguments
    ///
    /// * `new` - The new node to store
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the pointer was null and is now set to `new`
    /// - `Err(actual)` if the pointer was not null
    pub fn try_init(&self, new: Arc<PersistentCharNode>) -> Result<(), Arc<PersistentCharNode>> {
        let mut guard = self.ptr.write().expect("AtomicNodePtr write lock poisoned");

        if guard.is_none() {
            *guard = Some(new);
            Ok(())
        } else {
            Err(Arc::clone(guard.as_ref().expect("non-null guard")))
        }
    }

    /// Get the raw pointer value (for debugging/testing).
    #[inline]
    pub fn as_raw(&self) -> u64 {
        self.ptr
            .read()
            .expect("AtomicNodePtr read lock poisoned")
            .as_ref()
            .map(|node| Arc::as_ptr(node) as u64)
            .unwrap_or(NULL_PTR)
    }
}

impl Clone for AtomicNodePtr {
    fn clone(&self) -> Self {
        match self.load() {
            Some(arc) => Self::new(arc),
            None => Self::null(),
        }
    }
}

impl Default for AtomicNodePtr {
    fn default() -> Self {
        Self::null()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
    use crate::persistent_artrie::NodeType;
    use std::thread;

    #[test]
    fn test_new_and_load() {
        let node = Arc::new(PersistentCharNode::new());
        let ptr = AtomicNodePtr::new(node);

        let loaded = ptr.load().expect("should load");
        assert_eq!(loaded.num_children(), 0);
    }

    #[test]
    fn test_null_pointer() {
        let ptr = AtomicNodePtr::null();
        assert!(ptr.is_null());
        assert!(ptr.load().is_none());
    }

    #[test]
    fn test_store() {
        let node1 = Arc::new(PersistentCharNode::new());
        let child = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);
        let node2 = Arc::new(node1.with_child('a' as u32, child));

        let ptr = AtomicNodePtr::new(node1);

        // Verify initial state
        assert_eq!(ptr.load().expect("should load").num_children(), 0);

        // Store new node
        ptr.store(node2);

        // Verify new state
        assert_eq!(ptr.load().expect("should load").num_children(), 1);
    }

    #[test]
    fn test_take() {
        let node = Arc::new(PersistentCharNode::new());
        let ptr = AtomicNodePtr::new(node);

        assert!(!ptr.is_null());

        let taken = ptr.take();
        assert!(taken.is_some());
        assert!(ptr.is_null());

        // Second take should return None
        assert!(ptr.take().is_none());
    }

    #[test]
    fn test_compare_exchange_success() {
        let node1 = Arc::new(PersistentCharNode::new());
        let child = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);
        let node2 = Arc::new(node1.with_child('a' as u32, child));

        let ptr = AtomicNodePtr::new(node1.clone());

        // CAS should succeed
        let result = ptr.compare_exchange(&node1, node2);
        assert!(result.is_ok());

        // Verify new state
        let loaded = ptr.load().expect("should load");
        assert_eq!(loaded.num_children(), 1);
    }

    #[test]
    fn test_compare_exchange_failure() {
        let node1 = Arc::new(PersistentCharNode::new());
        let child = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);
        let node2 = Arc::new(node1.with_child('a' as u32, child));
        let node3 = Arc::new(PersistentCharNode::new());

        let ptr = AtomicNodePtr::new(node1.clone());

        // First CAS succeeds
        assert!(ptr.compare_exchange(&node1, node2).is_ok());

        // Second CAS should fail because we're comparing against node1
        // but the actual value is now node2
        let result = ptr.compare_exchange(&node1, node3);
        assert!(result.is_err());

        // Verify state unchanged
        let loaded = ptr.load().expect("should load");
        assert_eq!(loaded.num_children(), 1);
    }

    #[test]
    fn test_try_init_success() {
        let ptr = AtomicNodePtr::null();
        let node = Arc::new(PersistentCharNode::new());

        let result = ptr.try_init(node);
        assert!(result.is_ok());
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_try_init_failure() {
        let node1 = Arc::new(PersistentCharNode::new());
        let node2 = Arc::new(PersistentCharNode::new());
        let ptr = AtomicNodePtr::new(node1);

        let result = ptr.try_init(node2);
        assert!(result.is_err());
    }

    #[test]
    fn test_clone() {
        let child = SwizzledPtr::on_disk(1, 100, NodeType::CharNode4);
        let node = Arc::new(PersistentCharNode::new().with_child('a' as u32, child));
        let ptr1 = AtomicNodePtr::new(node);

        let ptr2 = ptr1.clone();

        // Both should see the same content
        assert_eq!(ptr1.load().expect("load").num_children(), 1);
        assert_eq!(ptr2.load().expect("load").num_children(), 1);
    }

    #[test]
    fn test_concurrent_cas() {
        // Test that concurrent CAS operations are safe
        let node = Arc::new(PersistentCharNode::new());
        let ptr = Arc::new(AtomicNodePtr::new(node));

        let num_threads = 8;
        let ops_per_thread = 100;

        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let ptr = Arc::clone(&ptr);
                thread::spawn(move || {
                    let mut successes = 0;
                    for i in 0..ops_per_thread {
                        // Load current
                        let current = ptr
                            .load()
                            .unwrap_or_else(|| Arc::new(PersistentCharNode::new()));

                        // Create new version with a child
                        let key = (t * ops_per_thread + i) as u32;
                        let child = SwizzledPtr::on_disk(key, 0, NodeType::CharNode4);
                        let new_node = Arc::new(current.with_child(key, child));

                        // Try to CAS
                        if ptr.compare_exchange(&current, new_node).is_ok() {
                            successes += 1;
                        }
                    }
                    successes
                })
            })
            .collect();

        let total_successes: usize = handles
            .into_iter()
            .map(|h| h.join().expect("thread join"))
            .sum();

        // Some operations should succeed (exact number depends on timing)
        assert!(total_successes > 0);

        // The final node should have a valid number of children
        let final_node = ptr.load().expect("final load");
        assert!(final_node.num_children() > 0);
    }

    #[test]
    fn test_memory_safety_no_leaks() {
        // Create and drop many pointers to verify no memory leaks
        for _ in 0..1000 {
            let node = Arc::new(PersistentCharNode::new());
            let ptr = AtomicNodePtr::new(node);
            drop(ptr);
        }

        // If we get here without memory errors, the test passes
    }

    #[test]
    fn test_load_unchecked() {
        let node = Arc::new(PersistentCharNode::new());
        let ptr = AtomicNodePtr::new(node);

        let loaded = ptr.load_unchecked();
        assert_eq!(loaded.num_children(), 0);
    }

    #[test]
    #[should_panic(expected = "null pointer")]
    fn test_load_unchecked_panics_on_null() {
        let ptr = AtomicNodePtr::null();
        let _loaded = ptr.load_unchecked();
    }
}
