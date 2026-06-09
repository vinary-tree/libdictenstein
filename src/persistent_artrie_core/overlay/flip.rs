//! `LockFreeOverlay<K, V, S>` — the SHARED GENERIC lock-free-overlay flip
//! (route + read-engine + flip + reestablish), extracted token-for-token from the
//! char variant so byte can reuse it rather than copy-paste
//! (`docs/design/overlay-flip-genericization.md` §2).
//!
//! # What this trait is
//!
//! A **seam trait with default-provided generic methods + variant-supplied seam
//! methods** (design §2). Each persistent ARTrie variant writes ONE thin `impl`
//! supplying only the seam (the owned-tree readers, the overlay publishers, the
//! WAL/field accessors, the per-variant counter monomorph); the data-loss-
//! critical generic logic — the route predicate, the overlay-read DFS walks in
//! `K::Unit` space, `flip_to_overlay` (incl. the WAL-regime stamp), and the
//! `reestablish` streaming-fold (the clear-owned-LAST control flow) — lives here
//! ONCE as default methods.
//!
//! Not a blanket impl (three distinct trie structs, no single type to blanket);
//! not a wrapper struct (reestablish mutates `&mut self` trie state while
//! reading the owned tree via `&self`, which a wrapper cannot express without a
//! lifetime mess across the `&self`-iter-before-`&mut`-clear ordering).
//! Coherence holds (trait in core, impls in variant modules, one crate — same as
//! the existing `TrieRoot for OverlayNode` blanket).
//!
//! # D1 — the #1 data-loss risk (READ THIS BEFORE EDITING A SEAM IMPL)
//!
//! The `owned_*` seam methods MUST read the OWNED tree directly. The structural
//! converter `build_overlay_root_from_owned` (used by `reestablish_overlay_from_owned`
//! and the F7 reopen converter) reads them while `route_overlay()` may ALREADY be TRUE
//! (the ctor flips before reestablishing), so routing an owned read through the public
//! `iter_prefix`/`get`/`contains`/`get_value` would read the EMPTY overlay, publish
//! nothing, then clear the owned tree LAST = TOTAL IRREVERSIBLE LOSS. Each `owned_*`
//! seam carries a `# Safety (data-loss)` contract; a CI grep gate (contract §6(a)) FAILS
//! if any `owned_*` seam body references
//! `route_overlay`/`iter_prefix(`/`self.get(`/`get_value(`/`contains(`.
//!
//! # NON-FAULTING read engine — DO NOT add disk fault-in
//!
//! Every overlay walk here descends **in-memory children only**
//! (`Child::as_in_mem`), never resolving `Child::OnDisk`. This is deliberate and
//! load-bearing: a faulting read racing a checkpoint/eviction that holds the
//! buffer-manager lock is the lock-ordering inversion that deadlocked the soak
//! for 75+ minutes (memory `feedback_production-deadlock-is-costly`). The walks
//! are therefore **resident-finals / last-checkpoint-consistent** (exact while no
//! overlay node is evicted; overlay eviction is `#[cfg(feature =
//! "bench-internals")]`/test-only, so the count is exact for this release).

use std::any::Any;

use crate::persistent_artrie_core::error::Result;
use crate::persistent_artrie_core::key_encoding::KeyEncoding;
// G5.2 (RT-1): the shared faulting read-fault default + its retry budget, so
// `overlay_value_get` is a single FAULTING default (BUG #46 parity for byte+char).
// `OverlayEvictable<K, V, S>: OverlayFaulter<K, V>` — the prior `OverlayFaulter`
// supertrait of `LockFreeOverlay` is now reached transitively through it.
use crate::persistent_artrie_core::overlay::evict::{
    OverlayEvictable, DEFAULT_MAX_FAULTIN_RETRIES,
};
use crate::persistent_artrie_core::overlay::node::OverlayNode;
use crate::value::DictionaryValue;
use std::sync::Arc;

// ============================================================================
// F7 — crash-injection FAIL POINTS for the Owned→Overlay conversion (crash-safety
// proptest, `tests/persistent_owned_to_overlay_conversion_crash.rs`).
//
// `convert_owned_to_overlay_on_reopen` consults [`f7_failpoint::armed`] BETWEEN each
// durable step and returns a simulated-crash `Err` when the armed point matches —
// modeling a power-cut at that point (the durable WAL/disk artifacts up to that step
// survive; the trie construction aborts, and the test reopens to assert recovery).
//
// This is a RUNTIME atomic (default DISARMED = `None`), ALWAYS compiled: the only
// consult site is the cold reopen-conversion path, so the cost in production is a single
// `Relaxed` load per Owned→Overlay reopen (negligible; never on the hot read/write path).
// Disarmed it is a strict no-op. The test arms it via [`f7_failpoint::arm`] / disarms via
// [`f7_failpoint::disarm`] around each reopen.
// ============================================================================

/// F7 conversion crash-injection fail points (see module doc).
pub mod f7_failpoint {
    use std::sync::atomic::{AtomicU8, Ordering};

    /// The conversion steps a crash can be injected BEFORE. `None` (0) = disarmed.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FailPoint {
        /// Disarmed — no crash injected (production default).
        None,
        /// Crash BEFORE the rotate (records-non-empty path) — i.e. before any durable
        /// conversion side effect. (On the cheap path: before the in-place Overlay stamp.)
        BeforeRotate,
        /// Crash AFTER the rotate/in-place-stamp's DURABLE side effect but BEFORE the
        /// Overlay header is durably stamped+fsync'd. On the ROTATE path this is the
        /// torn window the v4 FIX-D crash-loop targets (the tail is archived; the fresh
        /// active is records-empty + Owned-regime, not yet Overlay).
        AfterRotateBeforeStamp,
        /// Crash AFTER the Overlay stamp+fsync (the S2 durable commit point) but BEFORE
        /// the overlay is built from the dense image (`load_root_immutable_seam`).
        AfterStampBeforeBuild,
        /// Crash DURING the archive-aware drain (after the overlay is built + installed,
        /// before the drain applies the tail).
        DuringDrain,
    }

    impl FailPoint {
        fn as_u8(self) -> u8 {
            match self {
                FailPoint::None => 0,
                FailPoint::BeforeRotate => 1,
                FailPoint::AfterRotateBeforeStamp => 2,
                FailPoint::AfterStampBeforeBuild => 3,
                FailPoint::DuringDrain => 4,
            }
        }
        fn from_u8(v: u8) -> FailPoint {
            match v {
                1 => FailPoint::BeforeRotate,
                2 => FailPoint::AfterRotateBeforeStamp,
                3 => FailPoint::AfterStampBeforeBuild,
                4 => FailPoint::DuringDrain,
                _ => FailPoint::None,
            }
        }
    }

    static ARMED: AtomicU8 = AtomicU8::new(0);

    /// Arm the converter to simulate a crash BEFORE `fp` (test-only). Returns a guard
    /// that DISARMS on drop, so a `?`-early-return in the test still resets the global.
    pub fn arm(fp: FailPoint) -> ArmGuard {
        ARMED.store(fp.as_u8(), Ordering::SeqCst);
        ArmGuard
    }

    /// Disarm (back to production no-op behavior).
    pub fn disarm() {
        ARMED.store(0, Ordering::SeqCst);
    }

    /// The currently-armed fail point (`None` when disarmed = production).
    pub fn armed() -> FailPoint {
        FailPoint::from_u8(ARMED.load(Ordering::SeqCst))
    }

    /// RAII guard returned by [`arm`]; disarms on drop.
    #[must_use]
    pub struct ArmGuard;
    impl Drop for ArmGuard {
        fn drop(&mut self) {
            disarm();
        }
    }
}

/// `Some(())` re-wrapped as `V` iff `V == ()`, else `None`.
///
/// Membership finals (`V = ()`) carry `value: None` (`OverlayNode::as_final`
/// leaves the value unset), but the owned-tree semantics give every membership
/// term the value `()`. So when iterating values for the `V = ()` monomorph we
/// SYNTHESIZE `()` for each final via a SAFE `Any` re-wrap (the same zero-`unsafe`
/// pattern as the variant's `lockfree_value_route`). Generic over `K` is
/// unnecessary (the value is independent of the key encoding), so this is a free
/// function on `V` alone.
#[inline]
pub(crate) fn unit_as_v<V: DictionaryValue>() -> Option<V> {
    let unit = ();
    (&unit as &dyn Any).downcast_ref::<V>().cloned()
}

/// The SHARED GENERIC lock-free-overlay flip surface (design §2).
///
/// `K` is the key encoding (`ByteKey`/`CharKey`), `V` the value, `S` the block
/// storage. The variant supplies the seam (below); the default methods encode the
/// data-loss-critical generic logic.
///
/// `Self: Sized` (the default methods take `&self`/`&mut self` and downcast `self`
/// via `Any` in the seam) and `Self: 'static` (so `self` can be `Any` for the
/// value-route seam — guaranteed for the concrete trie monomorphs).
/// Outcome of a RANKED root publication (the durable empty-term `""` path —
/// [`LockFreeOverlay::publish_root_cas_ranked`]).
pub(crate) enum RootPublishOutcome {
    /// THIS call published a fresh root; carries the WINNING commit generation
    /// (claimed at the winning CAS iteration), which the durable caller binds via
    /// `commit_rank_and_mark`.
    Published(u64),
    /// The root was already in the target state (a concurrent op won between the
    /// caller's present-hoist and this CAS) — the caller `mark_committed_burned`s
    /// the appended LSN (idempotent NO-RANK; ranking a no-op would resurrect).
    AlreadyInState,
}

pub(crate) trait LockFreeOverlay<K: KeyEncoding, V: DictionaryValue, S>:
    OverlayEvictable<K, V, S> + Sized + 'static
{
    /// The per-variant counter monomorph (`u64` for char, `i64` for byte). THE
    /// divergence that makes the value-route a seam, not a blanket. `Copy` so the
    /// publisher/getter seams can pass it by value. `Serialize + DeserializeOwned` so
    /// the F5 WAL-tail applier can re-encode a recovered absolute-`i64` counter as the
    /// typed `CounterValue` via the shared `counter_codec` (`counter_leaf_to_i128` →
    /// `i128_to_counter_value`) — both `u64` and `i64` satisfy it.
    type CounterValue: 'static + Copy + serde::Serialize + serde::de::DeserializeOwned;

    // ========================================================================
    // REQUIRED SEAM (variant provides) — small accessors + the un-routed owned
    // readers + the overlay publishers.
    // ========================================================================

    /// The lock-free overlay's atomic root pointer, or `None` if the overlay is
    /// not installed (`enable_lockfree()` not yet run for this trie).
    fn lockfree_root(&self)
        -> Option<&crate::persistent_artrie_core::overlay::AtomicNodePtr<K, V>>;

    /// Install the lock-free overlay (an empty `AtomicNodePtr` root + lookup
    /// cache), stamping the WAL Overlay regime when the WAL is empty. Idempotent.
    fn enable_lockfree(&mut self);

    /// The WAL's current (next) LSN, or `None` for an in-memory trie (no WAL).
    fn wal_current_lsn(&self) -> Option<u64>;

    /// `true` iff the WAL header is stamped to the `Overlay` rank regime.
    fn wal_is_overlay_regime(&self) -> bool;

    /// Stamp the WAL header to the `Overlay` regime (no-op / best-effort if the
    /// WAL is non-empty or absent — the variant logs a warning on failure).
    fn wal_stamp_overlay_regime(&self);

    /// `true` iff `V` is an eligible overlay monomorph (`{(), CounterValue}`). The
    /// SOLE expression of the "overlay only for `V ∈ {(), counter}`" invariant.
    fn overlay_eligible_v() -> bool;

    // ---- overlay publishers (the per-variant write seam) ----

    /// Publish membership of `units` to the overlay via the variant's no-WAL CAS
    /// insert (the recovered terms are already durable in the WAL; re-logging
    /// would double-log).
    fn overlay_publish_membership(&self, units: &[K::Unit]);

    /// Read the overlay counter at `units` (the variant's `<CounterValue>`-downcast
    /// + lock-free point read), or `None` if absent.
    fn overlay_counter_get(&self, units: &[K::Unit]) -> Option<Self::CounterValue>;

    /// `true` iff `units` is present (final) in the overlay (the variant's
    /// `contains_lockfree`).
    fn overlay_contains(&self, units: &[K::Unit]) -> bool;

    /// **G5/F1 — publish an ARBITRARY-`V` value for `units` to the overlay via the
    /// variant's no-WAL path-copy CAS** (the recovered terms are already durable in
    /// the WAL; re-logging would double-log). The single generic value publisher used by
    /// the F5 WAL-tail applier ([`Self::apply_recovered_operation_overlay`]) for value
    /// inserts, CAS, AND counter increments (both absolute and the accumulated delta total —
    /// see the `Op::Increment` arms). SETs the value (last-writer = the CAS winner); at
    /// reestablish/replay the overlay is uncontended.
    fn overlay_publish_value(&self, units: &[K::Unit], value: V);

    /// **G5/F1 + G5.2/RT-1 — read the overlay leaf's ARBITRARY-`V` value at `units`**,
    /// FAULTING any `OnDisk` (evicted) interior node back in along the way, or `None`
    /// if absent/non-final. The value twin of [`Self::overlay_counter_get`], used by
    /// the arbitrary-`V` arm of [`Self::overlay_route_get_value`].
    ///
    /// **A shared FAULTING DEFAULT (was a per-variant seam).** This is the BUG #46 fix
    /// (regression-pinned by `tests/overlay_eviction_arbitrary_v_bug46.rs`): an
    /// in-process eviction CAN flip an interior overlay node (whose subtree holds
    /// finals) to `OnDisk`, so a NON-faulting walk returned `None` for every term under
    /// it until reopen. The counter (`overlay_counter_get` → `get_lockfree`) and
    /// membership (`overlay_contains` → `contains_lockfree`) arms ALREADY faulted; G5.2
    /// brings the arbitrary-`V` arm to the SAME faulting walk via the shared
    /// [`OverlayEvictable::find_leaf_faulting`] (the supertrait), making byte + char
    /// IDENTICAL here. `find_leaf_faulting` is infallible-in-practice — every branch
    /// returns `Ok` (it does its own bounded loser-safe install-CAS rebases + a final
    /// read-only liveness walk; a loader I/O error degrades to `Ok(None)`), so the
    /// `Err`-arm fallback is currently unreachable, preserving char's exact prior
    /// behavior while ALSO fixing byte's latent #46 (byte arbitrary-`V` value reads
    /// were non-faulting; no byte test asserted that — this is a strict improvement).
    ///
    /// SAFE on the READ path: this is the `get_value` route (no WAL lock held), NOT a
    /// pre-WAL-append insert present-hoist — so it does not trip the documented
    /// "faulting read before the WAL append racing checkpoint/eviction" lock-ordering
    /// inversion (the "75-minute hang"); the hot-insert hoists keep their NON-faulting
    /// walks.
    fn overlay_value_get(&self, units: &[K::Unit]) -> Option<V> {
        let root_slot = match self.lockfree_root() {
            Some(r) => r,
            None => return None,
        };
        match self.find_leaf_faulting(root_slot, units, DEFAULT_MAX_FAULTIN_RETRIES) {
            Ok(found) => found.and_then(|leaf| leaf.get_value()),
            // Unreachable in the current `find_leaf_faulting` (it returns `Ok` on every
            // path incl. a loader error). Kept as the conservative liveness degrade.
            Err(_) => None,
        }
    }

    /// **F5 — NO-WAL overlay remove of the NON-EMPTY term `units`** (the data-loss-
    /// critical reopen WAL-tail-into-overlay path's Remove arm — see
    /// [`Self::apply_recovered_operation_overlay`]). Clear the term's membership in
    /// the live overlay via the variant's existing single-arbiter `try_remove`
    /// path-copy + root CAS, in a bounded-retry loop, and invalidate the positive
    /// lookup cache. NO WAL append, NO commit-rank, NO watermark advance — the Remove
    /// is ALREADY durable in the WAL being replayed; re-logging would double-log and
    /// punch a watermark hole. The empty term "" is handled by the generic
    /// [`Self::overlay_remove`] default (the root non-final publisher), so this seam
    /// is NEVER called with an empty slice (the default asserts it). The MEMBERSHIP/
    /// VALUE distinction does not matter for remove (both clear finality + value), so
    /// this is generic over `V` (no counter monomorph). On a fault-in I/O error the
    /// removal is best-effort skipped (the durable image already reflects the remove —
    /// a later reopen retries); the durable record is intact.
    fn overlay_try_remove_path(&self, units: &[K::Unit]);

    // ========================================================================
    // DEFAULT-PROVIDED GENERIC METHODS — DO NOT OVERRIDE (they encode D1 +
    // clear-owned-LAST + the non-faulting resident-finals walks).
    // ========================================================================

    /// **L3.3 — the production router predicate: overlay-routed iff the overlay is installed.**
    ///
    /// `true` iff the lock-free overlay is live (`enable_lockfree()` has run ⇒ `lockfree_root`
    /// is `Some`). Since L3.3 deleted `kill_switch_to_owned` (the only writer of `OwnedTree`
    /// mode) and the `OverlayWriteMode` enum, an installed `lockfree_root` ALWAYS implies overlay
    /// routing — so the prior `&& uses_overlay()` conjunct is redundant. Every constructor installs
    /// the overlay (`overlay_eligible_v() == true` for all `V`; `::new()` calls `enable_lockfree`),
    /// so this is universally `true` in production — the owned tree is gone.
    #[inline]
    fn route_overlay(&self) -> bool {
        self.lockfree_root().is_some()
    }

    // ===== overlay-read engine (back len/is_empty/iter_prefix*/get_value) =====

    /// The current immutable overlay root (a hazard-protected `Arc` snapshot), or
    /// `None` if the lock-free overlay is not installed.
    #[inline]
    fn overlay_root_node(&self) -> Option<Arc<OverlayNode<K, V>>> {
        self.lockfree_root().and_then(|r| r.load())
    }

    /// Term count of the overlay (number of finalized nodes). Resident-finals only
    /// (the owned `len` is empty/cleared under the overlay regime — hence this
    /// walk).
    fn overlay_len(&self) -> usize {
        match self.overlay_root_node() {
            Some(root) => Self::overlay_count_finals(&root) as usize,
            None => 0,
        }
    }

    /// Recurses by key length (the overlay is un-path-compressed); depth-safe at
    /// the same bound as the production lock-free point reads.
    fn overlay_count_finals(node: &Arc<OverlayNode<K, V>>) -> u64 {
        let mut count = u64::from(node.is_final());
        for (_, child) in node.iter_children() {
            if let Some(child_arc) = child.as_in_mem() {
                count += Self::overlay_count_finals(child_arc);
            }
        }
        count
    }

    /// Cheap emptiness check — an early-out "any final?" walk, NOT `overlay_len()
    /// == 0` (which would be O(N) on a large overlay).
    fn overlay_is_empty(&self) -> bool {
        match self.overlay_root_node() {
            Some(root) => !Self::overlay_has_final(&root),
            None => true,
        }
    }

    fn overlay_has_final(node: &Arc<OverlayNode<K, V>>) -> bool {
        if node.is_final() {
            return true;
        }
        for (_, child) in node.iter_children() {
            if let Some(child_arc) = child.as_in_mem() {
                if Self::overlay_has_final(child_arc) {
                    return true;
                }
            }
        }
        false
    }

    /// Descend the overlay to the node at `prefix` (a `K::Unit` slice), in-memory
    /// only. Returns `None` iff a prefix unit has no in-memory edge — reproducing
    /// the owned `navigate_to_prefix` `Ok(None)` (absent path) vs `Ok(Some(_))`
    /// (present node, possibly childless) distinction. The empty prefix yields the
    /// root.
    fn overlay_navigate(&self, prefix: &[K::Unit]) -> Option<Arc<OverlayNode<K, V>>> {
        let mut node = self.overlay_root_node()?;
        for &unit in prefix {
            let child = node.find_child(unit)?;
            // Phase A (prefix-fault): descend an OnDisk prefix-path node READ-ONLY —
            // load the durable image via `fault_overlay_slot` (writes nothing, advances
            // no watermark) and continue into the transient `Arc`; NO install/CAS. A
            // null/unresolvable/failed slot returns `None` → absent path, exactly the
            // prior `as_in_mem()?` semantics, now extended over evicted prefixes so
            // enumeration does not under-report a subtree under an evicted node.
            node = match child.as_in_mem() {
                Some(c) => Arc::clone(c),
                None => self.fault_overlay_slot(child.as_on_disk()?)?,
            };
        }
        Some(node)
    }

    /// Overlay analogue of `iter_prefix`, in `K::Unit` space: `None` if the prefix
    /// path is absent, else `Some(unit_sequences)` (possibly empty), each a
    /// `Vec<K::Unit>`. Matches owned DFS pre-order. The variant's public method
    /// converts each `Vec<K::Unit>` to its public term via `K::units_to_term`.
    fn overlay_collect_units(&self, prefix: &[K::Unit]) -> Option<Vec<Vec<K::Unit>>> {
        match self.overlay_navigate(prefix) {
            None => None,
            Some(node) => {
                let mut terms = Vec::new();
                self.overlay_collect_finals(&node, prefix.to_vec(), &mut terms);
                Some(terms)
            }
        }
    }

    /// Pre-order DFS mirroring the owned `collect_terms_under_node` (final-first,
    /// then children in key order). Accumulates `Vec<K::Unit>` by pushing each
    /// edge `*key` (NOT building a `String` via `char::from_u32` — the variant
    /// converts units→term at the public boundary). Recurses by key length;
    /// depth-safe at the production point-read bound.
    fn overlay_collect_finals(
        &self,
        node: &Arc<OverlayNode<K, V>>,
        prefix: Vec<K::Unit>,
        out: &mut Vec<Vec<K::Unit>>,
    ) {
        if node.is_final() {
            out.push(prefix.clone());
        }
        for (key, child) in node.iter_children() {
            // Phase A (prefix-fault): fault an OnDisk child READ-ONLY (load the durable
            // image, recurse into the transient `Arc`; NO install/CAS — enumeration must
            // not bloat the overlay or un-evict). A null/failed slot is skipped
            // (fail-closed, point-read parity: a transient miss, never a fabricated term).
            let child_arc = match child.as_in_mem() {
                Some(c) => Arc::clone(c),
                None => match child
                    .as_on_disk()
                    .and_then(|ptr| self.fault_overlay_slot(ptr))
                {
                    Some(loaded) => loaded,
                    None => continue,
                },
            };
            let mut child_prefix = prefix.clone();
            child_prefix.push(*key);
            self.overlay_collect_finals(&child_arc, child_prefix, out);
        }
    }

    /// Overlay analogue of `iter_prefix_with_values`, in `K::Unit` space. For the
    /// counter monomorph each final's value is its counter (`get_value`); for `V =
    /// ()` each final's value is the synthesized `()` (membership finals carry no
    /// stored value — see `unit_as_v`).
    fn overlay_collect_units_with_values(
        &self,
        prefix: &[K::Unit],
    ) -> Option<Vec<(Vec<K::Unit>, V)>> {
        match self.overlay_navigate(prefix) {
            None => None,
            Some(node) => {
                let mut entries = Vec::new();
                self.overlay_collect_with_values(&node, prefix.to_vec(), &mut entries);
                Some(entries)
            }
        }
    }

    fn overlay_collect_with_values(
        &self,
        node: &Arc<OverlayNode<K, V>>,
        prefix: Vec<K::Unit>,
        out: &mut Vec<(Vec<K::Unit>, V)>,
    ) {
        if node.is_final() {
            // `get_value()` for a counter final is `Some(counter)`; for a `()`
            // final it is `None`, so fall back to the synthesized `()` for the `V
            // == ()` monomorph. For an ineligible `V` (never `route_overlay()`)
            // both are `None` and the final is skipped — harmless, as this path is
            // unreachable for ineligible `V`.
            if let Some(value) = node.get_value().or_else(unit_as_v::<V>) {
                out.push((prefix.clone(), value));
            }
        }
        for (key, child) in node.iter_children() {
            // Phase A (prefix-fault): fault an OnDisk child READ-ONLY (load + recurse
            // transiently, no install/CAS; null/failed → skip, point-read parity).
            let child_arc = match child.as_in_mem() {
                Some(c) => Arc::clone(c),
                None => match child
                    .as_on_disk()
                    .and_then(|ptr| self.fault_overlay_slot(ptr))
                {
                    Some(loaded) => loaded,
                    None => continue,
                },
            };
            let mut child_prefix = prefix.clone();
            child_prefix.push(*key);
            self.overlay_collect_with_values(&child_arc, child_prefix, out);
        }
    }

    // ===== flip / kill-switch =====

    /// **S5-10c — flip construction helper.** Make the lock-free overlay the live
    /// write target: `enable_lockfree()` (which stamps the WAL Overlay regime when
    /// the WAL is empty) then select `LockFreeOverlay` so `route_overlay()` becomes
    /// true. Returns the resulting `route_overlay() && stamped_overlay`.
    ///
    /// **V-1 gate:** a NO-OP returning `false` for `V ∉ {(), CounterValue}` — the
    /// authoritative chokepoint so no caller can enable a broken overlay for
    /// arbitrary `V`.
    fn flip_to_overlay(&mut self) -> bool {
        if !Self::overlay_eligible_v() {
            return false; // arbitrary V: never enable the overlay; stay OwnedTree.
        }
        self.enable_lockfree();
        // `enable_lockfree` stamps the WAL Overlay regime on its FIRST call (it
        // early-returns once `lockfree_root` is set). Belt-and-suspenders: restamp on
        // the fresh-WAL edge (`current_lsn() == 1`) so the V-2 verification below holds
        // even if the first stamp was skipped; a no-op once the Overlay regime is stamped.
        if self.wal_current_lsn() == Some(1) && !self.wal_is_overlay_regime() {
            self.wal_stamp_overlay_regime();
        }
        // V-2: `enable_lockfree` only `log::warn!`s if the Overlay-regime stamp
        // failed, then STILL enables the overlay — so verify the WAL is ACTUALLY
        // Overlay-regime. An Owned-regime WAL under overlay routing would make
        // recovery KEEP unranked orphans (resurrection). A trie with no WAL
        // (in-memory) cannot durably flip and also fails this check. The
        // create-flip caller hard-errors on a `false` return.
        let stamped_overlay = self.wal_current_lsn().is_some() && self.wal_is_overlay_regime();
        self.route_overlay() && stamped_overlay
    }

    // ===== reestablish =====
    //
    // **F7 — the three per-term reestablish folds (`reestablish_overlay_membership` /
    // `reestablish_overlay_counter` / `reestablish_overlay_value`) and the
    // `value_as_counter` helper they used were DELETED.** They are superseded by the KEPT
    // STRUCTURAL converter [`Self::reestablish_overlay_from_owned`] (which calls
    // [`Self::build_overlay_root_from_owned`]): it reproduces the SAME overlay (same
    // term-set + values, incl. counters > i64::MAX and the empty term "") via the SAME D1
    // un-routed `owned_*` seams, force-replaces the root, and clears owned LAST (RES-7) —
    // and is strictly MORE correct (it keeps a term-only counter member the per-term
    // counter fold dropped). The F7 reopen converter, the legacy-loader oracle, the
    // recovery-family ctors, and byte compaction all route through
    // `reestablish_overlay_from_owned` / `build_overlay_root_from_owned` now. The
    // membership-∪-value union the value fold pioneered (the "flag-2 fix") lives on in
    // `build_overlay_root_from_owned`.

    /// Re-wrap a `CounterValue` as `V` via a SAFE `Any` downcast iff `V ==
    /// CounterValue`, else `None`. Re-wraps an overlay counter read into the public `V`
    /// for the value-route ([`Self::overlay_route_get_value`]) and the F5 absolute-counter
    /// apply path.
    fn counter_as_value(counter: Self::CounterValue) -> Option<V> {
        (&counter as &dyn Any).downcast_ref::<V>().cloned()
    }

    /// **Generic value-route driver** (design §4). Route `get_value(units)` to the
    /// overlay for `V ∈ {(), CounterValue}` via a SAFE `Any` dispatch on `V` (zero
    /// `unsafe`; `K` is never a downcast target — it is baked into the concrete
    /// monomorph the seam names). Returns:
    /// - `Some(Some(v))` — present with value `v` (the counter, or `()` for
    ///   membership), re-wrapped as `V`;
    /// - `Some(None)` — handled by the overlay, term absent;
    /// - `None` — `V` is neither `()` nor `CounterValue` (arbitrary `V`); the
    ///   caller runs its owned-tree body (unreachable under `route_overlay()`).
    ///
    /// The per-variant `overlay_get_value` skin delegates here; the only seam it
    /// uses are [`Self::overlay_counter_get`] / [`Self::overlay_contains`], so the
    /// counter-monomorph naming stays in the ~2-LOC seam (design §4).
    fn overlay_route_get_value(&self, units: &[K::Unit]) -> Option<Option<V>> {
        use std::any::TypeId;
        // `V == CounterValue`: read the counter via the seam, re-wrap as `V`.
        if TypeId::of::<V>() == TypeId::of::<Self::CounterValue>() {
            return Some(
                self.overlay_counter_get(units)
                    .and_then(Self::counter_as_value),
            );
        }
        // `V == ()`: membership — present ⇒ `Some(())` re-wrapped as `V`.
        if TypeId::of::<V>() == TypeId::of::<()>() {
            return Some(if self.overlay_contains(units) {
                unit_as_v::<V>()
            } else {
                None
            });
        }
        // G5/F1: ARBITRARY `V` — read the leaf's `Option<V>` directly (non-faulting,
        // exact: overlay finals are never evicted in production, §2.4/RT5). `Some(_)`
        // tells the caller the overlay handled it (NEVER fall through to owned — the
        // NH1 read-side: a fall-through owned read post-flip sees the empty owned
        // tree). Unreachable until the F2 eligibility flip (route_overlay() false).
        Some(self.overlay_value_get(units))
    }

    // ========================================================================
    // EMPTY-TERM ROOT PUBLISHERS (empty-string support) — the FRESH-ROOT-CAS
    // discipline, shared by byte + char.
    //
    // The empty term "" is the unit-slice `&[]`, which navigates to the overlay
    // ROOT. The root is the UNIQUE node a concurrent non-empty insert COPIES (via
    // `with_child`, which snapshots flags into a fresh node) rather than SHARES —
    // so an in-place `try_set_final`/`try_set_value` on the live root is a LOST
    // UPDATE against that copy. Therefore every empty-term mutation publishes a
    // FRESH root (`as_final()`/`.with_value(v)`/`.as_non_final()`) via the root
    // `compare_exchange` — the SAME single-arbiter CAS every non-empty write uses.
    // (loom-gated: tests/persistent_lockfree_overlay_loom.rs.)
    // ========================================================================

    /// Claim the next commit generation (the per-iteration `commit_seq` of the
    /// durable root CAS). REQUIRED seam: byte/char return
    /// `self.commit_seq.fetch_add(1, AcqRel) + 1`. Used ONLY by the RANKED
    /// publisher so the generation binds to the WINNING CAS iteration — NEVER
    /// claimed once-before (that would mis-order a concurrent insert/remove of ""
    /// vs the CAS order, the split-LP data loss the ranked loop avoids).
    fn claim_commit_seq(&self) -> u64;

    /// Note a CAS retry (observability only). Default no-op; byte/char override to
    /// bump `self.cas_retries`.
    fn note_cas_retry(&self) {}

    /// UNRANKED fresh-root-CAS loop (no WAL, no commit rank) — the non-durable +
    /// reestablish empty-term publishers. `needs_publish(root)` short-circuits an
    /// idempotent no-op (returns `Ok(false)` without a CAS); `transform(root)`
    /// builds the fresh root published via `compare_exchange`. Bounded-retry
    /// lock-free; rebases on the freshly-loaded root each iteration.
    fn publish_root_cas(
        &self,
        transform: impl Fn(&OverlayNode<K, V>) -> Arc<OverlayNode<K, V>>,
        needs_publish: impl Fn(&OverlayNode<K, V>) -> bool,
    ) -> Result<bool> {
        let root_ptr = self.lockfree_root().ok_or_else(|| {
            crate::persistent_artrie_core::error::PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
            )
        })?;
        loop {
            let old = match root_ptr.load() {
                Some(r) => r,
                None => {
                    let _ = root_ptr.try_init(Arc::new(OverlayNode::new()));
                    continue;
                }
            };
            if !needs_publish(&old) {
                return Ok(false);
            }
            let new = transform(&old);
            match root_ptr.compare_exchange(&old, new) {
                Ok(_) => return Ok(true),
                Err(_) => {
                    self.note_cas_retry();
                    continue;
                }
            }
        }
    }

    /// RANKED fresh-root-CAS loop (the DURABLE empty-term path). Like
    /// [`Self::publish_root_cas`] but claims a `commit_seq` generation per
    /// iteration (via [`Self::claim_commit_seq`]) and returns the WINNING
    /// generation, so the durable caller binds `commit_rank_and_mark`.
    /// `already_in_state(root)` detects the concurrent-op-won idempotent case (a
    /// WAL record was appended but acked no change → the caller burns the LSN,
    /// never ranks it).
    fn publish_root_cas_ranked(
        &self,
        transform: impl Fn(&OverlayNode<K, V>) -> Arc<OverlayNode<K, V>>,
        already_in_state: impl Fn(&OverlayNode<K, V>) -> bool,
    ) -> Result<RootPublishOutcome> {
        let root_ptr = self.lockfree_root().ok_or_else(|| {
            crate::persistent_artrie_core::error::PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
            )
        })?;
        loop {
            // Per-iteration claim — the WINNING iteration's generation is returned.
            let generation = self.claim_commit_seq();
            let old = match root_ptr.load() {
                Some(r) => r,
                None => {
                    let _ = root_ptr.try_init(Arc::new(OverlayNode::new()));
                    continue;
                }
            };
            if already_in_state(&old) {
                return Ok(RootPublishOutcome::AlreadyInState);
            }
            let new = transform(&old);
            match root_ptr.compare_exchange(&old, new) {
                Ok(_) => return Ok(RootPublishOutcome::Published(generation)),
                Err(_) => {
                    self.note_cas_retry();
                    continue;
                }
            }
        }
    }

    /// Publish membership of the empty term "" (set the root final), NO WAL. Used
    /// by the non-durable `insert_cas("")` and the reestablish membership fold.
    /// Returns `Ok(true)` iff this call newly finalized the root.
    fn overlay_publish_root_membership(&self) -> Result<bool> {
        self.publish_root_cas(|r| Arc::new(r.as_final()), |r| !r.is_final())
    }

    /// Publish the empty term "" WITH a value (set the root final + value), NO
    /// WAL. Used by the reestablish counter fold. ALWAYS publishes (LWW upsert);
    /// no value-equality short-circuit (`DictionaryValue` does not bound
    /// `PartialEq`) — a redundant CAS on an identical value is correctness-neutral.
    fn overlay_publish_root_value(&self, value: V) -> Result<()> {
        self.publish_root_cas(
            move |r| Arc::new(r.as_final().with_value(value.clone())),
            |_| true,
        )
        .map(|_| ())
    }

    // ========================================================================
    // F5 — direct dense→overlay reopen loader: the GENERIC pieces (the
    // WAL-tail-into-overlay applier + the no-WAL overlay remove). The per-variant
    // walk-converter (`build_overlay_root_from_owned` / `load_root_immutable`) lives
    // in each variant module (it walks the variant's owned `CharTrieRoot`/`TrieRoot`)
    // and installs the pre-built root via `install_prebuilt_overlay_root` below.
    // See `docs/design/slice3-f5-loader-impl.md`.
    // ========================================================================

    /// **F5 GATE (S3: SWITCHED ON).** When `true` (the current S3 state), the reopen
    /// Overlay-regime branch uses the F5 direct dense→overlay loader
    /// (`load_root_immutable` + the archive-aware [`Self::reconcile_and_drain_overlay`]):
    /// reopen builds the lock-free overlay DIRECTLY from the dense image + drains the WAL
    /// tail (active + archived segments, FIX B) INTO the overlay, never materializing the
    /// owned tree into `self.root` (the F7 prerequisite). When `false` (the legacy oracle
    /// state, used by `open_with_legacy_loader`), reopen uses the LEGACY path (owned
    /// `load_root_from_disk` + owned `replay_records_lww` + `flip` +
    /// `reestablish_overlay_from_owned`). The
    /// `tests/persistent_f5_both_loaders_correspondence.rs` `open_with_f5_loader` ctors
    /// drive F5 regardless of this gate, so they stay a stable oracle either way.
    ///
    /// F5 runs ONLY for `RankRegime::Overlay`, overlay-eligible files (an Owned-regime
    /// file — legacy/un-flipped — keeps the owned loader). The
    /// `checkpoint_lsn = committed-watermark` capture ordering is UNTOUCHED (F5 is
    /// reopen-side only).
    const USE_F5_REOPEN_LOADER: bool = true;

    /// **F5 — install a PRE-BUILT overlay root** (the walk-converter's output) as the
    /// live lock-free overlay, instead of `enable_lockfree`'s EMPTY root. Sets
    /// `lockfree_root = Some(AtomicNodePtr::new(root))` + a fresh lookup cache via the
    /// variant seam [`Self::install_prebuilt_overlay_root_seam`], then selects
    /// `LockFreeOverlay` and stamps/verifies the WAL Overlay regime EXACTLY as
    /// [`Self::flip_to_overlay`] does (re-using its WAL-regime stamp logic), returning
    /// the resulting `route_overlay() && stamped_overlay`. Does NOT touch the owned
    /// tree (F5 adds ALONGSIDE; F7 deletes owned).
    ///
    /// **V-1 gate:** a NO-OP returning `false` for `V ∉ {(), CounterValue}`? No — F2
    /// made ALL `V` overlay-eligible, so this engages for any `V` (the same as
    /// `flip_to_overlay`). It mirrors `flip_to_overlay`'s V-2 stamp re-check so an
    /// Owned-regime WAL under overlay routing (which would KEEP unranked orphans on a
    /// later reopen) is surfaced as `false` (the caller hard-errors).
    fn install_prebuilt_overlay_root(&mut self, root: Arc<OverlayNode<K, V>>) -> bool {
        if !Self::overlay_eligible_v() {
            return false;
        }
        // Install the pre-built root + fresh cache (variant seam — it owns the
        // concrete `AtomicNodePtr`/cache field types). Idempotent guard inside the
        // seam: it only installs when `lockfree_root` is not already set (a fresh
        // reopen trie never has it set).
        self.install_prebuilt_overlay_root_seam(root);
        // Mirror `flip_to_overlay`'s empty-WAL restamp guard: F5 runs on an
        // already-Overlay (non-empty) WAL, so the stamp is normally already Overlay;
        // restamp only on the fresh-WAL edge case (defensive, matches flip).
        if self.wal_current_lsn() == Some(1) && !self.wal_is_overlay_regime() {
            self.wal_stamp_overlay_regime();
        }
        // V-2: verify the WAL is ACTUALLY Overlay-regime (an Owned-regime WAL under
        // overlay routing would resurrect unranked orphans on a later reopen).
        let stamped_overlay = self.wal_current_lsn().is_some() && self.wal_is_overlay_regime();
        self.route_overlay() && stamped_overlay
    }

    /// Variant seam for [`Self::install_prebuilt_overlay_root`]: set the concrete
    /// `lockfree_root` to `AtomicNodePtr::new(root)` and a fresh empty lookup cache.
    /// Idempotent (only installs if not already enabled). The variant owns the field
    /// types, so the install lives in the variant's `lockfree_cas.rs`.
    fn install_prebuilt_overlay_root_seam(&mut self, root: Arc<OverlayNode<K, V>>);

    /// **F5 — NO-WAL overlay remove of `units` (any term, incl. "")**. The empty term
    /// "" → publish a FRESH non-final root via [`Self::publish_root_cas`] (the no-WAL,
    /// no-rank twin of `remove_cas_durable`'s empty-term arm — `as_non_final` on a
    /// fresh root, the single-arbiter root CAS, NOT an in-place clear). A non-empty
    /// term → the variant seam [`Self::overlay_try_remove_path`]. Used ONLY by the F5
    /// WAL-tail applier for a `Remove` winner (a term inserted-into-the-dense-image
    /// then removed in the un-checkpointed WAL tail — it MUST be cleared from the
    /// rebuilt overlay or it RESURRECTS, the exact data-loss class F5 must not
    /// introduce). Errors from the empty-term root CAS are logged + swallowed
    /// (best-effort: the durable image already reflects the remove).
    fn overlay_remove(&self, units: &[K::Unit]) {
        if units.is_empty() {
            // Empty term "": clear root finality via a fresh non-final root CAS
            // (publish only if currently final — `needs_publish = is_final`).
            if let Err(e) = self.publish_root_cas(|r| Arc::new(r.as_non_final()), |r| r.is_final())
            {
                log::warn!(
                    "F5 overlay_remove(\"\"): root non-final CAS failed: {:?}",
                    e
                );
            }
            return;
        }
        self.overlay_try_remove_path(units);
    }

    /// **F5 (THE data-loss-critical path) — apply ONE reconciled
    /// [`RecoveredOperation`] INTO THE OVERLAY** (no WAL), via the SAME no-WAL
    /// publishers [`Self::build_overlay_root_from_owned`] uses. The overlay twin of the
    /// owned `apply_*_recovered_operation_no_wal`: where the owned applier mutates
    /// `self.root`, this publishes into the live lock-free overlay (which already
    /// holds the dense/checkpoint state from `load_root_immutable`). Returns `true`
    /// iff the op was applied (a value-deserialize failure logs + returns `false`,
    /// the SAME best-effort the owned applier has — it does NOT abort the replay).
    ///
    /// # Winners are applied in commit-visibility order
    ///
    /// [`Self::drain_segments_into_overlay`] feeds the winners in `(generation, lsn)`
    /// order, so applying them here reproduces the last-writer-wins final state —
    /// IDENTICAL to the owned applier consuming the SAME winner list. Single-threaded
    /// at reopen (no concurrent writers), so each publisher's root CAS is uncontended.
    ///
    /// # Per-op mapping (mirrors the owned applier exactly)
    ///
    /// * `Insert{value: Some}` / `Upsert` / successful `CompareAndSwap` → deserialize
    ///   `V`, then `overlay_publish_value` (non-empty) / `overlay_publish_root_value`
    ///   (""). A VALUE set, last-writer-wins.
    /// * `Insert{value: None}` → membership: `overlay_publish_membership` (non-empty) /
    ///   `overlay_publish_root_membership` ("").
    /// * `Remove` → `overlay_remove` (clear membership/value; "" via the root non-final
    ///   publisher). REQUIRED for correctness (else an inserted-then-removed-in-tail
    ///   term resurrects).
    /// * `Increment{result: Some(v)}` (a single absolute `Increment`) → decode `v` into `V`
    ///   via the shared `counter_codec` (`counter_leaf_to_i128::<V>` → `i128_to_counter_value`)
    ///   and SET via `overlay_publish_value` / `overlay_publish_root_value` ("") — a VALUE set,
    ///   NEVER an accumulate (an absolute `Increment` carries the post-increment count, incl. a
    ///   decrement).
    /// * `Increment{result: None}` (a `BatchIncrement` DELTA) → ACCUMULATE `delta`: a V-GENERIC
    ///   read-modify-write — read the current value (`overlay_value_get`, 0 if absent), add
    ///   `delta` in the i128 `counter_codec` substrate, and SET the total via
    ///   `overlay_publish_value`. This is generic over EVERY counter `V` (`i64` as well as the
    ///   `u64` monomorph); it does NOT use a `u64`-only `Any`-downcast seam. For "" the counter
    ///   path no-ops (the durable counter path never logs a "" increment), so a "" delta is
    ///   dropped — matching the owned applier's empty-term increment behavior.
    ///
    /// The empty-term "" branches use the RANKED/fresh-root-CAS root publishers
    /// (`overlay_publish_root_value`/`_membership` + the `overlay_remove` non-final
    /// root CAS), NEVER an in-place root mutation — the §2.2/G5-NEW-4 data-loss fix.
    fn apply_recovered_operation_overlay(
        &self,
        op: crate::persistent_artrie_core::recovery::RecoveredOperation,
    ) -> bool {
        use crate::persistent_artrie_core::recovery::RecoveredOperation as Op;
        // Decode the raw key bytes into this encoding's units once, up front. A key
        // that is not valid for the encoding (e.g. a non-UTF-8 byte sequence for the
        // char trie) cannot have been produced by this trie's writers — skip it
        // (best-effort, the owned applier's `String::from_utf8_lossy` is equally
        // lenient; we DROP rather than lossily-mangle so a bogus key never
        // materializes a wrong term).
        let units = match K::units_from_bytes(op.term()) {
            Some(u) => u,
            None => {
                log::warn!("F5 overlay replay: undecodable key for this encoding; skipping op");
                return false;
            }
        };
        let units: &[K::Unit] = units.as_slice();
        match op {
            Op::Insert { value, .. } => match value {
                Some(value_bytes) => {
                    match crate::serialization::bincode_compat::deserialize::<V>(&value_bytes) {
                        Ok(v) => {
                            self.overlay_publish_value_any(units, v);
                            true
                        }
                        Err(error) => {
                            log::warn!(
                                "F5 overlay replay: insert value deserialize failed: {:?}",
                                error
                            );
                            false
                        }
                    }
                }
                None => {
                    self.overlay_publish_membership_any(units);
                    true
                }
            },
            Op::Remove { .. } => {
                self.overlay_remove(units);
                true
            }
            Op::Upsert { value, .. } => {
                match crate::serialization::bincode_compat::deserialize::<V>(&value) {
                    Ok(v) => {
                        self.overlay_publish_value_any(units, v);
                        true
                    }
                    Err(error) => {
                        log::warn!(
                            "F5 overlay replay: upsert value deserialize failed: {:?}",
                            error
                        );
                        false
                    }
                }
            }
            Op::CompareAndSwap {
                new_value, success, ..
            } => {
                if !success {
                    return false;
                }
                match crate::serialization::bincode_compat::deserialize::<V>(&new_value) {
                    Ok(v) => {
                        self.overlay_publish_value_any(units, v);
                        true
                    }
                    Err(error) => {
                        log::warn!(
                            "F5 overlay replay: CAS value deserialize failed: {:?}",
                            error
                        );
                        false
                    }
                }
            }
            Op::Increment { delta, result, .. } => {
                match result {
                    // Absolute (single Increment): SET the counter to `v` (incl. 0). A single
                    // `WalRecord::Increment` carries the ABSOLUTE post-increment value (the
                    // owned write path logs the resulting count, NOT a delta), so replay must
                    // OVERWRITE the counter to `v` — NEVER accumulate. (Owned files can log
                    // an absolute Increment that DECREASES the count, e.g. 5 → 0 via a
                    // negative delta; the F7 converter routes such Owned tails into the
                    // overlay, so the absolute SET must be honored.)
                    Some(v) => {
                        // Decode the absolute value into `V` DIRECTLY via the shared
                        // `counter_codec` (the SAME path the owned applier
                        // `apply_recovered_operation_no_wal` uses: `counter_leaf_to_i128::<V>`
                        // of the i64 leaf bit-pattern → `i128_to_counter_value::<V>`). This
                        // decodes into the TRIE's value type `V` — NOT `Self::CounterValue` —
                        // so it is correct for ANY `Counter` V (`i64` as well as the overlay's
                        // `u64` counter monomorph); the codec returns `None` for a non-counter
                        // `V`, which can never carry an Increment record. Then publish it as a
                        // VALUE SET via `overlay_publish_value` / `overlay_publish_root_value`
                        // (path-copy / fresh-root CAS, last-writer = SET) — NOT an ADD /
                        // accumulate, which would mis-handle an absolute set, e.g. leaving 5
                        // instead of 0 for a 5 → 0 decrement. The counter
                        // is stored in the leaf value, so the value SET and the counter/value
                        // read address the SAME slot.
                        use crate::persistent_artrie_core::counter_codec;
                        let decoded = counter_codec::counter_leaf_to_i128::<V>(&v.to_le_bytes())
                            .and_then(counter_codec::i128_to_counter_value::<V>);
                        match decoded {
                            Some(vv) => {
                                if units.is_empty() {
                                    if let Err(e) = self.overlay_publish_root_value(vv) {
                                        log::warn!(
                                            "F5 overlay replay: root counter set failed: {:?}",
                                            e
                                        );
                                        return false;
                                    }
                                } else {
                                    self.overlay_publish_value(units, vv);
                                }
                                true
                            }
                            None => {
                                log::warn!("F5 overlay replay: increment-absolute value not a counter for this V; skipping");
                                false
                            }
                        }
                    }
                    // Delta (BatchIncrement entry): ACCUMULATE `delta` onto the current value
                    // (commutative on replay). This is a V-GENERIC READ-MODIFY-WRITE — the overlay
                    // twin of the owned `recompute_recovered_increment` + `upsert_impl_no_wal`
                    // (mutation_core.rs): read the current counter value GENERICALLY
                    // (`overlay_value_get`, 0 if absent), add `delta` in the i128 `counter_codec`
                    // substrate, then SET the total via the generic `overlay_publish_value`
                    // (path-copy SET, last-writer). It REPLACES the prior `overlay_publish_counter`,
                    // an `Any`-downcast seam to the `<u64,S>` monomorph that SILENTLY DROPPED the
                    // delta for every counter `V != u64` (e.g. `i64`) — a PRODUCTION data-loss bug:
                    // this arm is `drain_segments_into_overlay`'s sink (reached by the normal
                    // Overlay-regime reopen via `reconcile_and_drain_overlay`, the F7 converter, and
                    // the L1 recovery redirect), and the dropped op still returned `true`, so the
                    // recovery watermark seed over-claimed it as durably covered ⇒ the next
                    // `checkpoint()` made the reopen drain-skip the archived delta = PERMANENT loss.
                    // Recovery/drain replay is single-threaded, so the RMW is race-free (the same
                    // single-thread reopen invariant the owned applier relied on). Failure semantics
                    // mirror the owned applier EXACTLY (non-counter current value or out-of-range
                    // total → `false` = stop replay at the durable prefix). For "" there is no
                    // counter increment path → drop (owned-applier parity, red-team-confirmed: no
                    // durable "" delta is ever logged).
                    None => {
                        if units.is_empty() {
                            // No durable "" delta is ever logged; nothing to accumulate.
                            return true;
                        }
                        use crate::persistent_artrie_core::counter_codec;
                        let current_i128 = match self.overlay_value_get(units) {
                            Some(value) => {
                                match counter_codec::counter_value_to_i128::<V>(&value) {
                                    Some(n) => n,
                                    None => {
                                        log::warn!("F5 overlay replay: increment-delta current value is not a counter leaf for this V; stopping at durable prefix");
                                        return false;
                                    }
                                }
                            }
                            None => 0,
                        };
                        let final_i128 = current_i128 + delta as i128;
                        match counter_codec::i128_to_counter_value::<V>(final_i128) {
                            Some(value) => {
                                self.overlay_publish_value(units, value);
                                true
                            }
                            None => {
                                log::warn!("F5 overlay replay: increment-delta total is out of range for this V; stopping at durable prefix");
                                false
                            }
                        }
                    }
                }
            }
        }
    }

    /// `V`-generic membership publish: non-empty → `overlay_publish_membership`,
    /// empty "" → `overlay_publish_root_membership` (the fresh-root-CAS root
    /// publisher). The empty-term split the per-unit publishers cannot express.
    fn overlay_publish_membership_any(&self, units: &[K::Unit]) {
        if units.is_empty() {
            if let Err(e) = self.overlay_publish_root_membership() {
                log::warn!("F5 overlay replay: root membership publish failed: {:?}", e);
            }
        } else {
            self.overlay_publish_membership(units);
        }
    }

    /// `V`-generic value publish: non-empty → `overlay_publish_value`, empty "" →
    /// `overlay_publish_root_value` (the fresh-root-CAS root value publisher — the
    /// §2.2/G5-NEW-4 data-loss fix; NOT an in-place root mutation).
    fn overlay_publish_value_any(&self, units: &[K::Unit], value: V) {
        if units.is_empty() {
            if let Err(e) = self.overlay_publish_root_value(value) {
                log::warn!("F5 overlay replay: root value publish failed: {:?}", e);
            }
        } else {
            self.overlay_publish_value(units, value);
        }
    }

    // ========================================================================
    // F7 — crash-safe Owned→Overlay conversion-on-reopen + the shared
    // archive-aware FIX-B drain. See `docs/design/f7-owned-to-overlay-rotation.md`
    // (v4 / Round-5 CONVERGED).
    // ========================================================================

    /// **F7 seam — install the dense image as the live overlay** (the variant's
    /// `load_root_immutable`). The byte variant calls its 1-arg
    /// `load_root_immutable(root_ptr)`; the char variant calls its 2-arg
    /// `load_root_immutable(buffer_manager, root_ptr)` (it owns the concrete
    /// `BufferManager<S>` field). PRECONDITION: the WAL is already Overlay-regime (the
    /// V-2 `install_prebuilt_overlay_root` check fails otherwise), so the converter
    /// rotate-and-stamps BEFORE calling this.
    ///
    /// Returns `image_loaded`: `true` iff a VALID dense image was loaded; `false` iff the
    /// image was absent (`root_ptr == 0`) or corrupt (fell back to an empty image). The
    /// converter uses this to thread `loaded_from_disk = false` + `image_checkpoint_lsn = 0`
    /// into the drain when the image is absent/corrupt — otherwise the drain would SKIP WAL
    /// records `<= the active Checkpoint record's lsn` that the (absent) image does NOT
    /// actually cover, dropping committed data (the corrupt-descriptor fallback parity).
    fn load_root_immutable_seam(&mut self, root_ptr: u64) -> Result<bool>;

    /// **F7 (FIX B + FIX E) — drain a set of WAL segments INTO THE OVERLAY**, applying
    /// each per-segment regime, with the RES-3 prefix-gap guard.
    ///
    /// `segments` are LSN-ordered (`collect_wal_segments`). Each segment header carries
    /// its own regime: a converted Owned tail → `Owned` (KEEP unranked, orphan-KEEP); an
    /// Overlay tail archived under load → `Overlay` (DROP unranked orphans). Records are
    /// reconciled through [`reconcile_lww_with_regime`] with the per-LSN regime closure
    /// and the REAL `(loaded_from_disk, image_checkpoint_lsn)` (OBL-2 — the dense-image
    /// redo frontier, NOT the active-WAL Checkpoint record which is 0 post-rotate), then
    /// applied via [`Self::apply_recovered_operation_overlay`].
    ///
    /// # FIX E (RES-3 fail-loud)
    ///
    /// If a committed prefix is missing — the min surviving record LSN leaves a gap above
    /// `image_checkpoint_lsn` (`min_surviving_lsn > image_checkpoint_lsn + 1`) — return a
    /// corruption error rather than silently rebuild an incomplete trie. The image covers
    /// `1..=image_checkpoint_lsn`; the segments must cover the contiguous tail from
    /// `image_checkpoint_lsn + 1`. A raised minimum (a pruned un-subsumed prefix) is the
    /// data-loss the guard catches.
    ///
    /// `&self` — the overlay is mutated only through the lock-free publishers.
    fn drain_segments_into_overlay(
        &self,
        segments: &[std::path::PathBuf],
        loaded_from_disk: bool,
        image_checkpoint_lsn: crate::persistent_artrie_core::wal::Lsn,
    ) -> Result<usize> {
        use crate::persistent_artrie_core::recovery::RecoveryManager;
        use crate::persistent_artrie_core::wal::{Lsn, RankRegime, WalReader, WalRecord};
        use std::collections::{HashMap, HashSet};

        let mut all_records: Vec<(Lsn, WalRecord)> = Vec::new();
        let mut regime_by_lsn: HashMap<Lsn, RankRegime> = HashMap::new();
        // The lowest PHYSICALLY-present record LSN across all segments (BEFORE tx-filtering).
        // The RES-3 prefix-gap guard uses THIS, not the tx-surviving min: a record dropped
        // by Owned tx-resolution (an incomplete/aborted tx) is intentionally discarded, NOT
        // a pruned-prefix gap, so it must not trip the guard.
        let mut physical_min_lsn: Option<Lsn> = None;

        for segment_path in segments {
            // Per-segment regime from the WAL header; an unreadable header defaults to
            // Owned (the SAFE direction — keep, never drop).
            let seg_regime = WalReader::read_header(segment_path)
                .map(|h| h.regime())
                .unwrap_or(RankRegime::Owned);

            // **TRANSACTION FILTERING (Owned segments only).** An OWNED-regime segment may
            // carry document-transaction records (`BeginTx`/`CommitTx`/`AbortTx`); records
            // inside an INCOMPLETE or ABORTED transaction must be DROPPED, exactly as the
            // legacy owned reopen does (`replay_records_lww`'s Owned arm uses the
            // tx-FILTERED `RecoveryManager` ops, NOT the tx-unaware raw reconcile — see its
            // doc: "using the (tx-unaware) reconcile would resurrect aborted-tx data
            // records"). So for an Owned segment we resolve transactions via
            // `RecoveryManager` (which reads the segment as a WAL file) and keep ONLY the
            // raw records whose LSN survives tx-resolution. An OVERLAY-regime segment is
            // NEVER transactional (the durable overlay-write path emits no tx records), so
            // it keeps ALL its raw records (the CommitRank-aware `reconcile_lww` handles
            // the rest). `None` ⇒ keep all (the SAFE direction on a resolve error).
            let tx_surviving_lsns: Option<HashSet<Lsn>> = match seg_regime {
                RankRegime::Owned => match RecoveryManager::new(segment_path).recover() {
                    Ok(state) => Some(state.into_operations().iter().map(|op| op.lsn()).collect()),
                    Err(_) => None, // resolve error → keep all (SAFE: never silently drop)
                },
                RankRegime::Overlay => None,
            };

            let reader = match WalReader::new(segment_path) {
                Ok(r) => r,
                Err(_) => continue, // skip an unreadable segment (matches the raw rebuild)
            };
            for result in reader.iter() {
                let (lsn, record) = match result {
                    Ok(r) => r,
                    Err(_) => break, // stop at this segment's durable (CRC-valid) prefix
                };
                // Track the physical prefix minimum (before any tx-filter drop) for RES-3.
                physical_min_lsn = Some(physical_min_lsn.map_or(lsn, |m| m.min(lsn)));
                // Drop a data record whose LSN did NOT survive Owned tx-resolution. Keep
                // CommitRank markers regardless (they carry no membership effect and the
                // reconcile consumes them; an Owned segment has none anyway). Transaction
                // control records (`BeginTx`/`CommitTx`/`AbortTx`) are replay no-ops in
                // `recovered_operations_from_record`, so keeping or dropping them is inert —
                // we keep them for fidelity.
                if let Some(ref surviving) = tx_surviving_lsns {
                    let is_data = !matches!(
                        record,
                        WalRecord::BeginTx { .. }
                            | WalRecord::CommitTx { .. }
                            | WalRecord::AbortTx { .. }
                            | WalRecord::Checkpoint { .. }
                            | WalRecord::CommitRank { .. }
                            | WalRecord::VersionUpdate { .. }
                            | WalRecord::VersionDurable { .. }
                            | WalRecord::VersionGc { .. }
                    );
                    if is_data && !surviving.contains(&lsn) {
                        continue; // a data record inside an incomplete/aborted tx → DROP
                    }
                }
                regime_by_lsn.insert(lsn, seg_regime);
                all_records.push((lsn, record));
            }
        }

        // FIX E (RES-3): refuse a SILENT incomplete rebuild when a committed prefix is
        // missing. The image covers `[1, frontier]`; the surviving segments must continue
        // contiguously from `frontier + 1`. A pruned un-subsumed prefix raises the lowest
        // surviving LSN above `frontier + 1` → fail LOUD (a corruption error) instead of
        // dropping the prefix.
        //
        // **The frontier source (OBL-2 caveat).** The ideal frontier is the LOADED IMAGE's
        // redo frontier, but the on-disk descriptor does not record it; the only durable
        // source is the WAL `Checkpoint` record, which an OWNED checkpoint TRUNCATES away
        // (so `image_checkpoint_lsn == 0` for a converted file even though the image covers a
        // non-empty prefix). To avoid a FALSE-positive on a legitimately-checkpointed image
        // whose WAL records start above 1, we apply the loud guard ONLY to the NO-IMAGE
        // archive-rebuild case (`loaded_from_disk == false`), where the segments MUST cover
        // from LSN 1 (the original `rebuild_from_wal_segments_regime_aware` RES-3 rule, which
        // is reliable — no image means nothing covers `[1, min_lsn)`). When an image IS
        // present, the WAL-lifecycle invariant (records live strictly ABOVE the checkpoint
        // frontier after `set_min_lsn(frontier + 1)`) means the image covers `[1, min_lsn - 1]`,
        // so a high `min_lsn` is NORMAL, not a gap; and FIX-D's prune exemption already
        // prevents an un-subsumed segment from being pruned in the first place (the primary
        // defense — this loud guard is the belt-and-suspenders for the no-image rebuild).
        if let Some(min_lsn) = physical_min_lsn {
            let guard_applies = !loaded_from_disk;
            let frontier = if loaded_from_disk {
                image_checkpoint_lsn.max(min_lsn.saturating_sub(1))
            } else {
                0
            };
            if guard_applies && min_lsn > frontier.saturating_add(1) {
                return Err(
                    crate::persistent_artrie_core::error::PersistentARTrieError::corrupted(
                        format!(
                        "F7 archive drain has a prefix gap: lowest surviving WAL LSN is {min_lsn} \
                         (> {} ) with NO dense image to cover the prefix — the {}..{min_lsn} prefix \
                         was pruned, so the segments cannot fully reconstruct the trie. Refusing a \
                         silent incomplete rebuild (RES-3 / FIX-E).",
                        frontier.saturating_add(1),
                        frontier.saturating_add(1)
                    ),
                    ),
                );
            }
        }

        // FIX B: ONE global (generation, lsn) reconcile over all segments with the
        // per-segment regime, threading the REAL (loaded_from_disk, image_checkpoint_lsn)
        // (OBL-2). LSNs are globally monotone across rotate, so the single sort linearizes
        // same-term ops by commit generation regardless of which segment each came from.
        let winners = crate::persistent_artrie_core::recovery::reconcile_lww_with_regime(
            all_records,
            loaded_from_disk,
            image_checkpoint_lsn,
            |lsn| {
                regime_by_lsn
                    .get(&lsn)
                    .copied()
                    .unwrap_or(RankRegime::Owned)
            },
        );
        let mut applied = 0usize;
        for op in winners {
            if self.apply_recovered_operation_overlay(op) {
                applied += 1;
            }
        }
        Ok(applied)
    }

    /// **F7 (FIX B/FIX C orchestrator) — collect all WAL segments (archive + active) and
    /// drain them INTO THE OVERLAY** via [`Self::drain_segments_into_overlay`]. The single
    /// shared archive-aware reconcile for BOTH the Overlay-regime reopen arm (it now drains
    /// the archive, not just the active) AND the converter's post-stamp drain (FIX B).
    ///
    /// `image_checkpoint_lsn` MUST be the LOADED IMAGE/DESCRIPTOR redo frontier (OBL-2),
    /// NOT the active-WAL Checkpoint record (0 post-rotate). The watermark-base seed (FIX
    /// C) is the caller's responsibility (it constructs the trie with
    /// `CommittedWatermark::new(max_lsn_in_segments(...))`).
    ///
    /// Requires `Self: WalManaged` for `wal_collect_segments`.
    fn reconcile_and_drain_overlay(
        &self,
        config: &crate::persistent_artrie_core::wal::WalConfig,
        loaded_from_disk: bool,
        image_checkpoint_lsn: crate::persistent_artrie_core::wal::Lsn,
    ) -> Result<usize>
    where
        Self: crate::persistent_artrie_core::wal_managed::WalManaged,
    {
        let segments = self.wal_collect_segments(config)?;
        self.drain_segments_into_overlay(&segments, loaded_from_disk, image_checkpoint_lsn)
    }

    /// **F7 — crash-safe Owned→Overlay conversion-on-reopen (the converter).** Replaces
    /// the legacy stay-Owned reopen arm: an Owned-regime eligible file (compaction image,
    /// kill-switched, legacy) reopens INTO the overlay. Called by the reopen ctors when
    /// `rank == Owned`.
    ///
    /// Sequence (v4 / Round-5):
    /// - if [`WalManaged::wal_records_empty_on_disk`] (header-only — incl. a post-crash
    ///   high-`next_lsn` active AND a never-written kill-switched/created Owned file):
    ///   **CHEAP path** — stamp Overlay IN PLACE (no rotate; the records-empty gate) +
    ///   fsync, then F5 `load_root_immutable_seam(root_ptr)`, then drain ANY EXISTING
    ///   archive (a prior crash's tail) via FIX B. NO new rotate ⇒ NO empty segment minted
    ///   ⇒ the crash-loop never accumulates segments (FIX D).
    /// - else (records-NON-empty active — a genuine first conversion with un-archived
    ///   writes): **ROTATE** the tail to archive + re-stamp Overlay + re-assert floor +
    ///   fsync (OBL-1) via [`WalManaged::wal_rotate_and_restamp_overlay`], then F5
    ///   `load_root_immutable_seam(root_ptr)`, then drain the archived tail (+ any prior
    ///   archive) via FIX B. At most ONE rotate per conversion; a crash after this lands on
    ///   the CHEAP path next reopen.
    ///
    /// `image_checkpoint_lsn` is the LOADED IMAGE redo frontier (OBL-2). `was_loaded_from_disk`
    /// is `root_ptr != 0` (a dense image is present).
    ///
    /// A `?` at any step aborts `open` with the durable state intact (the cheap stamp is
    /// idempotent; the rotate is an additive archive rename; `load_root_immutable_seam`
    /// leaves the owned scratch intact on `Err`).
    ///
    /// Requires `Self: WalManaged`.
    fn convert_owned_to_overlay_on_reopen(
        &mut self,
        root_ptr: u64,
        was_loaded_from_disk: bool,
        image_checkpoint_lsn: crate::persistent_artrie_core::wal::Lsn,
        config: &crate::persistent_artrie_core::wal::WalConfig,
    ) -> Result<()>
    where
        Self: crate::persistent_artrie_core::wal_managed::WalManaged,
    {
        use crate::persistent_artrie_core::error::PersistentARTrieError;
        use f7_failpoint::FailPoint;

        if !Self::overlay_eligible_v() {
            // Defensive: the ctor gate (overlay_eligible_v) should already exclude this.
            // An ineligible V cannot route the overlay, so there is nothing to convert —
            // the file legitimately stays Owned. Return Ok (no-op).
            return Ok(());
        }

        // F7 crash-injection (test-only; disarmed = strict no-op): a simulated power-cut
        // BEFORE any durable conversion side effect. Reopen reads Owned → re-runs the
        // converter; converges.
        if f7_failpoint::armed() == FailPoint::BeforeRotate {
            return Err(PersistentARTrieError::corrupted(
                "F7 fail-point BeforeRotate (simulated crash before any conversion side effect)",
            ));
        }

        if self.wal_records_empty_on_disk() {
            // ===== CHEAP path (records-empty active — incl. post-crash high-next_lsn,
            // never-written kill-switched/created Owned) — NO rotate (FIX D). =====
            // Stamp Overlay in place gated on RECORDS-EMPTY-ON-DISK (FIX A widening) + fsync.
            // We must NOT use the `next_lsn==1`-gated `wal_stamp_overlay_regime` here: a
            // post-crash-after-rotate (or post-owned-checkpoint `set_min_lsn`) active is
            // records-empty BUT carries a HIGH `next_lsn`, which that gate would wrongly
            // reject — leaving the WAL Owned and resurrecting orphans on a later reopen.
            self.wal_stamp_overlay_regime_records_empty()?;
            // Verify the WAL is now Overlay-regime; if not, refuse (an Owned WAL under
            // overlay routing would resurrect orphans).
            if !self.wal_is_overlay_regime() {
                return Err(PersistentARTrieError::internal(
                    "F7 convert (cheap): Overlay regime stamp did not take on a records-empty active",
                ));
            }
        } else {
            // ===== ROTATE path (records-non-empty active — first conversion). =====
            // S1+S2: archive the Owned tail + re-stamp Overlay + re-assert floor + fsync
            // (OBL-1). The fresh active carries the high next_lsn (DG0 monotonicity). The
            // `AfterRotateBeforeStamp` fail point is consulted INSIDE
            // `WalWriter::rotate_and_restamp_overlay` (between the durable rename and the
            // stamp) so the torn window is exactly the v4 FIX-D scenario.
            let _archived = self.wal_rotate_and_restamp_overlay(config)?;
            if !self.wal_is_overlay_regime() {
                return Err(PersistentARTrieError::internal(
                    "F7 convert (rotate): Overlay regime stamp did not take after rotate",
                ));
            }
        }

        // F7 crash-injection: a simulated power-cut AFTER the Overlay stamp+fsync (the S2
        // durable commit point) but BEFORE the overlay is built. Reopen now reads Overlay
        // (durable) → the F5 arm drains the archive (OBLIGATION-A); converges.
        if f7_failpoint::armed() == FailPoint::AfterStampBeforeBuild {
            return Err(PersistentARTrieError::corrupted(
                "F7 fail-point AfterStampBeforeBuild (simulated crash after the Overlay stamp+fsync)",
            ));
        }

        // S3: build the overlay DIRECTLY from the dense image (the WAL is now durably
        // Overlay-regime, so install_prebuilt_overlay_root's V-2 check passes). Clears the
        // transient owned scratch. A `?` aborts with the durable image intact. `image_loaded`
        // is `false` if the image was absent/corrupt (fell back to an empty overlay) — the
        // drain below then uses `loaded_from_disk = false` + frontier 0 so it does NOT skip
        // WAL records the absent image fails to cover (corrupt-descriptor fallback parity).
        let image_loaded = self.load_root_immutable_seam(root_ptr)?;

        // F7 crash-injection: a simulated power-cut DURING the drain (overlay built +
        // installed in memory, but the in-memory overlay is discarded by the abort — the
        // durable WAL/image are untouched). Reopen reads Overlay → the F5 arm rebuilds +
        // drains; converges. (S3/S4 use only NO-WAL publishers, so nothing was logged.)
        if f7_failpoint::armed() == FailPoint::DuringDrain {
            return Err(PersistentARTrieError::corrupted(
                "F7 fail-point DuringDrain (simulated crash after build, before/during drain)",
            ));
        }

        // S4 / FIX B: drain ALL segments (the just-archived Owned tail under the ROTATE
        // path, OR any pre-existing archive under the CHEAP path, PLUS the now-Overlay
        // active which is records-empty so contributes nothing) into the overlay, with the
        // per-segment regime (converted Owned tail → KEEP) and the REAL
        // (loaded_from_disk, image_checkpoint_lsn) (OBL-2). The `loaded_from_disk` /
        // frontier reflect the ACTUAL image: a valid image subsumes records `<= frontier`;
        // an absent/corrupt image (image_loaded == false) subsumes nothing (frontier 0), so
        // every WAL record is replayed (corrupt-descriptor fallback parity). RES-3 fail-loud
        // on a real prefix gap.
        let effective_loaded = was_loaded_from_disk && image_loaded;
        let effective_ckpt = if effective_loaded {
            image_checkpoint_lsn
        } else {
            0
        };
        let _applied =
            self.reconcile_and_drain_overlay(config, effective_loaded, effective_ckpt)?;
        Ok(())
    }

    // **F7 — `replay_records_lww_overlay` (the ACTIVE-only WAL-tail-into-overlay replay)
    // DELETED.** It is fully superseded by the archive-aware [`Self::drain_segments_into_overlay`]
    // / [`Self::reconcile_and_drain_overlay`]: the drain treats the ACTIVE file as one
    // segment among the archived segments (`collect_wal_segments` includes both), applies
    // each segment's own header regime (the active is Overlay here, so the unranked-orphan
    // DROP + checkpoint-subsumed skip are inherited exactly as before), and additionally
    // recovers an archived Owned/Overlay tail (OBLIGATION-A / FIX B). All four reopen arms
    // route through `reconcile_and_drain_overlay` now.
}
