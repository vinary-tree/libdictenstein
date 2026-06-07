//! `LockFreeOverlay<K, V, S>` — the SHARED GENERIC lock-free-overlay flip
//! (route + read-engine + flip/kill-switch + reestablish), extracted
//! token-for-token from the char variant so byte can reuse it rather than
//! copy-paste (`docs/design/overlay-flip-genericization.md` §2).
//!
//! # What this trait is
//!
//! A **seam trait with default-provided generic methods + variant-supplied seam
//! methods** (design §2). Each persistent ARTrie variant writes ONE thin `impl`
//! supplying only the seam (the owned-tree readers, the overlay publishers, the
//! WAL/field accessors, the per-variant counter monomorph); the data-loss-
//! critical generic logic — the route predicate, the overlay-read DFS walks in
//! `K::Unit` space, `flip_to_overlay`/`kill_switch_to_owned` (incl. the
//! WAL-regime restamp), and the `reestablish` streaming-fold (the clear-owned-
//! LAST control flow) — lives here ONCE as default methods.
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
//! The `owned_*` seam methods MUST read the OWNED tree directly. The reestablish
//! folds (`reestablish_overlay_membership`/`_counter`) run while `route_overlay()`
//! is ALREADY TRUE (the ctor flips before dispatching reestablish), so routing an
//! owned read through the public `iter_prefix`/`get`/`contains`/`get_value` would
//! read the EMPTY overlay, publish nothing, then clear the owned tree LAST =
//! TOTAL IRREVERSIBLE LOSS. Each `owned_*` seam carries a `# Safety (data-loss)`
//! contract; a CI grep gate (contract §6(a)) FAILS if any `owned_*` seam body
//! references `route_overlay`/`iter_prefix(`/`self.get(`/`get_value(`/`contains(`.
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
use crate::persistent_artrie_core::overlay::node::OverlayNode;
use crate::persistent_artrie_core::overlay::write_mode::OverlayWriteMode;
use crate::value::DictionaryValue;
use std::sync::Arc;

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
    Sized + 'static
{
    /// The per-variant counter monomorph (`u64` for char, `i64` for byte). THE
    /// divergence that makes the value-route a seam, not a blanket. `Copy` so the
    /// publisher/getter seams can pass it by value. `Serialize + DeserializeOwned` so
    /// the F5 WAL-tail applier can re-encode a recovered absolute-`i64` counter as the
    /// typed `CounterValue` via bincode (`counter_value_from_i64`) — both `u64` and
    /// `i64` satisfy it.
    type CounterValue: 'static + Copy + serde::Serialize + serde::de::DeserializeOwned;

    // ========================================================================
    // REQUIRED SEAM (variant provides) — small accessors + the un-routed owned
    // readers + the overlay publishers.
    // ========================================================================

    /// The lock-free overlay's atomic root pointer, or `None` if the overlay is
    /// not installed (`enable_lockfree()` not yet run for this trie).
    fn lockfree_root(&self)
        -> Option<&crate::persistent_artrie_core::overlay::AtomicNodePtr<K, V>>;

    /// The current kill-switch mode for this trie.
    fn overlay_write_mode(&self) -> OverlayWriteMode;

    /// Set the kill-switch mode (restart-time switch; see `kill_switch_to_owned`).
    ///
    /// **F4:** `&self` — the field is an `AtomicEnumCell`, so the runtime kill-switch
    /// (`kill_switch_to_owned`, Tier-2) and the ctor-time `flip_to_overlay` (Tier-1)
    /// both write it without the outer trie lock.
    fn set_overlay_write_mode(&self, mode: OverlayWriteMode);

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

    /// Stamp the WAL header BACK to the `Owned` regime (best-effort; see above).
    fn wal_stamp_owned_regime(&self);

    /// `true` iff `V` is an eligible overlay monomorph (`{(), CounterValue}`). The
    /// SOLE expression of the "overlay only for `V ∈ {(), counter}`" invariant.
    fn overlay_eligible_v() -> bool;

    // ---- UN-ROUTED owned readers (D1 — MUST read the owned tree directly) ----

    /// The distinct first units of every owned term, plus whether the empty term
    /// is present. The disjoint first-unit partition cover the reestablish folds
    /// stream by (RES-6).
    ///
    /// # Safety (data-loss)
    ///
    /// MUST read the OWNED tree directly, NEVER via `route_overlay()` /
    /// `iter_prefix`/`get`/`contains`/`get_value`. Called by
    /// `reestablish_overlay_*` with `route_overlay()` ALREADY TRUE — a routed read
    /// would see the EMPTY overlay, and the fold then clears owned = total loss.
    fn owned_first_units(&self) -> Result<(Vec<K::Unit>, bool)>;

    /// Every owned term whose first unit is `unit` (i.e. under the single-unit
    /// prefix `[unit]`), as `Vec<K::Unit>` unit-sequences. The reestablish
    /// membership fold's per-partition chunk.
    ///
    /// # Safety (data-loss)
    ///
    /// MUST read the OWNED tree directly (see `owned_first_units`).
    fn owned_units_under(&self, prefix: &[K::Unit]) -> Result<Option<Vec<Vec<K::Unit>>>>;

    /// Every owned `(term, value)` under the single-unit prefix `[unit]`, as
    /// `(Vec<K::Unit>, V)`. The reestablish counter fold's per-partition chunk.
    ///
    /// # Safety (data-loss)
    ///
    /// MUST read the OWNED tree directly (see `owned_first_units`).
    fn owned_units_with_values_under(
        &self,
        prefix: &[K::Unit],
    ) -> Result<Option<Vec<(Vec<K::Unit>, V)>>>;

    /// The owned value of the empty term (`""`), or `None` if absent.
    ///
    /// # Safety (data-loss)
    ///
    /// MUST read the OWNED tree directly (see `owned_first_units`).
    fn owned_has_empty_term_value(&self) -> Option<V>;

    /// Clear the owned tree (set it empty + zero the owned length). The reestablish
    /// folds call this LAST, after every partition has been published to the
    /// overlay (RES-7 — a mid-stream `?` abort leaves the owned tree intact).
    fn clear_owned(&mut self);

    // ---- overlay publishers (the per-variant write seam) ----

    /// Publish membership of `units` to the overlay via the variant's no-WAL CAS
    /// insert (the recovered terms are already durable in the WAL; re-logging
    /// would double-log).
    fn overlay_publish_membership(&self, units: &[K::Unit]);

    /// Publish the counter `value` for `units` to the overlay via the variant's
    /// no-WAL CAS increment.
    fn overlay_publish_counter(&self, units: &[K::Unit], value: Self::CounterValue);

    /// Read the overlay counter at `units` (the variant's `<CounterValue>`-downcast
    /// + lock-free point read), or `None` if absent.
    fn overlay_counter_get(&self, units: &[K::Unit]) -> Option<Self::CounterValue>;

    /// `true` iff `units` is present (final) in the overlay (the variant's
    /// `contains_lockfree`).
    fn overlay_contains(&self, units: &[K::Unit]) -> bool;

    /// **G5/F1 — publish an ARBITRARY-`V` value for `units` to the overlay via the
    /// variant's no-WAL path-copy CAS** (the recovered terms are already durable in
    /// the WAL; re-logging would double-log). The value twin of
    /// [`Self::overlay_publish_counter`], used by the third reestablish fold
    /// [`Self::reestablish_overlay_value`]. SETs the value (last-writer = the CAS
    /// winner); at reestablish the overlay is fresh, so there is no contention.
    fn overlay_publish_value(&self, units: &[K::Unit], value: V);

    /// **G5/F1 — read the overlay leaf's ARBITRARY-`V` value at `units`** (the
    /// variant's non-faulting lock-free point read of the leaf `Option<V>`), or
    /// `None` if absent/non-final. The value twin of [`Self::overlay_counter_get`],
    /// used by the arbitrary-`V` arm of [`Self::overlay_route_get_value`].
    /// NON-FAULTING and exact: overlay finals are never evicted in production
    /// (§2.4 / RT5), so the resident-finals walk reads the durable value.
    fn overlay_value_get(&self, units: &[K::Unit]) -> Option<V>;

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

    /// **Flip F0 — the THIN production-write/read-path router predicate.**
    ///
    /// `true` iff the production path should route to the lock-free overlay for
    /// THIS trie: the kill-switch mode selects the overlay AND the overlay is
    /// actually live (`enable_lockfree()` has run, so `lockfree_root` is `Some`).
    /// Both conjuncts matter: a `LockFreeOverlay` mode with no overlay root (an
    /// arbitrary-`V` monomorph the F5 default flip deliberately does NOT enable)
    /// correctly falls back to the proven owned tree.
    #[inline]
    fn route_overlay(&self) -> bool {
        self.overlay_write_mode().uses_overlay() && self.lockfree_root().is_some()
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
            let child_arc = child.as_in_mem()?;
            node = Arc::clone(child_arc);
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
                Self::overlay_collect_finals(&node, prefix.to_vec(), &mut terms);
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
        node: &Arc<OverlayNode<K, V>>,
        prefix: Vec<K::Unit>,
        out: &mut Vec<Vec<K::Unit>>,
    ) {
        if node.is_final() {
            out.push(prefix.clone());
        }
        for (key, child) in node.iter_children() {
            if let Some(child_arc) = child.as_in_mem() {
                let mut child_prefix = prefix.clone();
                child_prefix.push(*key);
                Self::overlay_collect_finals(child_arc, child_prefix, out);
            }
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
                Self::overlay_collect_with_values(&node, prefix.to_vec(), &mut entries);
                Some(entries)
            }
        }
    }

    fn overlay_collect_with_values(
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
            if let Some(child_arc) = child.as_in_mem() {
                let mut child_prefix = prefix.clone();
                child_prefix.push(*key);
                Self::overlay_collect_with_values(child_arc, child_prefix, out);
            }
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
        self.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
        // Re-engaging the overlay after a `kill_switch_to_owned` (which stamped
        // Owned on a fresh WAL) must restamp Overlay — `enable_lockfree` only
        // stamps on its FIRST call (it early-returns once `lockfree_root` is set),
        // so a second engage would otherwise leave the WAL Owned-regime and fail
        // the V-2 stamp check below. Gated on a fresh WAL (`current_lsn() == 1`); a
        // no-op for the ctor flip (where `enable_lockfree` already stamped Overlay)
        // and for non-empty WALs.
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

    /// **Kill-switch — the public one-release fallback for the flip.** Revert the
    /// production write path from the lock-free overlay back to the proven owned
    /// tree: after this returns, `route_overlay()` is false, so writes/reads/
    /// checkpoint take the owned arm (the pre-flip behavior).
    ///
    /// In-session it takes effect immediately. Across a reopen it is RESTART-TIME:
    /// on a still-fresh WAL (`current_lsn() == 1`, e.g. immediately after
    /// `create()`) it ALSO restamps the durable regime to Owned so a later reopen
    /// STAYS owned (no re-flip) and owned-mode records survive recovery; on a
    /// NON-empty WAL this is intentionally a no-op (the durable regime is already
    /// fixed, so a reopen rebuilds the owned tree from the Overlay-regime WAL and
    /// re-flips). Mirrors `enable_lockfree`'s `current_lsn() == 1` empty-WAL stamp
    /// guard.
    fn kill_switch_to_owned(&self) {
        // **F4:** `&self` (Tier-2 runtime fallback). All callees are `&self`: the
        // `AtomicEnumCell` store + the WAL stamp helpers. No outer trie lock.
        self.set_overlay_write_mode(OverlayWriteMode::OwnedTree);
        if self.wal_current_lsn() == Some(1) {
            self.wal_stamp_owned_regime();
        }
    }

    // ===== reestablish (the data-loss-critical clear-owned-LAST folds) =====

    /// **Membership (`V = ()`) overlay reestablish.** Re-insert each recovered
    /// owned term into the overlay via the no-WAL publisher, streaming by first
    /// unit; clear the owned tree LAST (RES-7). The MEMBERSHIP twin of
    /// [`Self::reestablish_overlay_counter`].
    ///
    /// # D1 (CRITICAL)
    ///
    /// Reads the recovered OWNED tree via the UN-routed `owned_*` seam readers.
    /// This runs with `route_overlay()` ALREADY TRUE (the ctor flips before
    /// dispatching reestablish), so a routed read would see the EMPTY overlay —
    /// we'd copy nothing, then clear owned below = total irreversible loss. The
    /// owned readers bypass the route.
    fn reestablish_overlay_membership(&mut self) -> Result<()> {
        // Disjoint first-unit partition cover of the recovered owned terms.
        let (first_units, has_empty_term) = self.owned_first_units()?;
        // Empty-string support (H3): the empty term "" has no first unit, so the
        // per-unit chunks below never surface it — republish its membership to the
        // overlay ROOT directly (fresh-root-CAS) BEFORE clear_owned, else
        // `contains("")` is lost on EVERY reopen (the load path rebuilt the owned
        // tree with the empty-term finality, but `clear_owned` below wipes it and
        // the overlay — not owned — serves reads under `route_overlay()`).
        if has_empty_term {
            self.overlay_publish_root_membership()?;
        }
        for unit in first_units {
            let prefix = [unit];
            if let Some(chunk) = self.owned_units_under(&prefix)? {
                for units in &chunk {
                    self.overlay_publish_membership(units);
                }
            }
        }
        // Clear the owned tree LAST (RES-7: a mid-stream `?` abort leaves it intact).
        self.clear_owned();
        Ok(())
    }

    /// **Counter overlay reestablish.** Rebuild the immutable overlay from the
    /// recovered OWNED tree's `(term, value)` pairs, streaming by first unit so the
    /// heavy per-partition materialization is bounded to one first-unit at a time
    /// (RA-2). FALLIBLE: any owned-read error ABORTS (propagates `Err`) with the
    /// owned tree INTACT — the owned tree is cleared ONLY as the LAST step (RES-7).
    /// Re-inserts via the no-WAL counter publisher.
    ///
    /// # D1 (CRITICAL)
    ///
    /// See [`Self::reestablish_overlay_membership`] — reads the recovered owned
    /// tree via the UN-routed `owned_*` seam readers.
    fn reestablish_overlay_counter(&mut self) -> Result<()> {
        let (first_units, has_empty_term) = self.owned_first_units()?;
        // Empty-term partition first (it has no first unit — RES-6).
        // Empty-string support (H3): publish "" to the overlay ROOT via the
        // fresh-root-CAS value publisher (NOT `overlay_publish_counter`, which
        // routes through the guarded `increment_cas` and no-ops on ""). SET the
        // recovered value directly. Runs BEFORE clear_owned (RES-7).
        if has_empty_term {
            if let Some(v) = self.owned_has_empty_term_value() {
                self.overlay_publish_root_value(v)?;
            }
        }
        // One first-unit partition at a time: stream its (term, value) pairs,
        // publish each via the no-WAL overlay path, drop the chunk before the next
        // unit.
        for unit in first_units {
            let prefix = [unit];
            if let Some(chunk) = self.owned_units_with_values_under(&prefix)? {
                for (units, value) in &chunk {
                    if let Some(cv) = Self::value_as_counter(value) {
                        self.overlay_publish_counter(units, cv);
                    }
                }
            }
        }
        // Clear the owned tree LAST — only after every partition published. A mid-
        // stream `?` abort above returns Err with the owned tree untouched (RES-7).
        self.clear_owned();
        Ok(())
    }

    /// **G5/F1 — the THIRD reestablish fold: ARBITRARY `V` value.** The value twin
    /// of [`Self::reestablish_overlay_counter`]: rebuild the immutable overlay from
    /// the recovered OWNED tree's `(term, V)` pairs, streaming by first unit (RA-2).
    /// FALLIBLE: any owned-read error ABORTS (propagates `Err`) with the owned tree
    /// INTACT — cleared ONLY as the LAST step (RES-7). Re-inserts via the no-WAL
    /// value publisher [`Self::overlay_publish_value`].
    ///
    /// # D1 (CRITICAL)
    ///
    /// Reads the recovered owned tree via the UN-routed `owned_*` seam readers — the
    /// SAME `owned_first_units` / `owned_has_empty_term_value` /
    /// `owned_units_with_values_under` the counter fold uses. It adds NO new `owned_*`
    /// seam, so the D1 grep gate (`flip.rs` head) is inherited, NOT re-derived.
    fn reestablish_overlay_value(&mut self) -> Result<()> {
        let (first_units, has_empty_term) = self.owned_first_units()?;
        // Empty term "" → the overlay ROOT (NO WAL at reestablish — already durable;
        // UNRANKED publish is correct, no LSN to rank). A VALUED "" publishes the value;
        // a TERM-ONLY "" (membership, no value) publishes root membership. Runs BEFORE
        // clear_owned (RES-7).
        if has_empty_term {
            if let Some(v) = self.owned_has_empty_term_value() {
                self.overlay_publish_root_value(v)?;
            } else {
                self.overlay_publish_root_membership()?;
            }
        }
        // One first-unit partition at a time. flag-2 fix: an arbitrary-`V` trie may hold
        // TERM-ONLY members (`insert()` with no value) MIXED with valued terms; the value
        // stream below carries only valued terms, so term-only members were DROPPED on
        // reopen. Republish MEMBERSHIP for every recovered final first (carries the
        // term-only ones), THEN set the value on each valued final (`overlay_publish_value`
        // re-finalizes + carries the value, idempotent on the membership just published).
        for unit in first_units {
            let prefix = [unit];
            // (1) Membership for EVERY final under this unit (incl. term-only).
            if let Some(chunk) = self.owned_units_under(&prefix)? {
                for units in &chunk {
                    self.overlay_publish_membership(units);
                }
            }
            // (2) Value for each valued final (set last so it is never wiped).
            if let Some(chunk) = self.owned_units_with_values_under(&prefix)? {
                for (units, value) in &chunk {
                    self.overlay_publish_value(units, value.clone());
                }
            }
        }
        // Clear the owned tree LAST — a mid-stream `?` abort returns Err with the
        // owned tree untouched (RES-7).
        self.clear_owned();
        Ok(())
    }

    /// Re-wrap a `&V` as `CounterValue` via a SAFE `Any` downcast iff `V ==
    /// CounterValue`, else `None`. The reestablish counter fold uses this to feed
    /// the typed publisher seam from the generic `V`-valued owned chunk.
    fn value_as_counter(value: &V) -> Option<Self::CounterValue> {
        (value as &dyn Any)
            .downcast_ref::<Self::CounterValue>()
            .copied()
    }

    /// Re-wrap a `CounterValue` as `V` via a SAFE `Any` downcast iff `V ==
    /// CounterValue`, else `None`. The inverse of [`Self::value_as_counter`];
    /// re-wraps an overlay counter read into the public `V` for the value-route.
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
    /// (`load_root_immutable` + `replay_records_lww_overlay`): reopen builds the lock-
    /// free overlay DIRECTLY from the dense image + replays the WAL tail INTO the
    /// overlay, never materializing the owned tree into `self.root` (the F7
    /// prerequisite). When `false` (the S1/S2 dormant state), reopen uses the LEGACY
    /// path (owned `load_root_from_disk` + owned `replay_records_lww` + `flip` +
    /// `reestablish_overlay_dispatch`). **REVERSIBLE** — flip back to `false` to
    /// restore the proven legacy path with zero other changes. The
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
        self.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
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

    /// **F5 — build the overlay root from the (already eager-loaded) OWNED tree.**
    /// The COMPRESSION-AWARE, generic dense→overlay walk-converter: it enumerates every
    /// owned `(term-units, Option<V>)` + the empty term "" via the SAME D1 un-routed owned
    /// seam readers (`owned_first_units` / `owned_units_under` /
    /// `owned_units_with_values_under` / `owned_has_empty_term_value`) the reestablish
    /// folds use — those readers EXPAND `StringBucket` suffixes + compressed ART-node
    /// prefixes, so this handles BOTH an un-path-compressed Overlay image AND a
    /// path-compressed COMPACTED Overlay image (C-opt-1) without re-deriving the
    /// expansion. The enumeration is fed to the iterative, deep-term-safe
    /// [`build_overlay_root_from_terms`](crate::persistent_artrie_core::overlay::f5_build::build_overlay_root_from_terms).
    ///
    /// # D1 (CRITICAL)
    ///
    /// Reads the OWNED tree via the UN-routed `owned_*` seams (the SAME ones
    /// `reestablish_overlay_*` use). The F5 caller (`load_root_immutable`) calls this
    /// BEFORE installing the overlay (so `route_overlay()` is still false here — but the
    /// owned readers bypass the route regardless, so it is safe either way). It does NOT
    /// clear the owned tree (the caller decides; F5 leaves owned intact, F7 deletes it).
    ///
    /// # Membership + value (mixed)
    ///
    /// An owned trie may hold TERM-ONLY members MIXED with valued terms (e.g. a counter
    /// trie with a bare `insert(t)`, or any `()`/arbitrary-`V` trie). The membership
    /// stream (`owned_units_under`) carries EVERY final (incl. term-only); the value
    /// stream (`owned_units_with_values_under`) carries only valued finals. We union them
    /// so a term-only member is kept as membership and a valued term carries its value —
    /// the SAME flag-2 fix `reestablish_overlay_value` applies. (This is why F5 keeps a
    /// term-only counter member that the legacy `reestablish_overlay_counter` dropped —
    /// strictly more correct, no data loss.)
    fn build_overlay_root_from_owned(&self) -> Result<Arc<OverlayNode<K, V>>> {
        use crate::persistent_artrie_core::overlay::f5_build::build_overlay_root_from_terms;
        use std::collections::BTreeMap;

        let (first_units, has_empty_term) = self.owned_first_units()?;

        // Collect (units → Option<V>) for every non-empty owned final, streaming by first
        // unit (RA-2 — bound the per-partition materialization). A `BTreeMap` per term so
        // a value (set second) overrides a bare membership entry for the same term.
        let mut terms: BTreeMap<Vec<K::Unit>, Option<V>> = BTreeMap::new();
        for unit in first_units {
            let prefix = [unit];
            // (1) Membership for EVERY final under this unit (incl. term-only).
            if let Some(chunk) = self.owned_units_under(&prefix)? {
                for units in chunk {
                    terms.entry(units).or_insert(None);
                }
            }
            // (2) Value for each valued final (overrides the bare membership above).
            if let Some(chunk) = self.owned_units_with_values_under(&prefix)? {
                for (units, value) in chunk {
                    terms.insert(units, Some(value));
                }
            }
        }

        // The empty term "": Some(Some(v)) valued, Some(None) membership, None absent.
        let empty_term: Option<Option<V>> = if has_empty_term {
            Some(self.owned_has_empty_term_value())
        } else {
            None
        };

        Ok(build_overlay_root_from_terms::<K, V, _>(terms, empty_term))
    }

    /// **F5/F7 (R1) — STRUCTURAL owned→overlay reestablish for the RECOVERY-FAMILY
    /// ctors.** The structural-converter replacement for the per-term-publishing
    /// [`reestablish_overlay_dispatch`](crate::persistent_artrie_char::PersistentARTrieChar::reestablish_overlay_dispatch)
    /// in the create-flip + rebuild-in-memory recovery path (the `RebuildFromWal` /
    /// `recover_from_archives` ctors). It produces the SAME overlay (same term-set +
    /// same values, incl. counters > `i64::MAX` and the empty term "") because it
    /// reads the owned tree via the SAME un-routed `owned_*` seams via
    /// [`Self::build_overlay_root_from_owned`] — the strictly-more-correct membership∪
    /// value union (it keeps a term-only counter member the legacy
    /// `reestablish_overlay_counter` dropped).
    ///
    /// # Precondition (THE recovery-ctor invariant)
    ///
    /// Called from the recovery-family ctors UNDER the existing `route_overlay()`
    /// guard, so the overlay is ALREADY installed (`create`/`create_with_config` →
    /// `apply_create_flip` → `flip_to_overlay` set `lockfree_root = Some(EMPTY)`,
    /// selected `LockFreeOverlay`, and stamped the WAL Overlay regime) and the owned
    /// tree has just been REBUILT IN MEMORY (into `self.root`) from the WAL/archives.
    ///
    /// # The force-replace (and why it is data-loss-safe)
    ///
    /// [`Self::install_prebuilt_overlay_root`] NO-OPs here (its variant seam refuses
    /// to clobber an already-set `lockfree_root`), so this OVERWRITES the empty
    /// create-flip root via [`AtomicNodePtr::store`] — the unconditional, genuinely-
    /// atomic `ArcSwapOption` replace (old `Arc` dropped normally). This is safe
    /// because the overlay is EMPTY at this point: NOTHING has been published since
    /// the create-flip (the recovery ctor wrote ONLY the owned tree, never the
    /// overlay), so replacing the root loses no overlay data. The empty positive/
    /// negative `lockfree_cache` `flip_to_overlay` created stays valid (it holds no
    /// stale entries; the resident-finals walk is authoritative).
    ///
    /// Does NOT touch the WAL regime or the write mode — both were set by the
    /// create-flip and `reestablish_overlay_dispatch` does not touch them either, so
    /// the end state is identical. Clears the owned tree LAST (RES-7), matching the
    /// `reestablish_overlay_*` folds (a mid-stream `?` from `build_*` aborts with the
    /// owned tree INTACT, so the recovery ctor's data is recoverable). After this,
    /// the overlay root == the structural conversion of the rebuilt owned tree.
    fn reestablish_overlay_from_owned(&mut self) -> Result<()> {
        // (1) Build the overlay root structurally from the rebuilt-in-memory owned
        // tree (un-routed `owned_*` seams; a `?` aborts with owned INTACT — RES-7).
        let root = self.build_overlay_root_from_owned()?;
        // (2) FORCE-REPLACE the empty create-flip overlay root (the
        // `install_prebuilt_overlay_root` seam would NO-OP — `lockfree_root` is
        // already `Some`). `store` is the unconditional atomic `ArcSwapOption`
        // replace; safe because the overlay is empty (nothing published since the
        // create-flip), so no overlay data is lost.
        let root_ptr = self.lockfree_root().ok_or_else(|| {
            crate::persistent_artrie_core::error::PersistentARTrieError::internal(
                "reestablish_overlay_from_owned: overlay not installed (route_overlay() guard violated)",
            )
        })?;
        root_ptr.store(root);
        // (3) Clear the owned tree LAST (RES-7) — only after the overlay root is
        // published, so a mid-stream abort above leaves owned intact.
        self.clear_owned();
        Ok(())
    }

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
    /// publishers [`Self::reestablish_overlay_value`] uses. The overlay twin of the
    /// owned `apply_*_recovered_operation_no_wal`: where the owned applier mutates
    /// `self.root`, this publishes into the live lock-free overlay (which already
    /// holds the dense/checkpoint state from `load_root_immutable`). Returns `true`
    /// iff the op was applied (a value-deserialize failure logs + returns `false`,
    /// the SAME best-effort the owned applier has — it does NOT abort the replay).
    ///
    /// # Winners are applied in commit-visibility order
    ///
    /// [`Self::replay_records_lww_overlay`] feeds the winners in `(generation, lsn)`
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
    /// * `Increment{result: Some(v)}` (a single absolute `Increment`) → SET to `v` via
    ///   the counter publisher (`overlay_publish_counter` / the root value publisher).
    /// * `Increment{result: None}` (a `BatchIncrement` DELTA) → ACCUMULATE `delta` onto
    ///   the overlay's current counter via `overlay_publish_counter` (whose seam routes
    ///   through the counter-monomorph `increment_cas`, which ADDS). For "" the counter
    ///   path no-ops (the durable counter path never logs a "" increment), so a ""
    ///   delta is dropped — matching the owned applier's empty-term increment behavior.
    ///   `value_as_counter`/`counter_as_value` bridge the typed `CounterValue`.
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
                    // Absolute (single Increment): SET the counter to `v` (incl. 0).
                    Some(v) => {
                        // The reconcile carries the absolute value in the i64 WAL field.
                        // Re-encode it as the typed `V` (the counter monomorph), then
                        // publish as a value SET so an absolute-set-to-0 is honored
                        // (NOT accumulated).
                        match Self::counter_value_from_i64(v) {
                            Some(cv) => {
                                // For "" the counter publisher no-ops (durable counter
                                // path never logs ""); route "" through the value publisher
                                // so an absolute "" still SETs. counter_as_value re-wraps.
                                if units.is_empty() {
                                    if let Some(vv) = Self::counter_as_value(cv) {
                                        if let Err(e) = self.overlay_publish_root_value(vv) {
                                            log::warn!(
                                                "F5 overlay replay: root counter set failed: {:?}",
                                                e
                                            );
                                            return false;
                                        }
                                    }
                                } else {
                                    self.overlay_publish_counter(units, cv);
                                }
                                true
                            }
                            None => {
                                log::warn!("F5 overlay replay: increment-absolute value not a counter for this V; skipping");
                                false
                            }
                        }
                    }
                    // Delta (BatchIncrement entry): ACCUMULATE `delta` (commutative on
                    // replay). `overlay_publish_counter`'s seam routes through the
                    // counter-monomorph `increment_cas`, which ADDS the delta to the
                    // overlay's current value. A non-positive/overflowing delta is
                    // handled inside the seam's bound (same as the durable path). For ""
                    // there is no counter increment path → drop (owned applier parity).
                    None => {
                        if units.is_empty() {
                            // No durable "" delta is ever logged; nothing to accumulate.
                            return true;
                        }
                        match Self::counter_value_from_i64(delta) {
                            Some(cv) => {
                                self.overlay_publish_counter(units, cv);
                                true
                            }
                            None => {
                                log::warn!("F5 overlay replay: increment-delta not a counter for this V; skipping");
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

    /// Re-encode a recovered counter (carried as an `i64` in the reconcile stream) as
    /// the typed `CounterValue`, routed through the shared `counter_codec` i128
    /// substrate by decoding via the LEAF BYTES
    /// (`counter_leaf_to_i128(&v.to_le_bytes())`) — the SAME bit-pattern-faithful decode
    /// the owned char/byte appliers use (`value_from_recovered_i64` / `value_from_i64`).
    ///
    /// **Why leaf-bytes, NOT `v as i128`:** the absolute-`Increment` caller feeds the
    /// `WalRecord::Increment.result` field, which the write path fills with
    /// `counter_return_i64(new_count)` — the i64 BIT-PATTERN of the count, which is
    /// NEGATIVE for a `u64` count > `i64::MAX`. `v as i128` would keep it negative and
    /// `i128_to_counter_value::<u64>` would reject it (`None`), so the absolute increment
    /// would be DROPPED on Overlay-regime reopen = silent data loss (the counter reverts
    /// to its last checkpoint value). Decoding via the leaf bytes recovers the true
    /// `u64` magnitude (the 8 LE bytes of a negative i64 ARE the 8 LE bytes of the u64
    /// it represents — `bincode legacy`/fixint). For the DELTA caller (`v` a non-negative
    /// i64 chunk) both decodes agree, so the leaf decode is correct for BOTH call sites.
    /// The helper then range-checks into `CounterValue` (`None` for a non-counter `V`),
    /// confining the bincode round-trip to `counter_codec` so the v6 gate holds.
    fn counter_value_from_i64(v: i64) -> Option<Self::CounterValue> {
        use crate::persistent_artrie_core::counter_codec;
        let magnitude =
            counter_codec::counter_leaf_to_i128::<Self::CounterValue>(&v.to_le_bytes())?;
        counter_codec::i128_to_counter_value::<Self::CounterValue>(magnitude)
    }

    /// **F5 (THE data-loss-critical path) — replay the WAL tail INTO THE OVERLAY**
    /// (the overlay twin of the owned `replay_records_lww`). Reconcile the raw
    /// recovered records through the EXISTING [`reconcile_lww`] (the SAME call the
    /// owned replay makes) to get the per-term last-writer winners, then apply each
    /// INTO THE OVERLAY via [`Self::apply_recovered_operation_overlay`].
    ///
    /// `rank_regime` MUST be `Overlay` here (F5 runs only for Overlay-regime files —
    /// the S3 switch gate), so the reconcile's **unranked-orphan DROP is INHERITED**
    /// (a never-acked two-append-window orphan is dropped, resurrecting nothing) and
    /// the checkpoint-subsumed skip (`lsn <= checkpoint_lsn` when `loaded_from_disk`)
    /// is likewise inherited — we do NOT re-derive either. Returns the number of
    /// winners applied.
    ///
    /// Self-contained (no `&mut self`): the overlay is mutated only through the
    /// lock-free publishers (which take `&self`), so `replay_records_lww_overlay` is
    /// `&self` — unlike the owned `replay_records_lww` (`&mut self` for `self.root`).
    fn replay_records_lww_overlay(
        &self,
        recovered_ops: Vec<(
            crate::persistent_artrie_core::wal::Lsn,
            crate::persistent_artrie_core::wal::WalRecord,
        )>,
        loaded_from_disk: bool,
        checkpoint_lsn: crate::persistent_artrie_core::wal::Lsn,
        rank_regime: crate::persistent_artrie_core::wal::RankRegime,
    ) -> usize {
        let winners = crate::persistent_artrie_core::recovery::reconcile_lww(
            recovered_ops,
            loaded_from_disk,
            checkpoint_lsn,
            rank_regime,
        );
        let mut applied = 0usize;
        for op in winners {
            if self.apply_recovered_operation_overlay(op) {
                applied += 1;
            }
        }
        applied
    }
}
