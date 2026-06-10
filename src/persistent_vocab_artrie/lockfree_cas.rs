//! Lock-free CAS path helpers for `PersistentVocabARTrie` — the OVERLAY write primitives (V6).
//!
//! The public toggle (`install_overlay` is now `pub(crate)`, called only by the flip seam) +
//! the owned `insert_cas`/`is_lockfree_enabled`/`merge_lockfree_to_persistent` are deleted. The
//! immutable-trie CAS walk (`try_insert_lockfree_path` / `insert_lockfree_recursive` /
//! `create_lockfree_path` / `find_in_lockfree_trie`) is RETAINED — it is the structural-sharing
//! insert/lookup the overlay write path (`overlay_write_mode`) builds on.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use dashmap::DashMap;

use crate::persistent_artrie_char::nodes::persistent_node::Child as ChildGeneric;
use crate::persistent_artrie_char::nodes::{
    AtomicNodePtr as AtomicNodePtrGeneric, PersistentCharNode as PersistentCharNodeGeneric,
};

// Vocab overlay node value = `u64` vocabulary index (G1: char overlay node is now
// generic; the vocab instantiates it at `V = u64`).
type Child = ChildGeneric<u64>;
type AtomicNodePtr = AtomicNodePtrGeneric<u64>;
type PersistentCharNode = PersistentCharNodeGeneric<u64>;

impl<S: crate::persistent_artrie::block_storage::BlockStorage>
    super::dict_impl::PersistentVocabARTrie<S>
{
    /// Install the lock-free overlay infrastructure (root + cache + reverse map).
    ///
    /// `pub(crate)`: the ONLY caller is the flip seam (`flip_to_overlay` →
    /// `LockFreeOverlay::install_overlay`), which every production ctor runs at construction.
    /// Returns `true` if newly enabled, `false` if already enabled.
    pub(crate) fn install_overlay(&mut self) -> bool {
        if self.lockfree_root.is_some() {
            return false;
        }

        let root = Arc::new(PersistentCharNode::new());
        self.lockfree_root = Some(AtomicNodePtr::new(root));
        self.lockfree_cache = Some(DashMap::new());
        // The overlay's reverse index (id -> term) — the NON-BLOCKING inverse used by `get_term`.
        // Populated by `insert_overlay`; rebuilt from the image on reopen.
        self.reverse_term_map = Some(DashMap::new());

        true
    }

    /// Try to create a new root with the term inserted (lock-free version).
    ///
    /// Returns `Ok(new_root)` if successful, `Err(existing_idx)` if term already exists.
    pub(super) fn try_insert_lockfree_path(
        &self,
        root: &Arc<PersistentCharNode>,
        chars: &[u32],
        index: u64,
    ) -> std::result::Result<Arc<PersistentCharNode>, u64> {
        if chars.is_empty() {
            // Empty term - mark root as final
            if root.is_final() {
                return Err(root.get_value().unwrap_or(0));
            }
            let new_root = root.as_final().with_value(index);
            return Ok(Arc::new(new_root));
        }

        // Recursively create the path
        self.insert_lockfree_recursive(root, chars, 0, index)
    }

    /// Recursively create new nodes along the path (lock-free version).
    fn insert_lockfree_recursive(
        &self,
        node: &Arc<PersistentCharNode>,
        chars: &[u32],
        depth: usize,
        index: u64,
    ) -> std::result::Result<Arc<PersistentCharNode>, u64> {
        if depth == chars.len() {
            // Reached the end - mark as final
            if node.is_final() {
                return Err(node.get_value().unwrap_or(0));
            }
            let new_node = node.as_final().with_value(index);
            return Ok(Arc::new(new_node));
        }

        let c = chars[depth];

        match node.find_child(c) {
            Some(child) => {
                // Child exists - recurse
                if child.is_null() {
                    return Err(0); // Shouldn't happen
                }

                match child.as_in_mem() {
                    Some(child_arc) => {
                        let child_arc = Arc::clone(child_arc);

                        // Recurse into child
                        let new_child =
                            self.insert_lockfree_recursive(&child_arc, chars, depth + 1, index)?;

                        // Create new node owning the updated child by `Arc`
                        // (no raw-pointer smuggling).
                        let new_node = node.with_child(c, Child::InMem(new_child));
                        Ok(Arc::new(new_node))
                    }
                    None => {
                        // On-disk child - not supported in lock-free mode yet
                        Err(0)
                    }
                }
            }
            None => {
                // Child doesn't exist - create new path
                let new_child = self.create_lockfree_path(&chars[depth + 1..], index);
                let new_node = node.with_child(c, Child::InMem(new_child));
                Ok(Arc::new(new_node))
            }
        }
    }

    /// Create a new path from the remaining characters (lock-free version).
    fn create_lockfree_path(&self, chars: &[u32], index: u64) -> Arc<PersistentCharNode> {
        if chars.is_empty() {
            // Create final node with value
            let node = PersistentCharNode::new().as_final().with_value(index);
            return Arc::new(node);
        }

        // Build path bottom-up
        let mut current = Arc::new(PersistentCharNode::new().as_final().with_value(index));

        for &c in chars.iter().rev() {
            // Each parent owns its child by `Arc` (no raw-pointer smuggling).
            let parent = PersistentCharNode::new().with_child(c, Child::InMem(current));
            current = Arc::new(parent);
        }

        current
    }

    /// Find a term in the lock-free trie, returning its index if found.
    pub(super) fn find_in_lockfree_trie(
        &self,
        root: &Arc<PersistentCharNode>,
        chars: &[u32],
    ) -> Option<u64> {
        let mut current = root.clone();

        for &c in chars {
            match current.find_child(c) {
                Some(child) => {
                    if child.is_null() {
                        return None;
                    }
                    // On-disk children are not traversable here → `None`.
                    match child.as_in_mem() {
                        Some(child_arc) => current = Arc::clone(child_arc),
                        None => return None,
                    }
                }
                None => return None,
            }
        }

        current.get_value()
    }

    /// Get CAS retry statistics for monitoring lock contention.
    #[inline]
    pub fn cas_retries(&self) -> u64 {
        self.cas_retries.load(Ordering::Relaxed)
    }
}
