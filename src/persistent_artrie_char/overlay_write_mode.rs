//! Char seam impl of the shared [`LockFreeOverlay`] flip + thin inherent
//! wrappers preserving the char public surface.
//!
//! The overlay-flip genericization (`docs/design/overlay-flip-genericization.md`
//! §2, Step 1) extracted the lock-free-overlay flip (route + read-engine +
//! flip/kill-switch + reestablish) into the SHARED GENERIC
//! [`LockFreeOverlay`](crate::persistent_artrie_core::overlay::flip::LockFreeOverlay)
//! trait in `persistent_artrie_core::overlay::flip`. This module now holds only:
//!
//! 1. a re-export of [`OverlayWriteMode`] (hoisted to
//!    `persistent_artrie_core::overlay::write_mode`) so the many internal `use
//!    super::overlay_write_mode::OverlayWriteMode` sites still resolve;
//! 2. the char SEAM impl `impl LockFreeOverlay<CharKey, V, S> for
//!    PersistentARTrieChar<V, S>` (the per-variant owned readers / overlay
//!    publishers / WAL accessors / `CounterValue = u64`);
//! 3. thin inherent wrappers (`route_overlay`/`flip_to_overlay`/
//!    `kill_switch_to_owned`/`set_overlay_write_mode`/`overlay_eligible_v`) that
//!    DELEGATE to the trait, so the ~40 existing inherent-syntax call sites
//!    (`self.route_overlay()`, …) keep compiling and behaving IDENTICALLY.
//!
//! **Byte-identical guarantee.** The trait bodies are a token-for-token port of
//! the char originals; only the two boundary conversions changed (the overlay
//! read engine accumulates `Vec<u32>` and the seam converts via
//! `CharKey::units_to_term`/`units_from_str`, both defined to reproduce the exact
//! char behavior — `units_to_term` IS `char::from_u32(_).unwrap_or('\u{FFFD}')`
//! per unit). The existing E1 correspondence suite + the full nextest run are the
//! oracle.

// Re-export so `super::overlay_write_mode::OverlayWriteMode` resolves everywhere
// it was used before the hoist (ctors, persist, atomic_ops, document_tx, …).
pub(crate) use crate::persistent_artrie_core::overlay::write_mode::OverlayWriteMode;

use std::any::TypeId;
use std::sync::atomic::Ordering;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::Result;
use crate::persistent_artrie_core::key_encoding::{CharKey, KeyEncoding};
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::persistent_artrie_core::wal::RankRegime;
use crate::value::DictionaryValue;

use super::types::CharTrieRoot;

// ============================================================================
// Char seam impl of the shared LockFreeOverlay flip
// ============================================================================

impl<V: DictionaryValue, S: BlockStorage> LockFreeOverlay<CharKey, V, S>
    for super::PersistentARTrieChar<V, S>
{
    /// `u64` — the char counter monomorph (byte's is `i64`).
    type CounterValue = u64;

    // ---- small accessors ----

    #[inline]
    fn lockfree_root(
        &self,
    ) -> Option<&crate::persistent_artrie_core::overlay::AtomicNodePtr<CharKey, V>> {
        // `super::nodes::AtomicNodePtr<V>` IS `overlay::AtomicNodePtr<CharKey, V>`
        // (a type alias — see `nodes::atomic_ptr`), so this borrow is identity.
        self.lockfree_root.as_ref()
    }

    #[inline]
    fn overlay_write_mode(&self) -> OverlayWriteMode {
        self.overlay_write_mode
    }

    #[inline]
    fn set_overlay_write_mode(&mut self, mode: OverlayWriteMode) {
        self.overlay_write_mode = mode;
    }

    #[inline]
    fn enable_lockfree(&mut self) {
        // Delegate to the existing inherent `enable_lockfree` (lockfree_cas.rs),
        // which sets up the `AtomicNodePtr` root + cache and stamps the WAL Overlay
        // regime on an EMPTY WAL. Unchanged behavior.
        super::PersistentARTrieChar::enable_lockfree(self)
    }

    #[inline]
    fn wal_current_lsn(&self) -> Option<u64> {
        self.wal_writer.as_ref().map(|w| w.current_lsn())
    }

    #[inline]
    fn wal_is_overlay_regime(&self) -> bool {
        self.wal_writer
            .as_ref()
            .map(|w| w.rank_regime() == RankRegime::Overlay)
            .unwrap_or(false)
    }

    fn wal_stamp_overlay_regime(&self) {
        if let Some(ref writer) = self.wal_writer {
            if let Err(e) = writer.set_overlay_regime() {
                log::warn!("flip_to_overlay: could not stamp Overlay regime: {:?}", e);
            }
        }
    }

    fn wal_stamp_owned_regime(&self) {
        if let Some(ref writer) = self.wal_writer {
            if let Err(e) = writer.set_owned_regime() {
                log::warn!("kill_switch_to_owned: could not stamp Owned regime: {:?}", e);
            }
        }
    }

    /// **S5-12 (V-1)** — the char eligible monomorphs `{u64, ()}`.
    /// `DictionaryValue: 'static` ⇒ `TypeId` is callable.
    fn overlay_eligible_v() -> bool {
        TypeId::of::<V>() == TypeId::of::<u64>() || TypeId::of::<V>() == TypeId::of::<()>()
    }

    // ---- UN-ROUTED owned readers (D1 — read the OWNED tree directly) ----

    fn owned_first_units(&self) -> Result<(Vec<u32>, bool)> {
        // Disjoint first-code-point cover. D1: `owned_iter_prefix("")` is the
        // UN-routed owned reader (it walks `self.root`, never the overlay), so it is
        // safe even when the trie is already in overlay-write mode (the reestablish
        // caller flips before dispatching).
        use std::collections::BTreeSet;
        let mut first_units: BTreeSet<u32> = BTreeSet::new();
        let mut has_empty_term = false;
        if let Some(all_terms) = self.owned_iter_prefix("")? {
            for term in &all_terms {
                match term.chars().next() {
                    Some(c) => {
                        first_units.insert(c as u32);
                    }
                    None => has_empty_term = true,
                }
            }
        }
        Ok((first_units.into_iter().collect(), has_empty_term))
    }

    fn owned_units_under(&self, prefix: &[u32]) -> Result<Option<Vec<Vec<u32>>>> {
        // D1: UN-routed owned reader. Convert the single-unit prefix and each
        // recovered term to/from `Vec<u32>` via the `CharKey` boundary so the
        // generic fold publishes the SAME terms the char originals did.
        let prefix_str = CharKey::units_to_term(prefix);
        Ok(self.owned_iter_prefix(&prefix_str)?.map(|terms| {
            terms
                .iter()
                .map(|t| CharKey::units_from_str(t).into_vec())
                .collect()
        }))
    }

    fn owned_units_with_values_under(
        &self,
        prefix: &[u32],
    ) -> Result<Option<Vec<(Vec<u32>, V)>>> {
        // D1: UN-routed owned reader.
        let prefix_str = CharKey::units_to_term(prefix);
        Ok(self
            .owned_iter_prefix_with_values(&prefix_str)?
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|(t, v)| (CharKey::units_from_str(&t).into_vec(), v))
                    .collect()
            }))
    }

    fn owned_has_empty_term_value(&self) -> Option<V> {
        // D1: UN-routed owned reader (`owned_get` reads `self.root`).
        self.owned_get("").cloned()
    }

    fn clear_owned(&mut self) {
        self.root = CharTrieRoot::Empty;
        self.len.store(0, Ordering::Release);
    }

    // ---- overlay publishers (the per-variant write seam) ----

    fn overlay_publish_membership(&self, units: &[u32]) {
        // No-WAL CAS insert (recovered terms are already durable; re-logging would
        // double-log). Unchanged from the char membership reestablish.
        let term = CharKey::units_to_term(units);
        self.insert_cas(&term);
    }

    fn overlay_publish_counter(&self, units: &[u32], value: u64) {
        // `V == u64` in this routed branch (the counter reestablish runs only for
        // the u64 monomorph via the dispatch). SAFE `Any` downcast to the nameable
        // `<u64, S>` monomorph, then the no-WAL `increment_cas` — the same pattern
        // as the char `overlay_get_value`/`reestablish_overlay_dispatch`.
        use std::any::Any;
        let term = CharKey::units_to_term(units);
        if let Some(trie_u64) =
            (self as &dyn Any).downcast_ref::<super::PersistentARTrieChar<u64, S>>()
        {
            trie_u64.increment_cas(&term, value);
        }
    }

    fn overlay_counter_get(&self, units: &[u32]) -> Option<u64> {
        // SAFE `Any` downcast to `<u64, S>` + the lock-free point read.
        use std::any::Any;
        let term = CharKey::units_to_term(units);
        (self as &dyn Any)
            .downcast_ref::<super::PersistentARTrieChar<u64, S>>()
            .and_then(|trie_u64| trie_u64.get_lockfree(&term))
    }

    fn overlay_contains(&self, units: &[u32]) -> bool {
        let term = CharKey::units_to_term(units);
        self.contains_lockfree(&term)
    }
}

// ============================================================================
// Thin inherent wrappers — preserve the char public/`pub(crate)` surface by
// DELEGATING to the trait (so the ~40 existing inherent-syntax call sites and
// the external `route_overlay` tests keep compiling, behavior identical).
// ============================================================================

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// **Flip F0 — production-write/read-path router.** `true` iff reads/writes/
    /// checkpoint take the lock-free overlay path for this trie. Thin delegator to
    /// [`LockFreeOverlay::route_overlay`]. Kept `pub` (external tests call it).
    #[inline]
    pub fn route_overlay(&self) -> bool {
        <Self as LockFreeOverlay<CharKey, V, S>>::route_overlay(self)
    }

    /// **Restart-time kill-switch setter.** Thin delegator to the seam.
    // S5-12 flip API: exercised by tests; the production caller is the owner-gated
    // flip (not yet wired), so allow dead_code in non-test builds only.
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn set_overlay_write_mode(&mut self, mode: OverlayWriteMode) {
        <Self as LockFreeOverlay<CharKey, V, S>>::set_overlay_write_mode(self, mode)
    }

    /// **S5-12 (V-1)** — overlay-eligibility gate (`V ∈ {(), u64}`). Thin delegator.
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn overlay_eligible_v() -> bool {
        <Self as LockFreeOverlay<CharKey, V, S>>::overlay_eligible_v()
    }

    /// **S5-10c — flip construction helper.** Thin delegator to
    /// [`LockFreeOverlay::flip_to_overlay`].
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn flip_to_overlay(&mut self) -> bool {
        <Self as LockFreeOverlay<CharKey, V, S>>::flip_to_overlay(self)
    }

    /// **Kill-switch — one-release fallback.** Thin delegator to
    /// [`LockFreeOverlay::kill_switch_to_owned`]. Kept `pub` (external callers).
    #[inline]
    pub fn kill_switch_to_owned(&mut self) {
        <Self as LockFreeOverlay<CharKey, V, S>>::kill_switch_to_owned(self)
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
