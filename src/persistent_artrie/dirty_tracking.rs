//! Dirty-path tracking for selective persistence.
//!
//! Split out of byte `dict_impl.rs` (lines ~434-580, ~147 LOC) as
//! the twenty-fourth Phase-5 byte sub-module. These helpers
//! coordinate the "selective dirty subtree" optimization that lets
//! `serialize_impl::persist_to_disk` skip serializing clean subtrees:
//!
//! - `record_dirty_path` — marks all prefixes of a path as dirty
//!   (invalidating cached disk locations for re-serialization)
//! - `path_needs_persistence` — checks if a path is in the dirty set
//! - `propagate_dirty_to_root` — sets HAS_DIRTY_DESCENDANTS on the root
//! - `cache_disk_location` / `get_cached_disk_location` — manage the
//!   disk-location cache for clean-subtree skipping
//! - `resolve_and_cache_disk_location` — DiskRef resolution wrapper
//!   that caches the original location before mutation
//! - `clear_dirty_tracking_state` — post-checkpoint cleanup
//! - `clear_dirty_flags_recursive` / `clear_child_dirty_flags_recursive`
//!   — recursive flag-clearing
//!
//! All methods are `pub(super)` so the sibling modules
//! (`mutation_api`, `serialize_impl`, etc.) can call them.

use crate::value::DictionaryValue;

use super::block_storage::BlockStorage;
use super::dict_impl::{resolve_child_for_mutation_with_bm, PersistentARTrie, TrieRoot};
use super::swizzled_ptr::SwizzledPtr;
use super::transitions::ChildNode;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Record a path as dirty for selective persistence.
    ///
    /// Records all prefixes of the given path in `dirty_prefixes` and
    /// invalidates corresponding cached disk locations, since those nodes
    /// will need re-serialization.
    ///
    /// **F4:** `&self` — the dirty-prefix set is now a `Mutex`. Take the
    /// `persisted_disk_locations` `RwLock` and the `dirty_prefixes` `Mutex` in that
    /// order; never held across the OR root lock (the owned mutators DROP the OR
    /// guard before calling this).
    #[inline]
    pub(super) fn record_dirty_path(&self, path: &[u8]) {
        let mut cache = self.persisted_disk_locations.write();
        let mut dirty = self
            .dirty_prefixes
            .lock()
            .expect("dirty_prefixes mutex poisoned");
        for len in 0..=path.len() {
            let prefix = path[..len].to_vec();
            dirty.insert(prefix.clone());
            cache.remove(&prefix);
        }
    }

    /// Check if a path needs persistence.
    #[inline]
    pub(super) fn path_needs_persistence(&self, path: &[u8]) -> bool {
        self.dirty_prefixes
            .lock()
            .expect("dirty_prefixes mutex poisoned")
            .contains(path)
    }

    /// Propagate dirty flags up the ancestor chain.
    ///
    /// Sets the HAS_DIRTY_DESCENDANTS flag on the root node when any
    /// modification is made. For nested ART nodes along the path, the
    /// flag propagation happens during the serialization phase.
    ///
    /// **F4:** `&self` taking the OR write lock. The hot owned-insert/remove paths
    /// fold this inline (under their already-held OR guard, to avoid re-locking);
    /// this stand-alone form is retained for any other caller.
    #[allow(dead_code)]
    pub(super) fn propagate_dirty_to_root(&self) {
        if let TrieRoot::ArtNode { node, .. } = &mut *self.root.write() {
            node.header_mut().set_has_dirty_descendants(true);
        }
    }

    /// Cache a disk location for a path.
    #[inline]
    pub(super) fn cache_disk_location(&self, path: &[u8], ptr: SwizzledPtr) {
        self.persisted_disk_locations
            .write()
            .insert(path.to_vec(), ptr);
    }

    /// Get a cached disk location for a path if it exists and the subtree is clean.
    #[inline]
    pub(super) fn get_cached_disk_location(&self, path: &[u8]) -> Option<SwizzledPtr> {
        if self
            .dirty_prefixes
            .lock()
            .expect("dirty_prefixes mutex poisoned")
            .contains(path)
        {
            None
        } else {
            self.persisted_disk_locations.read().get(path).cloned()
        }
    }

    /// Resolve a DiskRef child for mutation, caching the original location.
    #[allow(dead_code)]
    pub(super) fn resolve_and_cache_disk_location(
        &self,
        child: &mut ChildNode,
        path: &[u8],
    ) -> bool {
        if let ChildNode::DiskRef { ptr } = child {
            self.cache_disk_location(path, ptr.clone());
        }

        resolve_child_for_mutation_with_bm(child, self.buffer_manager.as_ref())
    }

    /// Clear dirty tracking state after a successful checkpoint.
    ///
    /// **F4:** `&self` — clears the `dirty_prefixes` `Mutex` then the in-tree flags
    /// under the OR write lock. Released-and-reacquired (the Mutex first, then OR)
    /// — never both at once.
    pub(super) fn clear_dirty_tracking_state(&self) {
        self.dirty_prefixes
            .lock()
            .expect("dirty_prefixes mutex poisoned")
            .clear();
        self.clear_dirty_flags_recursive();
    }

    /// Recursively clear dirty flags on all nodes in the trie.
    ///
    /// **F4:** `&self` taking the OR write lock for the recursive flag clear.
    fn clear_dirty_flags_recursive(&self) {
        match &mut *self.root.write() {
            TrieRoot::Bucket(_) => {}
            TrieRoot::ArtNode { node, children, .. } => {
                node.header_mut().clear_dirty_flags();
                for (_, child) in children {
                    Self::clear_child_dirty_flags_recursive(child);
                }
            }
        }
    }

    /// Recursively clear dirty flags on a child node and its descendants.
    fn clear_child_dirty_flags_recursive(child: &mut ChildNode) {
        match child {
            ChildNode::Bucket(_) => {}
            ChildNode::ArtNode { node, children, .. } => {
                node.header_mut().clear_dirty_flags();
                for (_, c) in children {
                    Self::clear_child_dirty_flags_recursive(c);
                }
            }
            ChildNode::DiskRef { .. } => {}
        }
    }
}
