//! `cas_walk` — the SHARED lock-free CAS-walk SKELETON (G5.3', design
//! `docs/design/slice3-g5-overlay-genericization-2026-06-09.md` §G5.3').
//!
//! # What this module shares (and what it deliberately does NOT)
//!
//! Before G5.3' the byte (`persistent_artrie/lockfree_cas.rs`) and char
//! (`persistent_artrie/char/lockfree_cas.rs`) overlays carried token-for-token-
//! identical CAS-walk DESCENT logic (find-leaf, create-spine, build-value-spine,
//! and the OnDisk write-path fault-in copied ~7×) that differed only in the key
//! unit `K::Unit` (`u8` vs `u32`). This module lifts the COMMON descent ONCE,
//! generic over `<K: KeyEncoding, V>`:
//!
//!   * [`find_leaf_iterative`] / [`find_in_lockfree_trie`] — the non-faulting
//!     in-memory point-read walks (membership `bool` + leaf `Arc`).
//!   * [`create_spine`] — the bottom-up "build the remaining spine for a key
//!     suffix" path builder, parameterized by a leaf-maker closure (so a
//!     non-durable non-final leaf vs a durable `as_final` leaf vs a valued leaf
//!     are all the SAME reverse-iteration loop). SAME build order as the prior
//!     per-variant `create_lockfree_path` / `create_lockfree_path_final` — the
//!     on-disk serializer consumes node-build order, so this is format-preserving.
//!   * [`build_value_spine`] — the iterative valued path-copy (lifted from the
//!     per-variant `build_value_path_iterative`).
//!   * [`resolve_or_fault`] — the single OnDisk-child resolution primitive (the
//!     copy-pasted fault-in), returning a RICH [`ChildResolution`] so each
//!     (variant × method) keeps its OWN error/null/absent → enum mapping.
//!
//! What STAYS per-variant (the design's "must stay specialized" list):
//!   * the result/error enums (`LockfreeInsertResult` / `LockfreeRemoveResult` /
//!     `BuildPathError` / `DurableBuildError` …) and the public
//!     `insert_cas[_durable]` / `remove_cas_durable` entry points;
//!   * the byte DUAL-method (non-durable two-phase `try_set_final` arbiter +
//!     durable single-phase) vs char single `finalize`-flag method;
//!   * the per-(variant × method) OnDisk/IO/null/missing mapping (see the table
//!     in [`resolve_or_fault`]'s doc): typed fault errors propagate, null and
//!     absence retain operation-specific semantics, and only root-CAS loss retries;
//!   * the recovery generation (the durable global `commit_seq`, claimed by the
//!     CALLER's retry loop via [`OverlayCasWalk::claim_generation`] — NEVER the
//!     walk's `root.version()`; see the §MANDATORY-FIX-1 note below).
//!
//! # MANDATORY FIX 1 (data-loss) — generation comes from `claim_generation`, NOT the walk
//!
//! The recovery generation that flows into `reconcile_lww` is the durable global
//! `commit_seq` (restart-seeded), NOT a node's `root.version()`. The skeleton's
//! retry loop (the per-variant CALLER) claims the generation via
//! [`OverlayCasWalk::claim_generation`] (default `self.claim_commit_seq()`,
//! identical in both variants) and passes the CALLER-CLAIMED
//! `committed_generation` to `commit_rank_and_mark`. The walk's `root.version()`
//! is DROPPED inside the skeleton exactly as both variants do today — a
//! `make_*(_, published_root_version)` hook that read the walk's version would
//! re-introduce the A.2 cross-restart resurrection bug (post-restart version
//! resets → wrong replay order → resurrected/dropped term).
//!
//! # MANDATORY FIX 2 (correctness) — the rich `ChildResolution`
//!
//! [`resolve_or_fault`] returns the rich [`ChildResolution`] so typed fault
//! failures cannot collapse into absence or contention and each operation keeps
//! its exact null/missing semantics. See the table in its documentation.
//!
//! # REC 3 — descent shared, the `try_set_final` arbiter NEVER inherited by durable
//!
//! Only the DESCENT is shared. byte's non-durable two-phase publish (CAS a
//! NON-final spine, THEN the CALLER-level `try_set_final` arbiter) and its durable
//! single-phase publish (CAS a final spine) are NOT merged into one driver — the
//! durable arm must NEVER inherit `try_set_final` (a second commit point breaks
//! single-LP). The leaf-shape choice is explicit per path: the non-durable builder
//! returns the SHARED node (so the caller's `try_set_final` arbitrates) while the
//! durable builder bakes `as_final()` into a fresh node published ONLY via the root
//! CAS (the sole LP).

use std::sync::Arc;

use crate::persistent_artrie::core::error::PersistentARTrieError;
use crate::persistent_artrie::core::key_encoding::KeyEncoding;
use crate::persistent_artrie::core::overlay::durable_write::{
    PendingDurableMutation, RegistryEligibleMutation, SemanticMutationPublicationPermit,
};
use crate::persistent_artrie::core::overlay::flip::LockFreeOverlay;
use crate::persistent_artrie::core::overlay::node::{Child, OverlayNode};
use crate::value::DictionaryValue;

// ============================================================================
// ChildResolution — the RICH outcome of resolving one spine edge (FIX 2)
// ============================================================================

/// The outcome of resolving a single spine edge during a CAS-walk descent —
/// either an already-in-memory child, a freshly-faulted-in child, an I/O failure
/// faulting an evicted (`OnDisk`) child, a null filler slot, or a missing edge.
///
/// **Why a RICH enum (FIX 2).** The operation-specific null/absence semantics differ
/// (corruption, create, already present, or already absent), while every exact
/// fault error must retain its typed cause. Collapsing these outcomes to `Conflict`
/// would retry permanent failures indefinitely. The resolution primitive therefore
/// returns this enum and each operation maps every cell explicitly; only a failed
/// root compare-exchange is a retryable conflict.
///
/// `InMem` and `Faulted` are distinguished both for causal accounting and because
/// a faulted subtree must retain owned handles. `FaultFailed` boxes the error so
/// the common arms stay pointer-sized.
///
pub(crate) enum ChildResolution<'root, K: KeyEncoding, V> {
    /// The edge exists and the child is resident in memory. A child below the
    /// retained root remains borrowed; a child below a fault-owned node is cloned
    /// into an owned handle before the parent can move into its return frame.
    InMem(super::OverlayNodeHandle<'root, K, V>),
    /// The edge exists, the child was evicted (`OnDisk`), and the fault-in SUCCEEDED
    /// — descend into the freshly-loaded child (spliced `Child::InMem` by the
    /// caller, so the single root CAS stays the sole arbiter).
    Faulted(Arc<OverlayNode<K, V>>),
    /// The edge exists, the child was evicted, and the fault-in FAILED with a
    /// storage or decode error. Boxed so the common (pointer-sized) arms are not
    /// widened. Every durable builder propagates the original error unchanged.
    FaultFailed(Box<PersistentARTrieError>),
    /// The edge exists but holds a null filler slot (never a real child).
    Null,
    /// The edge does NOT exist (no child for this unit on this snapshot).
    Absent,
}

/// Whether to fault an evicted (`OnDisk`) child back in during [`resolve_or_fault`].
///
/// The byte VALUE path (`build_value_path_iterative`) historically returned `None`
/// on an OnDisk child WITHOUT faulting (the `as_in_mem()?` short-circuit). The byte
/// value path now DOES fault (it was migrated; the in-mem-only contract is gone),
/// so both variants' value paths fault — but the mode is retained so a caller that
/// must NOT fault (e.g. a strictly non-faulting read) can opt out without routing
/// through a different primitive. Today every CAS-walk caller uses [`Self::Fault`].
///
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaultMode {
    /// Fault an `OnDisk` child in (`Faulted` on success, `FaultFailed` on I/O error).
    Fault,
    /// Do NOT fault — an `OnDisk` (non-null) child resolves to [`ChildResolution::Null`]
    /// so the caller treats it as "no in-memory transition" (the NO-FAULT-IN mode).
    ///
    /// The FIX-2 red-team REQUIRED `resolve_or_fault` to support a no-fault-in mode
    /// (the alternative was excluding byte's value path from the shared primitive).
    /// Post-G5.2 every CAS-walk caller FAULTS (byte's value path was migrated to fault,
    /// matching its faulting value-READ), so this variant is currently unconstructed —
    /// retained as the designed no-fault seam (a strictly non-faulting read could opt in
    /// without a second primitive). `#[allow(dead_code)]` is the honest label.
    #[allow(dead_code)]
    NoFaultIn,
}

// ============================================================================
// Free COMMON descent functions — generic over <K: KeyEncoding, V>
// ============================================================================

/// Non-faulting iterative leaf find: descend `key[depth..]` through IN-MEMORY
/// children only, returning the final leaf `Arc` iff the full path exists AND the
/// leaf is final, else `None`. An `OnDisk` child short-circuits to `None` (the
/// lock-free overlay cannot traverse a disk ref without a faulter — the per-variant
/// faulting walk `find_leaf_faulting` handles eviction).
///
/// Token-for-token the prior per-variant leaf find (byte
/// `lockfree_cas.rs:555`, char `:1293`), now generic over `K::Unit`.
///
#[inline]
pub(crate) fn find_leaf_iterative<K: KeyEncoding, V: Clone>(
    node: &Arc<OverlayNode<K, V>>,
    key: &[K::Unit],
    depth: usize,
) -> Option<Arc<OverlayNode<K, V>>> {
    let mut current = node;
    let mut cursor = depth;
    while cursor < key.len() {
        let child = current.find_child(key[cursor])?;
        // Can't traverse disk refs in the lock-free overlay; `as_in_mem`
        // short-circuits an on-disk child to `None` without cloning the Arc.
        current = child.as_in_mem()?;
        cursor += 1;
    }

    current.is_final().then(|| Arc::clone(current))
}

/// Non-faulting iterative membership check: `true` iff `key[depth..]` reaches a
/// final node through IN-MEMORY children only. Token-for-token the prior
/// per-variant `find_in_lockfree_trie` (byte `lockfree_cas.rs:511`, char `:1252`),
/// now generic over `K::Unit`.
///
#[inline]
pub(crate) fn find_in_lockfree_trie<K: KeyEncoding, V: Clone>(
    node: &Arc<OverlayNode<K, V>>,
    key: &[K::Unit],
    depth: usize,
) -> bool {
    let mut current = node;
    let mut cursor = depth;
    while cursor < key.len() {
        let Some(child) = current.find_child(key[cursor]) else {
            return false;
        };
        let Some(child_arc) = child.as_in_mem() else {
            return false;
        };
        current = child_arc;
        cursor += 1;
    }
    current.is_final()
}

/// Build a NEW path for the remaining `suffix` units, bottom-up, with the terminal
/// leaf produced by `make_leaf`. Returns `(subtree_root, leaf)`:
///   * `subtree_root` — the top of the new path (the caller splices it as a child);
///   * `leaf` — the bottom node (the caller's `try_set_final` target on the
///     non-durable path; ignored on the durable path).
///
/// The leaf is `make_leaf()` (a non-final `OverlayNode::new()` for the non-durable
/// path, an `OverlayNode::new().as_final()` for the durable path, or an
/// `as_final().with_value(..)` valued leaf). The spine is then wrapped bottom-up via
/// `OverlayNode::new().with_child(unit, Child::InMem(child))` over `suffix.iter().rev()`
/// — the EXACT reverse-iteration order the prior per-variant `create_lockfree_path` /
/// `create_lockfree_path_final` used (so the on-disk serializer, which consumes
/// node-build order, sees an identical structure — format-preserving).
///
#[inline]
pub(crate) fn create_spine<K, V, F>(
    suffix: &[K::Unit],
    make_leaf: F,
) -> super::OverlayRootAndNode<K, V>
where
    K: KeyEncoding,
    V: Clone,
    F: FnOnce() -> Arc<OverlayNode<K, V>>,
{
    let leaf = make_leaf();
    if suffix.is_empty() {
        // No more units — the leaf is also the subtree root.
        return (Arc::clone(&leaf), leaf);
    }
    let mut current = Arc::clone(&leaf);
    for &unit in suffix.iter().rev() {
        // Each parent owns its child by `Arc` (no raw-pointer smuggling).
        let parent = OverlayNode::new().with_child(unit, Child::InMem(current));
        current = Arc::new(parent);
    }
    (current, leaf)
}

/// Rebuild a recorded root-to-parent path around `child`, from the deepest
/// parent back to the root.
///
/// This is the return transition of the explicit CAS-walk pushdown machine:
/// descent pushes `(parent, unit)` frames into [`super::OverlayPathSpine`], the
/// terminal transition constructs or transforms one child, and this loop pops
/// the frames while path-copying each immutable ancestor. Native call-stack use
/// is therefore independent of key depth.
///
/// The reverse fold preserves the exact structure and version-bump order of the
/// former recursive return path. It performs no publication; the caller's root
/// compare-and-swap remains the sole linearization point.
#[inline]
pub(crate) fn unwind_spine<K, V>(
    spine: super::OverlayPathSpine<'_, K, V>,
    mut child: Arc<OverlayNode<K, V>>,
) -> Arc<OverlayNode<K, V>>
where
    K: KeyEncoding,
    V: Clone,
{
    for frame in spine.into_iter().rev() {
        child = Arc::new(
            frame
                .node
                .node()
                .with_child(frame.unit, Child::InMem(child)),
        );
    }
    child
}

/// The ITERATIVE valued path-copy: descend `key[depth..]` from `node` collecting the
/// `(parent, unit)` spine (faulting `OnDisk` children in per `fault`), then rebuild
/// it bottom-up with a fresh `as_final().with_value(value)` leaf. Returns the new
/// root `Arc`, or a typed persistent-trie error if the bounded worklist cannot be
/// reserved, an evicted child cannot be faulted in, or the path contains an invalid
/// null edge. Construction failure is never a CAS conflict: callers retry only when
/// the subsequent root compare-exchange actually loses.
///
/// Lifted from the per-variant `build_value_path_iterative` (byte
/// `lockfree_cas.rs:1069`, char `:1348`) — SAME path-copy / absent-spine / valued-leaf
/// semantics and SAME bottom-up build order; only the recursion was already an
/// explicit `Vec`. ITERATIVE because the overlay spine is UN-path-compressed (one
/// node per unit), so a very long key would overflow a recursive stack.
///
/// `fault_in` is the per-variant loader (`load_overlay_node_from_disk`) threaded as a
/// closure so this free function names no `S`; its original error is propagated
/// unchanged.
///
#[inline]
pub(crate) fn build_value_spine<K, V, Fault>(
    node: &Arc<OverlayNode<K, V>>,
    key: &[K::Unit],
    depth: usize,
    value: V,
    fault: FaultMode,
    fault_in: Fault,
) -> crate::persistent_artrie::core::error::Result<Arc<OverlayNode<K, V>>>
where
    K: KeyEncoding,
    V: Clone,
    Fault: Fn(
        &crate::persistent_artrie::core::swizzled_ptr::SwizzledPtr,
    ) -> crate::persistent_artrie::core::error::Result<Arc<OverlayNode<K, V>>>,
{
    let remaining = key.len().checked_sub(depth).ok_or_else(|| {
        PersistentARTrieError::internal(format!(
            "valued overlay path depth {depth} exceeds key length {}",
            key.len()
        ))
    })?;
    let mut spine = super::OverlayPathSpine::<K, V>::new();
    let mut current = super::OverlayNodeHandle::Borrowed(node);
    let mut d = depth;
    loop {
        if d == key.len() {
            // Reached the leaf: bake finality + value into a fresh copy, then rebuild
            // every ancestor bottom-up (the path copy).
            let new_node = Arc::new(current.node().as_final().with_value(value));
            return Ok(unwind_spine(spine, new_node));
        }

        let unit = key[d];
        match resolve_or_fault(&current, unit, fault, |pointer| fault_in(pointer)) {
            ChildResolution::InMem(next) => {
                super::try_push_overlay_path_spine(
                    &mut spine,
                    super::OverlayPathFrame {
                        node: current,
                        unit,
                    },
                    remaining,
                )
                .map_err(|source| {
                    PersistentARTrieError::allocation_failed(
                        "valued overlay path-copy spine",
                        remaining,
                        source,
                    )
                })?;
                current = next;
                d += 1;
            }
            ChildResolution::Faulted(next) => {
                super::try_push_overlay_path_spine(
                    &mut spine,
                    super::OverlayPathFrame {
                        node: current,
                        unit,
                    },
                    remaining,
                )
                .map_err(|source| {
                    PersistentARTrieError::allocation_failed(
                        "valued overlay path-copy spine",
                        remaining,
                        source,
                    )
                })?;
                current = super::OverlayNodeHandle::Owned(next);
                d += 1;
            }
            ChildResolution::FaultFailed(error) => return Err(*error),
            ChildResolution::Null => {
                return Err(match fault {
                    FaultMode::NoFaultIn => PersistentARTrieError::internal(
                        "valued overlay path-copy encountered an evicted child while fault-in was disabled",
                    ),
                    FaultMode::Fault => PersistentARTrieError::corrupted(
                        "valued overlay path-copy encountered a null child edge",
                    ),
                });
            }
            ChildResolution::Absent => {
                // Child absent: build the remaining spine bottom-up (valued leaf),
                // splice at `unit`, then rebuild the collected spine.
                let leaf = Arc::new(OverlayNode::<K, V>::new().as_final().with_value(value));
                let mut sub = leaf;
                for &u in key[d + 1..].iter().rev() {
                    sub = Arc::new(OverlayNode::<K, V>::new().with_child(u, Child::InMem(sub)));
                }
                let new_node = Arc::new(current.node().with_child(unit, Child::InMem(sub)));
                return Ok(unwind_spine(spine, new_node));
            }
        }
    }
}

#[cfg(test)]
mod valued_path_tests {
    use super::{
        build_value_spine, resolve_or_fault, Child, ChildResolution, FaultMode, OverlayNode,
    };
    use crate::persistent_artrie::core::error::PersistentARTrieError;
    use crate::persistent_artrie::core::key_encoding::ByteKey;
    use crate::persistent_artrie::core::overlay::OverlayNodeHandle;
    use crate::persistent_artrie::core::swizzled_ptr::{NodeType, SwizzledPtr};
    use std::sync::Arc;

    #[test]
    fn resident_resolution_borrows_without_arc_traffic() {
        let child = Arc::new(OverlayNode::<ByteKey>::new());
        let root = Arc::new(
            OverlayNode::<ByteKey>::new().with_child(b'x', Child::InMem(Arc::clone(&child))),
        );
        let strong_before = Arc::strong_count(&child);
        let current = OverlayNodeHandle::Borrowed(&root);

        let resolution = resolve_or_fault(&current, b'x', FaultMode::Fault, |_| {
            unreachable!("resident resolution must not invoke the faulter")
        });

        match resolution {
            ChildResolution::InMem(OverlayNodeHandle::Borrowed(resolved)) => {
                assert!(Arc::ptr_eq(resolved, &child));
            }
            _ => panic!("resident child must remain borrowed"),
        }
        assert_eq!(Arc::strong_count(&child), strong_before);
    }

    #[test]
    fn descendants_of_fault_owned_nodes_remain_owned() {
        let child = Arc::new(OverlayNode::<ByteKey>::new());
        let parent = Arc::new(
            OverlayNode::<ByteKey>::new().with_child(b'x', Child::InMem(Arc::clone(&child))),
        );
        let strong_before = Arc::strong_count(&child);
        let current = OverlayNodeHandle::Owned(parent);

        let resolution = resolve_or_fault(&current, b'x', FaultMode::Fault, |_| {
            unreachable!("resident resolution must not invoke the faulter")
        });

        match resolution {
            ChildResolution::InMem(OverlayNodeHandle::Owned(resolved)) => {
                assert!(Arc::ptr_eq(&resolved, &child));
                assert_eq!(Arc::strong_count(&child), strong_before + 1);
            }
            _ => panic!("a child below a fault-owned node must be owned"),
        }
        assert_eq!(Arc::strong_count(&child), strong_before);
    }

    #[test]
    fn successful_fault_is_distinguished_from_a_resident_child() {
        let root = Arc::new(OverlayNode::<ByteKey>::new().with_child(
            b'x',
            Child::OnDisk(SwizzledPtr::on_disk(7, 11, NodeType::Node4)),
        ));
        let loaded = Arc::new(OverlayNode::<ByteKey>::new().as_final());
        let current = OverlayNodeHandle::Borrowed(&root);

        let resolution = resolve_or_fault(&current, b'x', FaultMode::Fault, |observed| {
            let location = observed.disk_location().expect("on-disk location");
            assert_eq!(location.block_id, 7);
            assert_eq!(location.offset, 11);
            assert_eq!(location.node_type, NodeType::Node4);
            Ok(Arc::clone(&loaded))
        });

        match resolution {
            ChildResolution::Faulted(resolved) => assert!(Arc::ptr_eq(&resolved, &loaded)),
            _ => panic!("a successful on-disk load must remain a Faulted outcome"),
        }
    }

    #[test]
    fn failed_fault_preserves_its_typed_error() {
        let root = Arc::new(OverlayNode::<ByteKey>::new().with_child(
            b'x',
            Child::OnDisk(SwizzledPtr::on_disk(7, 11, NodeType::Node4)),
        ));
        let current = OverlayNodeHandle::Borrowed(&root);

        let resolution = resolve_or_fault(&current, b'x', FaultMode::Fault, |_| {
            Err(PersistentARTrieError::BufferPoolExhausted {
                pinned_pages: 4,
                total_pages: 4,
            })
        });

        assert!(matches!(
            resolution,
            ChildResolution::FaultFailed(error)
                if matches!(
                    *error,
                    PersistentARTrieError::BufferPoolExhausted {
                        pinned_pages: 4,
                        total_pages: 4
                    }
                )
        ));
    }

    #[test]
    fn null_and_absent_edges_remain_distinct() {
        let root = Arc::new(
            OverlayNode::<ByteKey>::new().with_child(b'x', Child::OnDisk(SwizzledPtr::null())),
        );
        let current = OverlayNodeHandle::Borrowed(&root);

        let null = resolve_or_fault(&current, b'x', FaultMode::Fault, |_| {
            unreachable!("a null edge must not invoke the faulter")
        });
        let absent = resolve_or_fault(&current, b'y', FaultMode::Fault, |_| {
            unreachable!("an absent edge must not invoke the faulter")
        });

        assert!(matches!(null, ChildResolution::Null));
        assert!(matches!(absent, ChildResolution::Absent));
    }

    #[test]
    fn no_fault_mode_classifies_an_on_disk_edge_without_loading_it() {
        let root = Arc::new(OverlayNode::<ByteKey>::new().with_child(
            b'x',
            Child::OnDisk(SwizzledPtr::on_disk(7, 11, NodeType::Node4)),
        ));
        let current = OverlayNodeHandle::Borrowed(&root);

        let resolution = resolve_or_fault(&current, b'x', FaultMode::NoFaultIn, |_| {
            unreachable!("NoFaultIn must never invoke the loader")
        });

        assert!(matches!(resolution, ChildResolution::Null));
    }

    #[test]
    fn invalid_start_depth_is_a_typed_error() {
        let root = Arc::new(OverlayNode::<ByteKey>::new());
        let result = build_value_spine(
            &root,
            b"x",
            2,
            (),
            FaultMode::Fault,
            |_| -> crate::persistent_artrie::core::error::Result<_> {
                unreachable!("invalid depth must fail before fault-in")
            },
        );

        assert!(matches!(
            result,
            Err(PersistentARTrieError::InternalError { .. })
        ));
    }

    #[test]
    fn fault_in_error_is_not_reclassified_as_contention() {
        let root = Arc::new(OverlayNode::<ByteKey>::new().with_child(
            b'x',
            Child::OnDisk(SwizzledPtr::on_disk(1, 1, NodeType::Node4)),
        ));
        let result = build_value_spine(&root, b"x", 0, (), FaultMode::Fault, |_| {
            Err(PersistentARTrieError::BufferPoolExhausted {
                pinned_pages: 8,
                total_pages: 8,
            })
        });

        assert!(matches!(
            result,
            Err(PersistentARTrieError::BufferPoolExhausted {
                pinned_pages: 8,
                total_pages: 8
            })
        ));
    }

    #[test]
    fn null_child_is_reported_as_corruption() {
        let root = Arc::new(
            OverlayNode::<ByteKey>::new().with_child(b'x', Child::OnDisk(SwizzledPtr::null())),
        );
        let result = build_value_spine(
            &root,
            b"x",
            0,
            (),
            FaultMode::Fault,
            |_| -> crate::persistent_artrie::core::error::Result<_> {
                unreachable!("null child must fail before fault-in")
            },
        );

        assert!(matches!(
            result,
            Err(PersistentARTrieError::CorruptedFile { .. })
        ));
    }

    #[test]
    fn valued_path_spill_failure_is_typed_and_does_not_mutate_the_root() {
        use crate::persistent_artrie::core::overlay::{
            overlay_spine_failpoint, INLINE_OVERLAY_DEPTH,
        };

        let key = vec![b'x'; INLINE_OVERLAY_DEPTH + 1];
        let mut root = Arc::new(OverlayNode::<ByteKey>::new().as_final());
        for &unit in key.iter().rev() {
            root = Arc::new(OverlayNode::<ByteKey>::new().with_child(unit, Child::InMem(root)));
        }
        let root_before = Arc::clone(&root);
        let _failpoint = overlay_spine_failpoint::fail_next_spill();

        let result = build_value_spine(
            &root,
            &key,
            0,
            (),
            FaultMode::Fault,
            |_| -> crate::persistent_artrie::core::error::Result<_> {
                unreachable!("resident path must not fault")
            },
        );

        assert!(matches!(
            result,
            Err(PersistentARTrieError::AllocationFailed { .. })
        ));
        assert!(Arc::ptr_eq(&root, &root_before));
    }
}

/// Resolve a single spine edge `node[unit]` into a RICH [`ChildResolution`] — the
/// single OnDisk-child resolution primitive (the fault-in copy-pasted ~7× before
/// G5.3'). The CALLER maps the resolution to its OWN per-variant enum.
///
/// `fault_in` is the per-variant loader (`load_overlay_node_from_disk`) as a closure
/// so this names no `S`; it returns `Result<Arc, PersistentARTrieError>` so a real
/// I/O failure is distinguishable from a missing/null edge. Permanent fault errors
/// are propagated; they are never reclassified as retryable root-CAS conflicts.
///
/// # The (variant × method × resolution) mapping table (FIX 2 — assert each cell matches today)
///
/// Each cell is how that method maps the [`ChildResolution`] variant. `descend`
/// means descend into the resolved child. Verified against source at the cited lines.
///
/// ```text
///                                       │ InMem   │ Faulted │ FaultFailed       │ Null              │ Absent
/// ──────────────────────────────────────┼─────────┼─────────┼───────────────────┼───────────────────┼───────────────────
/// byte durable insert                   │ descend │ descend │ propagate error   │ corruption        │ create_spine
///   build_final_path_iterative          │         │         │                   │                   │  (final leaf)
/// byte durable remove                   │ descend │ descend │ propagate error   │ AlreadyAbsent     │ AlreadyAbsent
///   build_remove_path_iterative         │         │         │                   │                   │
/// byte VALUE path                       │ descend │ descend │ propagate error   │ corruption        │ create valued
///   build_value_path_iterative          │         │         │                   │                   │  spine
///   (byte:1100-1132)                    │         │         │                   │                   │
/// char insert                           │ descend │ descend │ Io(e) → IoError   │ AlreadyExists     │ create_spine
///   build_path_iterative                │         │         │                   │                   │  (finalize flag)
/// char remove                           │ descend │ descend │ Io(e) → IoError   │ AlreadyAbsent     │ AlreadyAbsent
///   build_remove_path_iterative         │         │         │                   │                   │
/// char VALUE path                       │ descend │ descend │ propagate error   │ corruption        │ create valued
///   build_value_path_iterative          │         │         │                   │                   │  spine
///   (char:1379-1414)                    │         │         │                   │                   │
/// vocabulary insert (NoFaultIn)         │ descend │ impossible │ impossible       │ Unavailable       │ create_spine
///   try_insert_lockfree_path            │         │ by mode │ by mode           │                   │  (valued leaf)
/// ```
///
/// NOTE the VALUE paths use [`build_value_spine`] rather than this primitive
/// directly; that machine propagates construction and fault failures. This
/// primitive serves the membership/remove builders, which need the richer
/// per-operation `FaultFailed` distinction.
///
#[inline]
pub(crate) fn resolve_or_fault<'root, K, V, Fault>(
    node: &super::OverlayNodeHandle<'root, K, V>,
    unit: K::Unit,
    fault: FaultMode,
    fault_in: Fault,
) -> ChildResolution<'root, K, V>
where
    K: KeyEncoding,
    V: Clone,
    Fault: FnOnce(
        &crate::persistent_artrie::core::swizzled_ptr::SwizzledPtr,
    ) -> crate::persistent_artrie::core::error::Result<Arc<OverlayNode<K, V>>>,
{
    match node {
        super::OverlayNodeHandle::Borrowed(node) => match node.find_child(unit) {
            Some(child) => {
                if let Some(child_arc) = child.as_in_mem() {
                    ChildResolution::InMem(super::OverlayNodeHandle::Borrowed(child_arc))
                } else if let Some(on_disk) = child.as_on_disk().filter(|p| !p.is_null()) {
                    match fault {
                        FaultMode::NoFaultIn => ChildResolution::Null,
                        FaultMode::Fault => match fault_in(on_disk) {
                            Ok(loaded) => ChildResolution::Faulted(loaded),
                            Err(e) => ChildResolution::FaultFailed(Box::new(e)),
                        },
                    }
                } else {
                    ChildResolution::Null
                }
            }
            None => ChildResolution::Absent,
        },
        super::OverlayNodeHandle::Owned(node) => match node.find_child(unit) {
            Some(child) => {
                if let Some(child_arc) = child.as_in_mem() {
                    ChildResolution::InMem(super::OverlayNodeHandle::Owned(Arc::clone(child_arc)))
                } else if let Some(on_disk) = child.as_on_disk().filter(|p| !p.is_null()) {
                    match fault {
                        FaultMode::NoFaultIn => ChildResolution::Null,
                        FaultMode::Fault => match fault_in(on_disk) {
                            Ok(loaded) => ChildResolution::Faulted(loaded),
                            Err(e) => ChildResolution::FaultFailed(Box::new(e)),
                        },
                    }
                } else {
                    ChildResolution::Null
                }
            }
            None => ChildResolution::Absent,
        },
    }
}

// ============================================================================
// RemoveAttempt — the UNIFORM core outcome of ONE durable-remove CAS attempt
// ============================================================================

/// The variant-agnostic outcome of a SINGLE durable membership-clear CAS attempt
/// (the [`OverlayCasWalk::try_remove_path_attempt`] hook). The per-variant
/// `LockfreeRemoveResult` enums (char's `Removed(u64)` / byte's `Removed`) collapse
/// to this UNIFORM core — and CRUCIALLY, FIX 1: the char variant's per-attempt
/// `root.version()` is DROPPED at the boundary (it is NOT carried in `Removed`), so
/// the skeleton's retry loop can ONLY rank with the CALLER-claimed
/// `committed_generation` ([`OverlayCasWalk::claim_generation`]). A
/// `Removed(published_root_version)` field would re-open the A.2 cross-restart
/// resurrection bug, so it deliberately carries nothing.
pub(crate) enum RemoveAttempt {
    /// The term was present and cleared: a new root with the freshly-cleared
    /// (non-final) leaf was published via the winning root CAS. Carries NO
    /// generation (FIX 1 — the skeleton ranks the CALLER-claimed `commit_seq`).
    Removed,
    /// The term is absent on this snapshot (full depth non-final, or a missing/null
    /// spine edge). No spine was published — the idempotent NO-RANK arm.
    AlreadyAbsent,
    /// The root CAS failed due to a concurrent modification — the caller re-finds
    /// and retries (re-claiming a fresh generation).
    Conflict,
    /// WRITE-PATH FAULT-IN I/O error (the Remove WAL record is ALREADY durable):
    /// the evicted prefix could not be faulted in to make the clear visible. The
    /// caller surfaces `Err(e)` (the durable-but-visible-after-reopen window).
    IoError(Box<PersistentARTrieError>),
}

// ============================================================================
// InsertAttempt — the UNIFORM core outcome of ONE durable-insert CAS attempt
// ============================================================================

/// The variant-agnostic outcome of a SINGLE durable membership-insert CAS attempt
/// (the [`OverlayCasWalk::try_insert_path_attempt`] hook) — the DURABLE
/// single-phase publish (a FRESH FINAL leaf inside the root CAS, the sole LP). The
/// per-variant `LockfreeInsertResult` (char `Inserted(node, version)`) /
/// `LockfreeDurableInsertResult` (byte `Inserted(version)`) collapse to this.
///
/// FIX 1: `Inserted` carries NEITHER the leaf NOR the per-attempt `root.version()`
/// — the DURABLE path does not hand a leaf to a caller-level `try_set_final` (the
/// root CAS fully arbitrates — REC 3, single-LP), and the generation is the
/// CALLER-claimed `commit_seq`, NEVER the walk's version.
///
/// This is the DURABLE-insert outcome ONLY. The NON-DURABLE `insert_cas`
/// two-phase publish (CAS a non-final spine, THEN the caller-level `try_set_final`)
/// is NOT routed through the skeleton (REC 3) and does not produce this.
pub(crate) enum InsertAttempt {
    /// The term was newly published FINAL via the WINNING root CAS (this op newly
    /// published it; a racer loses the CAS, retries, sees `AlreadyExists`). Carries
    /// NO generation (FIX 1 — the skeleton ranks the CALLER-claimed `commit_seq`).
    Inserted,
    /// The term is already present on this snapshot (the leaf is already final). No
    /// spine was published — the idempotent NO-RANK arm.
    AlreadyExists,
    /// The root CAS failed due to a concurrent modification — the caller re-finds
    /// and retries (re-claiming a fresh generation).
    Conflict,
    /// WRITE-PATH FAULT-IN I/O error (the Insert WAL record is ALREADY durable):
    /// the evicted prefix could not be faulted in to make the write visible. The
    /// caller surfaces `Err(e)` (the durable-but-visible-after-reopen window).
    IoError(Box<PersistentARTrieError>),
}

// ============================================================================
// P6 — the UNIFIED durable single-phase CAS outcome + cache direction.
//
// `drive_insert_cas` and `drive_remove_cas` (P3/P2) were 95%-identical retry
// loops differing only in (a) which attempt hook, (b) the cache direction
// (mark-present on insert vs invalidate on remove). P6 unifies their BODY into
// ONE `drive_cas` (REC 3: SAFE — both are DURABLE single-phase paths whose root
// CAS is the sole LP, NEITHER inherits the NON-durable `try_set_final` arbiter;
// the forbidden merge is byte's non-durable two-phase loop, which is NOT routed
// through the skeleton at all). The two public drivers stay as thin dispatchers
// so the insert-vs-remove distinction is explicit at the call boundary and the
// per-variant attempt enums (`InsertAttempt`/`RemoveAttempt`) stay separate.
// ============================================================================

/// The UNIFIED outcome of one durable single-phase CAS attempt, onto which both
/// [`InsertAttempt`] and [`RemoveAttempt`] map (FIX 1: NO generation field —
/// `drive_cas` ranks the CALLER-claimed `commit_seq`, never a walk version).
///
/// `pub(crate)` (not private) only because it appears in the signature of the
/// `pub(crate)` trait method `OverlayCasWalk::drive_cas` (which the two public
/// drivers call). It is never named outside this module's two dispatchers.
pub(crate) enum CasOutcome {
    /// The op applied (insert published / remove cleared) via the winning root CAS.
    Applied,
    /// Idempotent no-op (already-present insert race / already-absent remove). No
    /// publication — the NO-RANK + liveness-mark arm.
    Idempotent,
    /// Root CAS lost to a concurrent modification — retry (re-claim generation).
    Conflict,
    /// Fault-in I/O error (the WAL record is durable). Surface `Err(e)`.
    IoError(Box<PersistentARTrieError>),
}

/// Which way [`OverlayCasWalk::drive_cas`] touches the positive lookup cache on a
/// state-changing arm. Insert MARKS the term present (a later point read
/// short-circuits present); remove INVALIDATES it (§3.4 — a stale positive entry
/// would otherwise read present forever after a clear).
///
/// `pub(crate)` for the same reason as [`CasOutcome`] (it appears in `drive_cas`'s
/// `pub(crate)` signature); never named outside this module.
#[derive(Clone, Copy)]
pub(crate) enum CacheDirection {
    /// Insert: `mark_positive_cache` on both the Applied and Idempotent arms.
    MarkPresent,
    /// Remove: `invalidate_positive_cache` (FIRST, before mark) on both arms.
    Invalidate,
}

// ============================================================================
// OverlayCasWalk — the per-variant specialization hook trait + default skeleton
// ============================================================================

/// The SHARED CAS-walk SKELETON surface (G5.3'). A subtrait of [`LockFreeOverlay`]
/// (so the skeleton has the overlay root + `claim_commit_seq` + `note_cas_retry` in
/// scope). The default method [`Self::claim_generation`] is the FIX-1 generation
/// source — the durable global `commit_seq`, NEVER the walk's `root.version()`.
///
/// P0 (this scaffold) defines only the generation hook + its default. The per-variant
/// descent helpers (`find_*`, `create_spine`, `build_value_spine`, `resolve_or_fault`)
/// are FREE functions above (no trait dispatch needed — they take `&Arc<OverlayNode>`
/// directly), so the variants delegate to them from their inherent `pub(crate)` shims
/// (P1) without an extra trait method. Subsequent phases (P2-P6) add the
/// remove/insert skeleton default methods + their hooks here as they are routed.
///
/// `Self: Sized` so the default methods take `&self` on the concrete monomorph (no
/// `dyn` — fully monomorphized, the design's "hooks monomorphized" requirement).
pub(crate) trait OverlayCasWalk<K: KeyEncoding, V: DictionaryValue, S>:
    LockFreeOverlay<K, V, S> + Sized
{
    /// **MANDATORY FIX 1 — the recovery generation source.** Claim the commit
    /// generation for the CURRENT retry-loop iteration: the durable global
    /// `commit_seq` (restart-seeded), the SAME value `reconcile_lww` orders replay
    /// by. The default delegates to [`LockFreeOverlay::claim_commit_seq`] — already
    /// `self.commit_seq.fetch_add(1, AcqRel) + 1`, identical in both variants.
    ///
    /// The CALLER's retry loop claims this at the loop-top, RE-CLAIMS it each
    /// iteration (so a Conflict-retry discards the lost claim), and passes the
    /// CALLER-CLAIMED value to `commit_rank_and_mark` as the `committed_generation`.
    /// It MUST NEVER be sourced from the walk's `root.version()` (post-restart
    /// version resets → wrong replay order → resurrected/dropped term, the A.2 bug).
    #[inline]
    fn claim_generation(&self) -> u64 {
        self.claim_commit_seq()
    }

    // ========================================================================
    // P2 — DURABLE REMOVE skeleton (shared retry loop + Order-A tail).
    // The DESCENT stays in the per-variant `try_remove_path_attempt` hook
    // (it names the variant's iterative remove builder + result enum); the
    // skeleton owns ONLY the retry structure, the FIX-1 generation claim, the
    // cache-invalidate, and the data-loss-critical commit-rank/watermark ORDER.
    // ========================================================================

    /// **Per-variant remove SEAM hook — ONE durable membership-clear CAS attempt.**
    /// Loads the published root, builds a NEW spine whose target leaf is a FRESH
    /// `as_non_final` copy (the variant's iterative remove builder —
    /// `units`/`chars` decode + the per-(variant×method) OnDisk mapping live here),
    /// and CAS-publishes it via the root pointer. Returns the UNIFORM
    /// [`RemoveAttempt`] — the per-variant `LockfreeRemoveResult` is mapped to it at
    /// the boundary, DROPPING any per-attempt `root.version()` (FIX 1). NO WAL
    /// append (the skeleton owns Order-A step 1), NO commit rank (step 3).
    fn try_remove_path_attempt(
        &self,
        key_bytes: &[u8],
        permit: &SemanticMutationPublicationPermit<'_, RegistryEligibleMutation>,
    ) -> RemoveAttempt;

    /// **Per-variant cache-invalidate SEAM hook.** Remove `key_bytes`'s positive
    /// lookup-cache entry (the §3.4 DATA-CORRECTNESS guard: a remove that cleared
    /// the trie but left a stale positive cache entry would read present forever).
    /// Called by [`Self::drive_remove_cas`] on EVERY state-changing arm BEFORE
    /// `mark_committed`.
    fn invalidate_positive_cache(&self, key_bytes: &[u8]);

    /// **Order-A durable REMOVE retry-loop driver (shared).** Step 2 (the visibility
    /// CAS loop) + step 3 (commit-rank + watermark) of `remove_cas_durable`, for a
    /// NON-EMPTY term whose `Remove` WAL record was ALREADY appended durable at
    /// `data_lsn` (Order-A step 1, owned by the per-variant caller before the absent
    /// fast-path / "" special-case it must keep). The single durable append covers
    /// every CAS retry (we never re-append — that would burn LSNs + punch a watermark
    /// hole).
    ///
    /// FIX 1: the generation is claimed PER ITERATION via [`Self::claim_generation`]
    /// (the durable global `commit_seq`), RE-CLAIMED on a `Conflict` retry, and on a
    /// winning `Removed` it is THIS iteration's claim that is bound by
    /// `commit_rank_and_mark` — NEVER a per-attempt `root.version()` (the hook
    /// already dropped it). `key_bytes` is the raw key the data record mutated.
    ///
    /// Returns `Ok(true)` (cleared a present term — ranked), `Ok(false)`
    /// (idempotent AlreadyAbsent — NO-RANK + liveness mark), or `Err(e)` (a
    /// fault-in I/O error — the record is durable + replays on reopen; the watermark
    /// correctly stalls at `data_lsn`).
    fn drive_remove_cas(
        &self,
        key_bytes: &[u8],
        pending: PendingDurableMutation<'_, RegistryEligibleMutation>,
    ) -> crate::persistent_artrie::core::error::Result<bool>
    where
        Self: crate::persistent_artrie::core::overlay::durable_write::DurableOverlayWrite<
            K,
            V,
            S,
            PublicationEligibility = RegistryEligibleMutation,
        >,
    {
        // P6: delegate to the unified `drive_cas` core (REC 3-safe — durable
        // single-phase). The remove attempt maps to the UNIFIED `CasOutcome`
        // (DROPPING any per-attempt version — FIX 1) and the cache direction is
        // INVALIDATE (§3.4). `key_bytes` is the raw key the durable `Remove@data_lsn`
        // record mutated.
        self.drive_cas(
            key_bytes,
            pending,
            CacheDirection::Invalidate,
            |this, permit| match this.try_remove_path_attempt(key_bytes, permit) {
                RemoveAttempt::Removed => CasOutcome::Applied,
                RemoveAttempt::AlreadyAbsent => CasOutcome::Idempotent,
                RemoveAttempt::Conflict => CasOutcome::Conflict,
                RemoveAttempt::IoError(e) => CasOutcome::IoError(e),
            },
        )
    }

    // ========================================================================
    // P3 — DURABLE INSERT (single-phase) skeleton. ONLY the durable insert is
    // routed here; the NON-DURABLE `insert_cas` two-phase `try_set_final`
    // arbiter STAYS per-variant (REC 3 — the durable arm must never inherit
    // `try_set_final`, a second commit point that breaks single-LP).
    // ========================================================================

    /// **Per-variant durable-insert SEAM hook — ONE single-phase membership-insert
    /// CAS attempt.** Loads the published root, builds a NEW spine whose target leaf
    /// is a FRESH `as_final` copy (published FINAL inside the root CAS — the sole LP,
    /// the variant's durable iterative final-path builder; the `units`/`chars` decode + the
    /// per-(variant×method) OnDisk mapping live here), and CAS-publishes it. Returns
    /// the UNIFORM [`InsertAttempt`] — DROPPING any per-attempt leaf + `root.version()`
    /// (FIX 1, REC 3). NO WAL append (the skeleton owns Order-A step 1), NO rank.
    fn try_insert_path_attempt(
        &self,
        key_bytes: &[u8],
        permit: &SemanticMutationPublicationPermit<'_, RegistryEligibleMutation>,
    ) -> InsertAttempt;

    /// **Per-variant positive-cache mark SEAM hook.** Record `key_bytes` PRESENT in
    /// the positive lookup cache (the durable insert caches on BOTH the `Inserted`
    /// and the idempotent `AlreadyExists` arm — a subsequent point read short-circuits
    /// present). Called by [`Self::drive_insert_cas`].
    fn mark_positive_cache(&self, key_bytes: &[u8]);

    /// **Order-A durable INSERT (single-phase) retry-loop driver (shared).** Step 2
    /// (the visibility CAS loop, publishing a FRESH FINAL leaf inside the root CAS —
    /// the sole LP) + step 3 (commit-rank + watermark) of the durable membership
    /// insert, for a NON-EMPTY term whose `Insert` WAL record was ALREADY appended
    /// durable at `data_lsn` (Order-A step 1, owned by the per-variant caller before
    /// the non-faulting present-hoist it must keep). The single durable append covers
    /// every CAS retry (we never re-append).
    ///
    /// FIX 1: the generation is claimed PER ITERATION via [`Self::claim_generation`],
    /// RE-CLAIMED on `Conflict`, and on a winning `Inserted` it is THIS iteration's
    /// claim that `commit_rank_and_mark` binds — NEVER a per-attempt `root.version()`
    /// (the hook already dropped it).
    ///
    /// Returns `Ok(true)` (newly published — ranked), `Ok(false)` (idempotent
    /// AlreadyExists — NO-RANK + liveness mark; a concurrent insert won the race
    /// AFTER the caller's non-faulting present-hoist), or `Err(e)` (a fault-in I/O
    /// error — the record is durable + replays on reopen; the watermark stalls at
    /// `data_lsn`).
    fn drive_insert_cas(
        &self,
        key_bytes: &[u8],
        pending: PendingDurableMutation<'_, RegistryEligibleMutation>,
    ) -> crate::persistent_artrie::core::error::Result<bool>
    where
        Self: crate::persistent_artrie::core::overlay::durable_write::DurableOverlayWrite<
            K,
            V,
            S,
            PublicationEligibility = RegistryEligibleMutation,
        >,
    {
        // P6: delegate to the unified `drive_cas` core (REC 3-safe — durable
        // single-phase, NO `try_set_final`). The insert attempt maps to the UNIFIED
        // `CasOutcome` (DROPPING any per-attempt leaf + version — FIX 1, REC 3) and
        // the cache direction is MARK-PRESENT (the durable insert caches on BOTH the
        // Applied and Idempotent arm).
        self.drive_cas(
            key_bytes,
            pending,
            CacheDirection::MarkPresent,
            |this, permit| match this.try_insert_path_attempt(key_bytes, permit) {
                InsertAttempt::Inserted => CasOutcome::Applied,
                InsertAttempt::AlreadyExists => CasOutcome::Idempotent,
                InsertAttempt::Conflict => CasOutcome::Conflict,
                InsertAttempt::IoError(e) => CasOutcome::IoError(e),
            },
        )
    }

    // ========================================================================
    // P6 — the UNIFIED durable single-phase CAS retry-loop driver. ONE copy of
    // the FIX-1 generation claim + the data-loss-critical Order-A
    // commit-rank/watermark ORDER + the cache effect, shared by BOTH the durable
    // insert and durable remove (which differ only in the attempt closure + the
    // cache direction). REC 3-SAFE: both are durable single-phase (the root CAS
    // is the sole LP); the FORBIDDEN merge — byte's NON-durable two-phase
    // `try_set_final` loop — is NOT routed through the skeleton at all.
    // ========================================================================

    /// The unified Order-A durable single-phase CAS retry loop. `attempt` performs
    /// ONE root-CAS attempt and classifies it into the UNIFIED [`CasOutcome`]
    /// (DROPPING any per-attempt `root.version()` — FIX 1, so this loop can ONLY
    /// rank the CALLER-claimed generation). `cache` selects the positive-cache effect
    /// on the state-changing arms. The `Insert`/`Remove` WAL record was ALREADY
    /// appended durable at `data_lsn` (Order-A step 1, owned by the per-variant
    /// caller); the single append covers every retry (we never re-append).
    ///
    /// Returns `Ok(true)` (Applied — ranked), `Ok(false)` (Idempotent — NO-RANK +
    /// liveness mark), or `Err(e)` (a fault-in I/O error — the record is durable +
    /// replays on reopen; the watermark correctly stalls at `data_lsn`).
    fn drive_cas(
        &self,
        key_bytes: &[u8],
        pending: PendingDurableMutation<'_, RegistryEligibleMutation>,
        cache: CacheDirection,
        attempt: impl Fn(
            &Self,
            &SemanticMutationPublicationPermit<'_, RegistryEligibleMutation>,
        ) -> CasOutcome,
    ) -> crate::persistent_artrie::core::error::Result<bool>
    where
        Self: crate::persistent_artrie::core::overlay::durable_write::DurableOverlayWrite<
            K,
            V,
            S,
            PublicationEligibility = RegistryEligibleMutation,
        >,
    {
        // The positive-cache effect for the current direction (insert MARKs present,
        // remove INVALIDATEs — §3.4), applied FIRST on every state-changing arm
        // (before `mark_committed`).
        let touch_cache = |this: &Self| match cache {
            CacheDirection::MarkPresent => this.mark_positive_cache(key_bytes),
            CacheDirection::Invalidate => this.invalidate_positive_cache(key_bytes),
        };
        loop {
            // FIX 1: claim the durable global `commit_seq` at the loop-top, RE-CLAIMED
            // each iteration so a Conflict-retry discards the lost claim and takes a
            // fresh (higher) one. The winning iteration's claim is strictly monotone
            // in the global root-CAS order AND durable across restart — the recovery
            // generation `reconcile_lww` orders by, NEVER the walk's `root.version()`.
            let committed_generation = self.claim_generation();
            match attempt(self, pending.permit()) {
                CasOutcome::Applied => {
                    // Cache effect FIRST (before mark): the op is now visible.
                    touch_cache(self);
                    // Order-A step 2.5 + 3: bind the CALLER-claimed generation durable,
                    // then advance the watermark over BOTH LSNs.
                    let data_lsn = pending.commit_visible();
                    self.commit_rank_and_mark(data_lsn, key_bytes, committed_generation)?;
                    return Ok(true);
                }
                CasOutcome::Idempotent => {
                    // NO-RANK (a concurrent op won the race after the caller's hoist /
                    // present-check). Still touch the cache + `mark_committed` for
                    // LIVENESS (cover the burned LSN or the contiguous watermark stalls;
                    // the Overlay-regime replay drops the unranked record — no resurrect).
                    touch_cache(self);
                    let data_lsn = pending.cancel_unpublished();
                    self.mark_committed_burned(data_lsn);
                    return Ok(false);
                }
                CasOutcome::Conflict => {
                    self.note_cas_retry();
                    continue;
                }
                CasOutcome::IoError(e) => {
                    // The WAL record is durable; we could not make the op visible.
                    // Surface it; do NOT advance the watermark (the contiguous prefix
                    // correctly stalls at `data_lsn` until a later retry / recovery).
                    // Recovery replays the logged record — NOT a lost write. This is
                    // a terminal publication/fault/allocation failure, not a root-CAS
                    // conflict, so it must not inflate the contention counter.
                    return Err(*e);
                }
            }
        }
    }
}

// ============================================================================
// Send/Sync witness — the scaffold must not regress auto-Send/Sync
// ============================================================================

/// Compile-time witness that the shared CAS-walk types stay `Send + Sync` (the
/// overlay node auto-derives both; these free types must not introduce a non-`Send`
/// field). Zero `unsafe` — the assertion is a no-op generic fn, never called.
#[allow(dead_code)]
fn _assert_send_sync<K: KeyEncoding, V: Send + Sync + Clone + 'static>() {
    fn is_send_sync<T: Send + Sync>() {}
    is_send_sync::<ChildResolution<K, V>>();
    is_send_sync::<FaultMode>();
}
