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
//! The pointer stores an immutable `{ node, term_count }` revision in an
//! `arc_swap::ArcSwapOption` — a genuinely-atomic, lock-free `Arc` cell. Keeping
//! cardinality in the same published revision as the root prevents snapshots
//! from observing a root from one mutation and a count from another. An earlier
//! iteration stored raw
//! `Arc` pointers in an `AtomicU64`, which is unsound without an
//! epoch/hazard-pointer scheme because `load()` can race with replacement and
//! attempt to increment a freed allocation; a stopgap then retreated to a
//! `RwLock`, which reintroduced a lock on every "CAS". `ArcSwapOption` is the
//! sound *and* lock-free resolution: its `load` is protected by ArcSwap's
//! guarded reclamation, so a reader never touches a freed allocation, and no
//! lock serializes concurrent readers/writers.
//!
//! # Memory Safety
//!
//! - `load()` clones the current `Arc` via `load_full()` (lock-free, hazard-protected)
//! - `compare_exchange()` swaps only when the stored `Arc` is pointer-equal to
//!   the expected `Arc` (`ArcSwapOption::compare_and_swap`)
//! - rejected replacements are dropped normally

use std::sync::Arc;

use crate::persistent_artrie::core::eviction::{
    PackedResidencyDelta, PreparedPackedResidency, PublishedRegistryCatalog, RegistryFamily,
    RegistryTransitionAuthority, ResidencyHelpOutcome,
};

#[cfg(not(target_os = "wasi"))]
use arc_swap::ArcSwapOption;
#[cfg(target_os = "wasi")]
use std::sync::Mutex;

use super::node::OverlayNode;
use crate::persistent_artrie::core::key_encoding::KeyEncoding;

/// Null pointer sentinel value used by `as_raw` for diagnostics/tests.
const NULL_PTR: u64 = 0;

/// Identity of one published eviction-registry generation.
///
/// The token is deliberately opaque. Pointer identity, rather than a wrapping
/// integer, prevents ABA when a coordinator is disabled and later recreated.
/// Keeping the token in this neutral publication layer also prevents the root
/// primitive from depending on the eviction-registry implementation.
#[derive(Clone, Debug)]
pub(crate) struct EvictionBinding(Arc<()>);

impl EvictionBinding {
    pub(crate) fn new() -> Self {
        Self(Arc::new(()))
    }

    #[inline]
    pub(crate) fn same_publication(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Root-carried logical residency authority for one exact checkpoint catalog.
///
/// The catalog `Arc` gives the root direct ownership of immutable topology and
/// lock-free materialization arrays. Sparse deltas are embedded in the already
/// allocated root revision, avoiding a second allocation on point faults.
pub(crate) struct RootEvictionRevision {
    catalog: Arc<PublishedRegistryCatalog>,
    predecessor_ordinal: u32,
    ordinal: u32,
    resident_nodes: usize,
    resident_serialized_bytes: usize,
    delta: PackedResidencyDelta,
}

impl RootEvictionRevision {
    fn initial(
        catalog: Arc<PublishedRegistryCatalog>,
        resident_nodes: usize,
        resident_serialized_bytes: usize,
    ) -> Self {
        Self {
            catalog,
            predecessor_ordinal: 0,
            ordinal: 0,
            resident_nodes,
            resident_serialized_bytes,
            delta: PackedResidencyDelta::None,
        }
    }

    #[cfg(test)]
    fn settled_successor(&self) -> Self {
        Self {
            catalog: Arc::clone(&self.catalog),
            predecessor_ordinal: self.ordinal,
            ordinal: self.ordinal,
            resident_nodes: self.resident_nodes,
            resident_serialized_bytes: self.resident_serialized_bytes,
            delta: PackedResidencyDelta::None,
        }
    }

    fn try_exact_successor(&self, prepared: PreparedPackedResidency) -> Option<Self> {
        let (
            catalog,
            predecessor_ordinal,
            target_ordinal,
            resident_nodes,
            resident_serialized_bytes,
            delta,
        ) = prepared.into_root_parts(&self.catalog, self.ordinal)?;
        Some(Self {
            catalog,
            predecessor_ordinal,
            ordinal: target_ordinal,
            resident_nodes,
            resident_serialized_bytes,
            delta,
        })
    }

    #[inline]
    fn help<K: RegistryFamily>(&self) -> ResidencyHelpOutcome {
        K::residency(&self.catalog).help(self.predecessor_ordinal, self.ordinal, &self.delta)
    }

    #[inline(always)]
    pub(crate) fn binding(&self) -> &EvictionBinding {
        self.catalog.binding()
    }

    #[inline(always)]
    pub(crate) fn catalog(&self) -> &Arc<PublishedRegistryCatalog> {
        &self.catalog
    }

    #[inline(always)]
    #[cfg(test)]
    pub(crate) fn predecessor_ordinal(&self) -> u32 {
        self.predecessor_ordinal
    }

    #[inline(always)]
    pub(crate) fn ordinal(&self) -> u32 {
        self.ordinal
    }

    #[inline(always)]
    pub(crate) fn resident_totals(&self) -> (usize, usize) {
        (self.resident_nodes, self.resident_serialized_bytes)
    }
}

/// One immutable, atomically-published dictionary revision.
///
/// This is deliberately private: callers manipulate roots through
/// `AtomicNodePtr`, so the root and its exact cardinality cannot be separated.
struct PublishedRoot<K: KeyEncoding, V> {
    node: Arc<OverlayNode<K, V>>,
    term_count: usize,
    eviction: Option<Arc<RootEvictionRevision>>,
}

/// Exact identity of one atomically published root revision.
///
/// Retaining the enclosing `Arc<PublishedRoot>` distinguishes metadata-only
/// publications that intentionally retain the same node and cardinality. A
/// node `Arc` alone cannot distinguish a checkpoint binding publication from
/// its predecessor and is therefore insufficient for eviction/fault CAS.
pub(crate) struct RootRevision<K: KeyEncoding, V>(Arc<PublishedRoot<K, V>>);

impl<K: KeyEncoding, V> Clone for RootRevision<K, V> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<K: KeyEncoding, V> RootRevision<K, V> {
    #[inline]
    pub(crate) fn node(&self) -> &Arc<OverlayNode<K, V>> {
        &self.0.node
    }

    #[inline]
    pub(crate) fn term_count(&self) -> usize {
        self.0.term_count
    }

    #[inline]
    pub(crate) fn eviction_binding(&self) -> Option<&EvictionBinding> {
        self.0
            .eviction
            .as_deref()
            .map(RootEvictionRevision::binding)
    }

    #[inline]
    pub(crate) fn eviction_revision(&self) -> Option<&RootEvictionRevision> {
        self.0.eviction.as_deref()
    }

    #[inline]
    pub(crate) fn same_revision(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl<K: RegistryFamily, V> RootRevision<K, V> {
    /// Help only the descriptor attached to this exact published root.
    /// Unpublished losing candidates never acquire this authority.
    pub(crate) fn help_eviction_revision(&self) -> Option<&RootEvictionRevision> {
        let revision = self.eviction_revision()?;
        match revision.help::<K>() {
            ResidencyHelpOutcome::Complete => Some(revision),
            ResidencyHelpOutcome::Stale => None,
        }
    }
}

/// Allocation-complete metadata-only publication prepared before a checkpoint
/// enters the coordinator's registry critical section.
///
/// Both revisions retain the same immutable node and cardinality. The successor
/// differs only by its eviction binding. Constructing it in advance ensures the
/// registry swap, root CAS, and deferred stamp stores contain no allocation.
pub(crate) struct PreparedRootBinding<K: KeyEncoding, V> {
    expected: RootRevision<K, V>,
    next: Arc<PublishedRoot<K, V>>,
}

impl<K: KeyEncoding, V> PreparedRootBinding<K, V> {
    #[inline]
    pub(crate) fn binding(&self) -> &EvictionBinding {
        self.next
            .eviction
            .as_deref()
            .expect("prepared checkpoint root always has eviction metadata")
            .binding()
    }
}

/// Allocation-complete metadata-only detachment prepared for coordinator
/// retirement.
///
/// The successor preserves the immutable root and its exact cardinality while
/// removing any eviction-registry binding. Preparation occurs before the
/// lifecycle transaction whenever possible; publication requires the
/// coordinator-only [`RegistryTransitionAuthority`].
pub(crate) struct PreparedRootDetachment<K: KeyEncoding, V> {
    expected: RootRevision<K, V>,
    next: Arc<PublishedRoot<K, V>>,
}

/// Allocation-complete exact-binding-preserving root replacement.
///
/// Construction validates the captured revision's binding and allocates the
/// complete successor revision before the coordinator lifecycle transaction.
/// Publication additionally requires an unforgeable lifecycle authority, so
/// no production caller can perform the root CAS outside the registry
/// revalidation-and-residency-commit critical section.
pub(crate) struct PreparedBoundRootTransition<K: KeyEncoding, V> {
    expected: RootRevision<K, V>,
    next: Arc<PublishedRoot<K, V>>,
}

impl<K: KeyEncoding, V> PreparedBoundRootTransition<K, V> {
    #[inline]
    pub(crate) fn binding(&self) -> &EvictionBinding {
        self.next
            .eviction
            .as_ref()
            .expect("prepared bound root transition always retains eviction metadata")
            .binding()
    }

    pub(crate) fn next_eviction_revision(&self) -> &RootEvictionRevision {
        self.next
            .eviction
            .as_deref()
            .expect("prepared bound root transition always retains eviction metadata")
    }
}

impl<K: RegistryFamily, V> PreparedBoundRootTransition<K, V> {
    /// Exact aggregate removed by this root candidate. Point-fault candidates
    /// return `None` because their totals increase rather than decrease.
    pub(crate) fn evicted_totals(&self) -> Option<(usize, usize)> {
        let predecessor = self.expected.eviction_revision()?.resident_totals();
        let successor = self.next_eviction_revision().resident_totals();
        Some((
            predecessor.0.checked_sub(successor.0)?,
            predecessor.1.checked_sub(successor.1)?,
        ))
    }

    /// Enumerate LRU hashes cleared by the candidate directly from its sparse
    /// delta and immutable catalog. No per-path commit records are retained.
    pub(crate) fn for_each_cleared_path_hash(&self, mut visit: impl FnMut(u64)) -> bool {
        let revision = self.next_eviction_revision();
        let mut valid = true;
        revision.delta.for_each_cleared_path(|path_index| {
            if let Some(path_hash) = K::path_hash(&revision.catalog, path_index) {
                visit(path_hash);
            } else {
                valid = false;
            }
        });
        valid
    }
}

/// One durable-stamp store prepared during serialization and applied only after
/// the matching registry and root binding have been atomically installed.
pub(crate) struct DeferredDurableStamp<K: KeyEncoding, V> {
    node: Arc<OverlayNode<K, V>>,
    raw: u64,
}

impl<K: KeyEncoding, V: Clone> DeferredDurableStamp<K, V> {
    pub(crate) fn new(node: Arc<OverlayNode<K, V>>, raw: u64) -> Self {
        debug_assert_ne!(raw, 0, "a durable stamp must name a disk record");
        Self { node, raw }
    }

    #[inline]
    pub(crate) fn apply(&self) {
        self.node.set_durable_stamp(self.raw);
    }
}

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
    #[cfg(not(target_os = "wasi"))]
    ptr: ArcSwapOption<PublishedRoot<K, V>>,
    /// WASI Preview 1 has no threads; its ArcSwap pointer publication is not
    /// portable, so use a target-local serialized cell with identical semantics.
    #[cfg(target_os = "wasi")]
    ptr: Mutex<Option<Arc<PublishedRoot<K, V>>>>,
}

// Manual `Debug` so neither `K::Unit` nor `V` need `Debug`.
impl<K: KeyEncoding, V> std::fmt::Debug for AtomicNodePtr<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        #[cfg(not(target_os = "wasi"))]
        let is_null = self.ptr.load().is_none();
        #[cfg(target_os = "wasi")]
        let is_null = self
            .ptr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_none();
        f.debug_struct("AtomicNodePtr")
            .field("is_null", &is_null)
            .finish()
    }
}

impl<K: KeyEncoding, V: Clone> AtomicNodePtr<K, V> {
    /// Create a new atomic pointer from an Arc.
    ///
    /// The Arc's reference count is NOT incremented - ownership is transferred.
    pub fn new(node: Arc<OverlayNode<K, V>>) -> Self {
        Self::new_with_term_count(node, 0)
    }

    /// Create a new atomic revision with an exact cardinality.
    pub fn new_with_term_count(node: Arc<OverlayNode<K, V>>, term_count: usize) -> Self {
        let revision = Arc::new(PublishedRoot {
            node,
            term_count,
            eviction: None,
        });
        Self {
            #[cfg(not(target_os = "wasi"))]
            ptr: ArcSwapOption::new(Some(revision)),
            #[cfg(target_os = "wasi")]
            ptr: Mutex::new(Some(revision)),
        }
    }

    /// Create a null atomic pointer.
    pub fn null() -> Self {
        Self {
            #[cfg(not(target_os = "wasi"))]
            ptr: ArcSwapOption::empty(),
            #[cfg(target_os = "wasi")]
            ptr: Mutex::new(None),
        }
    }

    /// Check if the pointer is null.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.load().is_none()
    }

    /// Load the current node pointer.
    ///
    /// This increments the Arc's reference count before returning,
    /// so the caller receives a valid Arc that they own.
    pub fn load(&self) -> Option<Arc<OverlayNode<K, V>>> {
        self.load_with_term_count().map(|(node, _)| node)
    }

    /// Load a coherent root and cardinality from one published revision.
    pub fn load_with_term_count(&self) -> Option<(Arc<OverlayNode<K, V>>, usize)> {
        self.load_revision()
            .map(|revision| (Arc::clone(revision.node()), revision.term_count()))
    }

    /// Load one exact root revision, including its eviction binding.
    pub(crate) fn load_revision(&self) -> Option<RootRevision<K, V>> {
        #[cfg(not(target_os = "wasi"))]
        {
            self.ptr.load_full().map(RootRevision)
        }
        #[cfg(target_os = "wasi")]
        {
            self.ptr
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
                .map(|revision| RootRevision(Arc::clone(revision)))
        }
    }

    /// Return the cardinality associated with the currently-published root.
    #[inline]
    pub fn term_count(&self) -> usize {
        self.load_with_term_count()
            .map_or(0, |(_, term_count)| term_count)
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
        let term_count = self.term_count();
        self.store_with_term_count(node, term_count);
    }

    /// Atomically replace both the root and its exact cardinality.
    pub fn store_with_term_count(&self, node: Arc<OverlayNode<K, V>>, term_count: usize) {
        let revision = Arc::new(PublishedRoot {
            node,
            term_count,
            eviction: None,
        });
        #[cfg(not(target_os = "wasi"))]
        self.ptr.store(Some(revision));
        #[cfg(target_os = "wasi")]
        {
            *self
                .ptr
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(revision);
        }
    }

    /// Store null, returning the old value.
    pub fn take(&self) -> Option<Arc<OverlayNode<K, V>>> {
        #[cfg(not(target_os = "wasi"))]
        {
            self.ptr
                .swap(None)
                .map(|revision| Arc::clone(&revision.node))
        }
        #[cfg(target_os = "wasi")]
        {
            self.ptr
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .map(|revision| Arc::clone(&revision.node))
        }
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
    ) -> super::OverlayCasResult<K, V> {
        self.compare_exchange_counted(expected, new, 0)
    }

    /// Atomically replace a root and adjust its cardinality in the same
    /// publication operation.
    ///
    /// `term_count_delta` must be `1` for a newly inserted terminal, `-1` for
    /// a removed terminal, and `0` for structural/value-only rewrites.
    pub fn compare_exchange_counted(
        &self,
        expected: &Arc<OverlayNode<K, V>>,
        new: Arc<OverlayNode<K, V>>,
        term_count_delta: isize,
    ) -> super::OverlayCasResult<K, V> {
        // Genuinely-atomic CAS: swap `new` in iff the stored Arc is pointer-equal
        // to `expected`. `&Arc<_>` implements `AsRaw`, so we compare by the node's
        // raw pointer with no extra refcount bump. `compare_and_swap` returns the
        // value stored BEFORE the operation; success <=> it is pointer-equal to
        // `expected`.
        #[cfg(not(target_os = "wasi"))]
        {
            let Some(current) = self.ptr.load_full() else {
                return Err(Arc::new(OverlayNode::new()));
            };
            if !Arc::ptr_eq(&current.node, expected) {
                return Err(Arc::clone(&current.node));
            }
            let term_count = current
                .term_count
                .checked_add_signed(term_count_delta)
                .expect("published ARTrie term count overflow/underflow");
            let next = Arc::new(PublishedRoot {
                node: new,
                term_count,
                eviction: None,
            });
            let prev = self.ptr.compare_and_swap(&current, Some(next));
            match &*prev {
                Some(p) if Arc::ptr_eq(p, &current) => Ok(Arc::clone(&p.node)),
                Some(p) => Err(Arc::clone(&p.node)),
                None => Err(Arc::new(OverlayNode::new())),
            }
        }
        #[cfg(target_os = "wasi")]
        {
            let mut slot = self
                .ptr
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match slot.as_ref() {
                Some(actual) if Arc::ptr_eq(&actual.node, expected) => {
                    let previous = Arc::clone(&actual.node);
                    let term_count = actual
                        .term_count
                        .checked_add_signed(term_count_delta)
                        .expect("published ARTrie term count overflow/underflow");
                    *slot = Some(Arc::new(PublishedRoot {
                        node: new,
                        term_count,
                        eviction: None,
                    }));
                    Ok(previous)
                }
                Some(actual) => Err(Arc::clone(&actual.node)),
                None => Err(Arc::new(OverlayNode::new())),
            }
        }
    }

    /// Weak compare and exchange (may spuriously fail).
    pub fn compare_exchange_weak(
        &self,
        expected: &Arc<OverlayNode<K, V>>,
        new: Arc<OverlayNode<K, V>>,
    ) -> super::OverlayCasResult<K, V> {
        self.compare_exchange(expected, new)
    }

    /// Publish a semantic root rewrite against one exact revision.
    ///
    /// A semantic rewrite always clears the eviction binding in the same CAS
    /// that publishes the new root. Consequently there is no interval in which
    /// a changed trie can still authorize candidates from an older registry.
    pub(crate) fn compare_exchange_revision_counted(
        &self,
        expected: &RootRevision<K, V>,
        new: Arc<OverlayNode<K, V>>,
        term_count_delta: isize,
    ) -> Result<RootRevision<K, V>, Option<RootRevision<K, V>>> {
        let term_count = expected
            .term_count()
            .checked_add_signed(term_count_delta)
            .expect("published ARTrie term count overflow/underflow");
        self.compare_exchange_revision(
            expected,
            Arc::new(PublishedRoot {
                node: new,
                term_count,
                eviction: None,
            }),
        )
    }

    /// Allocate and validate a binding-preserving replacement before entering
    /// the coordinator lifecycle transaction.
    #[cfg(test)]
    pub(crate) fn prepare_bound_root_transition(
        expected: &RootRevision<K, V>,
        required_binding: &EvictionBinding,
        new: Arc<OverlayNode<K, V>>,
    ) -> Option<PreparedBoundRootTransition<K, V>>
    where
        K: RegistryFamily,
    {
        let eviction = expected.eviction_revision()?;
        if !eviction.binding().same_publication(required_binding) {
            return None;
        }
        if eviction.help::<K>() != ResidencyHelpOutcome::Complete {
            return None;
        }
        Some(PreparedBoundRootTransition {
            expected: expected.clone(),
            next: Arc::new(PublishedRoot {
                node: new,
                term_count: expected.term_count(),
                eviction: Some(Arc::new(eviction.settled_successor())),
            }),
        })
    }

    /// Construct the final exact successor once, consuming the packed
    /// descriptor into the candidate root before its CAS.
    pub(crate) fn prepare_exact_root_transition(
        expected: &RootRevision<K, V>,
        new: Arc<OverlayNode<K, V>>,
        prepared: PreparedPackedResidency,
    ) -> Option<PreparedBoundRootTransition<K, V>>
    where
        K: RegistryFamily,
    {
        let eviction = expected.eviction_revision()?;
        let successor = eviction.try_exact_successor(prepared)?;
        Some(PreparedBoundRootTransition {
            expected: expected.clone(),
            next: Arc::new(PublishedRoot {
                node: new,
                term_count: expected.term_count(),
                eviction: Some(Arc::new(successor)),
            }),
        })
    }

    /// Publish a preallocated exact-binding-preserving replacement.
    ///
    /// [`RegistryTransitionAuthority`] can be constructed only by the eviction
    /// coordinator while its lifecycle gate is held. The coordinator also
    /// holds the registry write lock, has revalidated the exact generation and
    /// residency delta, and commits that delta before releasing either lock.
    pub(crate) fn publish_bound_root_transition(
        &self,
        prepared: &PreparedBoundRootTransition<K, V>,
    ) -> Result<RootRevision<K, V>, Option<RootRevision<K, V>>>
    where
        K: RegistryFamily,
    {
        let published =
            self.compare_exchange_revision(&prepared.expected, Arc::clone(&prepared.next));
        if published.is_ok() {
            let outcome = prepared.next_eviction_revision().help::<K>();
            debug_assert!(matches!(
                outcome,
                ResidencyHelpOutcome::Complete | ResidencyHelpOutcome::Stale
            ));
        }
        published
    }

    /// Test-only direct wrapper for the atomic binding guard. Production code
    /// must use [`Self::publish_bound_root_transition`] through the coordinator.
    #[cfg(test)]
    fn compare_exchange_revision_preserving_binding(
        &self,
        expected: &RootRevision<K, V>,
        required_binding: &EvictionBinding,
        new: Arc<OverlayNode<K, V>>,
    ) -> Result<RootRevision<K, V>, Option<RootRevision<K, V>>>
    where
        K: RegistryFamily,
    {
        let Some(prepared) = Self::prepare_bound_root_transition(expected, required_binding, new)
        else {
            return Err(Some(expected.clone()));
        };
        self.compare_exchange_revision(&prepared.expected, prepared.next)
    }

    /// Prepare an unchanged root/count revision for binding to a registry.
    ///
    /// This allocation-bearing phase runs before registry publication. The
    /// resulting object can be CAS-published without allocation while holding
    /// the coordinator registry write lock.
    pub(crate) fn prepare_checkpoint_binding(
        expected: &RootRevision<K, V>,
        catalog: Arc<PublishedRegistryCatalog>,
        resident_nodes: usize,
        resident_serialized_bytes: usize,
    ) -> PreparedRootBinding<K, V> {
        PreparedRootBinding {
            expected: expected.clone(),
            next: Arc::new(PublishedRoot {
                node: Arc::clone(expected.node()),
                term_count: expected.term_count(),
                eviction: Some(Arc::new(RootEvictionRevision::initial(
                    catalog,
                    resident_nodes,
                    resident_serialized_bytes,
                ))),
            }),
        }
    }

    /// Test seam for exercising the otherwise astronomical ordinal-rollover
    /// boundary without performing `u32::MAX` successful publications.
    #[cfg(test)]
    pub(crate) fn prepare_checkpoint_binding_at_ordinal(
        expected: &RootRevision<K, V>,
        catalog: Arc<PublishedRegistryCatalog>,
        ordinal: u32,
        resident_nodes: usize,
        resident_serialized_bytes: usize,
    ) -> PreparedRootBinding<K, V> {
        PreparedRootBinding {
            expected: expected.clone(),
            next: Arc::new(PublishedRoot {
                node: Arc::clone(expected.node()),
                term_count: expected.term_count(),
                eviction: Some(Arc::new(RootEvictionRevision {
                    catalog,
                    predecessor_ordinal: ordinal,
                    ordinal,
                    resident_nodes,
                    resident_serialized_bytes,
                    delta: PackedResidencyDelta::None,
                })),
            }),
        }
    }

    /// Publish a prebuilt checkpoint binding against its exact captured root.
    ///
    /// The caller holds the coordinator registry write lock. A losing CAS leaves
    /// the root untouched so the caller can restore the preceding registry
    /// before releasing that lock.
    pub(crate) fn publish_checkpoint_binding(
        &self,
        prepared: &PreparedRootBinding<K, V>,
    ) -> Result<RootRevision<K, V>, Option<RootRevision<K, V>>> {
        self.compare_exchange_revision(&prepared.expected, Arc::clone(&prepared.next))
    }

    /// Prepare an unchanged root/count revision with no registry binding.
    ///
    /// The exact expected revision, rather than its node pointer alone, is
    /// captured so retirement cannot detach a checkpoint publication that won a
    /// preceding race without first observing that newer revision.
    pub(crate) fn prepare_retirement_detachment(
        expected: &RootRevision<K, V>,
    ) -> PreparedRootDetachment<K, V> {
        PreparedRootDetachment {
            expected: expected.clone(),
            next: Arc::new(PublishedRoot {
                node: Arc::clone(expected.node()),
                term_count: expected.term_count(),
                eviction: None,
            }),
        }
    }

    /// Publish a preallocated retirement detachment.
    ///
    /// The caller holds the stable coordinator lifecycle gate and registry write
    /// lock. The root CAS therefore linearizes before exact-registry retirement, and
    /// a semantic writer that wins the race has already published an unbound
    /// revision of its own.
    pub(crate) fn publish_retirement_detachment(
        &self,
        prepared: &PreparedRootDetachment<K, V>,
        _authority: &RegistryTransitionAuthority<'_>,
    ) -> Result<RootRevision<K, V>, Option<RootRevision<K, V>>> {
        self.compare_exchange_revision(&prepared.expected, Arc::clone(&prepared.next))
    }

    fn compare_exchange_revision(
        &self,
        expected: &RootRevision<K, V>,
        new: Arc<PublishedRoot<K, V>>,
    ) -> Result<RootRevision<K, V>, Option<RootRevision<K, V>>> {
        #[cfg(not(target_os = "wasi"))]
        {
            let previous = self.ptr.compare_and_swap(&expected.0, Some(new));
            match &*previous {
                Some(actual) if Arc::ptr_eq(actual, &expected.0) => {
                    Ok(RootRevision(Arc::clone(actual)))
                }
                Some(actual) => Err(Some(RootRevision(Arc::clone(actual)))),
                None => Err(None),
            }
        }
        #[cfg(target_os = "wasi")]
        {
            let mut slot = self
                .ptr
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match slot.as_ref() {
                Some(actual) if Arc::ptr_eq(actual, &expected.0) => {
                    let previous = RootRevision(Arc::clone(actual));
                    *slot = Some(new);
                    Ok(previous)
                }
                Some(actual) => Err(Some(RootRevision(Arc::clone(actual)))),
                None => Err(None),
            }
        }
    }

    /// Try to set a null pointer to a new value.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the pointer was null and is now set to `new`
    /// - `Err(actual)` if the pointer was not null
    pub fn try_init(&self, new: Arc<OverlayNode<K, V>>) -> Result<(), Arc<OverlayNode<K, V>>> {
        self.try_init_with_term_count(new, 0)
    }

    /// Try to initialize a null pointer with a coherent root/count revision.
    pub fn try_init_with_term_count(
        &self,
        new: Arc<OverlayNode<K, V>>,
        term_count: usize,
    ) -> Result<(), Arc<OverlayNode<K, V>>> {
        let revision = Arc::new(PublishedRoot {
            node: new,
            term_count,
            eviction: None,
        });
        // CAS None -> Some(new), atomically.
        #[cfg(not(target_os = "wasi"))]
        {
            let prev = self
                .ptr
                .compare_and_swap(&None::<Arc<PublishedRoot<K, V>>>, Some(revision));
            match &*prev {
                None => Ok(()),
                Some(p) => Err(Arc::clone(&p.node)),
            }
        }
        #[cfg(target_os = "wasi")]
        {
            let mut slot = self
                .ptr
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match slot.as_ref() {
                None => {
                    *slot = Some(revision);
                    Ok(())
                }
                Some(actual) => Err(Arc::clone(&actual.node)),
            }
        }
    }

    /// Get the raw pointer value (for debugging/testing).
    #[inline]
    pub fn as_raw(&self) -> u64 {
        self.load()
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
        match self.load_with_term_count() {
            Some((arc, term_count)) => Self::new_with_term_count(arc, term_count),
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

    fn empty_catalog(binding: EvictionBinding) -> Arc<PublishedRegistryCatalog> {
        Arc::new(PublishedRegistryCatalog::empty_for_binding(binding))
    }

    #[test]
    fn semantic_root_wrapper_remains_three_machine_words() {
        assert_eq!(
            std::mem::size_of::<PublishedRoot<ByteKey, ()>>(),
            3 * std::mem::size_of::<usize>(),
            "optional eviction state must remain pointer-sized so semantic CAS allocations do not change size class"
        );
    }

    #[test]
    fn residency_publication_metadata_stays_within_qualified_size_classes() {
        let word = std::mem::size_of::<usize>();
        assert!(
            std::mem::size_of::<RootEvictionRevision>() <= 8 * word,
            "root-carried residency authority must remain compact"
        );
        assert!(
            std::mem::size_of::<PreparedPackedResidency>() <= 10 * word,
            "transient sparse/rebased preparation must not accumulate catalog payloads"
        );
        assert!(
            std::mem::size_of::<PublishedRegistryCatalog>() <= 40 * word,
            "published catalog must retain only operational metadata and two packed arrays"
        );
    }

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
    fn counted_cas_publishes_root_and_cardinality_together() {
        let node0 = Arc::new(ByteNode::new());
        let node1 = Arc::new(node0.as_final());
        let node2 = Arc::new(node1.as_non_final());
        let ptr = ByteAtomicNodePtr::new_with_term_count(Arc::clone(&node0), 0);

        assert!(ptr
            .compare_exchange_counted(&node0, Arc::clone(&node1), 1)
            .is_ok());
        let (published, count) = ptr.load_with_term_count().expect("revision");
        assert!(Arc::ptr_eq(&published, &node1));
        assert_eq!(count, 1);

        assert!(ptr
            .compare_exchange_counted(&node0, Arc::clone(&node2), -1)
            .is_err());
        let (published, count) = ptr.load_with_term_count().expect("revision");
        assert!(Arc::ptr_eq(&published, &node1));
        assert_eq!(count, 1, "a losing CAS cannot adjust cardinality");

        assert!(ptr
            .compare_exchange_counted(&node1, Arc::clone(&node2), -1)
            .is_ok());
        let (published, count) = ptr.load_with_term_count().expect("revision");
        assert!(Arc::ptr_eq(&published, &node2));
        assert_eq!(count, 0);
    }

    #[test]
    fn checkpoint_binding_is_an_exact_metadata_revision() {
        let node = Arc::new(ByteNode::new());
        let ptr = ByteAtomicNodePtr::new_with_term_count(Arc::clone(&node), 7);
        let captured = ptr.load_revision().expect("captured revision");
        let binding = EvictionBinding::new();
        let prepared = ByteAtomicNodePtr::prepare_checkpoint_binding(
            &captured,
            empty_catalog(binding.clone()),
            0,
            0,
        );

        assert!(ptr.publish_checkpoint_binding(&prepared).is_ok());
        let published = ptr.load_revision().expect("bound revision");
        assert!(!captured.same_revision(&published));
        assert!(Arc::ptr_eq(captured.node(), published.node()));
        assert_eq!(published.term_count(), 7);
        assert!(published
            .eviction_binding()
            .is_some_and(|actual| actual.same_publication(&binding)));
    }

    #[test]
    fn stale_semantic_cas_loses_to_checkpoint_binding_then_retry_clears_it() {
        let node = Arc::new(ByteNode::new());
        let ptr = ByteAtomicNodePtr::new_with_term_count(Arc::clone(&node), 0);
        let stale = ptr.load_revision().expect("stale writer revision");
        let binding = EvictionBinding::new();
        let prepared = ByteAtomicNodePtr::prepare_checkpoint_binding(
            &stale,
            empty_catalog(binding.clone()),
            0,
            0,
        );
        assert!(ptr.publish_checkpoint_binding(&prepared).is_ok());

        let inserted = Arc::new(node.as_final());
        assert!(ptr
            .compare_exchange_revision_counted(&stale, Arc::clone(&inserted), 1)
            .is_err());

        let retry = ptr.load_revision().expect("writer retry revision");
        assert!(retry
            .eviction_binding()
            .is_some_and(|actual| actual.same_publication(&binding)));
        assert!(ptr
            .compare_exchange_revision_counted(&retry, Arc::clone(&inserted), 1)
            .is_ok());
        let published = ptr.load_revision().expect("semantic revision");
        assert!(Arc::ptr_eq(published.node(), &inserted));
        assert_eq!(published.term_count(), 1);
        assert!(published.eviction_binding().is_none());
    }

    #[test]
    fn structural_cas_requires_and_preserves_exact_binding() {
        let node = Arc::new(ByteNode::new());
        let ptr = ByteAtomicNodePtr::new(Arc::clone(&node));
        let initial = ptr.load_revision().expect("initial revision");
        let binding = EvictionBinding::new();
        let prepared = ByteAtomicNodePtr::prepare_checkpoint_binding(
            &initial,
            empty_catalog(binding.clone()),
            0,
            0,
        );
        assert!(ptr.publish_checkpoint_binding(&prepared).is_ok());

        let bound = ptr.load_revision().expect("bound revision");
        let wrong = EvictionBinding::new();
        let structural = Arc::new(node.with_child(
            b'x',
            Child::OnDisk(SwizzledPtr::on_disk(1, 7, NodeType::Node4)),
        ));
        assert!(ptr
            .compare_exchange_revision_preserving_binding(&bound, &wrong, Arc::clone(&structural),)
            .is_err());
        assert!(
            ptr.compare_exchange_revision_preserving_binding(
                &bound,
                &binding,
                Arc::clone(&structural),
            )
            .is_ok()
        );
        let published = ptr.load_revision().expect("structural revision");
        assert!(Arc::ptr_eq(published.node(), &structural));
        assert!(published
            .eviction_binding()
            .is_some_and(|actual| actual.same_publication(&binding)));
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
                        let child =
                            Child::OnDisk(SwizzledPtr::on_disk(t * 64 + i, 0, NodeType::Node4));
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
