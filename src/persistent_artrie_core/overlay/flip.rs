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
pub(crate) trait LockFreeOverlay<K: KeyEncoding, V: DictionaryValue, S>: Sized + 'static {
    /// The per-variant counter monomorph (`u64` for char, `i64` for byte). THE
    /// divergence that makes the value-route a seam, not a blanket. `Copy` so the
    /// publisher/getter seams can pass it by value.
    type CounterValue: 'static + Copy;

    // ========================================================================
    // REQUIRED SEAM (variant provides) — small accessors + the un-routed owned
    // readers + the overlay publishers.
    // ========================================================================

    /// The lock-free overlay's atomic root pointer, or `None` if the overlay is
    /// not installed (`enable_lockfree()` not yet run for this trie).
    fn lockfree_root(
        &self,
    ) -> Option<&crate::persistent_artrie_core::overlay::AtomicNodePtr<K, V>>;

    /// The current kill-switch mode for this trie.
    fn overlay_write_mode(&self) -> OverlayWriteMode;

    /// Set the kill-switch mode (restart-time switch; see `kill_switch_to_owned`).
    fn set_overlay_write_mode(&mut self, mode: OverlayWriteMode);

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
    fn kill_switch_to_owned(&mut self) {
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
        // Disjoint first-unit partition cover of the recovered owned terms (the
        // empty term has no first unit; membership ignores it — it carries no
        // value to publish and the per-unit chunks below re-publish it via
        // `owned_units_under` only if it surfaces under some unit, which it never
        // does, matching the char membership fold which also drops `""`).
        let (first_units, _has_empty_term) = self.owned_first_units()?;
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
        if has_empty_term {
            if let Some(v) = self.overlay_counter_value_of_owned_empty() {
                self.overlay_publish_counter(&[], v);
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

    /// The owned empty-term value re-expressed as `CounterValue` (for the counter
    /// reestablish's empty-term partition), or `None` if absent / not a counter
    /// monomorph. A SAFE `Any` re-wrap on the value (`V`/`CounterValue` both
    /// `'static`), never on `K`.
    fn overlay_counter_value_of_owned_empty(&self) -> Option<Self::CounterValue> {
        let v = self.owned_has_empty_term_value()?;
        Self::value_as_counter(&v)
    }

    /// Re-wrap a `&V` as `CounterValue` via a SAFE `Any` downcast iff `V ==
    /// CounterValue`, else `None`. The reestablish counter fold uses this to feed
    /// the typed publisher seam from the generic `V`-valued owned chunk.
    fn value_as_counter(value: &V) -> Option<Self::CounterValue> {
        (value as &dyn Any).downcast_ref::<Self::CounterValue>().copied()
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
            return Some(self.overlay_counter_get(units).and_then(Self::counter_as_value));
        }
        // `V == ()`: membership — present ⇒ `Some(())` re-wrapped as `V`.
        if TypeId::of::<V>() == TypeId::of::<()>() {
            return Some(if self.overlay_contains(units) {
                unit_as_v::<V>()
            } else {
                None
            });
        }
        None
    }
}
