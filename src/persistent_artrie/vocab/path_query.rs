//! Path-based query API for `PersistentVocabARTrie<S>` — OVERLAY-ONLY (V6).
//!
//! The owned tree is deleted; every method walks the lock-free overlay:
//! - `get_root_children` / `get_children_at_path` — children (label, is_final) of a node
//! - `is_final_at_path` — predicate over the node reached by a path
//! - `iter_terms` / `iter_terms_with_prefix` — full-term enumeration
//!
//! `get_*`/`is_final_*` use `overlay_navigate` (O(path) descent); `iter_*` use
//! `overlay_collect_units` (DFS under a prefix). All in `K::Unit` (u32) space.

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::core::overlay::flip::LockFreeOverlay;

impl<S: BlockStorage> super::dict_impl::PersistentVocabARTrie<S> {
    /// Children `(label, is_final)` of the overlay root — backs `Dictionary::root`.
    pub fn get_root_children(&self) -> Vec<(char, bool)> {
        self.get_children_at_path(&[])
    }

    /// Children `(label, is_final)` of the overlay node reached by `path`.
    pub fn get_children_at_path(&self, path: &[char]) -> Vec<(char, bool)> {
        let units: Vec<u32> = path.iter().map(|&c| c as u32).collect();
        let Some(node) = self.overlay_navigate(&units) else {
            return Vec::new();
        };
        node.iter_children()
            .filter_map(|(key, child)| {
                // Vocab never evicts (OverlayFaulter::fault_overlay_slot -> None), so every
                // overlay child is in-mem; an on-disk child (impossible here) is skipped.
                let child_node = child.as_in_mem()?;
                char::from_u32(*key).map(|c| (c, child_node.is_final()))
            })
            .collect()
    }

    /// Whether the overlay node reached by `path` is final (a stored term).
    pub fn is_final_at_path(&self, path: &[char]) -> bool {
        let units: Vec<u32> = path.iter().map(|&c| c as u32).collect();
        self.overlay_navigate(&units)
            .map(|node| node.is_final())
            .unwrap_or(false)
    }

    /// Iterate over all terms in the vocabulary (overlay enumeration).
    pub fn iter_terms(&self) -> impl Iterator<Item = String> + '_ {
        let terms: Vec<String> = self
            .overlay_collect_units(&[])
            .unwrap_or_default()
            .into_iter()
            .map(|u| u.iter().filter_map(|&c| char::from_u32(c)).collect())
            .collect();
        terms.into_iter()
    }

    /// Iterate over terms with the given prefix (overlay enumeration).
    pub fn iter_terms_with_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> impl Iterator<Item = String> + 'a {
        let prefix_units: Vec<u32> = prefix.chars().map(|c| c as u32).collect();
        let terms: Vec<String> = self
            .overlay_collect_units(&prefix_units)
            .unwrap_or_default()
            .into_iter()
            .map(|u| u.iter().filter_map(|&c| char::from_u32(c)).collect())
            .collect();
        terms.into_iter()
    }
}
