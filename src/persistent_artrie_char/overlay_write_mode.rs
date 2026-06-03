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
    #[inline]
    pub(crate) fn route_overlay(&self) -> bool {
        self.overlay_write_mode.uses_overlay() && self.lockfree_root.is_some()
    }

    /// **Flip §8 — restart-time kill-switch setter.** Select the production
    /// write-path representation. This is a RESTART-TIME switch (set the mode then
    /// reopen — the WAL is the shared source of truth, both trees recoverable from
    /// it), NOT a hot toggle: under `LockFreeOverlay` the owned tree is not
    /// written, so a hot flip back to `OwnedTree` would read a stale owned tree.
    /// Used to fall back to the proven owned path for one release if the overlay
    /// path regresses in production.
    #[inline]
    pub(crate) fn set_overlay_write_mode(&mut self, mode: OverlayWriteMode) {
        self.overlay_write_mode = mode;
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
}
