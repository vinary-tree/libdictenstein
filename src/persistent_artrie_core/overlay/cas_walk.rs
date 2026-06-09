//! `cas_walk` — the SHARED lock-free CAS-walk SKELETON (G5.3', design
//! `docs/design/slice3-g5-overlay-genericization-2026-06-09.md` §G5.3').
//!
//! # What this module shares (and what it deliberately does NOT)
//!
//! Before G5.3' the byte (`persistent_artrie/lockfree_cas.rs`) and char
//! (`persistent_artrie_char/lockfree_cas.rs`) overlays carried token-for-token-
//! identical CAS-walk DESCENT logic (find-leaf, create-spine, build-value-spine,
//! and the OnDisk write-path fault-in copied ~7×) that differed only in the key
//! unit `K::Unit` (`u8` vs `u32`). This module lifts the COMMON descent ONCE,
//! generic over `<K: KeyEncoding, V>`:
//!
//!   * [`find_leaf_recursive`] / [`find_in_lockfree_trie`] — the non-faulting
//!     in-memory point-read walks (membership `bool` + leaf `Arc`).
//!   * [`create_spine`] — the bottom-up "build the remaining spine for a key
//!     suffix" path builder, parameterized by a leaf-maker closure (so a
//!     non-durable non-final leaf vs a durable `as_final` leaf vs a valued leaf
//!     are all the SAME reverse-iteration loop). SAME build order as the prior
//!     per-variant `create_lockfree_path` / `create_lockfree_path_final` — the
//!     on-disk serializer consumes node-build order, so this is format-preserving.
//!   * [`build_value_spine`] — the iterative valued path-copy (lifted from the
//!     already-iterative per-variant `build_value_path_recursive`).
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
//!     in [`resolve_or_fault`]'s doc) — byte non-durable-insert's
//!     `FaultFailed/Null → AlreadyExists` (TERMINAL, NOT a retry) is preserved;
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
//! byte has FOUR distinct OnDisk/IO mappings; char is uniform (fault-in, only a
//! real I/O failure surfaces). [`resolve_or_fault`] returns the RICH
//! [`ChildResolution`] so the (variant × method) mapping stays per-variant — see
//! the table in its doc.
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

use crate::persistent_artrie_core::error::PersistentARTrieError;
use crate::persistent_artrie_core::key_encoding::KeyEncoding;
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::persistent_artrie_core::overlay::node::{Child, OverlayNode};
use crate::value::DictionaryValue;

// ============================================================================
// ChildResolution — the RICH outcome of resolving one spine edge (FIX 2)
// ============================================================================

/// The outcome of resolving a single spine edge during a CAS-walk descent —
/// either an already-in-memory child, a freshly-faulted-in child, an I/O failure
/// faulting an evicted (`OnDisk`) child, a null filler slot, or a missing edge.
///
/// **Why a RICH enum (FIX 2).** byte has FOUR distinct OnDisk/IO mappings (see the
/// table in [`resolve_or_fault`]) and char is uniform; collapsing them to a single
/// `→ Conflict` would silently change byte's non-durable-insert behavior
/// (`FaultFailed/Null → AlreadyExists`, TERMINAL — a livelock if turned into a
/// retry) and lose char's `IoError`. So the resolution primitive returns this and
/// each (variant × method) maps the cells itself.
///
/// `InMem` and `Faulted` are distinguished so a caller could (today none do) tell a
/// resident child from a freshly-faulted one; both carry the descend target.
/// `FaultFailed` boxes the error so the common arms stay pointer-sized.
///
pub(crate) enum ChildResolution<K: KeyEncoding, V> {
    /// The edge exists and the child is resident in memory — descend into it.
    InMem(Arc<OverlayNode<K, V>>),
    /// The edge exists, the child was evicted (`OnDisk`), and the fault-in SUCCEEDED
    /// — descend into the freshly-loaded child (spliced `Child::InMem` by the
    /// caller, so the single root CAS stays the sole arbiter).
    Faulted(Arc<OverlayNode<K, V>>),
    /// The edge exists, the child was evicted, and the fault-in FAILED with a
    /// buffer-manager I/O error. Boxed so the common (pointer-sized) arms are not
    /// widened. Each method maps this per-variant (byte non-durable-insert: TERMINAL
    /// `AlreadyExists`; byte durable / char: retry `Conflict` / surface `IoError`).
    FaultFailed(Box<PersistentARTrieError>),
    /// The edge exists but holds a null filler slot (never a real child).
    Null,
    /// The edge does NOT exist (no child for this unit on this snapshot).
    Absent,
}

/// Whether to fault an evicted (`OnDisk`) child back in during [`resolve_or_fault`].
///
/// The byte VALUE path (`build_value_path_recursive`) historically returned `None`
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

/// Non-faulting recursive leaf find: descend `key[depth..]` through IN-MEMORY
/// children only, returning the final leaf `Arc` iff the full path exists AND the
/// leaf is final, else `None`. An `OnDisk` child short-circuits to `None` (the
/// lock-free overlay cannot traverse a disk ref without a faulter — the per-variant
/// faulting walk `find_leaf_faulting` handles eviction).
///
/// Token-for-token the prior per-variant `find_leaf_recursive` (byte
/// `lockfree_cas.rs:555`, char `:1293`), now generic over `K::Unit`.
///
#[inline]
pub(crate) fn find_leaf_recursive<K: KeyEncoding, V: Clone>(
    node: &Arc<OverlayNode<K, V>>,
    key: &[K::Unit],
    depth: usize,
) -> Option<Arc<OverlayNode<K, V>>> {
    if depth == key.len() {
        return if node.is_final() {
            Some(Arc::clone(node))
        } else {
            None
        };
    }
    let child = node.find_child(key[depth])?;
    // Can't traverse disk refs in the lock-free overlay; `as_in_mem` returns
    // `None` for an on-disk child, short-circuiting via `?` (owned `Arc`).
    let child_arc = child.as_in_mem()?;
    find_leaf_recursive(&Arc::clone(child_arc), key, depth + 1)
}

/// Non-faulting recursive membership check: `true` iff `key[depth..]` reaches a
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
    if depth >= key.len() {
        return node.is_final();
    }
    let key_unit = key[depth];
    if let Some(child) = node.find_child(key_unit) {
        if let Some(child_arc) = child.as_in_mem() {
            return find_in_lockfree_trie(&Arc::clone(child_arc), key, depth + 1);
        }
    }
    false
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
) -> (Arc<OverlayNode<K, V>>, Arc<OverlayNode<K, V>>)
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

/// The ITERATIVE valued path-copy: descend `key[depth..]` from `node` collecting the
/// `(parent, unit)` spine (faulting `OnDisk` children in per `fault`), then rebuild
/// it bottom-up with a fresh `as_final().with_value(value)` leaf. Returns the new
/// root `Arc`, or `None` if an `OnDisk` child blocked the copy (a fault failure, or
/// a null filler, or [`FaultMode::NoFaultIn`]) — the caller's increment/value seam
/// treats `None` as a transient conflict / a durable-but-deferred error.
///
/// Lifted from the already-iterative per-variant `build_value_path_recursive` (byte
/// `lockfree_cas.rs:1069`, char `:1348`) — SAME path-copy / absent-spine / valued-leaf
/// semantics and SAME bottom-up build order; only the recursion was already an
/// explicit `Vec`. ITERATIVE because the overlay spine is UN-path-compressed (one
/// node per unit), so a very long key would overflow a recursive stack.
///
/// `fault_in` is the per-variant loader (`load_overlay_node_from_disk`) threaded as a
/// closure so this free function names no `S`; it returns `Result<Arc, _>` and the
/// `.ok()?` collapses an I/O error to `None` (the EXACT prior value-path behavior:
/// both variants' value paths return `None` on a fault-in I/O error — no rich error).
///
#[inline]
pub(crate) fn build_value_spine<K, V, Fault>(
    node: &Arc<OverlayNode<K, V>>,
    key: &[K::Unit],
    depth: usize,
    value: V,
    fault: FaultMode,
    fault_in: Fault,
) -> Option<Arc<OverlayNode<K, V>>>
where
    K: KeyEncoding,
    V: Clone,
    Fault: Fn(
        &crate::persistent_artrie_core::swizzled_ptr::SwizzledPtr,
    ) -> crate::persistent_artrie_core::error::Result<Arc<OverlayNode<K, V>>>,
{
    let mut spine: Vec<(Arc<OverlayNode<K, V>>, K::Unit)> =
        Vec::with_capacity(key.len().saturating_sub(depth));
    let mut current = Arc::clone(node);
    let mut d = depth;
    loop {
        if d == key.len() {
            // Reached the leaf: bake finality + value into a fresh copy, then rebuild
            // every ancestor bottom-up (the path copy).
            let mut new_node = Arc::new(current.as_final().with_value(value));
            for (parent, unit) in spine.into_iter().rev() {
                new_node = Arc::new(parent.with_child(unit, Child::InMem(new_node)));
            }
            return Some(new_node);
        }

        let unit = key[d];
        match current.find_child(unit) {
            Some(child) => {
                let child_arc = if let Some(child_arc) = child.as_in_mem() {
                    // In-memory child: descend (path-copy on the way back up).
                    Arc::clone(child_arc)
                } else {
                    // WRITE-PATH FAULT-IN: the child was EVICTED to OnDisk. Fault it
                    // back in then descend, splicing it InMem — the single root CAS
                    // stays the sole arbiter. On I/O error (or NoFaultIn / null) return
                    // `None` (the prior `as_in_mem()? ` / `.ok()?` semantics).
                    match fault {
                        FaultMode::NoFaultIn => return None,
                        FaultMode::Fault => {
                            let on_disk = child.as_on_disk().filter(|p| !p.is_null())?;
                            fault_in(on_disk).ok()?
                        }
                    }
                };
                spine.push((current, unit));
                current = child_arc;
                d += 1;
            }
            None => {
                // Child absent: build the remaining spine bottom-up (valued leaf),
                // splice at `unit`, then rebuild the collected spine.
                let leaf = Arc::new(OverlayNode::<K, V>::new().as_final().with_value(value));
                let mut sub = leaf;
                for &u in key[d + 1..].iter().rev() {
                    sub = Arc::new(OverlayNode::<K, V>::new().with_child(u, Child::InMem(sub)));
                }
                let mut new_node = Arc::new(current.with_child(unit, Child::InMem(sub)));
                for (parent, u) in spine.into_iter().rev() {
                    new_node = Arc::new(parent.with_child(u, Child::InMem(new_node)));
                }
                return Some(new_node);
            }
        }
    }
}

/// Resolve a single spine edge `node[unit]` into a RICH [`ChildResolution`] — the
/// single OnDisk-child resolution primitive (the fault-in copy-pasted ~7× before
/// G5.3'). The CALLER maps the resolution to its OWN per-variant enum.
///
/// `fault_in` is the per-variant loader (`load_overlay_node_from_disk`) as a closure
/// so this names no `S`; it returns `Result<Arc, PersistentARTrieError>` so a real
/// I/O failure is distinguishable from a missing/null edge (which is what
/// distinguishes byte's `FaultFailed → AlreadyExists` from char's `Io → IoError`).
///
/// # The (variant × method × resolution) mapping table (FIX 2 — assert each cell matches today)
///
/// Each cell is how that method maps the [`ChildResolution`] variant. `descend`
/// means recurse into the resolved child. Verified against source at the cited lines.
///
/// ```text
///                                       │ InMem   │ Faulted │ FaultFailed       │ Null              │ Absent
/// ──────────────────────────────────────┼─────────┼─────────┼───────────────────┼───────────────────┼───────────────────
/// byte non-durable insert               │ descend │ descend │ AlreadyExists     │ AlreadyExists     │ create_spine
///   build_path_recursive (byte:395-421) │         │         │  (TERMINAL, false)│  (TERMINAL, false)│  (non-final leaf)
/// byte durable insert                   │ descend │ descend │ Conflict (retry)  │ Conflict (retry)  │ create_spine
///   build_final_path_recursive          │         │         │                   │                   │  (final leaf)
///   (byte:920-952)                      │         │         │                   │                   │
/// byte durable remove                   │ descend │ descend │ Conflict (retry)  │ AlreadyExists     │ AlreadyExists
///   build_remove_path_recursive         │         │         │                   │  (= absent)       │  (= absent)
///   (byte:1030-1059)                    │         │         │                   │                   │
/// byte VALUE path                       │ descend │ descend │ None              │ None              │ create valued
///   build_value_path_recursive          │         │         │  (no rich error)  │  (no rich error)  │  spine
///   (byte:1100-1132)                    │         │         │                   │                   │
/// char insert                           │ descend │ descend │ Io(e) → IoError   │ AlreadyExists     │ create_spine
///   build_path_recursive (char:1040-)   │         │         │                   │                   │  (finalize flag)
/// char remove                           │ descend │ descend │ Io(e) → IoError   │ AlreadyAbsent     │ AlreadyAbsent
///   build_remove_path_recursive         │         │         │                   │                   │
///   (char:911-954)                      │         │         │                   │                   │
/// char VALUE path                       │ descend │ descend │ None              │ None              │ create valued
///   build_value_path_recursive          │         │         │  (no rich error)  │  (no rich error)  │  spine
///   (char:1379-1414)                    │         │         │                   │                   │
/// ```
///
/// NOTE the VALUE paths use [`build_value_spine`] (which already folds resolution +
/// the `None`-on-fault-failure mapping) rather than this primitive directly, so
/// their two cells are realized there; this primitive serves the membership/remove
/// builders (which need the RICH `FaultFailed` distinction).
///
#[inline]
pub(crate) fn resolve_or_fault<K, V, Fault>(
    node: &OverlayNode<K, V>,
    unit: K::Unit,
    fault: FaultMode,
    fault_in: Fault,
) -> ChildResolution<K, V>
where
    K: KeyEncoding,
    V: Clone,
    Fault: FnOnce(
        &crate::persistent_artrie_core::swizzled_ptr::SwizzledPtr,
    ) -> crate::persistent_artrie_core::error::Result<Arc<OverlayNode<K, V>>>,
{
    match node.find_child(unit) {
        Some(child) => {
            if let Some(child_arc) = child.as_in_mem() {
                ChildResolution::InMem(Arc::clone(child_arc))
            } else if let Some(on_disk) = child.as_on_disk().filter(|p| !p.is_null()) {
                match fault {
                    FaultMode::NoFaultIn => ChildResolution::Null,
                    FaultMode::Fault => match fault_in(on_disk) {
                        Ok(loaded) => ChildResolution::Faulted(loaded),
                        Err(e) => ChildResolution::FaultFailed(Box::new(e)),
                    },
                }
            } else {
                // Null filler (never a real child).
                ChildResolution::Null
            }
        }
        None => ChildResolution::Absent,
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
    // (it names the variant's `build_remove_path_recursive` + result enum); the
    // skeleton owns ONLY the retry structure, the FIX-1 generation claim, the
    // cache-invalidate, and the data-loss-critical commit-rank/watermark ORDER.
    // ========================================================================

    /// **Per-variant remove SEAM hook — ONE durable membership-clear CAS attempt.**
    /// Loads the published root, builds a NEW spine whose target leaf is a FRESH
    /// `as_non_final` copy (the variant's `build_remove_path_recursive` —
    /// `units`/`chars` decode + the per-(variant×method) OnDisk mapping live here),
    /// and CAS-publishes it via the root pointer. Returns the UNIFORM
    /// [`RemoveAttempt`] — the per-variant `LockfreeRemoveResult` is mapped to it at
    /// the boundary, DROPPING any per-attempt `root.version()` (FIX 1). NO WAL
    /// append (the skeleton owns Order-A step 1), NO commit rank (step 3).
    fn try_remove_path_attempt(&self, key_bytes: &[u8]) -> RemoveAttempt;

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
        data_lsn: crate::persistent_artrie_core::wal::Lsn,
    ) -> crate::persistent_artrie_core::error::Result<bool>
    where
        Self: crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite<K, V, S>,
    {
        // P6: delegate to the unified `drive_cas` core (REC 3-safe — durable
        // single-phase). The remove attempt maps to the UNIFIED `CasOutcome`
        // (DROPPING any per-attempt version — FIX 1) and the cache direction is
        // INVALIDATE (§3.4). `key_bytes` is the raw key the durable `Remove@data_lsn`
        // record mutated.
        self.drive_cas(
            key_bytes,
            data_lsn,
            CacheDirection::Invalidate,
            |this| match this.try_remove_path_attempt(key_bytes) {
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
    /// the variant's durable `build_path_recursive(finalize=true)` /
    /// `build_final_path_recursive`; the `units`/`chars` decode + the
    /// per-(variant×method) OnDisk mapping live here), and CAS-publishes it. Returns
    /// the UNIFORM [`InsertAttempt`] — DROPPING any per-attempt leaf + `root.version()`
    /// (FIX 1, REC 3). NO WAL append (the skeleton owns Order-A step 1), NO rank.
    fn try_insert_path_attempt(&self, key_bytes: &[u8]) -> InsertAttempt;

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
        data_lsn: crate::persistent_artrie_core::wal::Lsn,
    ) -> crate::persistent_artrie_core::error::Result<bool>
    where
        Self: crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite<K, V, S>,
    {
        // P6: delegate to the unified `drive_cas` core (REC 3-safe — durable
        // single-phase, NO `try_set_final`). The insert attempt maps to the UNIFIED
        // `CasOutcome` (DROPPING any per-attempt leaf + version — FIX 1, REC 3) and
        // the cache direction is MARK-PRESENT (the durable insert caches on BOTH the
        // Applied and Idempotent arm).
        self.drive_cas(
            key_bytes,
            data_lsn,
            CacheDirection::MarkPresent,
            |this| match this.try_insert_path_attempt(key_bytes) {
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
        data_lsn: crate::persistent_artrie_core::wal::Lsn,
        cache: CacheDirection,
        attempt: impl Fn(&Self) -> CasOutcome,
    ) -> crate::persistent_artrie_core::error::Result<bool>
    where
        Self: crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite<K, V, S>,
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
            match attempt(self) {
                CasOutcome::Applied => {
                    // Cache effect FIRST (before mark): the op is now visible.
                    touch_cache(self);
                    // Order-A step 2.5 + 3: bind the CALLER-claimed generation durable,
                    // then advance the watermark over BOTH LSNs.
                    self.commit_rank_and_mark(data_lsn, key_bytes, committed_generation)?;
                    return Ok(true);
                }
                CasOutcome::Idempotent => {
                    // NO-RANK (a concurrent op won the race after the caller's hoist /
                    // present-check). Still touch the cache + `mark_committed` for
                    // LIVENESS (cover the burned LSN or the contiguous watermark stalls;
                    // the Overlay-regime replay drops the unranked record — no resurrect).
                    touch_cache(self);
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
                    // Recovery replays the logged record — NOT a lost write.
                    self.note_cas_retry();
                    let _ = data_lsn;
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
