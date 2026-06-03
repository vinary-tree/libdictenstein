//! `OverlayWriteMode` — the kill-switch enum selecting the production write-path
//! representation (owned-tree vs lock-free overlay), shared by every persistent
//! ARTrie variant.
//!
//! This enum was originally char-specific
//! (`persistent_artrie_char::overlay_write_mode`); the overlay-flip
//! genericization (`docs/design/overlay-flip-genericization.md` §2/§A) hoists it
//! into `persistent_artrie_core::overlay` so the generic
//! [`LockFreeOverlay`](super::flip::LockFreeOverlay) trait — and every variant's
//! seam impl — can name it without an upward reference. The variant's
//! `overlay_write_mode` field is now `OverlayWriteMode` (this type), made `pub`.
//!
//! # The flip, in one sentence
//!
//! The lock-free persistent ARTrie's Phase-E plan routes production
//! `insert`/`increment`/`checkpoint` through the Order-A lock-free overlay. That
//! flip is **data-loss-critical and irreversible**, so the plan mandates a
//! kill-switch fallback for one release: if the overlay path regresses in
//! production, an operator can fall back to the proven owned-tree path WITHOUT a
//! code change/rollback. [`OverlayWriteMode`] is that switch.

/// Selects which representation the production write path uses.
///
/// Default is [`OwnedTree`](Self::OwnedTree) — the proven, currently-shipping
/// owned trie path. [`LockFreeOverlay`](Self::LockFreeOverlay) is the
/// Phase-E target (Order-A WAL-then-CAS over the immutable overlay), reserved for
/// the irreversible flip and its one-release fallback window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlayWriteMode {
    /// The production owned-tree write path (`self.write()` + owned trie mutators
    /// + owned-snapshot checkpoint). The current, formally-verified default.
    /// Selecting this after the flip is the one-release kill-switch fallback.
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
    /// [`LockFreeOverlay`](Self::LockFreeOverlay) for the eligible counter
    /// monomorphs (`V ∈ {(), u64}` for char; `{(), i64}` for byte).
    #[inline]
    pub fn uses_overlay(self) -> bool {
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
