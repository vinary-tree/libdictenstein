//! Kill-switch scaffold for the **irreversible** Phase-E write-path flip.
//!
//! The lock-free persistent char ARTrie's Phase-E plan routes production
//! `insert`/`increment`/`checkpoint` through the Order-A lock-free overlay
//! (`docs/design/lockfree-cas-artrie.md`, plan
//! `/home/dylon/.claude/plans/carefully-review-the-staged-iridescent-wind.md`).
//! That flip is **data-loss-critical and irreversible**, so the plan mandates a
//! "kill-switch fallback for one release": if the overlay path regresses in
//! production, an operator must be able to fall back to the proven owned-tree
//! path WITHOUT a code change/rollback.
//!
//! [`OverlayWriteMode`] is that switch, added NOW as **reversible scaffolding**
//! so the future flip can read it. It is wired as an **inert default**
//! ([`OverlayWriteMode::OwnedTree`]) that changes NO current behavior: nothing in
//! the production `insert`/`increment`/`checkpoint` path consults it yet (the
//! routing is the owner-gated irreversible step, explicitly OUT OF SCOPE for the
//! reversible hardening). It exists so the flip is a one-line `match` rather than
//! a new field threaded through all eight constructors at flip time.

/// Selects which representation the production write path uses.
///
/// Default is [`OwnedTree`](Self::OwnedTree) — the proven, currently-shipping
/// owned `CharTrieRoot` path. [`LockFreeOverlay`](Self::LockFreeOverlay) is the
/// Phase-E target (Order-A WAL-then-CAS over the immutable overlay), reserved for
/// the irreversible flip and its one-release fallback window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OverlayWriteMode {
    /// The production owned-tree write path (`self.write()` + owned
    /// `CharTrieRoot` mutators + owned-snapshot checkpoint). The current,
    /// formally-verified default. Selecting this after the flip is the
    /// one-release kill-switch fallback.
    #[default]
    OwnedTree,
    /// The lock-free overlay write path (Order-A `insert_cas_durable` /
    /// `try_increment_cas_durable`, checkpoint via `capture_snapshot_immutable`
    /// reclaiming by the committed watermark). The Phase-E flip target; NOT yet
    /// wired into the production path (owner go/no-go gated).
    #[allow(dead_code)] // Constructed only post-flip; the variant is scaffold.
    LockFreeOverlay,
}

impl OverlayWriteMode {
    /// `true` when the production path should use the lock-free overlay. Before
    /// the flip's default switch (F5) this is `false` for the inert `OwnedTree`
    /// default; after F5 the constructor sets it to
    /// [`LockFreeOverlay`](Self::LockFreeOverlay) for `V ∈ {(), u64}`.
    #[inline]
    pub(crate) fn uses_overlay(self) -> bool {
        matches!(self, OverlayWriteMode::LockFreeOverlay)
    }
}

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// **Flip F0 — the THIN production-write-path router predicate.**
    ///
    /// `true` iff the production write path should route to the lock-free overlay
    /// for THIS trie: the kill-switch mode selects the overlay AND the overlay is
    /// actually live (`enable_lockfree()` has run, so `lockfree_root` is `Some`).
    /// Both conjuncts matter: a `LockFreeOverlay` mode with no overlay root (e.g.
    /// an arbitrary-`V` monomorph that the F5 default flip deliberately does NOT
    /// enable) correctly falls back to the proven owned tree.
    ///
    /// Each production mutator gains ONE top-level `match self.route_overlay()`
    /// branch whose `false` arm is the verbatim current owned-tree body (the
    /// one-release fallback) and whose `true` arm wires the proven Order-A overlay
    /// primitives — NO mutation logic is duplicated (design §2/§6).
    /// Public flip-state predicate (pairs with [`Self::kill_switch_to_owned`]): `true`
    /// iff reads/writes/checkpoint take the lock-free overlay path for this trie.
    #[inline]
    pub fn route_overlay(&self) -> bool {
        self.overlay_write_mode.uses_overlay() && self.lockfree_root.is_some()
    }

    /// **Flip §8 — restart-time kill-switch setter.** Select the production
    /// write-path representation. This is a RESTART-TIME switch (set the mode then
    /// reopen — the WAL is the shared source of truth, both trees recoverable from
    /// it), NOT a hot toggle: under `LockFreeOverlay` the owned tree is not
    /// written, so a hot flip back to `OwnedTree` would read a stale owned tree.
    /// Used to fall back to the proven owned path for one release if the overlay
    /// path regresses in production.
    // S5-12 flip API: exercised by tests; the production caller is the owner-gated
    // flip (not yet wired), so allow dead_code in non-test builds only.
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn set_overlay_write_mode(&mut self, mode: OverlayWriteMode) {
        self.overlay_write_mode = mode;
    }

    /// **S5-12 (V-1) — the SOLE expression of the "overlay only for `V ∈ {(), u64}`"
    /// invariant.** The lock-free overlay's valued path is u64-specialized
    /// (`build_value_path_recursive`); membership (`()`) needs no value. For any other
    /// `V` the overlay is write-broken (increment/merge/begin_document reject; batch
    /// routes to an undefined arbitrary-`V` overlay path), so the flip MUST be a no-op
    /// and the proven owned path runs. `DictionaryValue: 'static` ⇒ `TypeId` is callable.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn overlay_eligible_v() -> bool {
        use std::any::TypeId;
        TypeId::of::<V>() == TypeId::of::<u64>() || TypeId::of::<V>() == TypeId::of::<()>()
    }

    /// **S5-10c — flip construction helper (NOT wired into any production ctor; the
    /// S5-12 owner-GO flip wires it for the `V ∈ {(), u64}` monomorphs).** Make the
    /// lock-free overlay the live write target: `enable_lockfree()` (which stamps the
    /// WAL Overlay regime when the WAL is empty) then select `LockFreeOverlay` so
    /// `route_overlay()` becomes true. Returns the resulting `route_overlay()`.
    ///
    /// **V-1 gate:** a NO-OP returning `false` for `V ∉ {(), u64}` — the authoritative
    /// chokepoint so no caller can enable a broken overlay for arbitrary `V` (the
    /// design wrongly assumed `enable_lockfree` already refused this; it does not).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn flip_to_overlay(&mut self) -> bool {
        if !Self::overlay_eligible_v() {
            return false; // arbitrary V: never enable the overlay; stay OwnedTree.
        }
        self.enable_lockfree();
        self.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
        // Re-engaging the overlay after a `kill_switch_to_owned` (which stamped Owned on
        // a fresh WAL) must restamp Overlay — `enable_lockfree` only stamps on its FIRST
        // call (it early-returns once `lockfree_root` is set), so a second engage would
        // otherwise leave the WAL Owned-regime and fail the V-2 stamp check below. Gated
        // on a fresh WAL (`current_lsn() == 1`); a no-op for the ctor flip (where
        // `enable_lockfree` already stamped Overlay) and for non-empty WALs.
        if let Some(ref writer) = self.wal_writer {
            if writer.current_lsn() == 1
                && writer.rank_regime() != crate::persistent_artrie_core::wal::RankRegime::Overlay
            {
                if let Err(e) = writer.set_overlay_regime() {
                    log::warn!("flip_to_overlay: could not stamp Overlay regime: {:?}", e);
                }
            }
        }
        // V-2: `enable_lockfree` only `log::warn!`s if the Overlay-regime stamp failed,
        // then STILL enables the overlay — so verify the WAL is ACTUALLY Overlay-regime.
        // An Owned-regime WAL under overlay routing would make recovery KEEP unranked
        // orphans (resurrection). A trie with no WAL (in-memory) cannot durably flip and
        // also fails this check. The create-flip caller hard-errors on a `false` return.
        let stamped_overlay = self
            .wal_writer
            .as_ref()
            .map(|w| w.rank_regime() == crate::persistent_artrie_core::wal::RankRegime::Overlay)
            .unwrap_or(false);
        self.route_overlay() && stamped_overlay
    }

    /// **Kill-switch — the public one-release fallback for the S5-12 flip.** Revert the
    /// production write path from the lock-free overlay back to the proven owned tree:
    /// after this returns, `route_overlay()` is false, so writes/reads/checkpoint take
    /// the owned arm (the pre-flip behavior). Use it if the overlay path needs to be
    /// disabled in a deployed binary without rebuilding.
    ///
    /// In-session it takes effect immediately (the next op routes to the owned tree).
    /// Across a reopen it is RESTART-TIME: it deliberately does NOT restamp the WAL
    /// regime (`set_owned_regime` is valid only on an EMPTY WAL), so a reopen of a
    /// non-empty Overlay-regime WAL rebuilds the owned tree and re-flips — the durable
    /// regime is governed by the WAL, the in-memory mode by this switch.
    pub fn kill_switch_to_owned(&mut self) {
        self.set_overlay_write_mode(OverlayWriteMode::OwnedTree);
        // On a still-fresh WAL (no records appended — `current_lsn() == 1`, e.g.
        // immediately after `create()`), also restamp the durable regime to Owned so a
        // later reopen STAYS owned (no re-flip) and owned-mode records survive recovery
        // (otherwise the create-flip's Overlay stamp makes recovery DROP them as
        // two-append-window orphans). On a NON-empty WAL this is intentionally a no-op —
        // the documented restart-time semantics: the durable regime is already fixed, so
        // a reopen rebuilds the owned tree from the Overlay-regime WAL and re-flips. This
        // mirrors `enable_lockfree`'s `current_lsn() == 1` empty-WAL stamp guard.
        if let Some(ref writer) = self.wal_writer {
            if writer.current_lsn() == 1 {
                if let Err(e) = writer.set_owned_regime() {
                    log::warn!("kill_switch_to_owned: could not stamp Owned regime: {:?}", e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OverlayWriteMode;

    #[test]
    fn default_is_owned_tree_and_inert() {
        // The scaffold MUST default to the proven owned path and report that the
        // overlay is not in use — proving it changes no current behavior.
        assert_eq!(OverlayWriteMode::default(), OverlayWriteMode::OwnedTree);
        assert!(!OverlayWriteMode::default().uses_overlay());
    }

    #[test]
    fn overlay_variant_reports_overlay() {
        assert!(OverlayWriteMode::LockFreeOverlay.uses_overlay());
    }

    /// S5-10c: `flip_to_overlay` makes `route_overlay()` true (overlay is the live
    /// write target); `kill_switch_to_owned` reverts it to the owned path.
    #[test]
    fn flip_to_overlay_then_kill_switch_round_trips_route_overlay() {
        use crate::persistent_artrie_char::PersistentARTrieChar;
        std::fs::create_dir_all("target/test-tmp").ok();
        let dir = tempfile::Builder::new()
            .prefix("flip-helper")
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp");
        let path = dir.path().join("t.artc");
        let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create");

        // Post-flip: `create()` create-flips an eligible-V (u64) trie, so a FRESH trie
        // already routes to the overlay. Round-trip the kill-switch from there.
        assert!(
            trie.route_overlay(),
            "create-flip routes a fresh eligible-V (u64) trie to the overlay"
        );
        trie.kill_switch_to_owned();
        assert!(
            !trie.route_overlay(),
            "kill_switch_to_owned must revert to the owned path"
        );
        assert!(
            trie.flip_to_overlay(),
            "flip_to_overlay must re-engage the overlay"
        );
        assert!(trie.route_overlay());
    }

    /// S5-12 (V-1): the TypeId gate — `overlay_eligible_v()` is true only for
    /// `{u64, ()}`, and `flip_to_overlay` is a NO-OP for arbitrary `V` (which would
    /// otherwise get a write-broken overlay). Arbitrary V stays on the owned path.
    #[test]
    fn v1_typeid_gate_flip_is_noop_for_arbitrary_v() {
        use crate::persistent_artrie_char::PersistentARTrieChar;
        assert!(PersistentARTrieChar::<u64>::overlay_eligible_v());
        assert!(PersistentARTrieChar::<()>::overlay_eligible_v());
        assert!(!PersistentARTrieChar::<String>::overlay_eligible_v());

        std::fs::create_dir_all("target/test-tmp").ok();
        let dir = tempfile::Builder::new()
            .prefix("v1-gate")
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp");
        let path = dir.path().join("t.artc");
        let mut trie = PersistentARTrieChar::<String>::create(&path).expect("create");
        assert!(
            !trie.flip_to_overlay(),
            "flip_to_overlay must be a no-op for arbitrary V"
        );
        assert!(
            !trie.route_overlay(),
            "an arbitrary-V trie must stay on the owned path (no broken overlay)"
        );
    }
}
