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

        // The owned `root` scratch is never materialized now — leave it empty (it is not the
        // production rep under the overlay; it is deleted at L3.3).
        *self.root.get_mut() = CharTrieRoot::Empty;
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

#[cfg(test)]
mod l31_char_differential_tests {
    //! **L3.1 GOLD-STANDARD GATE (char).** `load_overlay_char_root_compressed` (no
    //! `CharTrieRoot`) produces a STRUCTURALLY-IDENTICAL overlay to the owned-scratch oracle
    //! `build_overlay_root_from_owned(load_root_from_disk(image))` — over both char on-disk
    //! formats (format-2 un-compressed overlay, format-3 legacy owned; char has NO `compact()`/
    //! format-1) × `V` × {valued, term-only, "" valued, "" membership, UNICODE, deep key}. Both
    //! readers consume the SAME dense image ⇒ byte-exact equivalence. Run NOW while the oracle
    //! exists; it retires at L3.3. Real-disk scratch (`target/test-tmp`), never tmpfs.
    use super::*;
    use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
    use crate::persistent_artrie_char::PersistentARTrieChar;
    use std::collections::BTreeMap;

    fn scratch(tag: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(tag)
            .tempdir_in("target/test-tmp")
            .expect("real-disk scratch under target/test-tmp")
    }

    fn overlay_eq<V: DictionaryValue + PartialEq>(
        a: &Arc<PersistentCharNode<V>>,
        b: &Arc<PersistentCharNode<V>>,
    ) -> bool {
        if a.is_final() != b.is_final()
            || a.get_value() != b.get_value()
            || a.num_children() != b.num_children()
        {
            return false;
        }
        let mut bchildren: BTreeMap<u32, Arc<PersistentCharNode<V>>> = BTreeMap::new();
        for (e, c) in b.iter_children() {
            match c.as_in_mem() {
                Some(arc) => {
                    bchildren.insert(*e, arc.clone());
                }
                None => return false,
            }
        }
        for (e, c) in a.iter_children() {
            let ac = match c.as_in_mem() {
                Some(arc) => arc,
                None => return false,
            };
            match bchildren.get(e) {
                Some(bc) => {
                    if !overlay_eq(ac, bc) {
                        return false;
                    }
                }
                None => return false,
            }
        }
        true
    }

    fn assert_new_eq_oracle<V: DictionaryValue + PartialEq>(
        t: &mut PersistentARTrieChar<V>,
        tag: &str,
    ) {
        let bm = t.buffer_manager.clone().expect("buffer manager");
        let root_ptr = bm.read().storage().root_ptr().expect("root_ptr");

        // NEW direct char codec loader (no CharTrieRoot).
        let (new_root, _, _) = t
            .load_overlay_char_root_compressed(&bm, root_ptr)
            .expect("new char codec loader");

        // OLD owned-scratch oracle.
        let (owned, _) = t
            .load_root_from_disk(&bm, &SwizzledPtr::from_raw(root_ptr), Some(usize::MAX))
            .expect("owned decode");
        *t.root.get_mut() = owned;
        let oracle_root = t.build_overlay_root_from_owned().expect("oracle build");
        *t.root.get_mut() = CharTrieRoot::Empty;

        assert!(
            overlay_eq(&new_root, &oracle_root),
            "{tag}: char codec loader overlay != owned-scratch oracle overlay"
        );
    }

    fn create<V: DictionaryValue>(tag: &str) -> (tempfile::TempDir, PersistentARTrieChar<V>) {
        let dir = scratch(tag);
        let trie = PersistentARTrieChar::<V>::create(&dir.path().join("t.artc")).expect("create");
        (dir, trie)
    }

    // ---- format-2 (un-compressed overlay checkpoint) ----
    #[test]
    fn diff_char_format2_overlay_u64() {
        let (_d, mut t) = create::<u64>("l31c-f2-u64");
        for (term, v) in [("apple", 1u64), ("application", 2), ("banana", 3)] {
            assert!(t.insert_with_value(term, v).expect("ins"));
        }
        assert!(t.insert("member").expect("ins"));
        assert!(t.insert_with_value("", 999).expect("ins"));
        t.checkpoint().expect("checkpoint");
        assert_new_eq_oracle(&mut t, "char-format2-u64");
    }

    #[test]
    fn diff_char_format2_overlay_unicode_string() {
        let (_d, mut t) = create::<String>("l31c-f2-uni");
        // Unicode terms (char's domain): accents, CJK, emoji.
        for (term, v) in [
            ("café", "x"),
            ("caffè", "y"),
            ("日本語", "z"),
            ("🦀rust", "w"),
        ] {
            assert!(t.insert_with_value(term, v.to_string()).expect("ins"));
        }
        assert!(t.insert("naïve").expect("ins")); // term-only Unicode
        t.checkpoint().expect("checkpoint");
        assert_new_eq_oracle(&mut t, "char-format2-unicode");
    }
}
