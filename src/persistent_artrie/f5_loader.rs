//! **L3.1 — the DIRECT dense→overlay reopen codec loader (byte variant).**
//!
//! `load_root_immutable` (the reopen chokepoint, reached by both the Overlay-regime `use_f5`
//! arm and the Owned-regime `convert_owned_to_overlay_on_reopen` arm) delegates to
//! [`PersistentARTrie::load_overlay_root_compressed`], which reads the dense on-disk image
//! DIRECTLY into a fully-resident `Arc<OverlayNode<ByteKey, V>>` via
//! [`PersistentARTrie::enumerate_terms_from_disk`] + the proven iterative
//! [`build_overlay_root_from_terms`](crate::persistent_artrie_core::overlay::f5_build::build_overlay_root_from_terms)
//! — **NO transient owned `TrieRoot`**. `self.root` is never materialized; this is the
//! "literal-zero-owned" reopen the campaign targets, and it let L3.3 delete
//! `load_root_from_disk_with_arena` + `TrieRoot` + the owned converters + the D1
//! owned readers outright.
//!
//! # The single dense-image walk (all three on-disk formats)
//!
//! [`PersistentARTrie::enumerate_terms_from_disk`] is one eager iterative DFS over the arena
//! records that yields `(term-units, Option<V>)` + the empty term "":
//! - **format-2** (un-compressed Overlay checkpoint, one N4/16/48/256 record per unit): plain
//!   single-byte edges.
//! - **format-1** (CX-compacted: node-header `prefix_len > 0` chunks): each node folds its own
//!   `prefix` into the path at entry (the L2.1 fold), so compressed runs reconstruct losslessly.
//! - **format-3** (LEGACY owned `serialize_root`: `StringBucket` leaf records with multi-byte
//!   suffixes): each bucket entry contributes `path ++ suffix`. Legacy files can be rebuilt — a
//!   reopened legacy file auto-rewrites to the new format on its next checkpoint.
//!
//! A final node / bucket entry yields its `read_node_value`-or-`None` ONCE, so the membership∪
//! value union is intrinsic. **Equivalence to the proven path was the L3.1 GOLD-STANDARD GATE**
//! (`l31_differential_tests`): this loader produced a byte-exact overlay vs the owned-scratch
//! oracle (`load_root_from_disk_with_arena` + the owned→overlay converter) over every
//! format × `V` × shape — verified BEFORE L3.3 retired that oracle.

use std::sync::Arc;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie_core::key_encoding::ByteKey;
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::persistent_artrie_core::overlay::node::OverlayNode;
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrie<V, S> {
    /// **F5/BLOCKER#4 — load the dense image DIRECTLY into a pre-built lock-free overlay
    /// root** (NO transient owned `TrieRoot`; the owned `self.root` is left empty).
    ///
    /// 1. Build the overlay root from the dense image via the codec
    ///    [`Self::load_overlay_root_compressed`] (`enumerate_terms_from_disk` +
    ///    `build_overlay_root_from_terms`), which falls back to an EMPTY overlay +
    ///    `image_loaded = false` on a corrupt/absent image (the in-loader fallback).
    /// 2. Install it (`install_prebuilt_overlay_root`: select LockFreeOverlay + verify the
    ///    Overlay regime — V-2; HARD-ERROR on a `false`).
    ///
    /// `&mut self`. Returns `(dense-image term count, image_loaded)` — NOT incl. the WAL
    /// tail (the caller drains it via `reconcile_and_drain_overlay` after, from frontier 0
    /// when `image_loaded == false`).
    ///
    /// **`root_ptr == 0`** (empty/absent dense image): install an EMPTY overlay root +
    /// `image_loaded = false`.
    pub(crate) fn load_root_immutable(&mut self, root_ptr: u64) -> Result<(usize, bool)> {
        // **L3.1/BLOCKER#4:** build the overlay root DIRECTLY from the dense image via the codec
        // loader (NO transient owned `TrieRoot`), with an IN-LOADER corrupt-image fallback: a
        // corrupt/absent image yields an EMPTY overlay + `image_loaded = false` (the caller then
        // drains the WAL from frontier 0, recovering everything the absent image fails to cover)
        // rather than `?`-aborting `open()`. Returns `(term_count, image_loaded)` — byte twin of
        // char's signature. (The owned decoder `load_root_from_disk_with_arena` + `TrieRoot` were
        // deleted at L3.3c-C2.)
        let (overlay_root, term_count, image_loaded) =
            self.load_overlay_root_compressed(root_ptr)?;

        // Install + select LockFreeOverlay + verify the Overlay regime (V-2).
        if !self.install_prebuilt_overlay_root(overlay_root) {
            return Err(PersistentARTrieError::internal(
                "F5 load_root_immutable: install_prebuilt_overlay_root did not engage \
                 (WAL not Overlay-regime, or ineligible V)",
            ));
        }

        // L3.3c: the owned `root` field is deleted — the overlay (installed above) is the sole
        // representation. The owned term count is meaningless under the overlay (overlay finals are
        // counted directly), so reset the legacy counter to 0.
        self.term_count
            .store(0, std::sync::atomic::Ordering::Release);

        Ok((term_count, image_loaded))
    }

    /// **L3.1 — the direct dense→overlay codec loader (NO `TrieRoot`).**
    ///
    /// Enumerates `(term, Option<V>)` + the empty term "" DIRECTLY from the dense image via
    /// [`PersistentARTrie::enumerate_terms_from_disk`] (all three on-disk formats:
    /// un-compressed overlay, CX node-prefix-compressed, legacy owned bucket-suffix), then builds
    /// the fully-resident (every child `Child::InMem`) overlay root via the proven iterative
    /// [`build_overlay_root_from_terms`](crate::persistent_artrie_core::overlay::f5_build::build_overlay_root_from_terms).
    /// `root_ptr == 0` ⇒ an EMPTY overlay root. The eager all-`InMem` result satisfies the
    /// residency invariant that `capture_overlay_snapshot`/`evict`/`count_overlay_finals` (which
    /// walk only `as_in_mem()`) require — a lazily-`OnDisk` reopened root would silently vanish
    /// from the first post-reopen checkpoint.
    ///
    /// Equivalence anchor (the L3.1 differential gate): for every image this produced the SAME
    /// overlay as the now-retired owned-scratch oracle (`load_root_from_disk_with_arena` + the
    /// owned→overlay converter), proven over format × `V` × {valued, term-only, "" valued, ""
    /// membership} × deep keys BEFORE this became the only reopen path.
    pub(crate) fn load_overlay_root_compressed(
        &self,
        root_ptr: u64,
    ) -> Result<(Arc<OverlayNode<ByteKey, V>>, usize, bool)> {
        use crate::persistent_artrie_core::overlay::f5_build::build_overlay_root_from_terms;

        // `root_ptr == 0` ⇒ an EMPTY overlay + `image_loaded = false` (no dense image present).
        if root_ptr == 0 {
            let empty = build_overlay_root_from_terms::<ByteKey, V, _>(
                std::collections::BTreeMap::new(),
                None,
            );
            return Ok((empty, 0, false));
        }

        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("L3.1 load_overlay_root_compressed: no buffer manager")
        })?;
        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("L3.1 load_overlay_root_compressed: no arena manager")
        })?;

        // BLOCKER#4 in-loader fallback (mirror char): a corrupt dense image (decode error) ⇒ an
        // EMPTY overlay + `image_loaded = false`, so the caller drains the WAL from frontier 0
        // rather than `?`-aborting `open()`. A valid image ⇒ `image_loaded = true`.
        match Self::enumerate_terms_from_disk(buffer_manager, arena_manager, root_ptr) {
            Ok((terms, empty_term, term_count)) => {
                let overlay_root =
                    build_overlay_root_from_terms::<ByteKey, V, _>(terms, empty_term);
                Ok((overlay_root, term_count as usize, true))
            }
            Err(e) => {
                log::warn!(
                    "L3.1 load_overlay_root_compressed: corrupt dense image at root_ptr {:#x}: \
                     {:?}; falling back to an empty overlay (the WAL drain recovers from frontier 0)",
                    root_ptr,
                    e
                );
                let empty = build_overlay_root_from_terms::<ByteKey, V, _>(
                    std::collections::BTreeMap::new(),
                    None,
                );
                Ok((empty, 0, false))
            }
        }
    }
}
