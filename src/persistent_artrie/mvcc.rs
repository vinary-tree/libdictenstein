//! Byte-variant `TrieRoot` implementation for MVCC snapshot reads.
//!
//! The generic MVCC primitives (`MvccStats`, `MvccStatsTracker`, `TrieRoot`
//! trait, `ReadTransaction<T>`, `EpochGuard`) live in
//! [`crate::persistent_artrie_core::mvcc`] and are re-exported here for
//! backward-compatible call-sites. This module adds only the byte-side
//! `impl TrieRoot for PersistentNode`.

use std::sync::Arc;

// Re-export the generic primitives so existing `persistent_artrie::mvcc::*`
// imports keep working unchanged.
pub use crate::persistent_artrie_core::mvcc::{
    EpochGuard, MvccStats, MvccStatsTracker, ReadTransaction, TrieRoot,
};

use crate::persistent_artrie::nodes::PersistentNode;

impl TrieRoot for PersistentNode {
    type Key = u8;

    fn is_final(&self) -> bool {
        PersistentNode::is_final(self)
    }

    fn find_child(&self, key: u8) -> Option<Arc<Self>> {
        if let Some(child_ptr) = PersistentNode::find_child(self, key) {
            if !child_ptr.is_on_disk() {
                if let Some(ptr) = child_ptr.as_ptr::<PersistentNode>() {
                    unsafe {
                        Arc::increment_strong_count(ptr);
                        return Some(Arc::from_raw(ptr));
                    }
                }
            }
        }
        None
    }

    fn get_value(&self) -> Option<u64> {
        PersistentNode::get_value(self)
    }
}
