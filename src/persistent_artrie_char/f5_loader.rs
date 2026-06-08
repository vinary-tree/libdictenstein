//! **F5 (Slice 3) — the direct dense→overlay reopen loader (char variant).**
//!
//! `load_root_immutable` is the F5-B loader: it reuses the EXISTING owned loader
//! [`PersistentARTrieChar::load_root_from_disk`] **EAGER** (fully materialize) to decode
//! the dense disk image into the owned `CharTrieRoot<V>` (a TRANSIENT scratch in
//! `self.root`), then the generic, COMPRESSION-AWARE converter
//! [`crate::persistent_artrie_core::overlay::flip::LockFreeOverlay::build_overlay_root_from_owned`]
//! turns it into a single `Arc<PersistentCharNode<V>>` (deep-term-safe, iterative). The
//! result is installed as the live lock-free overlay via `install_prebuilt_overlay_root`,
//! and the transient owned tree is CLEARED — so reopen does not leave the owned tree
//! materialized as a production representation, and the WAL tail is replayed INTO THE
//! OVERLAY (not the owned tree) by the generic `replay_records_lww_overlay`.
//!
//! F5 ADDS this loader ALONGSIDE the existing reopen path; it is **gated** by
//! [`LockFreeOverlay::USE_F5_REOPEN_LOADER`] and exercised by the S2 tests via
//! `open_with_f5_loader`. The owned tree / mutators / legacy reopen path are NOT deleted
//! (that is F7).
//!
//! # Why the converter goes through the OWNED-term enumeration (not a node walk)
//!
//! An Overlay-regime dense image is USUALLY un-path-compressed (the overlay serializer
//! writes one node per unit), but a **COMPACTED** Overlay file is PATH-COMPRESSED
//! (C-opt-1: `compact()` rebuilds a dense owned image via the owned-staging path —
//! `StringBucket` + compressed prefixes — then re-stamps the Overlay regime). A
//! node-structural converter on the raw owned nodes would have to re-implement
//! bucket/prefix expansion (new data-loss-critical code). Instead the generic converter
//! enumerates the owned tree via the proven D1 owned readers (which already expand all
//! compression) and builds the overlay from the `(term-units, value)` enumeration — the
//! compression handling lives ONCE in the existing readers. See
//! `docs/design/slice3-f5-loader-impl.md`.

use std::sync::Arc;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
use crate::persistent_artrie_core::key_encoding::CharKey;
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;

use super::nodes::persistent_node::PersistentCharNode;
use super::types::CharTrieRoot;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// **F5 — load the dense image into a pre-built lock-free overlay root** (the owned
    /// tree is a TRANSIENT decode scratch, cleared after conversion — reopen never leaves
    /// it as a production representation).
    ///
    /// 1. Eager-load the dense image via the EXISTING `load_root_from_disk`
    ///    (`eager_depth = Some(usize::MAX)` → `load_char_node_from_disk_iterative`, so the
    ///    owned readers never fault) INTO `self.root`.
    /// 2. Build the overlay root from the owned tree via the generic, COMPRESSION-AWARE
    ///    [`LockFreeOverlay::build_overlay_root_from_owned`] (handles both un-compressed
    ///    and compacted/path-compressed Overlay images).
    /// 3. Install it as the live overlay (`install_prebuilt_overlay_root`: selects
    ///    LockFreeOverlay + verifies the WAL Overlay regime — V-2; HARD-ERROR on a `false`
    ///    so a recovery-unsafe Owned-regime-under-overlay never engages).
    /// 4. CLEAR the transient owned tree (F5's goal — owned not left materialized).
    ///
    /// `&mut self`. Returns the term count loaded from the dense image (NOT incl. the WAL
    /// tail — the caller replays the tail via `replay_records_lww_overlay` after).
    ///
    /// **`root_ptr == 0`** (empty dense image): install an EMPTY overlay root.
    pub(crate) fn load_root_immutable(
        &mut self,
        buffer_manager: &Arc<RwLock<crate::persistent_artrie::buffer_manager::BufferManager<S>>>,
        root_ptr: u64,
    ) -> Result<(usize, bool)> {
        // (1) Eager-load the dense image into the TRANSIENT owned tree.
        //
        // **F7 corrupt-image fallback.** If the dense image fails to load (a corrupt root
        // descriptor / arena), FALL BACK to an EMPTY image — exactly as the legacy owned
        // reopen does (`load_root_from_disk` error ⇒ `loaded_from_disk = false` ⇒ rebuild
        // from the WAL). The caller (the F5 arm / the F7 converter) then recovers everything
        // from the WAL drain. Refusing to trust a malformed image avoids fabricating
        // checkpointed contents from corrupt bytes. Returns `image_loaded` so the caller can
        // pass `loaded_from_disk = false` + `image_checkpoint_lsn = 0` to the drain when the
        // image is absent/corrupt (else the drain would wrongly SKIP WAL records `<= the
        // active Checkpoint record's lsn`, dropping data the absent image does NOT cover).
        let (term_count, image_loaded) = if root_ptr == 0 {
            (0usize, false)
        } else {
            let root_swizzled = SwizzledPtr::from_raw(root_ptr);
            match self.load_root_from_disk(buffer_manager, &root_swizzled, Some(usize::MAX)) {
                Ok((owned_root, len)) => {
                    *self.root.get_mut() = owned_root;
                    self.len.store(len, std::sync::atomic::Ordering::Release);
                    (len, true)
                }
                Err(e) => {
                    log::warn!(
                        "F7 load_root_immutable: dense image load failed ({:?}); falling back to \
                         an EMPTY image + WAL drain (legacy fallback parity)",
                        e
                    );
                    *self.root.get_mut() = CharTrieRoot::Empty;
                    self.len.store(0, std::sync::atomic::Ordering::Release);
                    (0usize, false)
                }
            }
        };

        // (2) Build the overlay root from the owned tree (compression-aware, iterative).
        let overlay_root: Arc<PersistentCharNode<V>> =
            <Self as LockFreeOverlay<CharKey, V, S>>::build_overlay_root_from_owned(self)?;

        // (3) Install the pre-built overlay root + select LockFreeOverlay + verify the
        // Overlay regime (V-2). A `false` ⇒ an Owned-regime WAL under overlay routing
        // (recovery would KEEP unranked orphans = resurrection); refuse.
        if !self.install_prebuilt_overlay_root(overlay_root) {
            return Err(PersistentARTrieError::internal(
                "F5 load_root_immutable: install_prebuilt_overlay_root did not engage \
                 (WAL not Overlay-regime, or ineligible V)",
            ));
        }

        // (4) Clear the transient owned tree — F5 does not leave it materialized. (The
        // production read/checkpoint path is now overlay-routed; the owned tree was only
        // the decode scratch the converter read.)
        *self.root.get_mut() = CharTrieRoot::Empty;
        self.len.store(0, std::sync::atomic::Ordering::Release);

        Ok((term_count, image_loaded))
    }
}

#[cfg(test)]
mod deep_term_converter_tests {
    //! **F5 converter deep-safety (in isolation).** The generic
    //! [`build_overlay_root_from_terms`](crate::persistent_artrie_core::overlay::f5_build::build_overlay_root_from_terms)
    //! is iterative in BOTH phases (per-unit insert loop + work-stack convert) and its
    //! builder/`OverlayNode` `Drop`s are iterative, so it builds + drops a ~100k-unit
    //! single-key overlay on the DEFAULT test stack where a recursive converter would
    //! overflow. The integration suite drives the FULL reopen at ~100k on a large-stack
    //! thread (the unrelated recursive read paths need it); this proves the F5-ADDED
    //! converter is never the deep-term bottleneck.

    use crate::persistent_artrie_core::key_encoding::CharKey;
    use crate::persistent_artrie_core::overlay::f5_build::build_overlay_root_from_terms;

    #[test]
    fn build_overlay_root_from_terms_work_stack_safe_at_100k_char() {
        const DEPTH: usize = 100_000;
        const EDGE: u32 = 'a' as u32;
        // A single ~100k-unit key (the overlay spine is 1 node/unit ⇒ ~100k deep).
        let units: Vec<u32> = std::iter::repeat(EDGE).take(DEPTH).collect();
        let root =
            build_overlay_root_from_terms::<CharKey, u64, _>(vec![(units, Some(42u64))], None);

        // Verify the overlay spine ITERATIVELY (descend the single edge DEPTH times).
        let mut cur = root.clone();
        let mut depth = 0usize;
        while let Some(child) = cur.find_child(EDGE).and_then(|c| c.as_in_mem()).cloned() {
            depth += 1;
            cur = child;
        }
        assert_eq!(depth, DEPTH, "converted overlay spine has the full depth");
        assert!(cur.is_final(), "deep leaf is final");
        assert_eq!(cur.get_value(), Some(42), "deep leaf value round-tripped");
        // Drop the deep spine (both builder + OverlayNode Drops are iterative).
        drop(cur);
        drop(root);
    }
}
