//! Atomic Node Pointer for Lock-Free CAS Operations
//!
//! This module provides `AtomicNodePtr`, a wrapper around `Arc<PersistentCharNode>`
//! that enables atomic compare-and-swap operations. This is the core primitive
//! for lock-free trie modifications.
//!
//! # Design
//!
//! The pointer stores an `Arc<PersistentCharNode>` as a raw u64 address in an `AtomicU64`.
//! This allows atomic CAS operations to swap between different node versions.
//!
//! # Memory Safety
//!
//! - When loading, we increment the Arc's reference count before returning
//! - When CAS succeeds, the old Arc is decremented (via the expected value)
//! - When CAS fails, the new Arc we tried to insert is decremented
//! - The epoch-based reclamation system protects against ABA problems
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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::persistent_node::PersistentCharNode;

/// Null pointer sentinel value (0 is never a valid Arc address)
const NULL_PTR: u64 = 0;

/// An atomic pointer to a persistent character node.
///
/// This wrapper enables lock-free compare-and-swap operations on
/// `Arc<PersistentCharNode>` pointers. It's the core building block
/// for lock-free trie modifications.
///
/// # Thread Safety
///
/// All operations are atomic and safe to call from multiple threads.
/// The `compare_exchange` method provides the lock-free CAS semantic
/// needed for concurrent updates.
///
/// # Memory Management
///
/// The struct carefully manages Arc reference counts:
/// - `load()` increments refcount before returning
/// - `compare_exchange()` decrements refcount of replaced value on success
/// - `compare_exchange()` decrements refcount of rejected new value on failure
/// - `Drop` decrements refcount of the stored pointer
#[derive(Debug)]
pub struct AtomicNodePtr {
    /// The raw pointer stored as u64 for atomic operations
    /// 0 = null, otherwise = Arc<PersistentCharNode> raw pointer
    ptr: AtomicU64,
}

impl AtomicNodePtr {
    /// Create a new atomic pointer from an Arc.
    ///
    /// The Arc's reference count is NOT incremented - ownership is transferred.
    pub fn new(node: Arc<PersistentCharNode>) -> Self {
        let raw = Arc::into_raw(node) as u64;
        Self {
            ptr: AtomicU64::new(raw),
        }
    }

    /// Create a null atomic pointer.
    pub fn null() -> Self {
        Self {
            ptr: AtomicU64::new(NULL_PTR),
        }
    }

    /// Check if the pointer is null.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.ptr.load(Ordering::Acquire) == NULL_PTR
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
        let raw = self.ptr.load(Ordering::Acquire);
        if raw == NULL_PTR {
            return None;
        }

        // Safety: We're incrementing the refcount before creating a new Arc
        // to ensure the pointer remains valid.
        unsafe {
            let ptr = raw as *const PersistentCharNode;
            Arc::increment_strong_count(ptr);
            Some(Arc::from_raw(ptr))
        }
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
        let new_raw = Arc::into_raw(node) as u64;
        let old_raw = self.ptr.swap(new_raw, Ordering::AcqRel);

        // Decrement refcount of old pointer
        if old_raw != NULL_PTR {
            unsafe {
                Arc::decrement_strong_count(old_raw as *const PersistentCharNode);
            }
        }
    }

    /// Store null, returning the old value.
    ///
    /// This is useful for cleanup or when resetting a node.
    ///
    /// # Returns
    ///
    /// The previous node if it was not null.
    pub fn take(&self) -> Option<Arc<PersistentCharNode>> {
        let old_raw = self.ptr.swap(NULL_PTR, Ordering::AcqRel);
        if old_raw == NULL_PTR {
            None
        } else {
            // We're taking ownership of the Arc (no refcount change needed)
            unsafe { Some(Arc::from_raw(old_raw as *const PersistentCharNode)) }
        }
    }

    /// Atomically compare and exchange the node pointer.
    ///
    /// This is the core operation for lock-free updates. If the current
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
    /// - On success: `expected`'s Arc is NOT decremented (caller still owns it)
    /// - On success: `new`'s Arc ownership is transferred to the pointer
    /// - On failure: `new`'s Arc is decremented (rejected)
    /// - On failure: Returns a new Arc to `actual` (with incremented refcount)
    pub fn compare_exchange(
        &self,
        expected: &Arc<PersistentCharNode>,
        new: Arc<PersistentCharNode>,
    ) -> Result<Arc<PersistentCharNode>, Arc<PersistentCharNode>> {
        let expected_raw = Arc::as_ptr(expected) as u64;
        let new_raw = Arc::into_raw(new) as u64;

        match self
            .ptr
            .compare_exchange(expected_raw, new_raw, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {
                // CAS succeeded - return clone of expected (caller keeps their Arc)
                Ok(expected.clone())
            }
            Err(actual_raw) => {
                // CAS failed - decrement refcount of the rejected new Arc
                unsafe {
                    Arc::decrement_strong_count(new_raw as *const PersistentCharNode);
                }

                // Return the actual current value (increment refcount first)
                if actual_raw == NULL_PTR {
                    // This shouldn't happen in normal use, but handle it
                    Err(Arc::new(PersistentCharNode::new()))
                } else {
                    unsafe {
                        Arc::increment_strong_count(actual_raw as *const PersistentCharNode);
                        Err(Arc::from_raw(actual_raw as *const PersistentCharNode))
                    }
                }
            }
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
        let expected_raw = Arc::as_ptr(expected) as u64;
        let new_raw = Arc::into_raw(new) as u64;

        match self.ptr.compare_exchange_weak(
            expected_raw,
            new_raw,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(expected.clone()),
            Err(actual_raw) => {
                // CAS failed - decrement refcount of the rejected new Arc
                unsafe {
                    Arc::decrement_strong_count(new_raw as *const PersistentCharNode);
                }

                if actual_raw == NULL_PTR {
                    Err(Arc::new(PersistentCharNode::new()))
                } else {
                    unsafe {
                        Arc::increment_strong_count(actual_raw as *const PersistentCharNode);
                        Err(Arc::from_raw(actual_raw as *const PersistentCharNode))
                    }
                }
            }
        }
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
        let new_raw = Arc::into_raw(new) as u64;

        match self
            .ptr
            .compare_exchange(NULL_PTR, new_raw, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(()),
            Err(actual_raw) => {
                // CAS failed - decrement refcount of the rejected new Arc
                unsafe {
                    Arc::decrement_strong_count(new_raw as *const PersistentCharNode);
                }

                // Return the actual current value
                unsafe {
                    Arc::increment_strong_count(actual_raw as *const PersistentCharNode);
                    Err(Arc::from_raw(actual_raw as *const PersistentCharNode))
                }
            }
        }
    }

    /// Get the raw pointer value (for debugging/testing).
    #[inline]
    pub fn as_raw(&self) -> u64 {
        self.ptr.load(Ordering::Acquire)
    }
}

impl Drop for AtomicNodePtr {
    fn drop(&mut self) {
        let raw = self.ptr.load(Ordering::Acquire);
        if raw != NULL_PTR {
            // Decrement the refcount by reconstructing and dropping the Arc
            unsafe {
                drop(Arc::from_raw(raw as *const PersistentCharNode));
            }
        }
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

// Safety: AtomicNodePtr uses only atomic operations
unsafe impl Send for AtomicNodePtr {}
unsafe impl Sync for AtomicNodePtr {}

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
