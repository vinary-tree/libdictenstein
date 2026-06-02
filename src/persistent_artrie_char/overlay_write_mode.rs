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
    /// `true` when the production path should use the lock-free overlay. Today
    /// this is always `false` (the field is the inert `OwnedTree` default), so
    /// callers gated on it are no-ops until the irreversible flip sets the field
    /// to [`LockFreeOverlay`](Self::LockFreeOverlay).
    #[allow(dead_code)] // Read site lands with the flip (out of scope here).
    #[inline]
    pub(crate) fn uses_overlay(self) -> bool {
        matches!(self, OverlayWriteMode::LockFreeOverlay)
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
