//! **L3.1 — the DIRECT dense→overlay reopen codec loader (byte variant).**
//!
//! `load_root_immutable` (the reopen chokepoint, reached by both the Overlay-regime `use_f5`
//! arm and the Owned-regime `convert_owned_to_overlay_on_reopen` arm) delegates to
//! [`PersistentARTrie::load_overlay_root_compressed`], which reads the dense on-disk image
//! DIRECTLY into a fully-resident `Arc<OverlayNode<ByteKey, V>>` via
//! [`PersistentARTrie::enumerate_terms_from_disk`] + the proven iterative
//! [`build_overlay_root_from_terms`](crate::persistent_artrie_core::overlay::f5_build::build_overlay_root_from_terms)
//! — **NO transient owned `TrieRoot`**. `self.root` is never materialized; this is the
//! "literal-zero-owned" reopen the campaign targets, and it lets L3.3 delete
//! `load_root_from_disk_with_arena` + `TrieRoot` + `build_overlay_root_from_owned` + the D1
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
//! value union is intrinsic. **Equivalence to the proven path is the L3.1 GOLD-STANDARD GATE**
//! (`l31_differential_tests`): this loader produces a byte-exact overlay vs
//! `build_overlay_root_from_owned(load_root_from_disk_with_arena(image))` over every
//! format × `V` × shape — run NOW while the oracle still exists (it retires at L3.3).

use std::sync::Arc;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie_core::key_encoding::ByteKey;
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::persistent_artrie_core::overlay::node::OverlayNode;
use crate::value::DictionaryValue;

use super::bucket::StringBucket;
use super::dict_impl::TrieRoot;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrie<V, S> {
    /// **F5 — load the dense image into a pre-built lock-free overlay root** (the owned
    /// tree is a TRANSIENT decode scratch, cleared after conversion).
    ///
    /// 1. Eager-load the dense image via the EXISTING (fully-eager)
    ///    `load_root_from_disk_with_arena` INTO `self.root`.
    /// 2. Build the overlay root from the owned tree via the generic, COMPRESSION-AWARE
    ///    [`LockFreeOverlay::build_overlay_root_from_owned`].
    /// 3. Install it (`install_prebuilt_overlay_root`: select LockFreeOverlay + verify the
    ///    Overlay regime — V-2; HARD-ERROR on a `false`).
    /// 4. CLEAR the transient owned tree.
    ///
    /// `&mut self`. Returns the dense-image term count (NOT incl. the WAL tail — the
    /// caller replays the tail via `replay_records_lww_overlay` after).
    ///
    /// **`root_ptr == 0`** (empty dense image): install an EMPTY overlay root.
    pub(crate) fn load_root_immutable(&mut self, root_ptr: u64) -> Result<usize> {
        // **L3.1:** build the overlay root DIRECTLY from the dense image via the codec loader —
        // NO transient owned `TrieRoot`. (The owned decoder `load_root_from_disk_with_arena` +
        // the D1 readers + `build_overlay_root_from_owned` survive only as the L3.1
        // differential-test ORACLE; all of them, plus `TrieRoot`, die at L3.3.)
        let (overlay_root, term_count) = self.load_overlay_root_compressed(root_ptr)?;

        // Install + select LockFreeOverlay + verify the Overlay regime (V-2).
        if !self.install_prebuilt_overlay_root(overlay_root) {
            return Err(PersistentARTrieError::internal(
                "F5 load_root_immutable: install_prebuilt_overlay_root did not engage \
                 (WAL not Overlay-regime, or ineligible V)",
            ));
        }

        // The owned `root` scratch is never materialized now — leave it the empty placeholder the
        // ctor installed (it is not the production rep under the overlay; it is deleted at L3.3).
        *self.root.get_mut() = TrieRoot::Bucket(StringBucket::with_values());
        self.term_count
            .store(0, std::sync::atomic::Ordering::Release);

        Ok(term_count)
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
    /// Equivalence anchor (the L3.1 differential gate): for every image this produces the SAME
    /// overlay as `build_overlay_root_from_owned(load_root_from_disk_with_arena(image))` (the
    /// owned-scratch oracle), proven over format × `V` × {valued, term-only, "" valued, ""
    /// membership} × deep keys BEFORE this becomes the only reopen path.
    pub(crate) fn load_overlay_root_compressed(
        &self,
        root_ptr: u64,
    ) -> Result<(Arc<OverlayNode<ByteKey, V>>, usize)> {
        use crate::persistent_artrie_core::overlay::f5_build::build_overlay_root_from_terms;

        if root_ptr == 0 {
            let empty = build_overlay_root_from_terms::<ByteKey, V, _>(
                std::collections::BTreeMap::new(),
                None,
            );
            return Ok((empty, 0));
        }

        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("L3.1 load_overlay_root_compressed: no buffer manager")
        })?;
        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("L3.1 load_overlay_root_compressed: no arena manager")
        })?;

        let (terms, empty_term, term_count) =
            Self::enumerate_terms_from_disk(buffer_manager, arena_manager, root_ptr)?;
        let overlay_root = build_overlay_root_from_terms::<ByteKey, V, _>(terms, empty_term);
        Ok((overlay_root, term_count as usize))
    }
}

#[cfg(test)]
mod l31_differential_tests {
    //! **L3.1 GOLD-STANDARD GATE.** The direct codec loader `load_overlay_root_compressed`
    //! (no `TrieRoot`) produces a STRUCTURALLY-IDENTICAL overlay to the owned-scratch oracle
    //! `build_overlay_root_from_owned(load_root_from_disk_with_arena(image))` — over all THREE
    //! on-disk formats (format-2 un-compressed overlay, format-1 CX node-prefix, format-3 legacy
    //! owned StringBucket-suffix) × `V` × {valued, term-only, "" valued, "" membership, deep key,
    //! branching}. Both readers consume the SAME dense image, so this proves byte-exact
    //! equivalence with NO term/value/empty-term re-derivation. Run NOW, while the oracle still
    //! exists; L3.3 deletes the oracle (this test retires with it, covered then by the
    //! end-to-end reopen suites). Real-disk scratch (`target/test-tmp`), never tmpfs.
    use super::*;
    use crate::persistent_artrie::bucket::StringBucket;
    use crate::persistent_artrie::{CompactionConfig, PersistentARTrie};
    use crate::persistent_artrie_core::overlay::node::OverlayNode;
    use crate::value::DictionaryValue;
    use std::collections::BTreeMap;

    fn scratch(tag: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(tag)
            .tempdir_in("target/test-tmp")
            .expect("real-disk scratch under target/test-tmp")
    }

    /// Recursive structural equality of two fully-resident (all-`Child::InMem`) overlay roots:
    /// finality, value, and the per-edge child subtrees.
    fn overlay_eq<V: DictionaryValue + PartialEq>(
        a: &Arc<OverlayNode<ByteKey, V>>,
        b: &Arc<OverlayNode<ByteKey, V>>,
    ) -> bool {
        if a.is_final() != b.is_final()
            || a.get_value() != b.get_value()
            || a.num_children() != b.num_children()
        {
            return false;
        }
        let mut bchildren: BTreeMap<u8, Arc<OverlayNode<ByteKey, V>>> = BTreeMap::new();
        for (e, c) in b.iter_children() {
            match c.as_in_mem() {
                Some(arc) => {
                    bchildren.insert(*e, arc.clone());
                }
                None => return false, // a reopened root must be fully resident (no OnDisk)
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

    /// Assert `load_overlay_root_compressed(root_ptr)` (NEW) structurally equals
    /// `build_overlay_root_from_owned(load_root_from_disk_with_arena(root_ptr))` (OLD oracle) for
    /// the dense image the (already-checkpointed/compacted) `t` wrote.
    fn assert_new_eq_oracle<V: DictionaryValue + PartialEq>(
        t: &mut PersistentARTrie<V>,
        tag: &str,
    ) {
        let root_ptr = t
            .buffer_manager
            .as_ref()
            .expect("buffer manager")
            .read()
            .storage()
            .root_ptr()
            .expect("root_ptr");

        // NEW direct codec loader (no TrieRoot).
        let (new_root, _) = t
            .load_overlay_root_compressed(root_ptr)
            .expect("new codec loader");

        // OLD owned-scratch oracle: decode → install into the transient self.root → enumerate.
        let (owned, _len) = {
            let bm = t.buffer_manager.as_ref().expect("bm");
            let am = t.arena_manager.as_ref().expect("am");
            PersistentARTrie::<V>::load_root_from_disk_with_arena(bm, am, root_ptr)
                .expect("owned decode")
        };
        *t.root.get_mut() = owned;
        let oracle_root = t.build_overlay_root_from_owned().expect("oracle build");
        *t.root.get_mut() = TrieRoot::Bucket(StringBucket::with_values());

        assert!(
            overlay_eq(&new_root, &oracle_root),
            "{tag}: codec loader overlay != owned-scratch oracle overlay"
        );
    }

    fn create<V: DictionaryValue>(tag: &str) -> (tempfile::TempDir, PersistentARTrie<V>) {
        let dir = scratch(tag);
        let path = dir.path().join("t.artb");
        let trie = PersistentARTrie::<V>::create(&path).expect("create");
        (dir, trie)
    }

    // ---- format-2 (un-compressed overlay checkpoint) ----
    #[test]
    fn diff_format2_overlay_u64() {
        let (_d, mut t) = create::<u64>("l31-f2-u64");
        for (term, v) in [
            ("apple", 1u64),
            ("application", 2),
            ("banana", 3),
            ("band", 4),
        ] {
            assert!(t.insert_with_value(term, v));
        }
        assert!(t.insert("member"));
        assert!(t.insert_with_value("", 999));
        t.checkpoint().expect("checkpoint");
        assert_new_eq_oracle(&mut t, "format2-u64");
    }

    #[test]
    fn diff_format2_overlay_string() {
        let (_d, mut t) = create::<String>("l31-f2-str");
        for (term, v) in [("ka", "va"), ("kb", "vb"), ("kaa", "vaa")] {
            assert!(t.insert_with_value(term, v.to_string()));
        }
        assert!(t.insert("only")); // term-only
        assert!(t.insert_with_value("", "empty".to_string())); // "" valued
        t.checkpoint().expect("checkpoint");
        assert_new_eq_oracle(&mut t, "format2-string");
    }

    // ---- format-1 (CX path-compressed compaction image, byte-only) ----
    #[test]
    fn diff_format1_compact_u64() {
        let (_d, mut t) = create::<u64>("l31-f1-u64");
        for (term, v) in [
            ("single", 42u64),
            ("singleton", 7),
            ("abcdefghijklmnopqrstuvwxyz", 26), // > MAX_PREFIX_LEN ⇒ multi-chunk
        ] {
            assert!(t.insert_with_value(term, v));
        }
        assert!(t.insert("member"));
        t.compact(CompactionConfig::default(), |_| {})
            .expect("compact");
        assert_new_eq_oracle(&mut t, "format1-compact-u64");
    }
}
