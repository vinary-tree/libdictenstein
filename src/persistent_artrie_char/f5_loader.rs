//! **L3.1 — the DIRECT dense→overlay reopen codec loader (char variant).**
//!
//! `load_root_immutable` (the reopen chokepoint, both reopen arms) delegates to
//! [`PersistentARTrieChar::load_overlay_char_root_compressed`], which reads the dense char
//! image DIRECTLY into a fully-resident `Arc<PersistentCharNode<V>>`
//! (= `OverlayNode<CharKey, V>`) via [`PersistentARTrieChar::enumerate_char_terms_from_disk`]
//! + the proven iterative
//! [`build_overlay_root_from_terms`](crate::persistent_artrie_core::overlay::f5_build::build_overlay_root_from_terms)
//! — **NO transient owned `CharTrieRoot`/`CharTrieNodeInner`**. This lets L3.3 delete
//! `load_root_from_disk` + `CharTrieRoot` + `CharTrieNodeInner` + the char owned readers.
//! A corrupt/absent image falls back IN-LOADER to an EMPTY root + `image_loaded = false`
//! (the caller drains the WAL from frontier 0), never aborting `open()`.
//!
//! # The single dense-image walk (char is simpler than byte)
//!
//! [`PersistentARTrieChar::enumerate_char_terms_from_disk`] is one eager iterative DFS over
//! the arena records yielding `(term-units: Vec<u32>, Option<V>)` + the empty term "". Char
//! has NO `compact()` (no node-prefix format-1) and NO suffix-bucket — a `CharBucket` is a
//! single-char-edge FAN-OUT node decoded natively by `deserialize_char_node_v2` — so the walk
//! is UNIFORM: every node (root included; char root finality/value live on the RECORD, not the
//! descriptor) reads `(is_final, value, prefix, children)` from its record, yields its
//! `value`-or-`None` ONCE (membership∪value intrinsic), and pushes children at `+edge`. The
//! enumerator loads the arenas first (the char ctor does NOT — only `load_root_from_disk` did,
//! which this replaces). **Equivalence to the proven path is the GOLD-STANDARD GATE**
//! (`l31_char_differential_tests`): byte-exact vs
//! `build_overlay_root_from_owned(load_root_from_disk(image))` over format × `V` × Unicode ×
//! shape — run NOW while the oracle exists (it retires at L3.3).

use std::sync::Arc;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie_core::key_encoding::CharKey;
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;

use super::nodes::persistent_node::PersistentCharNode;

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
        // **L3.1:** build the overlay root DIRECTLY from the dense image via the codec loader —
        // NO transient owned `CharTrieRoot`. (`load_root_from_disk` + `CharTrieRoot`/
        // `CharTrieNodeInner` + `build_overlay_root_from_owned` + the char D1 readers survive only
        // as the L3.1 differential-test ORACLE; all die at L3.3.) Preserves char's IN-LOADER
        // corrupt-image fallback (`image_loaded = false` ⇒ the caller drains the WAL from
        // frontier 0, covering everything the absent/corrupt image does not).
        let (overlay_root, term_count, image_loaded) =
            self.load_overlay_char_root_compressed(buffer_manager, root_ptr)?;

        // Install the pre-built overlay root + select LockFreeOverlay + verify the Overlay
        // regime (V-2). A `false` ⇒ an Owned-regime WAL under overlay routing (recovery would
        // KEEP unranked orphans = resurrection); refuse.
        if !self.install_prebuilt_overlay_root(overlay_root) {
            return Err(PersistentARTrieError::internal(
                "F5 load_root_immutable: install_prebuilt_overlay_root did not engage \
                 (WAL not Overlay-regime, or ineligible V)",
            ));
        }

        // L3.3c: the owned tree is deleted; the overlay is the sole representation.
        self.len.store(0, std::sync::atomic::Ordering::Release);

        Ok((term_count, image_loaded))
    }

    /// **L3.1 — the direct dense→overlay codec loader, char (NO `CharTrieRoot`).**
    ///
    /// Enumerates `(term-units, Option<V>)` + the empty term "" DIRECTLY from the dense char
    /// image via [`PersistentARTrieChar::enumerate_char_terms_from_disk`] (char has no
    /// `compact()`/node-prefix format-1 and no suffix-bucket — `CharBucket` is a fan-out node),
    /// then builds the fully-resident (every child `Child::InMem`) overlay root via the proven
    /// iterative [`build_overlay_root_from_terms`](crate::persistent_artrie_core::overlay::f5_build::build_overlay_root_from_terms).
    /// Returns `(root, term_count, image_loaded)`. `root_ptr == 0` OR a decode error ⇒ an EMPTY
    /// overlay root + `image_loaded = false` (char's in-loader corrupt-image fallback — a corrupt
    /// image must drain the WAL, NOT abort `open()`).
    pub(crate) fn load_overlay_char_root_compressed(
        &self,
        buffer_manager: &Arc<RwLock<crate::persistent_artrie::buffer_manager::BufferManager<S>>>,
        root_ptr: u64,
    ) -> Result<(Arc<PersistentCharNode<V>>, usize, bool)> {
        use crate::persistent_artrie_core::overlay::f5_build::build_overlay_root_from_terms;

        if root_ptr == 0 {
            let empty = build_overlay_root_from_terms::<CharKey, V, _>(
                std::collections::BTreeMap::new(),
                None,
            );
            return Ok((empty, 0, false));
        }

        match self.enumerate_char_terms_from_disk(buffer_manager, root_ptr) {
            Ok((terms, empty_term, term_count)) => {
                let root = build_overlay_root_from_terms::<CharKey, V, _>(terms, empty_term);
                Ok((root, term_count as usize, true))
            }
            Err(e) => {
                // In-loader corrupt-image fallback (char parity with the prior loader).
                log::warn!(
                    "L3.1 char codec loader: dense image load failed ({:?}); falling back to an \
                     EMPTY image + WAL drain (corrupt-image parity)",
                    e
                );
                let empty = build_overlay_root_from_terms::<CharKey, V, _>(
                    std::collections::BTreeMap::new(),
                    None,
                );
                Ok((empty, 0, false))
            }
        }
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
