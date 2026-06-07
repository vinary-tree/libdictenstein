//! S5-12 **E1 read-flip** — overlay-backed reads for `PersistentARTrieChar<V, S>`.
//!
//! When `route_overlay()` is true (the production write/read path is the immutable
//! lock-free overlay; `V ∈ {(), u64}` only), the public read methods route here
//! instead of walking the owned tree `self.root` (which is CLEARED on an
//! overlay-regime reopen — `reestablish_overlay_*`). These walks are the read-side
//! symmetry of the Order-A write guards.
//!
//! # Genericized — the engine now lives in the shared trait
//!
//! The overlay-read DFS engine (count/navigate/collect in `K::Unit` space) was
//! extracted into the SHARED GENERIC
//! [`LockFreeOverlay`](crate::persistent_artrie_core::overlay::flip::LockFreeOverlay)
//! trait (`docs/design/overlay-flip-genericization.md` §2). The char methods below
//! are now thin **per-variant skins** over that engine: they call
//! `overlay_collect_units`/`overlay_collect_units_with_values` (which accumulate
//! `Vec<u32>`) and map each unit-sequence to a `String` via `CharKey::units_to_term`
//! (`char::from_u32(_).unwrap_or('\u{FFFD}')` per unit — the EXACT prior behavior).
//! `overlay_len`/`overlay_is_empty` delegate directly. The value point-read
//! (`overlay_get_value`) stays char-specific (the per-variant value-route).
//!
//! # NON-FAULTING — DO NOT add disk fault-in
//!
//! The shared engine descends **in-memory children only** (`Child::as_in_mem`),
//! never resolving `Child::OnDisk` — a faulting read racing a checkpoint/eviction
//! that holds the buffer-manager lock is the lock-ordering inversion that
//! deadlocked the soak for 75+ minutes (memory
//! `feedback_production-deadlock-is-costly`). Hence `len`/`iter`/`iter_prefix` are
//! **resident-finals / last-checkpoint-consistent** (exact while no overlay node is
//! evicted; overlay eviction is `#[cfg(feature = "bench-internals")]`/test-only).

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::Result;
use crate::persistent_artrie_core::key_encoding::{CharKey, KeyEncoding};
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    // ===== count / emptiness (back `len`/`term_count`/`is_empty`) =====

    /// Term count of the overlay (number of finalized nodes). Resident-finals only
    /// (see the module doc). Thin delegator to the shared engine.
    pub(crate) fn overlay_len(&self) -> usize {
        <Self as LockFreeOverlay<CharKey, V, S>>::overlay_len(self)
    }

    /// Cheap emptiness check — an early-out "any final?" walk. Thin delegator.
    pub(crate) fn overlay_is_empty(&self) -> bool {
        <Self as LockFreeOverlay<CharKey, V, S>>::overlay_is_empty(self)
    }

    // ===== prefix navigation + collection (back `iter`/`iter_prefix*`) =====

    /// Overlay analogue of `iter_prefix`: `Ok(None)` if the prefix path is absent,
    /// else `Ok(Some(terms))` (possibly empty). The shared engine enumerates
    /// `Vec<u32>` unit-sequences; this skin maps each to a `String` via
    /// `CharKey::units_to_term` (reproducing the exact prior `char::from_u32` output).
    pub(crate) fn overlay_iter_prefix(&self, prefix: &str) -> Result<Option<Vec<String>>> {
        let prefix_units = CharKey::units_from_str(prefix);
        Ok(
            <Self as LockFreeOverlay<CharKey, V, S>>::overlay_collect_units(self, &prefix_units)
                .map(|seqs| seqs.iter().map(|u| CharKey::units_to_term(u)).collect()),
        )
    }

    /// Overlay analogue of `iter_prefix_with_values`. For `V = u64` each final's
    /// value is its counter (`get_value`); for `V = ()` each final's value is the
    /// synthesized `()` (membership finals carry no stored value — see `unit_as_v`).
    /// The shared engine handles the value synthesis; this skin maps the units to a
    /// `String`.
    pub(crate) fn overlay_iter_prefix_with_values(
        &self,
        prefix: &str,
    ) -> Result<Option<Vec<(String, V)>>> {
        let prefix_units = CharKey::units_from_str(prefix);
        Ok(
            <Self as LockFreeOverlay<CharKey, V, S>>::overlay_collect_units_with_values(
                self,
                &prefix_units,
            )
            .map(|seqs| {
                seqs.into_iter()
                    .map(|(u, v)| (CharKey::units_to_term(&u), v))
                    .collect()
            }),
        )
    }

    // ===== value point-read (backs `get_value`) =====

    /// Route `get_value(term)` to the overlay for `V ∈ {u64, ()}` via a SAFE `Any`
    /// dispatch (the `lockfree_value_route` pattern; zero `unsafe`). Returns:
    /// - `Some(Some(v))` — the term is present with value `v` (the `u64` counter, or
    ///   `()` for membership), re-wrapped as `V`;
    /// - `Some(None)` — handled by the overlay, term absent;
    /// - `None` — `V` is neither `u64` nor `()` (arbitrary `V`); the caller runs its
    ///   owned-tree body. (Unreachable under `route_overlay()`, which is gated to the
    ///   eligible monomorphs, but kept as a correct fall-through.)
    ///
    /// Thin char skin: convert `term`→`Vec<u32>` and delegate to the shared
    /// [`LockFreeOverlay::overlay_route_get_value`] driver, whose only seam are the
    /// char `overlay_counter_get` (= `<u64>` `get_lockfree`) and `overlay_contains`
    /// (= `contains_lockfree`) — so the per-variant counter-monomorph naming stays
    /// in the seam (design §2/§4). Behavior identical to the prior inline route.
    pub(crate) fn overlay_get_value(&self, term: &str) -> Option<Option<V>>
    where
        S: 'static,
    {
        let units = CharKey::units_from_str(term);
        <Self as LockFreeOverlay<CharKey, V, S>>::overlay_route_get_value(self, &units)
    }
}
