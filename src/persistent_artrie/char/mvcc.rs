//! Char-variant `TrieRoot` implementation for MVCC snapshot reads.
//!
//! Holds `impl TrieRoot for PersistentCharNode` so the byte-variant
//! `persistent_artrie::mvcc` module does not have to import
//! `crate::persistent_artrie::char::nodes::PersistentCharNode`, which was the
//! previous byte→char back-edge.

// G4 Phase 6: this per-variant `impl TrieRoot for PersistentCharNode<V>` is
// SUPERSEDED by the single blanket `impl<K, V> TrieRoot for OverlayNode<K, V>` in
// `crate::persistent_artrie::core::overlay` (the DRY bonus). Because
// `PersistentCharNode<V>` is now the alias `OverlayNode<CharKey, V>`, the blanket
// already covers it with the identical `Key=u32, Value=V` — keeping this impl
// would be a duplicate-impl coherence error. Commented out (not deleted) per
// project policy, with the provenance pointer above.
//
// use std::sync::Arc;
// use crate::persistent_artrie::char::nodes::PersistentCharNode;
// use crate::persistent_artrie::core::mvcc::TrieRoot;
//
// impl<V: Clone + Send + Sync + 'static> TrieRoot for PersistentCharNode<V> {
//     type Key = u32;
//     type Value = V;
//     fn is_final(&self) -> bool { PersistentCharNode::is_final(self) }
//     fn find_child(&self, key: u32) -> Option<Arc<Self>> {
//         PersistentCharNode::find_child(self, key).and_then(|child| child.as_in_mem().map(Arc::clone))
//     }
//     fn get_value(&self) -> Option<V> { PersistentCharNode::get_value(self) }
// }
