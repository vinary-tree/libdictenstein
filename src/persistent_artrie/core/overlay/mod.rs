//! Shared lock-free overlay node (G4 unification).
//!
//! The byte (`u8`) and char (`u32`) lock-free overlays used to carry
//! token-for-token-identical node implementations (`persistent_node.rs` /
//! `atomic_ptr.rs`) that differed only in the key-unit type, `MAX_PREFIX_LEN`
//! (12 vs 6), the inline zero filler (`0u8` vs `0u32`), and prose. G4 collapses
//! both into a single generic
//! [`OverlayNode<K, V>`](crate::persistent_artrie::core::overlay::OverlayNode) /
//! [`AtomicNodePtr<K, V>`](crate::persistent_artrie::core::overlay::AtomicNodePtr)
//! parameterized over `K: KeyEncoding` (its `Unit` is the key-unit width) and
//! the value `V`. The variants alias it:
//!
//! ```text
//! // byte:  pub type PersistentNode<V = ()>     = OverlayNode<ByteKey, V>;
//! //        pub type AtomicNodePtr<V = ()>      = overlay::AtomicNodePtr<ByteKey, V>;
//! // char:  pub type PersistentCharNode<V = ()> = OverlayNode<CharKey, V>;
//! //        pub type AtomicNodePtr<V = ()>      = overlay::AtomicNodePtr<CharKey, V>;
//! // vocab: consumes the char alias at <u64> (unchanged).
//! ```
//!
//! Lives in `persistent_artrie::core` so the layering invariant holds: `SwizzledPtr`
//! is canonically `persistent_artrie::core::swizzled_ptr`, so this module imports it
//! with **zero** upward reference. Zero `unsafe` — `Send`/`Sync` auto-derive.

pub mod atomic_ptr;
// G5.1 — the shared, key-encoding-generic overlay-backed `DictionaryNode` handle
// (`OverlayDictionaryNode<K, V>`), aliased by both variants as `PersistentARTrieNode`
// (byte) / `PersistentARTrieCharNode` (char). Auto-derives Send/Sync (no `unsafe`).
pub mod dict_node;
// The SAFE object-safe fault-in capability for the overlay-backed `DictionaryNode`
// (resolves `Child::OnDisk` overlay children during a graph walk without naming `S`
// and without `unsafe`). See `faulter.rs`.
pub mod faulter;
pub mod node;

// The shared lock-free-overlay flip (route + read-engine + flip/kill-switch +
// reestablish), generic over `K: KeyEncoding` (overlay-flip genericization §2).
pub(crate) mod flip;
// The shared Order-A durable-write skeleton (Template Method): the durability
// gate + the append→publish→mark ordering + the commit-rank/watermark tail +
// the full increment template, as default methods over per-variant seams
// (overlay-durable-architecture.md, trait 2).
pub(crate) mod durable_write;
// The shared checkpoint route-split skeleton (Template Method): the "capture the
// LIVE representation (overlay vs owned)" decision + the RES-4 total-loss guard,
// as a default method over per-variant capture/publish seams (trait 3).
pub(crate) mod checkpoint;
// The shared overlay-eviction + read-fault primitives (the 1c overwrite-race-safe
// single-node evict + the read-path single-level fault-in walk), lifted K-generic
// over `OverlayNode<K, V>` from char's proven impl via the `OverlayEvictable`
// subtrait of `OverlayFaulter` (overlay-eviction-v4 design §4). The per-attempt
// primitives are default methods over three variant-specific accessors; the
// loaders + registry plumbing + batch driver stay variant-specific.
pub(crate) mod evict;
// F5 (Slice 3): the generic, compression-aware dense→overlay builder
// (`build_overlay_root_from_terms`) used by the F5 dense-image reopen loaders
// (`load_overlay_root_compressed` / `load_overlay_char_root_compressed`).
pub(crate) mod f5_build;
// CX (task #43): the path-compressing overlay↔dense codec's shared, K-generic,
// pure no-truncation reference law. Production uses the allocation-free checked
// chunk-bound arithmetic in `compressed_serialize`; this module is the exhaustive
// differential oracle for those bounds.
#[cfg(test)]
pub(crate) mod codec;
// CX-universal: the ONE generic path-compressed overlay serializer (`OverlayCompressedSerialize<K,V>`
// default-method loop + `peel_chain_generic`); per-variant seams cover the format-specific leaves.
pub(crate) mod compressed_serialize;
// G5.3' — the shared lock-free CAS-walk SKELETON (free generic-over-`<K,V>` COMMON
// descent helpers + the `OverlayCasWalk<K,V,S>` trait with per-variant
// specialization hooks + DEFAULT skeleton methods). The COMMON descent
// (find/spine/resolve_or_fault) is shared; the result/error enums + the
// `try_set_final` two-phase publish stay per-variant. See
// `docs/design/slice3-g5-overlay-genericization-2026-06-09.md` §G5.3'.
pub(crate) mod cas_walk;
#[cfg(test)]
pub(crate) mod test_support;

pub use atomic_ptr::AtomicNodePtr;
pub(crate) use atomic_ptr::{
    DeferredDurableStamp, EvictionBinding, PreparedBoundRootTransition, PreparedRootBinding,
    PreparedRootDetachment, RootRevision,
};
pub use dict_node::OverlayDictionaryNode;
pub use faulter::OverlayFaulter;
pub use node::{flags, Child, OverlayNode};
// F7 — the crash-injection fail points for the Owned→Overlay conversion crash-safety
// proptest (`tests/persistent_owned_to_overlay_conversion_crash.rs`). Re-exported `pub`
// from the `pub(crate)` `flip` module so the integration test can arm/disarm them;
// DISARMED by default (a single `Relaxed`/`SeqCst` atomic load on the cold reopen-convert
// path = zero production effect).
pub use flip::f7_failpoint;

use std::sync::Arc;

use crate::persistent_artrie::core::key_encoding::KeyEncoding;

/// Number of path-machine frames retained inline before a fallible heap spill.
///
/// This covers the common shallow-key regime without heap traffic while leaving
/// arbitrary-depth walks stack-safe. All overlay path machines use the same
/// boundary so mutation, eviction, and enumeration have consistent behavior.
pub(crate) const INLINE_OVERLAY_DEPTH: usize = 16;

/// A node retained by an iterative path-copy machine.
///
/// Resident nodes borrow from the immutable root snapshot held by the caller, so
/// descending a resident path performs no atomic reference-count operation. Once
/// a disk fault creates a detached node, the machine switches that subtree to
/// owned handles; no reference borrowed from an owned handle survives an
/// iteration, which keeps the representation lifetime-safe without pinning,
/// arenas, raw pointers, or self-references.
pub(crate) enum OverlayNodeHandle<'root, K: KeyEncoding, V> {
    /// Node reachable from the caller-retained immutable root snapshot.
    Borrowed(&'root Arc<OverlayNode<K, V>>),
    /// Node loaded by a fault or descended from a fault-owned subtree.
    Owned(Arc<OverlayNode<K, V>>),
}

impl<'root, K: KeyEncoding, V> OverlayNodeHandle<'root, K, V> {
    /// Borrow the immutable node independently of how the machine retains it.
    #[inline]
    pub(crate) fn node(&self) -> &OverlayNode<K, V> {
        match self {
            Self::Borrowed(node) => node.as_ref(),
            Self::Owned(node) => node.as_ref(),
        }
    }

    /// Produce an owned handle only when the caller's result contract requires
    /// node identity beyond the retained root snapshot. Resident descent itself
    /// never calls this method, so it performs at most one `Arc` clone per result.
    #[inline]
    pub(crate) fn into_arc(self) -> Arc<OverlayNode<K, V>> {
        match self {
            Self::Borrowed(node) => Arc::clone(node),
            Self::Owned(node) => node,
        }
    }
}

/// One return frame in the borrowed/owned overlay path-copy machine.
pub(crate) struct OverlayPathFrame<'root, K: KeyEncoding, V> {
    pub(crate) node: OverlayNodeHandle<'root, K, V>,
    pub(crate) unit: K::Unit,
}

/// Root-to-node return frames for mutation paths.
pub(crate) type OverlayPathSpine<'root, K, V> =
    smallvec::SmallVec<[OverlayPathFrame<'root, K, V>; INLINE_OVERLAY_DEPTH]>;

#[cfg(test)]
pub(crate) mod overlay_spine_failpoint {
    use std::cell::Cell;
    use std::marker::PhantomData;
    use std::rc::Rc;

    thread_local! {
        static FAIL_NEXT_SPILL: Cell<bool> = const { Cell::new(false) };
    }

    /// Thread-local, test-only reservation failure injection.
    ///
    /// The guard is deliberately `!Send`: the armed state and the exercised
    /// overlay walk must remain on the same thread. Dropping the guard disarms
    /// an injection that was not consumed because an earlier assertion failed.
    pub(crate) struct Guard {
        _not_send: PhantomData<Rc<()>>,
    }

    pub(crate) fn fail_next_spill() -> Guard {
        FAIL_NEXT_SPILL.with(|armed| {
            assert!(
                !armed.replace(true),
                "overlay-spine failpoint already armed"
            );
        });
        Guard {
            _not_send: PhantomData,
        }
    }

    pub(super) fn take() -> bool {
        FAIL_NEXT_SPILL.with(|armed| armed.replace(false))
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            FAIL_NEXT_SPILL.with(|armed| armed.set(false));
        }
    }
}

/// Append one path-copy frame, retaining shallow paths inline and performing at
/// most one fallible, exact-capacity spill for a known-bounded walk.
///
/// `maximum_frames` is the caller's proven upper bound for the complete walk.
/// Reservation is delayed until the first frame beyond
/// [`INLINE_OVERLAY_DEPTH`], so an early missing edge neither allocates nor
/// reserves memory it will never use. Once a spill is required, the exact
/// remaining capacity is reserved in one operation.
pub(crate) fn try_push_overlay_path_spine<'root, K: KeyEncoding, V>(
    spine: &mut OverlayPathSpine<'root, K, V>,
    frame: OverlayPathFrame<'root, K, V>,
    maximum_frames: usize,
) -> Result<(), smallvec::CollectionAllocErr> {
    if spine.len() == spine.capacity() {
        let additional = maximum_frames
            .checked_sub(spine.len())
            .filter(|additional| *additional > 0)
            .expect("overlay path spine upper bound must exceed its current length");
        #[cfg(test)]
        if overlay_spine_failpoint::take() {
            return Err(smallvec::CollectionAllocErr::CapacityOverflow);
        }
        spine.try_reserve_exact(additional)?;
    }
    spine.push(frame);
    Ok(())
}

/// Child slots keyed by their incoming key unit, in insertion order.
///
/// The inverse pairing of the internal `OverlayPathSpine`: here the unit *precedes* the node
/// it reaches. Used when staging children before a node is published.
pub type OverlayChildren<K, V> = Vec<(<K as KeyEncoding>::Unit, Arc<OverlayNode<K, V>>)>;

/// A single `(key unit, child)` slot, as produced by a completed subtree build.
pub type OverlayChildSlot<K, V> = (<K as KeyEncoding>::Unit, Arc<OverlayNode<K, V>>);

/// Outcome of a compare-and-swap publish attempt.
///
/// `Ok` carries the node that is now installed; `Err` carries the node found
/// instead, so the caller can retry against the observed state rather than
/// re-reading it.
pub type OverlayCasResult<K, V> = Result<Arc<OverlayNode<K, V>>, Arc<OverlayNode<K, V>>>;

/// A `(published root, published node)` pair returned by a copy-on-write republish.
pub type OverlayRootAndNode<K, V> = (Arc<OverlayNode<K, V>>, Arc<OverlayNode<K, V>>);

use crate::persistent_artrie::core::mvcc::TrieRoot;

/// G4 Phase 6 (DRY bonus): the single `TrieRoot` impl for the unified overlay
/// node, replacing the two near-identical per-variant impls (byte
/// `persistent_artrie::mvcc` and char `persistent_artrie::char::mvcc`).
///
/// `Key = K::Unit` (`u8` for byte, `u32` for char — both satisfy `Key: Copy`);
/// `Value = V`. For `OverlayNode<ByteKey, i64>` this yields `Key=u8, Value=i64`
/// (identical to the old hand-written byte impl) and for `OverlayNode<CharKey, V>`
/// it yields `Key=u32, Value=V` (identical to the old char impl) — so the blanket
/// subsumes both exactly. Coherence holds: both `TrieRoot` and `OverlayNode` live
/// in `persistent_artrie::core`, so the blanket is canonical here (no orphan-rule
/// issue, single crate).
impl<K: KeyEncoding, V: Clone + Send + Sync + 'static> TrieRoot for OverlayNode<K, V> {
    type Key = K::Unit;
    type Value = V;

    fn is_final(&self) -> bool {
        OverlayNode::is_final(self)
    }

    fn find_child(&self, key: K::Unit) -> Option<Arc<Self>> {
        // `as_in_mem` yields `None` for an on-disk (or absent) child, so this MVCC
        // snapshot read simply borrows the owned child `Arc` and clones it — the
        // old raw-pointer smuggling (`as_ptr` + `unsafe Arc::from_raw`) is gone.
        OverlayNode::find_child(self, key).and_then(|child| child.as_in_mem().map(Arc::clone))
    }

    fn get_value(&self) -> Option<V> {
        OverlayNode::get_value(self)
    }
}

#[cfg(test)]
mod stack_safety_tests {
    use super::{
        try_push_overlay_path_spine, OverlayNode, OverlayNodeHandle, OverlayPathFrame,
        OverlayPathSpine, INLINE_OVERLAY_DEPTH,
    };
    use crate::persistent_artrie::core::key_encoding::ByteKey;
    use std::sync::Arc;

    #[test]
    fn overlay_spine_stays_inline_then_reserves_its_complete_bound_once() {
        const MAXIMUM_FRAMES: usize = 64;
        let node = Arc::new(OverlayNode::<ByteKey>::new());
        let strong_before = Arc::strong_count(&node);
        let mut spine = OverlayPathSpine::<ByteKey, ()>::new();

        for unit in 0..INLINE_OVERLAY_DEPTH {
            try_push_overlay_path_spine(
                &mut spine,
                OverlayPathFrame {
                    node: OverlayNodeHandle::Borrowed(&node),
                    unit: unit as u8,
                },
                MAXIMUM_FRAMES,
            )
            .expect("inline pushes cannot allocate");
        }
        assert!(!spine.spilled());
        assert_eq!(Arc::strong_count(&node), strong_before);

        try_push_overlay_path_spine(
            &mut spine,
            OverlayPathFrame {
                node: OverlayNodeHandle::Borrowed(&node),
                unit: INLINE_OVERLAY_DEPTH as u8,
            },
            MAXIMUM_FRAMES,
        )
        .expect("the exact spill fits the proven bound");
        assert!(spine.spilled());
        assert!(spine.capacity() >= MAXIMUM_FRAMES);
        let spilled_capacity = spine.capacity();

        for unit in INLINE_OVERLAY_DEPTH + 1..MAXIMUM_FRAMES {
            try_push_overlay_path_spine(
                &mut spine,
                OverlayPathFrame {
                    node: OverlayNodeHandle::Borrowed(&node),
                    unit: unit as u8,
                },
                MAXIMUM_FRAMES,
            )
            .expect("the first spill reserved the complete bound");
        }
        assert_eq!(spine.len(), MAXIMUM_FRAMES);
        assert_eq!(spine.capacity(), spilled_capacity);
        assert_eq!(Arc::strong_count(&node), strong_before);
    }
}
