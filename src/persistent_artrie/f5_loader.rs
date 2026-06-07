//! **F5 (Slice 3) — the direct dense→overlay reopen loader (byte variant).**
//!
//! The byte twin of `persistent_artrie_char::f5_loader`. `load_root_immutable` is the
//! F5-B loader: it reuses the EXISTING (already fully-eager) owned loader
//! [`PersistentARTrie::load_root_from_disk_with_arena`] to decode the dense disk image
//! into the owned `TrieRoot<V>` (a TRANSIENT scratch in `self.root`), then the generic,
//! COMPRESSION-AWARE converter
//! [`crate::persistent_artrie_core::overlay::flip::LockFreeOverlay::build_overlay_root_from_owned`]
//! turns it into a single `Arc<PersistentNode<V>>` (deep-term-safe, iterative). The
//! result is installed as the live lock-free overlay via `install_prebuilt_overlay_root`,
//! and the transient owned tree is CLEARED.
//!
//! F5 ADDS this loader ALONGSIDE the existing reopen path; it is **gated** by
//! [`LockFreeOverlay::USE_F5_REOPEN_LOADER`] and exercised by the S2 tests via
//! `open_with_f5_loader`. The owned tree / mutators / legacy reopen path are NOT deleted
//! (that is F7).
//!
//! # Why the converter goes through the OWNED-term enumeration (not a node walk)
//!
//! An Overlay-regime byte file's dense image is USUALLY un-path-compressed (the byte
//! overlay serializer writes N4/N16/N48/N256 nodes, one per unit, no `StringBucket`), but
//! a **COMPACTED** Overlay file is PATH-COMPRESSED (C-opt-1: `compact()` rebuilds a dense
//! owned image via the owned-staging path — buckets + compressed prefixes — then re-stamps
//! the Overlay regime). The generic converter enumerates the owned tree via the proven D1
//! owned readers (which EXPAND `StringBucket` suffixes + compressed prefixes) and builds
//! the overlay from the `(term-units, value)` enumeration, so the compression handling
//! lives ONCE in the existing readers — no new data-loss-critical expansion code. See
//! `docs/design/slice3-f5-loader-impl.md`.

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie_core::key_encoding::ByteKey;
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
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
        // (1) Eager-load the dense image into the TRANSIENT owned tree.
        let term_count = if root_ptr == 0 {
            0usize
        } else {
            let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
                PersistentARTrieError::internal("F5 load_root_immutable: no buffer manager")
            })?;
            let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
                PersistentARTrieError::internal("F5 load_root_immutable: no arena manager")
            })?;
            let (owned_root, len) =
                Self::load_root_from_disk_with_arena(buffer_manager, arena_manager, root_ptr)?;
            *self.root.get_mut() = owned_root;
            self.term_count
                .store(len as usize, std::sync::atomic::Ordering::Release);
            len as usize
        };

        // (2) Build the overlay root from the owned tree (compression-aware, iterative).
        let overlay_root =
            <Self as LockFreeOverlay<ByteKey, V, S>>::build_overlay_root_from_owned(self)?;

        // (3) Install + select LockFreeOverlay + verify the Overlay regime (V-2).
        if !self.install_prebuilt_overlay_root(overlay_root) {
            return Err(PersistentARTrieError::internal(
                "F5 load_root_immutable: install_prebuilt_overlay_root did not engage \
                 (WAL not Overlay-regime, or ineligible V)",
            ));
        }

        // (4) Clear the transient owned tree (F5 does not leave it materialized).
        *self.root.get_mut() = TrieRoot::Bucket(StringBucket::with_values());
        self.term_count
            .store(0, std::sync::atomic::Ordering::Release);

        Ok(term_count)
    }
}
