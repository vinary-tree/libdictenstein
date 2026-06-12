//! Thread-safe node pointer for CAS-style operations — the single generic shared
//! between the byte and char lock-free overlays (G4 unification).
//!
//! This module provides `AtomicNodePtr`, a wrapper around
//! `Arc<OverlayNode<K, V>>` that exposes compare-and-swap-style operations for
//! concurrent trie modifications. Both variants alias it:
//!
//! ```text
//! // byte:  pub type AtomicNodePtr<V = ()> = OverlayAtomicNodePtr<ByteKey, V>;
//! // char:  pub type AtomicNodePtr<V = ()> = OverlayAtomicNodePtr<CharKey, V>;
//! ```
//!
//! # Design
//!
//! The pointer stores an `Arc<OverlayNode<K, V>>` in an `arc_swap::ArcSwapOption`
//! — a genuinely-atomic, lock-free `Arc` cell. An earlier iteration stored raw
//! `Arc` pointers in an `AtomicU64`, which is unsound without an
//! epoch/hazard-pointer scheme because `load()` can race with replacement and
//! attempt to increment a freed allocation; a stopgap then retreated to a
//! `RwLock`, which reintroduced a lock on every "CAS". `ArcSwapOption` is the
//! sound *and* lock-free resolution: its `load` is protected by an internal
//! deferred-reclaim scheme, so a reader never touches a freed allocation, and no
//! lock serializes concurrent readers/writers.
//!
//! # Memory Safety
//!
//! - `load()` clones the current `Arc` via `load_full()` (lock-free, hazard-protected)
//! - `compare_exchange()` swaps only when the stored `Arc` is pointer-equal to
//!   the expected `Arc` (`ArcSwapOption::compare_and_swap`)
//! - rejected replacements are dropped normally

use std::sync::Arc;

use arc_swap::ArcSwapOption;

use super::node::OverlayNode;
use crate::persistent_artrie::core::key_encoding::KeyEncoding;

/// Null pointer sentinel value used by `as_raw` for diagnostics/tests.
const NULL_PTR: u64 = 0;

/// A CAS-style pointer to an [`OverlayNode`].
///
/// Generic over the key encoding `K` and value `V` (default `()`). This wrapper
/// enables thread-safe compare-and-swap-style operations on `Arc<OverlayNode<K, V>>`
/// pointers while keeping `Arc` ownership inside Rust's safe memory model.
///
/// # Memory Management
///
/// - `load()` clones the stored `Arc`
/// - `compare_exchange()` returns a clone of the replaced or actual node
/// - replaced/rejected nodes are dropped by normal `Arc` ownership
pub struct AtomicNodePtr<K: KeyEncoding, V = ()> {
    /// The current node slot — a genuinely-atomic, lock-free `Arc` cell.
    ptr: ArcSwapOption<OverlayNode<K, V>>,
}

// Manual `Debug` so neither `K::Unit` nor `V` need `Debug`.
impl<K: KeyEncoding, V> std::fmt::Debug for AtomicNodePtr<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtomicNodePtr")
            .field("is_null", &self.ptr.load().is_none())
            .finish()
    }
}

impl<K: KeyEncoding, V: Clone> AtomicNodePtr<K, V> {
    /// Create a new atomic pointer from an Arc.
    ///
    /// The Arc's reference count is NOT incremented - ownership is transferred.
    pub fn new(node: Arc<OverlayNode<K, V>>) -> Self {
        Self {
            ptr: ArcSwapOption::new(Some(node)),
        }
    }

    /// Create a null atomic pointer.
    pub fn null() -> Self {
        Self {
            ptr: ArcSwapOption::empty(),
        }
    }

    /// Check if the pointer is null.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.ptr.load().is_none()
    }

    /// Load the current node pointer.
    ///
    /// This increments the Arc's reference count before returning,
    /// so the caller receives a valid Arc that they own.
    pub fn load(&self) -> Option<Arc<OverlayNode<K, V>>> {
        self.ptr.load_full()
    }

    /// Load the current node, panicking if null.
    ///
    /// # Panics
    ///
    /// Panics if the pointer is null.
    #[inline]
    pub fn load_unchecked(&self) -> Arc<OverlayNode<K, V>> {
        self.load()
            .expect("AtomicNodePtr::load_unchecked called on null pointer")
    }

    /// Store a new node pointer.
    ///
    /// This atomically replaces the current pointer with the new one.
    /// The old pointer's Arc is decremented.
    pub fn store(&self, node: Arc<OverlayNode<K, V>>) {
        self.ptr.store(Some(node));
    }

    /// Store null, returning the old value.
    pub fn take(&self) -> Option<Arc<OverlayNode<K, V>>> {
        self.ptr.swap(None)
    }

    /// Atomically compare and exchange the node pointer.
    ///
    /// If the current pointer equals `expected`, it's replaced with `new`.
    /// Otherwise, the operation fails and returns the actual current value.
    ///
    /// # Returns
    ///
    /// - `Ok(old)` if CAS succeeded (old == expected)
    /// - `Err(actual)` if CAS failed (actual != expected)
    pub fn compare_exchange(
        &self,
        expected: &Arc<OverlayNode<K, V>>,
        new: Arc<OverlayNode<K, V>>,
    ) -> Result<Arc<OverlayNode<K, V>>, Arc<OverlayNode<K, V>>> {
        // Genuinely-atomic CAS: swap `new` in iff the stored Arc is pointer-equal
        // to `expected`. `&Arc<_>` implements `AsRaw`, so we compare by the node's
        // raw pointer with no extra refcount bump. `compare_and_swap` returns the
        // value stored BEFORE the operation; success <=> it is pointer-equal to
        // `expected`.
        let prev = self.ptr.compare_and_swap(expected, Some(new));
        match &*prev {
            Some(p) if Arc::ptr_eq(p, expected) => Ok(Arc::clone(p)),
            Some(p) => Err(Arc::clone(p)),
            None => Err(Arc::new(OverlayNode::new())),
        }
    }

    /// Weak compare and exchange (may spuriously fail).
    pub fn compare_exchange_weak(
        &self,
        expected: &Arc<OverlayNode<K, V>>,
        new: Arc<OverlayNode<K, V>>,
    ) -> Result<Arc<OverlayNode<K, V>>, Arc<OverlayNode<K, V>>> {
        self.compare_exchange(expected, new)
    }

    /// Try to set a null pointer to a new value.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the pointer was null and is now set to `new`
    /// - `Err(actual)` if the pointer was not null
    pub fn try_init(&self, new: Arc<OverlayNode<K, V>>) -> Result<(), Arc<OverlayNode<K, V>>> {
        // CAS None -> Some(new), atomically.
        let prev = self
            .ptr
            .compare_and_swap(&None::<Arc<OverlayNode<K, V>>>, Some(new));
        match &*prev {
            None => Ok(()),
            Some(p) => Err(Arc::clone(p)),
        }
    }

    /// Get the raw pointer value (for debugging/testing).
    #[inline]
    pub fn as_raw(&self) -> u64 {
        self.ptr
            .load()
            .as_ref()
            .map(|node| Arc::as_ptr(node) as u64)
            .unwrap_or(NULL_PTR)
    }
}

// Generic `Clone`/`Default` for any `<K, V: Clone>` (the pre-G4 char/byte impls
// were `V = ()`-only; widening to `<K, V: Clone>` is strictly more general and
// removes the char/byte inconsistency — both just call `Self::new`/`null`).
impl<K: KeyEncoding, V: Clone> Clone for AtomicNodePtr<K, V> {
    fn clone(&self) -> Self {
        match self.load() {
            Some(arc) => Self::new(arc),
            None => Self::null(),
        }
    }
}

impl<K: KeyEncoding, V: Clone> Default for AtomicNodePtr<K, V> {
    fn default() -> Self {
        Self::null()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_artrie::core::key_encoding::{ByteKey, CharKey};
    use crate::persistent_artrie::core::overlay::node::Child;
    use crate::persistent_artrie::core::swizzled_ptr::{NodeType, SwizzledPtr};

    // Exercise both instantiations of the shared pointer (byte `<u8>` keys and
    // char `<u32>` keys) at the default `<()>` membership value.
    type ByteNode = OverlayNode<ByteKey, ()>;
    type ByteAtomicNodePtr = AtomicNodePtr<ByteKey, ()>;
    type CharNode = OverlayNode<CharKey, ()>;
    type CharAtomicNodePtr = AtomicNodePtr<CharKey, ()>;

    #[test]
    fn test_new_and_load_byte() {
        let node = Arc::new(ByteNode::new());
        let ptr = ByteAtomicNodePtr::new(node);
        let loaded = ptr.load().expect("should load");
        assert_eq!(loaded.num_children(), 0);
    }

    #[test]
    fn test_null_pointer_char() {
        let ptr = CharAtomicNodePtr::null();
        assert!(ptr.is_null());
        assert!(ptr.load().is_none());
    }

    #[test]
    fn test_store_byte() {
        let node1 = Arc::new(ByteNode::new());
        let child = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::Node4));
        let node2 = Arc::new(node1.with_child(b'a', child));
        let ptr = ByteAtomicNodePtr::new(node1);
        assert_eq!(ptr.load().expect("should load").num_children(), 0);
        ptr.store(node2);
        assert_eq!(ptr.load().expect("should load").num_children(), 1);
    }

    #[test]
    fn test_take_char() {
        let node = Arc::new(CharNode::new());
        let ptr = CharAtomicNodePtr::new(node);
        assert!(!ptr.is_null());
        let taken = ptr.take();
        assert!(taken.is_some());
        assert!(ptr.is_null());
        assert!(ptr.take().is_none());
    }

    #[test]
    fn test_compare_exchange_success_byte() {
        let node1 = Arc::new(ByteNode::new());
        let child = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::Node4));
        let node2 = Arc::new(node1.with_child(b'a', child));
        let ptr = ByteAtomicNodePtr::new(node1.clone());
        assert!(ptr.compare_exchange(&node1, node2).is_ok());
        assert_eq!(ptr.load().expect("should load").num_children(), 1);
    }

    #[test]
    fn test_compare_exchange_failure_char() {
        let node1 = Arc::new(CharNode::new());
        let child = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::CharNode4));
        let node2 = Arc::new(node1.with_child('a' as u32, child));
        let node3 = Arc::new(CharNode::new());
        let ptr = CharAtomicNodePtr::new(node1.clone());
        assert!(ptr.compare_exchange(&node1, node2).is_ok());
        let result = ptr.compare_exchange(&node1, node3);
        assert!(result.is_err());
        assert_eq!(ptr.load().expect("should load").num_children(), 1);
    }

    #[test]
    fn test_try_init_byte() {
        let ptr = ByteAtomicNodePtr::null();
        let node = Arc::new(ByteNode::new());
        assert!(ptr.try_init(node).is_ok());
        assert!(!ptr.is_null());

        let other = Arc::new(ByteNode::new());
        assert!(ptr.try_init(other).is_err());
    }

    #[test]
    fn test_clone_char() {
        let child = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::CharNode4));
        let node = Arc::new(CharNode::new().with_child('a' as u32, child));
        let ptr1 = CharAtomicNodePtr::new(node);
        let ptr2 = ptr1.clone();
        assert_eq!(ptr1.load().expect("load").num_children(), 1);
        assert_eq!(ptr2.load().expect("load").num_children(), 1);
    }

    #[test]
    fn test_load_unchecked_byte() {
        let node = Arc::new(ByteNode::new());
        let ptr = ByteAtomicNodePtr::new(node);
        assert_eq!(ptr.load_unchecked().num_children(), 0);
    }

    #[test]
    #[should_panic(expected = "null pointer")]
    fn test_load_unchecked_panics_on_null_char() {
        let ptr = CharAtomicNodePtr::null();
        let _loaded = ptr.load_unchecked();
    }

    // =========================================================================
    // Cross-instantiation generic coverage
    //
    // The CAS contract below is written ONCE over an arbitrary `K: KeyEncoding`
    // and invoked for BOTH `ByteKey` and `CharKey` — the both-instantiation
    // pointer coverage the pre-G4 per-variant `atomic_ptr.rs` suites provided,
    // now over the single unified pointer type.
    // =========================================================================

    use crate::persistent_artrie::core::key_encoding::KeyEncoding;
    use std::thread;

    /// `compare_exchange` succeeds only against the currently-stored Arc, and a
    /// stale `expected` is rejected with the actual value returned.
    fn check_cas_contract<K: KeyEncoding>() {
        let n1 = Arc::new(OverlayNode::<K, ()>::new());
        let n2 = Arc::new(n1.as_final());
        let n3 = Arc::new(OverlayNode::<K, ()>::new());
        let ptr = AtomicNodePtr::<K, ()>::new(Arc::clone(&n1));

        // Stale expected (n3 was never stored) is rejected.
        assert!(ptr.compare_exchange(&n3, Arc::clone(&n2)).is_err());
        // Correct expected succeeds.
        assert!(ptr.compare_exchange(&n1, Arc::clone(&n2)).is_ok());
        // n1 is no longer current ⇒ rejected, returns actual (n2).
        let actual = ptr
            .compare_exchange(&n1, Arc::clone(&n3))
            .expect_err("stale expected after a winning CAS must fail");
        assert!(Arc::ptr_eq(&actual, &n2));
    }

    /// Many concurrent CAS attempts are safe and at least one wins; the final
    /// published node is reachable.
    fn check_concurrent_cas<K: KeyEncoding>()
    where
        K::Unit: TryFrom<u32>,
        <K::Unit as TryFrom<u32>>::Error: std::fmt::Debug,
    {
        let ptr = Arc::new(AtomicNodePtr::<K, ()>::new(Arc::new(
            OverlayNode::<K, ()>::new(),
        )));
        let total: usize = (0..8u32)
            .map(|t| {
                let ptr = Arc::clone(&ptr);
                thread::spawn(move || {
                    let mut wins = 0;
                    for i in 0..64u32 {
                        let cur = ptr
                            .load()
                            .unwrap_or_else(|| Arc::new(OverlayNode::<K, ()>::new()));
                        let key = K::Unit::try_from((t * 64 + i) % 250).expect("unit fits");
                        let child = Child::OnDisk(SwizzledPtr::on_disk(
                            (t * 64 + i) as u32,
                            0,
                            NodeType::Node4,
                        ));
                        let next = Arc::new(cur.with_child(key, child));
                        if ptr.compare_exchange(&cur, next).is_ok() {
                            wins += 1;
                        }
                    }
                    wins
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().expect("thread join"))
            .sum();
        assert!(total > 0, "at least one CAS must win");
        assert!(ptr.load().expect("final load").num_children() > 0);
    }

    /// Constructing and dropping many pointers leaks nothing (no panic / no UAF).
    fn check_no_leak_churn<K: KeyEncoding>() {
        for _ in 0..500 {
            let ptr = AtomicNodePtr::<K, ()>::new(Arc::new(OverlayNode::<K, ()>::new()));
            drop(ptr);
        }
    }

    #[test]
    fn generic_cas_contract_byte() {
        check_cas_contract::<ByteKey>();
    }

    #[test]
    fn generic_cas_contract_char() {
        check_cas_contract::<CharKey>();
    }

    #[test]
    fn generic_concurrent_cas_byte() {
        check_concurrent_cas::<ByteKey>();
    }

    #[test]
    fn generic_concurrent_cas_char() {
        check_concurrent_cas::<CharKey>();
    }

    #[test]
    fn generic_no_leak_churn_byte() {
        check_no_leak_churn::<ByteKey>();
    }

    #[test]
    fn generic_no_leak_churn_char() {
        check_no_leak_churn::<CharKey>();
    }
}
