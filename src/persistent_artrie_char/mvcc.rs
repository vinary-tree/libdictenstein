//! Char-variant `TrieRoot` implementation for MVCC snapshot reads.
//!
//! Holds `impl TrieRoot for PersistentCharNode` so the byte-variant
//! `persistent_artrie::mvcc` module does not have to import
//! `crate::persistent_artrie_char::nodes::PersistentCharNode`, which was the
//! previous byte→char back-edge.

use std::sync::Arc;

use crate::persistent_artrie_char::nodes::PersistentCharNode;
use crate::persistent_artrie_core::mvcc::TrieRoot;

impl TrieRoot for PersistentCharNode {
    type Key = u32;

    fn is_final(&self) -> bool {
        PersistentCharNode::is_final(self)
    }

    fn find_child(&self, key: u32) -> Option<Arc<Self>> {
        if let Some(child_ptr) = PersistentCharNode::find_child(self, key) {
            if !child_ptr.is_on_disk() {
                if let Some(ptr) = child_ptr.as_ptr::<PersistentCharNode>() {
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
        PersistentCharNode::get_value(self)
    }
}
