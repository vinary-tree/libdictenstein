//! Lock-Free Persistent Overlay Node — the single generic shared between the
//! byte (`u8`) and char (`u32`) lock-free overlays (G4 unification).
//!
//! This module provides a lock-free node using persistent (immutable) data
//! structures. Modifications create new versions of the node rather than mutating
//! in place, enabling lock-free concurrent access. It is parameterized over the
//! key encoding `K: KeyEncoding` (its `Unit` is `u8` for byte tries, `u32` for
//! char tries) and the value `V`.
//!
//! Before G4 the byte node (`persistent_artrie::nodes::persistent_node`) and the
//! char node (`persistent_artrie_char::nodes::persistent_node`) were
//! token-for-token identical except for four things — the key-unit type, the
//! `MAX_PREFIX_LEN` (12 vs 6), the inline zero filler (`0u8` vs `0u32`), and prose.
//! All four are now absorbed by `K::Unit`, `K::MAX_PREFIX_LEN`, and `K::UNIT_ZERO`.
//! Both variants alias this type:
//!
//! ```text
//! // byte:  pub type PersistentNode<V = ()>     = OverlayNode<ByteKey, V>;
//! // char:  pub type PersistentCharNode<V = ()> = OverlayNode<CharKey, V>;
//! ```
//!
//! # Design
//!
//! Child storage uses a tiered `ChildStore` enum:
//!
//! ```text
//! ChildStore::Inline  (0-4 children, ~85% of nodes)
//!   → Zero heap allocation. Clone is pure value copy.
//!   → Linear scan for lookups (faster than binary search at this size).
//!
//! ChildStore::Heap    (5+ children)
//!   → Owned Vec<K::Unit> + Vec<Child>. Clone is flat contiguous copy.
//!   → Binary search for lookups.
//! ```
//!
//! For lock-free concurrent updates, we CAS on a pointer to the node
//! (`super::atomic_ptr::AtomicNodePtr`):
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
//! when their reference count drops to zero. Child slots are owned (`Child`),
//! so dropping a superseded node version decrements its children's refcounts —
//! reclamation falls out of ordinary `Arc` refcounting, with no leak.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use crate::persistent_artrie_core::key_encoding::KeyEncoding;
use crate::persistent_artrie_core::swizzled_ptr::SwizzledPtr;
// `DictionaryValue` is the bound callers supply for the genericized value `V`; it
// is imported for the doc reference on `value: Option<V>` below. The methods stay
// on `impl<K, V: Clone>` (the membership/leak-fix logic only needs `Clone`); the
// trait bound is supplied by callers in the variants' `lockfree_cas.rs`.
#[allow(unused_imports)]
use crate::value::DictionaryValue;

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

/// Maximum number of children in the inline storage tier.
///
/// Nodes with 0-4 children use fully inline storage (zero heap allocation);
/// adding a 5th child promotes to the Heap tier.
const INLINE_CAPACITY: usize = 4;

// ============================================================================
// Child: an owned child slot (the leak fix)
// ============================================================================

/// A child slot in an [`OverlayNode`].
///
/// # Why an enum instead of a bare `SwizzledPtr`
///
/// The lock-free overlay used to smuggle an in-memory `Arc<OverlayNode>` through
/// a `SwizzledPtr` (a `u64`) via `Arc::into_raw`. Because `SwizzledPtr` is a plain
/// integer with no `Drop`, **every superseded node version leaked its children**:
/// the `Arc` refcount was never decremented when an old node was dropped. Reading
/// a child back also required `unsafe { Arc::from_raw(..) }` on every traversal.
///
/// `Child` makes ownership explicit and correct, with **zero `unsafe`**:
/// - `InMem(Arc<..>)` owns the child. Dropping the parent drops this `Arc`,
///   decrementing the child's refcount, so reclamation falls out of ordinary
///   `Arc` refcounting — a child is freed exactly when no live node version
///   references it (including versions still held by concurrent readers through
///   the `arc-swap` root). No epoch machinery is required for *correctness*.
/// - `OnDisk(SwizzledPtr)` is an on-disk reference (a serialized block location).
///   It owns no heap allocation, so its `Drop` is a no-op.
///
/// `pub` (not `pub(crate)`) because it appears in the signatures of `pub`
/// methods on the variants' re-exported nodes (`find_child`, `with_child`,
/// `child_at`, `iter_children`) — exactly as the `pub` `SwizzledPtr` it replaced
/// did.
///
/// Generic over the key encoding `K` (so the in-mem arm names the
/// correctly-parameterized node) and value `V` (default `()`). `Clone` and `Debug`
/// are hand-written below to bound only `V` (not `K`), so neither `K` nor `V` need
/// extra bounds beyond what the methods require.
pub enum Child<K: KeyEncoding, V = ()> {
    /// An in-memory child node, owned by `Arc` (reclaimed via refcount on drop).
    InMem(Arc<OverlayNode<K, V>>),
    /// An on-disk reference to a serialized subtree (a swizzled block location).
    OnDisk(SwizzledPtr),
}

// Manual `Clone` bounding only `V: Clone` (a `#[derive(Clone)]` would demand
// `K: Clone` on the type param and leak it into call-sites; hand-writing keeps the
// bound minimal — exactly as the pre-G4 byte/char files did for the `V`-only case).
impl<K: KeyEncoding, V: Clone> Clone for Child<K, V> {
    fn clone(&self) -> Self {
        match self {
            Child::InMem(node) => Child::InMem(Arc::clone(node)),
            Child::OnDisk(ptr) => Child::OnDisk(ptr.clone()),
        }
    }
}

// Manual `Debug` so neither `K::Unit` nor `V` need `Debug`; the in-memory arm is
// summarized without recursing.
impl<K: KeyEncoding, V> std::fmt::Debug for Child<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Child::InMem(_) => f.write_str("Child::InMem(..)"),
            Child::OnDisk(p) => f.debug_tuple("Child::OnDisk").field(p).finish(),
        }
    }
}

impl<K: KeyEncoding, V> Child<K, V> {
    /// The placeholder for unused inline-array slots: a null on-disk reference.
    ///
    /// Only `keys[..count]` / `children[..count]` of an `Inline` store are ever
    /// read; the remaining slots hold this cheap, ownership-free filler.
    #[inline]
    fn empty() -> Self {
        Child::OnDisk(SwizzledPtr::null())
    }

    /// `true` if this slot is an empty/null on-disk reference (filler or unset).
    #[inline]
    pub fn is_null(&self) -> bool {
        matches!(self, Child::OnDisk(p) if p.is_null())
    }

    /// `true` if this slot is an on-disk reference rather than an in-memory node.
    #[inline]
    pub fn is_on_disk(&self) -> bool {
        matches!(self, Child::OnDisk(_))
    }

    /// Borrow the in-memory child `Arc`, if this slot holds one.
    #[inline]
    pub fn as_in_mem(&self) -> Option<&Arc<OverlayNode<K, V>>> {
        match self {
            Child::InMem(node) => Some(node),
            Child::OnDisk(_) => None,
        }
    }

    /// Borrow the on-disk reference, if this slot holds one.
    #[inline]
    pub fn as_on_disk(&self) -> Option<&SwizzledPtr> {
        match self {
            Child::OnDisk(ptr) => Some(ptr),
            Child::InMem(_) => None,
        }
    }
}

// ============================================================================
// ChildStore: Tiered child storage for OverlayNode
// ============================================================================

/// Tiered child storage that eliminates heap allocation for most nodes.
///
/// Generic over `K` (key encoding) and `V` (value, default `()`); `Clone`/`Debug`
/// are hand-written below to bound only `V`.
enum ChildStore<K: KeyEncoding, V = ()> {
    /// 0-4 children stored inline (no heap allocation).
    ///
    /// Keys are sorted in ascending order. Only `keys[..count]` /
    /// `children[..count]` are valid; the rest hold `Child::empty()`.
    Inline {
        /// Number of valid children (0-4).
        count: u8,
        /// Sorted child keys. Only `[..count]` is valid.
        keys: [K::Unit; INLINE_CAPACITY],
        /// Child slots corresponding to keys. Only `[..count]` is valid.
        children: [Child<K, V>; INLINE_CAPACITY],
    },

    /// 5+ children in owned Vecs.
    ///
    /// Keys are sorted in ascending order. Both Vecs always have the same length.
    Heap {
        /// Sorted child keys.
        keys: Vec<K::Unit>,
        /// Child slots corresponding to keys.
        children: Vec<Child<K, V>>,
    },
}

// Manual `Clone` bounding only `V: Clone`.
impl<K: KeyEncoding, V: Clone> Clone for ChildStore<K, V> {
    fn clone(&self) -> Self {
        match self {
            ChildStore::Inline {
                count,
                keys,
                children,
            } => ChildStore::Inline {
                count: *count,
                keys: *keys,
                children: children.clone(),
            },
            ChildStore::Heap { keys, children } => ChildStore::Heap {
                keys: keys.clone(),
                children: children.clone(),
            },
        }
    }
}

// Manual `Debug` so neither `K::Unit` nor `V` need `Debug`.
impl<K: KeyEncoding, V> std::fmt::Debug for ChildStore<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChildStore::Inline { count, .. } => f
                .debug_struct("ChildStore::Inline")
                .field("count", count)
                .finish_non_exhaustive(),
            ChildStore::Heap { keys, .. } => f
                .debug_struct("ChildStore::Heap")
                .field("len", &keys.len())
                .finish_non_exhaustive(),
        }
    }
}

// Bound-free helpers (no `V: Clone`) — required by `OverlayNode`'s iterative `Drop`,
// which cannot add a `V: Clone` bound the struct lacks (E0367).
impl<K: KeyEncoding, V> ChildStore<K, V> {
    /// Create an empty inline child store WITHOUT requiring `V: Clone`. The body
    /// needs no `V` (it uses `K::UNIT_ZERO` + the bound-free `Child::empty()`).
    #[inline]
    fn empty_inline() -> Self {
        ChildStore::Inline {
            count: 0,
            keys: [K::UNIT_ZERO; INLINE_CAPACITY],
            children: [
                Child::empty(),
                Child::empty(),
                Child::empty(),
                Child::empty(),
            ],
        }
    }

    /// Replace this store with an empty inline store, returning the old store by
    /// value so its owned `Child`s can be consumed without recursion. Used by
    /// `OverlayNode`'s iterative `Drop` to move a node's children out BEFORE the
    /// node's own field-drop runs (which then sees an empty store).
    #[inline]
    fn take(&mut self) -> Self {
        std::mem::replace(self, Self::empty_inline())
    }

    /// Consume this store, pushing every owned in-memory child `Arc` into `out`
    /// (on-disk children own no heap allocation, so they drop here cheaply). `self`
    /// is taken by value so the `Child`s MOVE out — refcounts unchanged (no clone),
    /// the property the reclaim/leak witnesses depend on. No `unsafe`.
    fn drain_in_mem_into(self, out: &mut Vec<Arc<OverlayNode<K, V>>>) {
        match self {
            ChildStore::Inline {
                count, children, ..
            } => {
                // Move only the valid prefix; array `into_iter()` (edition 2021)
                // yields each `Child` by value.
                for child in children.into_iter().take(count as usize) {
                    if let Child::InMem(arc) = child {
                        out.push(arc);
                    }
                }
            }
            ChildStore::Heap { children, .. } => {
                for child in children {
                    if let Child::InMem(arc) = child {
                        out.push(arc);
                    }
                }
            }
        }
    }
}

impl<K: KeyEncoding, V: Clone> ChildStore<K, V> {
    /// Create an empty inline child store. Delegates to the bound-free
    /// [`Self::empty_inline`] so the iterative `Drop` (which cannot require
    /// `V: Clone`) builds the SAME empty store.
    #[inline]
    fn new() -> Self {
        Self::empty_inline()
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
    fn find_child(&self, key: K::Unit) -> Option<&Child<K, V>> {
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
    fn has_child(&self, key: K::Unit) -> bool {
        self.find_child(key).is_some()
    }

    /// Get the child at a specific index.
    #[inline]
    fn child_at(&self, index: usize) -> Option<(&K::Unit, &Child<K, V>)> {
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
    fn slices(&self) -> (&[K::Unit], &[Child<K, V>]) {
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
    fn with_child(&self, key: K::Unit, child: Child<K, V>) -> Self {
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
                        let mut new_children = children.clone();
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
                    let mut new_children = children.clone();

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
                        let mut new_children = children.clone();
                        new_children[idx] = child;
                        ChildStore::Heap {
                            keys: keys.clone(),
                            children: new_children,
                        }
                    }
                    Err(idx) => {
                        // Insert at sorted position
                        let mut new_keys = keys.clone();
                        let mut new_children = children.clone();
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
    fn without_child(&self, key: K::Unit) -> Option<Self> {
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
                let mut new_children = children.clone();

                for i in pos..n - 1 {
                    new_keys[i] = new_keys[i + 1];
                    new_children[i] = new_children[i + 1].clone();
                }
                // Clear the now-unused last slot
                new_keys[n - 1] = K::UNIT_ZERO;
                new_children[n - 1] = Child::empty();

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
                    let mut new_keys = [K::UNIT_ZERO; INLINE_CAPACITY];
                    let mut new_children = [
                        Child::empty(),
                        Child::empty(),
                        Child::empty(),
                        Child::empty(),
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
                    let mut new_children = children.clone();
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
                let n = *count as usize;
                n * (std::mem::size_of::<K::Unit>() + std::mem::size_of::<Child<K, V>>())
            }
            ChildStore::Heap { keys, children } => {
                keys.capacity() * std::mem::size_of::<K::Unit>()
                    + children.capacity() * std::mem::size_of::<Child<K, V>>()
            }
        }
    }
}

// ============================================================================
// OverlayNode
// ============================================================================

/// A lock-free persistent overlay node using tiered child storage.
///
/// Generic over the key encoding `K` (`K::Unit` is `u8`/`u32`) and value `V`
/// (default `()`). This node type uses `ChildStore` for keys and children,
/// enabling efficient zero-allocation storage for the ~85% of nodes with ≤4
/// children. All modifications return a new node rather than mutating in place.
///
/// # Thread Safety
///
/// Individual nodes are immutable after creation (except for atomic flags).
/// Thread-safe concurrent access is achieved by CAS-swapping pointers to nodes
/// using `super::atomic_ptr::AtomicNodePtr`.
///
/// # Memory Layout
///
/// - `version`: Monotonic version counter for detecting modifications
/// - `store`: Tiered child storage (Inline for 0-4, Heap for 5+)
/// - `flags`: Atomic flags (IS_FINAL, IS_DIRTY, etc.)
/// - `value`: Immutable `Option<V>` value for final nodes
/// - `prefix`: Compressed path prefix for path compression
///
/// `Debug` is hand-written below so neither `K::Unit` nor `V` need `Debug`.
pub struct OverlayNode<K: KeyEncoding, V = ()> {
    /// Monotonic version counter (incremented on each modification)
    version: AtomicU64,

    /// Tiered child storage (Inline for 0-4 children, Heap for 5+)
    store: ChildStore<K, V>,

    /// Node flags (IS_FINAL, IS_DIRTY, IS_LEAF, HAS_VALUE)
    /// Atomic to allow setting final flag during concurrent insert race
    flags: AtomicU8,

    /// Value for final nodes. **Immutable** (set at node construction / path-copy):
    /// arbitrary `V` cannot live in an atomic, so finalization+value are baked into
    /// the path-copied node and arbitrated by the root CAS (the single-phase model
    /// the vocab overlay already uses). For `V = ()` (membership) this is the
    /// niche-sized `Option<()>` and is never set.
    value: Option<V>,

    /// Compressed prefix for path compression (up to `K::MAX_PREFIX_LEN` units)
    prefix: Arc<[K::Unit]>,

    /// Length of the valid prefix (may be less than prefix.len())
    prefix_len: u8,
}

// Manual `Debug` so neither `K::Unit` nor `V` need `Debug`. `V: Clone` because the
// inherent accessors live on `impl<K, V: Clone>`.
impl<K: KeyEncoding, V: Clone> std::fmt::Debug for OverlayNode<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayNode")
            .field("is_final", &self.is_final())
            .field("num_children", &self.num_children())
            .field("has_value", &self.value.is_some())
            .field("prefix_len", &self.prefix_len)
            .finish()
    }
}

impl<K: KeyEncoding, V: Clone> OverlayNode<K, V> {
    /// Create a new empty node.
    pub fn new() -> Self {
        Self {
            version: AtomicU64::new(0),
            store: ChildStore::new(),
            flags: AtomicU8::new(0),
            value: None,
            prefix: Arc::new([]),
            prefix_len: 0,
        }
    }

    /// Create a new node with a prefix.
    pub fn with_prefix(prefix: &[K::Unit]) -> Self {
        let prefix_len = prefix.len().min(K::MAX_PREFIX_LEN) as u8;
        let prefix_data: Arc<[K::Unit]> = prefix[..prefix_len as usize].into();

        Self {
            version: AtomicU64::new(0),
            store: ChildStore::new(),
            flags: AtomicU8::new(0),
            value: None,
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
    pub fn prefix(&self) -> &[K::Unit] {
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
        self.value.is_some()
    }

    /// Get the node value (cloned) for final nodes.
    #[inline]
    pub fn get_value(&self) -> Option<V> {
        self.value.clone()
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

    /// Find a child by key (lock-free read).
    ///
    /// Uses linear scan for Inline nodes (≤4 children) and binary search
    /// for Heap nodes (5+ children).
    #[inline]
    pub fn find_child(&self, key: K::Unit) -> Option<&Child<K, V>> {
        self.store.find_child(key)
    }

    /// Check if a child exists for the given key.
    #[inline]
    pub fn has_child(&self, key: K::Unit) -> bool {
        self.store.has_child(key)
    }

    /// Get the child at a specific index.
    #[inline]
    pub fn child_at(&self, index: usize) -> Option<(&K::Unit, &Child<K, V>)> {
        self.store.child_at(index)
    }

    /// Iterate over all (key, child) pairs.
    pub fn iter_children(&self) -> impl Iterator<Item = (&K::Unit, &Child<K, V>)> {
        let (keys, children) = self.store.slices();
        keys.iter().zip(children.iter())
    }

    /// Create a new version of this node with an added child.
    ///
    /// This does NOT modify the current node - it returns a new node
    /// with the child added. For Inline nodes (≤4 children), this is
    /// a pure value copy with zero heap allocation.
    ///
    /// # Returns
    ///
    /// A new node with the child added (or replaced if key exists).
    pub fn with_child(&self, key: K::Unit, child: Child<K, V>) -> Self {
        Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire) + 1),
            store: self.store.with_child(key, child),
            flags: AtomicU8::new(self.flags.load(Ordering::Acquire)),
            value: self.value.clone(),
            prefix: self.prefix.clone(),
            prefix_len: self.prefix_len,
        }
    }

    /// Create a new version with a child removed.
    ///
    /// # Returns
    ///
    /// - `Some(new_node)` if the key existed and was removed
    /// - `None` if the key didn't exist
    pub fn without_child(&self, key: K::Unit) -> Option<Self> {
        self.store.without_child(key).map(|new_store| Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire) + 1),
            store: new_store,
            flags: AtomicU8::new(self.flags.load(Ordering::Acquire)),
            value: self.value.clone(),
            prefix: self.prefix.clone(),
            prefix_len: self.prefix_len,
        })
    }

    /// Create a new version with a different prefix.
    pub fn with_prefix_replaced(&self, prefix: &[K::Unit]) -> Self {
        let prefix_len = prefix.len().min(K::MAX_PREFIX_LEN) as u8;
        let prefix_data: Arc<[K::Unit]> = prefix[..prefix_len as usize].into();

        Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire) + 1),
            store: self.store.clone(),
            flags: AtomicU8::new(self.flags.load(Ordering::Acquire)),
            value: self.value.clone(),
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
            value: self.value.clone(),
            prefix: self.prefix.clone(),
            prefix_len: self.prefix_len,
        }
    }

    /// Create a new version marked as **NOT final** (the safe mirror of
    /// [`Self::as_final`]) — the node primitive behind a proven overlay DELETE
    /// (design "R-B", §2).
    ///
    /// Clears `IS_FINAL`+`HAS_VALUE` on a COPY (immutability preserved — the
    /// original node is untouched, so concurrent readers holding it see no change)
    /// and drops the value (mirroring an owned remove, which discards the value).
    /// The child store and prefix are **RETAINED**: removing a term that is a
    /// proper prefix of a longer term must keep the longer term reachable
    /// (removing "cat" must keep "cats"). Subtree compaction is a future
    /// optimization, out of scope here — exactly as owned remove also leaves the
    /// (now non-final) node in place.
    ///
    /// # Why a fresh copy, never an in-place clear (design §3.5)
    ///
    /// [`Self::try_set_final`]'s in-place `fetch_or(IS_FINAL)` is monotone-safe
    /// (an early observer of a 0→1 flip is benign — membership only ever grows on
    /// that path). A 1→0 clear is NOT monotone: an in-place `fetch_and(!IS_FINAL)`
    /// racing an in-place `fetch_or(IS_FINAL)` on the SAME shared node has no
    /// serialization point and could resurrect or lose a write. By producing a
    /// fresh cleared node version here and publishing it ONLY via the overlay's
    /// single root CAS, the clear is atomic with one specific published root and
    /// the root-CAS arbiter linearizes it. The node's `flags` is therefore only
    /// ever flipped in-place 0→1 (by `try_set_final`); the 1→0 transition happens
    /// solely on a fresh copy via this method, arbitrated by the root CAS. The
    /// `LockFreeOverlayRemoveCas_Unsafe.cfg` negative control proves the in-place
    /// alternative violates last-writer-wins.
    ///
    /// ZERO `unsafe` (same construction shape as `as_final`; `Send`/`Sync`
    /// unaffected — every field stays `Send + Sync`).
    pub fn as_non_final(&self) -> Self {
        Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire) + 1),
            // SUBTREE RETAINED: remove "cat" must keep "cats" reachable.
            store: self.store.clone(),
            flags: AtomicU8::new(
                self.flags.load(Ordering::Acquire) & !(flags::IS_FINAL | flags::HAS_VALUE),
            ),
            // Drop the value (mirror owned remove).
            value: None,
            prefix: self.prefix.clone(),
            prefix_len: self.prefix_len,
        }
    }

    /// Create a new version with a value set.
    pub fn with_value(&self, value: V) -> Self {
        Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire) + 1),
            store: self.store.clone(),
            flags: AtomicU8::new(self.flags.load(Ordering::Acquire) | flags::HAS_VALUE),
            value: Some(value),
            prefix: self.prefix.clone(),
            prefix_len: self.prefix_len,
        }
    }

    /// Match this node's prefix against a key slice.
    ///
    /// Returns the number of matching units (0 to prefix_len).
    pub fn match_prefix(&self, key: &[K::Unit]) -> usize {
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
    pub fn prefix_matches(&self, key: &[K::Unit]) -> bool {
        self.match_prefix(key) == self.prefix_len()
    }

    /// Get estimated memory usage in bytes.
    pub fn memory_usage(&self) -> usize {
        // Base struct size
        let base = std::mem::size_of::<Self>();

        // Child store (heap portion only — inline is part of base)
        let store_heap = self.store.memory_usage();

        // Prefix Arc
        let prefix_size = self.prefix.len() * std::mem::size_of::<K::Unit>();

        base + store_heap + prefix_size
    }
}

impl<K: KeyEncoding, V: Clone> Default for OverlayNode<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: KeyEncoding, V: Clone> Clone for OverlayNode<K, V> {
    fn clone(&self) -> Self {
        Self {
            version: AtomicU64::new(self.version.load(Ordering::Acquire)),
            store: self.store.clone(),
            flags: AtomicU8::new(self.flags.load(Ordering::Acquire)),
            value: self.value.clone(),
            prefix: self.prefix.clone(),
            prefix_len: self.prefix_len,
        }
    }
}

// Iterative Drop — dismantle the (possibly deep) Arc-linked overlay spine WITHOUT
// recursion. The overlay spine is UN-path-compressed (one node per key unit), so a
// long key builds a spine hundreds of levels deep; the compiler-generated drop is
// recursive (drop node → drop its `store` → drop each `Child::InMem(Arc)` → last
// owner recursively drops that node → …), one stack frame per level, and a ~500-deep
// spine OVERFLOWS the stack. This explicit `Drop` flattens the descent onto a heap
// worklist.
//
// SAFETY / CORRECTNESS (zero `unsafe`): reclamation is driven purely by `Arc`
// refcounting via `Arc::try_unwrap`:
//   * SOLE owner of a child `Arc` ⇒ `try_unwrap` yields the node by value; we drain
//     ITS children onto the worklist and let the now-childless node drop (its `store`
//     is empty ⇒ the re-entrant `drop` hits the `is_empty()` early return — at most
//     ONE extra frame, never a chain).
//   * SHARED `Arc` (a concurrent reader / another root version still holds it) ⇒
//     `try_unwrap` returns `Err(arc)`; the `Arc` just drops, decrementing the count.
//     Whoever becomes the last owner dismantles it later by this same routine.
// No node is freed while referenced (no UAF), none twice (each `Arc` has exactly one
// last owner), none leaked. This is what `reclaim_tests` (`strong_count == 1` after
// drop) witnesses. `V` is untouched (only dropped with its node); `V: Clone` is
// required solely because `ChildStore::new`/`take` live on the `V: Clone` impl.
impl<K: KeyEncoding, V> Drop for OverlayNode<K, V> {
    fn drop(&mut self) {
        // Move our own children out so this node's subsequent field-drop sees an empty
        // store (and thus does not recurse). A leaf yields an empty store ⇒ the drain
        // pushes nothing ⇒ the loop is a no-op; `Vec::new()` does not allocate until
        // the first push, so leaf drops stay allocation-free.
        let mut worklist: Vec<Arc<OverlayNode<K, V>>> = Vec::new();
        self.store.take().drain_in_mem_into(&mut worklist);
        while let Some(arc) = worklist.pop() {
            // Become the sole owner if we can; otherwise the Arc drops (refcount--).
            if let Ok(mut node) = Arc::try_unwrap(arc) {
                // Empty the node's store BEFORE it drops, pushing grandchildren onto
                // the worklist; the node then drops with an empty store → the
                // re-entrant `drop` finds nothing (no deep recursion).
                node.store.take().drain_in_mem_into(&mut worklist);
            }
        }
    }
}

// `Send`/`Sync` are AUTO-DERIVED (the leak-fix removed the last reason for a
// manual `unsafe impl`). Every field is `Send + Sync` when `K::Unit: Send + Sync`
// (it is, per the `KeyEncoding::Unit` bound) and `V: Send + Sync` (guaranteed by
// the `DictionaryValue` bound callers supply): the `AtomicU64`/`AtomicU8` fields,
// `Arc<[K::Unit]>`, `prefix_len: u8`, `value: Option<V>`, and the `ChildStore`
// whose child slots are owned `Child = InMem(Arc<OverlayNode>) | OnDisk(SwizzledPtr)`
// (both arms `Send + Sync` when `K::Unit`/`V` are). NO `unsafe impl`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_artrie_core::key_encoding::{ByteKey, CharKey};
    use crate::persistent_artrie_core::swizzled_ptr::NodeType;

    // The shared `OverlayNode` is exercised at BOTH instantiations: `ByteKey`
    // (the byte overlay's `<u8>` keys) and `CharKey` (the char overlay's `<u32>`
    // keys). These tests are the byte+char "instantiation smoke" coverage that the
    // old per-variant `persistent_node.rs` test suites provided, now run once over
    // the unified type. `<()>` = membership node, `<u64>` = a value-carrying node.
    type ByteNode = OverlayNode<ByteKey, ()>;
    type ByteValuedNode = OverlayNode<ByteKey, u64>;
    type CharNode = OverlayNode<CharKey, ()>;
    type CharValuedNode = OverlayNode<CharKey, u64>;

    #[test]
    fn test_new_node() {
        let node = ByteNode::new();
        assert_eq!(node.num_children(), 0);
        assert!(node.is_empty());
        assert!(!node.is_final());
        assert!(!node.has_value());
        assert_eq!(node.version(), 0);

        let cnode = CharNode::new();
        assert_eq!(cnode.num_children(), 0);
        assert!(cnode.is_empty());
    }

    #[test]
    fn test_with_prefix_byte() {
        let prefix: Vec<u8> = b"hello".to_vec();
        let node = ByteNode::with_prefix(&prefix);
        assert_eq!(node.prefix_len(), 5);
        assert_eq!(node.prefix(), b"hello");
    }

    #[test]
    fn test_with_prefix_char() {
        let prefix: Vec<u32> = "hello".chars().map(|c| c as u32).collect();
        let node = CharNode::with_prefix(&prefix);
        assert_eq!(node.prefix_len(), 5);
        let got: Vec<u32> = node.prefix().to_vec();
        assert_eq!(got, prefix);
    }

    #[test]
    fn test_prefix_max_length_byte() {
        // 16 bytes > 12 ⇒ truncated to ByteKey::MAX_PREFIX_LEN (12).
        let prefix: Vec<u8> = b"abcdefghijklmnop".to_vec();
        let node = ByteNode::with_prefix(&prefix);
        assert_eq!(node.prefix_len(), ByteKey::MAX_PREFIX_LEN);
        assert_eq!(node.prefix_len(), 12);
        assert_eq!(node.prefix(), b"abcdefghijkl");
    }

    #[test]
    fn test_prefix_max_length_char() {
        // 9 chars > 6 ⇒ truncated to CharKey::MAX_PREFIX_LEN (6).
        let prefix: Vec<u32> = "abcdefghi".chars().map(|c| c as u32).collect();
        let node = CharNode::with_prefix(&prefix);
        assert_eq!(node.prefix_len(), CharKey::MAX_PREFIX_LEN);
        assert_eq!(node.prefix_len(), 6);
        let got: Vec<u32> = node.prefix().to_vec();
        assert_eq!(
            got,
            "abcdef".chars().map(|c| c as u32).collect::<Vec<u32>>()
        );
    }

    #[test]
    fn test_with_child_immutability_byte() {
        let node = ByteNode::new();
        let child = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::Node4));
        let node2 = node.with_child(b'a', child);
        assert_eq!(node.num_children(), 0);
        assert_eq!(node2.num_children(), 1);
        assert!(node2.has_child(b'a'));
    }

    #[test]
    fn test_with_child_sorted_order_char() {
        let mut node = CharNode::new();
        let keys: [u32; 4] = ['z' as u32, 'a' as u32, 'm' as u32, 'f' as u32];
        for &k in &keys {
            let child = Child::OnDisk(SwizzledPtr::on_disk(k, 0, NodeType::CharNode4));
            node = node.with_child(k, child);
        }
        assert_eq!(node.num_children(), 4);
        let collected: Vec<u32> = node.iter_children().map(|(&k, _)| k).collect();
        assert_eq!(
            collected,
            vec!['a' as u32, 'f' as u32, 'm' as u32, 'z' as u32]
        );
    }

    #[test]
    fn test_with_child_replace_byte() {
        let child1 = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::Node4));
        let ptr2 = SwizzledPtr::on_disk(2, 200, NodeType::Node4);
        let child2_raw = ptr2.to_raw();
        let node = ByteNode::new().with_child(b'a', child1);
        assert_eq!(node.num_children(), 1);
        let node2 = node.with_child(b'a', Child::OnDisk(ptr2));
        assert_eq!(node2.num_children(), 1);
        let found = node2.find_child(b'a').expect("should find child");
        assert_eq!(
            found.as_on_disk().expect("on-disk child").to_raw(),
            child2_raw
        );
    }

    #[test]
    fn test_without_child_byte() {
        let child = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::Node4));
        let node = ByteNode::new()
            .with_child(b'a', child.clone())
            .with_child(b'b', child.clone())
            .with_child(b'c', child);
        assert_eq!(node.num_children(), 3);
        let node2 = node.without_child(b'b').expect("should remove");
        assert_eq!(node2.num_children(), 2);
        assert!(node2.has_child(b'a'));
        assert!(!node2.has_child(b'b'));
        assert!(node2.has_child(b'c'));
        assert_eq!(node.num_children(), 3);
    }

    #[test]
    fn test_without_child_not_found_byte() {
        let node = ByteNode::new();
        assert!(node.without_child(b'x').is_none());
    }

    #[test]
    fn test_try_set_final_byte() {
        let node = ByteNode::new();
        assert!(node.try_set_final());
        assert!(node.is_final());
        assert!(!node.try_set_final());
        assert!(node.is_final());
    }

    #[test]
    fn test_version_increment_char() {
        let node = CharNode::new();
        assert_eq!(node.version(), 0);
        let child = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::CharNode4));
        let node2 = node.with_child('a' as u32, child);
        assert_eq!(node2.version(), 1);
        let node3 = node2.as_final();
        assert_eq!(node3.version(), 2);
    }

    #[test]
    fn test_prefix_matching_byte() {
        let prefix: Vec<u8> = b"hello".to_vec();
        let node = ByteNode::with_prefix(&prefix);
        assert_eq!(node.match_prefix(b"helloworld"), 5);
        assert!(node.prefix_matches(b"helloworld"));
        assert_eq!(node.match_prefix(b"help"), 3);
        assert!(!node.prefix_matches(b"help"));
        assert_eq!(node.match_prefix(b"world"), 0);
        assert!(!node.prefix_matches(b"world"));
    }

    #[test]
    fn test_clone_byte_valued() {
        let node = ByteValuedNode::new();
        let child = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::Node4));
        let node = node.with_child(b'a', child).as_final().with_value(42);
        let cloned = node.clone();
        assert_eq!(cloned.num_children(), 1);
        assert!(cloned.is_final());
        assert_eq!(cloned.get_value(), Some(42));
        assert_eq!(cloned.version(), node.version());
    }

    #[test]
    fn test_as_final_char() {
        let node = CharNode::new();
        assert!(!node.is_final());
        let final_node = node.as_final();
        assert!(final_node.is_final());
        assert!(!node.is_final());
    }

    #[test]
    fn test_with_value_char_valued() {
        let node = CharValuedNode::new();
        assert!(!node.has_value());
        let valued_node = node.with_value(123);
        assert!(valued_node.has_value());
        assert_eq!(valued_node.get_value(), Some(123));
        assert!(!node.has_value());
    }

    #[test]
    fn test_iter_children_byte() {
        let child = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::Node4));
        let node = ByteNode::new()
            .with_child(b'c', child.clone())
            .with_child(b'a', child.clone())
            .with_child(b'b', child);
        let pairs: Vec<(u8, u64)> = node
            .iter_children()
            .map(|(&k, c)| (k, c.as_on_disk().expect("on-disk child").to_raw()))
            .collect();
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0].0, b'a');
        assert_eq!(pairs[1].0, b'b');
        assert_eq!(pairs[2].0, b'c');
    }

    #[test]
    fn test_all_byte_values() {
        // All byte values 0-255 (exercises the Heap tier) on the byte node.
        let mut node = ByteNode::new();
        let child = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::Node4));
        for key in 0u8..=255 {
            node = node.with_child(key, child.clone());
        }
        assert_eq!(node.num_children(), 256);
        assert!(matches!(node.store, ChildStore::Heap { .. }));
        for key in 0u8..=255 {
            assert!(node.has_child(key), "should find key {}", key);
        }
        let collected: Vec<u8> = node.iter_children().map(|(&k, _)| k).collect();
        let expected: Vec<u8> = (0u8..=255).collect();
        assert_eq!(collected, expected);
    }

    #[test]
    fn test_inline_to_heap_promotion_byte() {
        let mut node = ByteNode::new();
        for i in 0..4u8 {
            let child = Child::OnDisk(SwizzledPtr::on_disk(
                i as u32,
                (i as u32) * 100,
                NodeType::Node4,
            ));
            node = node.with_child(i + 100, child);
        }
        assert_eq!(node.num_children(), 4);
        assert!(matches!(node.store, ChildStore::Inline { .. }));
        let child = Child::OnDisk(SwizzledPtr::on_disk(5, 500, NodeType::Node4));
        node = node.with_child(104, child);
        assert_eq!(node.num_children(), 5);
        assert!(matches!(node.store, ChildStore::Heap { .. }));
        let keys: Vec<u8> = node.iter_children().map(|(&k, _)| k).collect();
        assert_eq!(keys, vec![100, 101, 102, 103, 104]);
    }

    #[test]
    fn test_heap_to_inline_demotion_char() {
        let mut node = CharNode::new();
        for i in 0..5u32 {
            let child = Child::OnDisk(SwizzledPtr::on_disk(i, i * 100, NodeType::CharNode4));
            node = node.with_child(i + 100, child);
        }
        assert!(matches!(node.store, ChildStore::Heap { .. }));
        let node2 = node.without_child(102).expect("should remove");
        assert_eq!(node2.num_children(), 4);
        assert!(matches!(node2.store, ChildStore::Inline { .. }));
        let keys: Vec<u32> = node2.iter_children().map(|(&k, _)| k).collect();
        assert_eq!(keys, vec![100, 101, 103, 104]);
    }

    #[test]
    fn test_heap_stays_heap_above_threshold_byte() {
        let mut node = ByteNode::new();
        for i in 0..6u8 {
            let child = Child::OnDisk(SwizzledPtr::on_disk(
                i as u32,
                (i as u32) * 100,
                NodeType::Node4,
            ));
            node = node.with_child(i + 100, child);
        }
        assert!(matches!(node.store, ChildStore::Heap { .. }));
        let node2 = node.without_child(102).expect("should remove");
        assert_eq!(node2.num_children(), 5);
        assert!(matches!(node2.store, ChildStore::Heap { .. }));
    }

    #[test]
    fn test_child_at_byte() {
        let child = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::Node4));
        let node = ByteNode::new()
            .with_child(b'b', child.clone())
            .with_child(b'a', child);
        let (k, _) = node.child_at(0).expect("should exist");
        assert_eq!(*k, b'a');
        let (k, _) = node.child_at(1).expect("should exist");
        assert_eq!(*k, b'b');
        assert!(node.child_at(2).is_none());
    }

    #[test]
    fn test_inline_replace_preserves_count_byte() {
        let child1 = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::Node4));
        let child2 = Child::OnDisk(SwizzledPtr::on_disk(2, 200, NodeType::Node4));
        let node = ByteNode::new()
            .with_child(b'a', child1.clone())
            .with_child(b'b', child1);
        assert_eq!(node.num_children(), 2);
        assert!(matches!(node.store, ChildStore::Inline { .. }));
        let node2 = node.with_child(b'a', child2);
        assert_eq!(node2.num_children(), 2);
        assert!(matches!(node2.store, ChildStore::Inline { .. }));
    }

    #[test]
    fn test_supersized_unicode_keys_char() {
        // Codepoints beyond the BMP exercise the full u32 width that `u8` cannot
        // hold — the genuine reason the char overlay is `u32`, not `char`-narrowed.
        let mut node = CharNode::new();
        let keys: [u32; 3] = [0x1F600, 0x10FFFF, 'a' as u32];
        for &k in &keys {
            let child = Child::OnDisk(SwizzledPtr::on_disk(k, 0, NodeType::CharNode4));
            node = node.with_child(k, child);
        }
        assert_eq!(node.num_children(), 3);
        assert!(node.has_child(0x1F600));
        assert!(node.has_child(0x10FFFF));
        // Sorted ascending by u32 value: 'a'(0x61) < 0x1F600 < 0x10FFFF.
        let collected: Vec<u32> = node.iter_children().map(|(&k, _)| k).collect();
        assert_eq!(collected, vec!['a' as u32, 0x1F600, 0x10FFFF]);
    }

    // =========================================================================
    // Cross-instantiation generic coverage
    //
    // Every node behavior below is written ONCE over an arbitrary
    // `K: KeyEncoding` and a key-builder, then invoked for BOTH `ByteKey` (`u8`
    // keys) and `CharKey` (`u32` keys). This is the both-instantiation coverage
    // the pre-G4 per-variant `persistent_node.rs` test suites provided — now run
    // over the single unified type so byte and char share exactly one test body.
    // The `NodeType` argument keeps the on-disk `SwizzledPtr` tags variant-correct
    // (`Node4` for byte, `CharNode4` for char).
    // =========================================================================

    fn an_on_disk_child<K: KeyEncoding, V>(raw: u32, nt: NodeType) -> Child<K, V> {
        Child::OnDisk(SwizzledPtr::on_disk(raw, 0, nt))
    }

    /// Build a sorted node from `keys` and assert sorted ascending iteration,
    /// `find_child`/`has_child`, `child_at`, and immutability of the original.
    fn check_sorted_order<K: KeyEncoding>(keys: &[K::Unit], nt: NodeType)
    where
        K::Unit: Into<u64>,
    {
        let mut node = OverlayNode::<K, ()>::new();
        let original = OverlayNode::<K, ()>::new();
        for (i, &k) in keys.iter().enumerate() {
            node = node.with_child(k, an_on_disk_child::<K, ()>(i as u32, nt));
        }
        assert_eq!(node.num_children(), keys.len());
        // Original is unchanged (persistent data structure).
        assert_eq!(original.num_children(), 0);

        // Iteration is sorted ascending by unit value.
        let collected: Vec<u64> = node.iter_children().map(|(&k, _)| k.into()).collect();
        let mut sorted = collected.clone();
        sorted.sort_unstable();
        assert_eq!(
            collected, sorted,
            "children must iterate in ascending key order"
        );

        // Every inserted key is findable; `child_at` agrees with iteration order.
        for &k in keys {
            assert!(node.has_child(k), "inserted key must be present");
            assert!(node.find_child(k).is_some());
        }
        for (idx, (&k, _)) in node.iter_children().enumerate() {
            let (ck, _) = node.child_at(idx).expect("child_at within bounds");
            assert_eq!(*ck, k);
        }
        assert!(node.child_at(keys.len()).is_none());
    }

    /// Insert `count` distinct keys (forcing Inline→Heap promotion past 4), then
    /// remove one and assert demotion back to Inline at ≤4, staying Heap above.
    fn check_tier_transitions<K: KeyEncoding>(base: u32, nt: NodeType)
    where
        K::Unit: TryFrom<u32>,
        <K::Unit as TryFrom<u32>>::Error: std::fmt::Debug,
    {
        let mk = |u: u32| K::Unit::try_from(u).expect("unit fits");
        let mut node = OverlayNode::<K, ()>::new();
        for i in 0..4u32 {
            node = node.with_child(mk(base + i), an_on_disk_child::<K, ()>(i, nt));
        }
        assert_eq!(node.num_children(), 4);
        assert!(matches!(node.store, ChildStore::Inline { .. }));
        // 5th child promotes to Heap.
        node = node.with_child(mk(base + 4), an_on_disk_child::<K, ()>(4, nt));
        assert_eq!(node.num_children(), 5);
        assert!(matches!(node.store, ChildStore::Heap { .. }));
        // Removing back to 4 demotes to Inline.
        let demoted = node
            .without_child(mk(base + 2))
            .expect("remove present key");
        assert_eq!(demoted.num_children(), 4);
        assert!(matches!(demoted.store, ChildStore::Inline { .. }));
        // A 6→5 removal stays Heap.
        let six = node.with_child(mk(base + 5), an_on_disk_child::<K, ()>(5, nt));
        let five = six.without_child(mk(base + 2)).expect("remove present key");
        assert_eq!(five.num_children(), 5);
        assert!(matches!(five.store, ChildStore::Heap { .. }));
        // Removing an absent key returns None.
        assert!(OverlayNode::<K, ()>::new()
            .without_child(mk(base))
            .is_none());
    }

    /// Exercise the immutable value carry + finalization arbiter over `<K, u64>`.
    fn check_value_and_final<K: KeyEncoding>(nt: NodeType)
    where
        K::Unit: TryFrom<u32>,
        <K::Unit as TryFrom<u32>>::Error: std::fmt::Debug,
    {
        let k0 = K::Unit::try_from(b'a' as u32).expect("unit fits");
        let node = OverlayNode::<K, u64>::new();
        assert!(!node.is_final());
        assert!(!node.has_value());
        let valued = node
            .with_child(k0, an_on_disk_child::<K, u64>(1, nt))
            .as_final()
            .with_value(7);
        assert!(valued.is_final());
        assert!(valued.has_value());
        assert_eq!(valued.get_value(), Some(7));
        assert_eq!(valued.num_children(), 1);
        // try_set_final is a single-winner arbiter on a FRESH node.
        let fresh = OverlayNode::<K, u64>::new();
        assert!(fresh.try_set_final());
        assert!(!fresh.try_set_final());
        // Original membership node is unchanged.
        assert!(!node.is_final());
        assert!(node.get_value().is_none());
    }

    /// Prefix store/truncation/matching over `<K, ()>` with a `K::MAX_PREFIX_LEN`
    /// boundary check.
    fn check_prefix<K: KeyEncoding>(units: &[K::Unit])
    where
        K::Unit: PartialEq,
    {
        let node = OverlayNode::<K, ()>::with_prefix(units);
        let expect_len = units.len().min(K::MAX_PREFIX_LEN);
        assert_eq!(node.prefix_len(), expect_len);
        assert_eq!(node.prefix(), &units[..expect_len]);
        // A key equal to the (truncated) prefix fully matches.
        let key = &units[..expect_len];
        assert_eq!(node.match_prefix(key), expect_len);
        assert!(node.prefix_matches(key));
    }

    #[test]
    fn generic_sorted_order_byte() {
        check_sorted_order::<ByteKey>(&[b'z', b'a', b'm', b'f'], NodeType::Node4);
    }

    #[test]
    fn generic_sorted_order_char() {
        check_sorted_order::<CharKey>(
            &['z' as u32, 'a' as u32, 0x1F600, 'f' as u32],
            NodeType::CharNode4,
        );
    }

    #[test]
    fn generic_tier_transitions_byte() {
        check_tier_transitions::<ByteKey>(100, NodeType::Node4);
    }

    #[test]
    fn generic_tier_transitions_char() {
        check_tier_transitions::<CharKey>(0x3000, NodeType::CharNode4);
    }

    #[test]
    fn generic_value_and_final_byte() {
        check_value_and_final::<ByteKey>(NodeType::Node4);
    }

    #[test]
    fn generic_value_and_final_char() {
        check_value_and_final::<CharKey>(NodeType::CharNode4);
    }

    #[test]
    fn generic_prefix_byte() {
        // 16 > MAX_PREFIX_LEN (12) ⇒ truncation path exercised.
        let units: Vec<u8> = b"abcdefghijklmnop".to_vec();
        check_prefix::<ByteKey>(&units);
        // A short prefix (no truncation).
        check_prefix::<ByteKey>(b"hi");
    }

    #[test]
    fn generic_prefix_char() {
        // 9 > MAX_PREFIX_LEN (6) ⇒ truncation path exercised.
        let units: Vec<u32> = "abcdefghi".chars().map(|c| c as u32).collect();
        check_prefix::<CharKey>(&units);
        // Include a beyond-BMP scalar in a short prefix (no truncation).
        let short: Vec<u32> = vec!['h' as u32, 0x1F600];
        check_prefix::<CharKey>(&short);
    }

    #[test]
    fn generic_memory_usage_is_monotonic_byte() {
        let empty = OverlayNode::<ByteKey, ()>::new().memory_usage();
        let with_one = OverlayNode::<ByteKey, ()>::new()
            .with_child(b'a', an_on_disk_child::<ByteKey, ()>(1, NodeType::Node4))
            .memory_usage();
        assert!(
            with_one >= empty,
            "adding a child must not shrink reported usage"
        );
    }

    #[test]
    fn generic_memory_usage_is_monotonic_char() {
        let empty = OverlayNode::<CharKey, ()>::new().memory_usage();
        let with_one = OverlayNode::<CharKey, ()>::new()
            .with_child(
                'a' as u32,
                an_on_disk_child::<CharKey, ()>(1, NodeType::CharNode4),
            )
            .memory_usage();
        assert!(
            with_one >= empty,
            "adding a child must not shrink reported usage"
        );
    }

    // ---- Second cross-instantiation batch: replace / remove-shift / version /
    //      find-miss / large-fanout-exhaustive (mirrors the remaining pre-G4
    //      per-variant node tests, run once over both keys). ----

    /// Replacing an existing key keeps `num_children` and swaps the child slot.
    fn check_replace_child<K: KeyEncoding>(nt: NodeType)
    where
        K::Unit: TryFrom<u32>,
        <K::Unit as TryFrom<u32>>::Error: std::fmt::Debug,
    {
        let k = K::Unit::try_from(b'a' as u32).expect("unit fits");
        let raw1 = SwizzledPtr::on_disk(1, 100, nt).to_raw();
        let raw2 = SwizzledPtr::on_disk(2, 200, nt).to_raw();
        let node =
            OverlayNode::<K, ()>::new().with_child(k, Child::OnDisk(SwizzledPtr::from_raw(raw1)));
        assert_eq!(node.num_children(), 1);
        let found1 = node
            .find_child(k)
            .expect("present")
            .as_on_disk()
            .expect("on-disk")
            .to_raw();
        assert_eq!(found1, raw1);
        let node2 = node.with_child(k, Child::OnDisk(SwizzledPtr::from_raw(raw2)));
        assert_eq!(
            node2.num_children(),
            1,
            "replace must not change child count"
        );
        let found2 = node2
            .find_child(k)
            .expect("present")
            .as_on_disk()
            .expect("on-disk")
            .to_raw();
        assert_eq!(found2, raw2);
    }

    /// Removing a middle key shifts the survivors left and preserves order; the
    /// original is unchanged.
    fn check_remove_middle<K: KeyEncoding>(keys: &[K::Unit], remove: K::Unit, nt: NodeType)
    where
        K::Unit: Into<u64> + PartialEq,
    {
        let mut node = OverlayNode::<K, ()>::new();
        for (i, &k) in keys.iter().enumerate() {
            node = node.with_child(k, an_on_disk_child::<K, ()>(i as u32, nt));
        }
        let before = node.num_children();
        let node2 = node.without_child(remove).expect("remove a present key");
        assert_eq!(node2.num_children(), before - 1);
        assert!(!node2.has_child(remove));
        // Survivors remain sorted ascending.
        let survivors: Vec<u64> = node2.iter_children().map(|(&k, _)| k.into()).collect();
        let mut sorted = survivors.clone();
        sorted.sort_unstable();
        assert_eq!(survivors, sorted);
        // Original unchanged.
        assert_eq!(node.num_children(), before);
        assert!(node.has_child(remove));
    }

    /// `find_child` misses return None (incl. the sorted early-exit), and
    /// `version` increments by one per structural edit.
    fn check_find_miss_and_version<K: KeyEncoding>(nt: NodeType)
    where
        K::Unit: TryFrom<u32>,
        <K::Unit as TryFrom<u32>>::Error: std::fmt::Debug,
    {
        let mk = |u: u32| K::Unit::try_from(u).expect("unit fits");
        let node = OverlayNode::<K, ()>::new();
        assert_eq!(node.version(), 0);
        assert!(node.find_child(mk(b'x' as u32)).is_none());

        let node2 = node.with_child(mk(b'm' as u32), an_on_disk_child::<K, ()>(1, nt));
        assert_eq!(node2.version(), 1);
        // A key below the smallest present key misses via the sorted early-exit.
        assert!(node2.find_child(mk(b'a' as u32)).is_none());
        // A key above misses too.
        assert!(node2.find_child(mk(b'z' as u32)).is_none());

        let node3 = node2.as_final();
        assert_eq!(
            node3.version(),
            2,
            "as_final is a structural edit ⇒ +1 version"
        );
    }

    /// Exhaustively fill a wide key range (forcing the Heap tier) and confirm
    /// every key is findable and iteration is the full sorted sequence.
    fn check_large_fanout<K: KeyEncoding>(range: std::ops::RangeInclusive<u32>, nt: NodeType)
    where
        K::Unit: TryFrom<u32> + Into<u64>,
        <K::Unit as TryFrom<u32>>::Error: std::fmt::Debug,
    {
        let mut node = OverlayNode::<K, ()>::new();
        let mut expected: Vec<u64> = Vec::new();
        for u in range.clone() {
            let k = K::Unit::try_from(u).expect("unit fits");
            node = node.with_child(k, an_on_disk_child::<K, ()>(u, nt));
            expected.push(k.into());
        }
        assert_eq!(node.num_children(), expected.len());
        assert!(matches!(node.store, ChildStore::Heap { .. }));
        for u in range {
            let k = K::Unit::try_from(u).expect("unit fits");
            assert!(node.has_child(k), "key {u} must be present");
        }
        let collected: Vec<u64> = node.iter_children().map(|(&k, _)| k.into()).collect();
        expected.sort_unstable();
        assert_eq!(collected, expected);
    }

    #[test]
    fn generic_replace_child_byte() {
        check_replace_child::<ByteKey>(NodeType::Node4);
    }

    #[test]
    fn generic_replace_child_char() {
        check_replace_child::<CharKey>(NodeType::CharNode4);
    }

    #[test]
    fn generic_remove_middle_byte() {
        check_remove_middle::<ByteKey>(&[b'a', b'b', b'c'], b'b', NodeType::Node4);
    }

    #[test]
    fn generic_remove_middle_char() {
        check_remove_middle::<CharKey>(
            &['a' as u32, 'b' as u32, 0x1F600],
            'b' as u32,
            NodeType::CharNode4,
        );
    }

    #[test]
    fn generic_find_miss_and_version_byte() {
        check_find_miss_and_version::<ByteKey>(NodeType::Node4);
    }

    #[test]
    fn generic_find_miss_and_version_char() {
        check_find_miss_and_version::<CharKey>(NodeType::CharNode4);
    }

    #[test]
    fn generic_large_fanout_byte() {
        // Full u8 range exercises the Heap tier exhaustively.
        check_large_fanout::<ByteKey>(0..=255, NodeType::Node4);
    }

    #[test]
    fn generic_large_fanout_char() {
        // A wide u32 window (beyond a single byte) exercises the Heap tier and the
        // genuine u32 key width.
        check_large_fanout::<CharKey>(0x2000..=0x2100, NodeType::CharNode4);
    }

    // ---- Third cross-instantiation batch: with_prefix_replaced / is_empty /
    //      Default / Debug / heap-tier Clone (the remaining method surface). ----

    /// `with_prefix_replaced` swaps the prefix (with truncation), keeps children,
    /// and bumps the version; `Default` equals `new`; `Debug` is non-empty.
    fn check_prefix_replaced_default_debug<K: KeyEncoding>(units: &[K::Unit], nt: NodeType)
    where
        K::Unit: TryFrom<u32> + PartialEq,
        <K::Unit as TryFrom<u32>>::Error: std::fmt::Debug,
    {
        let k0 = K::Unit::try_from(b'a' as u32).expect("unit fits");
        let node = OverlayNode::<K, ()>::new().with_child(k0, an_on_disk_child::<K, ()>(1, nt));
        assert!(!node.is_empty());
        let v0 = node.version();
        let replaced = node.with_prefix_replaced(units);
        let expect_len = units.len().min(K::MAX_PREFIX_LEN);
        assert_eq!(replaced.prefix_len(), expect_len);
        assert_eq!(replaced.prefix(), &units[..expect_len]);
        assert_eq!(
            replaced.num_children(),
            1,
            "replacing prefix keeps children"
        );
        assert_eq!(replaced.version(), v0 + 1);

        // Default == new (empty membership node).
        let d: OverlayNode<K, ()> = Default::default();
        assert!(d.is_empty());
        assert!(!d.is_final());

        // Debug renders without panicking and mentions the type.
        let s = format!("{:?}", replaced);
        assert!(s.contains("OverlayNode"));
    }

    /// Cloning a Heap-tier node deep-copies the child set (the clone is an
    /// independent value; mutating-by-version of one does not affect the other).
    fn check_heap_clone<K: KeyEncoding>(nt: NodeType)
    where
        K::Unit: TryFrom<u32> + Into<u64>,
        <K::Unit as TryFrom<u32>>::Error: std::fmt::Debug,
    {
        let mut node = OverlayNode::<K, ()>::new();
        for u in 0..6u32 {
            let k = K::Unit::try_from(0x40 + u).expect("unit fits");
            node = node.with_child(k, an_on_disk_child::<K, ()>(u, nt));
        }
        assert!(matches!(node.store, ChildStore::Heap { .. }));
        let cloned = node.clone();
        assert_eq!(cloned.num_children(), node.num_children());
        let a: Vec<u64> = node.iter_children().map(|(&k, _)| k.into()).collect();
        let b: Vec<u64> = cloned.iter_children().map(|(&k, _)| k.into()).collect();
        assert_eq!(a, b);
        assert_eq!(cloned.version(), node.version());
    }

    #[test]
    fn generic_prefix_replaced_default_debug_byte() {
        check_prefix_replaced_default_debug::<ByteKey>(b"abcdefghijklmnop", NodeType::Node4);
    }

    #[test]
    fn generic_prefix_replaced_default_debug_char() {
        let units: Vec<u32> = "abcdefghi".chars().map(|c| c as u32).collect();
        check_prefix_replaced_default_debug::<CharKey>(&units, NodeType::CharNode4);
    }

    #[test]
    fn generic_heap_clone_byte() {
        check_heap_clone::<ByteKey>(NodeType::Node4);
    }

    #[test]
    fn generic_heap_clone_char() {
        check_heap_clone::<CharKey>(NodeType::CharNode4);
    }

    // ---- R-B (proven overlay DELETE) node primitive: `as_non_final`. ----
    //
    // The safe mirror of `as_final`: it clears `IS_FINAL`+`HAS_VALUE` and drops
    // the value on a FRESH COPY (immutability preserved), while RETAINING the
    // child store + prefix (so removing a prefix term keeps the longer term
    // reachable). These tests are the RB0 gate; both instantiations run.

    /// `as_non_final` clears finality + value on a copy, retains children, leaves
    /// the original untouched, and round-trips with `as_final`.
    fn check_as_non_final<K: KeyEncoding>(nt: NodeType)
    where
        K::Unit: TryFrom<u32>,
        <K::Unit as TryFrom<u32>>::Error: std::fmt::Debug,
    {
        let kx = K::Unit::try_from(b'x' as u32).expect("unit fits");
        // A final, value-carrying node with one child.
        let original = OverlayNode::<K, u64>::new()
            .with_child(kx, an_on_disk_child::<K, u64>(1, nt))
            .as_final()
            .with_value(42);
        assert!(original.is_final());
        assert!(original.has_value());
        assert_eq!(original.get_value(), Some(42));
        assert_eq!(original.num_children(), 1);
        let v_before = original.version();

        // Clearing produces a NON-final, value-less copy that RETAINS the child.
        let cleared = original.as_non_final();
        assert!(!cleared.is_final(), "as_non_final must clear IS_FINAL");
        assert!(!cleared.has_value(), "as_non_final must clear HAS_VALUE");
        assert_eq!(
            cleared.get_value(),
            None,
            "as_non_final must drop the value (None, not Some(0))"
        );
        assert_eq!(
            cleared.num_children(),
            1,
            "as_non_final must RETAIN children (remove \"cat\" keeps \"cats\")"
        );
        assert!(
            cleared.has_child(kx),
            "the retained child must still be found"
        );
        assert_eq!(
            cleared.version(),
            v_before + 1,
            "as_non_final is a structural edit ⇒ +1 version"
        );

        // ORIGINAL UNCHANGED (persistent data structure — a concurrent reader
        // holding the original observes no clear).
        assert!(original.is_final(), "original must remain final");
        assert!(original.has_value(), "original must keep its value");
        assert_eq!(original.get_value(), Some(42));

        // Round-trip: as_non_final ∘ as_final returns to a final, child-retaining
        // node (the value is NOT restored — as_final does not synthesize a value).
        let refinal = cleared.as_final();
        assert!(refinal.is_final(), "re-finalized node must be final again");
        assert!(
            !refinal.has_value(),
            "re-finalizing a cleared node does not resurrect the dropped value"
        );
        assert_eq!(refinal.num_children(), 1, "round-trip must keep children");
    }

    /// Deep-child retention: a node "cat" cleared via `as_non_final` must keep its
    /// "cats" descendant final and reachable (the prefix-of-a-longer-term case).
    fn check_as_non_final_deep_child<K: KeyEncoding>(_nt: NodeType)
    where
        K::Unit: TryFrom<u32>,
        <K::Unit as TryFrom<u32>>::Error: std::fmt::Debug,
    {
        // Build "cat" (final) with a child edge 's' → "cats" leaf (final): the
        // node-local shape of `root -'c'-'a'-> cat[final] -'s'-> cats[final]`.
        let ks = K::Unit::try_from(b's' as u32).expect("unit fits");
        let cats_leaf = Arc::new(OverlayNode::<K, ()>::new().as_final());
        let cat = OverlayNode::<K, ()>::new()
            .with_child(ks, Child::InMem(Arc::clone(&cats_leaf)))
            .as_final();
        assert!(cat.is_final());
        assert!(cat.has_child(ks));

        // "Remove cat": clear finality on a fresh copy; the 's' → "cats" edge and
        // the final "cats" leaf must survive.
        let cat_removed = cat.as_non_final();
        assert!(!cat_removed.is_final(), "\"cat\" must no longer be final");
        let surviving = cat_removed
            .find_child(ks)
            .expect("\"cats\" edge must survive removing \"cat\"")
            .as_in_mem()
            .expect("the surviving child is in-memory");
        assert!(
            surviving.is_final(),
            "\"cats\" must remain final after removing the prefix \"cat\""
        );
        // The original "cat" node is still final (immutability).
        assert!(cat.is_final(), "original \"cat\" node unchanged");
    }

    #[test]
    fn generic_as_non_final_byte() {
        check_as_non_final::<ByteKey>(NodeType::Node4);
    }

    #[test]
    fn generic_as_non_final_char() {
        check_as_non_final::<CharKey>(NodeType::CharNode4);
    }

    #[test]
    fn generic_as_non_final_deep_child_byte() {
        check_as_non_final_deep_child::<ByteKey>(NodeType::Node4);
    }

    #[test]
    fn generic_as_non_final_deep_child_char() {
        check_as_non_final_deep_child::<CharKey>(NodeType::CharNode4);
    }
}
