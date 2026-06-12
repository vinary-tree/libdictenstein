//! Byte-variant `TrieRoot` implementation for MVCC snapshot reads.
//!
//! The generic MVCC primitives (`MvccStats`, `MvccStatsTracker`, `TrieRoot`
//! trait, `ReadTransaction<T>`, `EpochGuard`) live in
//! [`crate::persistent_artrie::core::mvcc`] and are re-exported here for
//! backward-compatible call-sites. The byte-side `impl TrieRoot` is, as of G4
//! Phase 6, provided by the blanket `impl<K, V> TrieRoot for OverlayNode<K, V>`
//! in `crate::persistent_artrie::core::overlay` (the byte node is now its
//! `<ByteKey>` alias), so this module is pure re-export plumbing.

// `use std::sync::Arc;` removed — the only consumer was the now-superseded
// per-variant `TrieRoot` impl (commented out below).

// Re-export the generic primitives so existing `persistent_artrie::mvcc::*`
// imports keep working unchanged.
pub use crate::persistent_artrie::core::mvcc::{
    EpochGuard, MvccStats, MvccStatsTracker, ReadTransaction, TrieRoot,
};

// G4 Phase 6: this per-variant `impl TrieRoot for PersistentNode<i64>` is
// SUPERSEDED by the single blanket `impl<K, V> TrieRoot for OverlayNode<K, V>` in
// `crate::persistent_artrie::core::overlay` (the DRY bonus). Because
// `PersistentNode<V>` is now the alias `OverlayNode<ByteKey, V>`, the blanket
// already covers `OverlayNode<ByteKey, i64>` with the identical `Key=u8,
// Value=i64` — keeping this impl would be a duplicate-impl coherence error.
// Commented out (not deleted) per project policy, with the provenance pointer.
// The generic MVCC re-exports above (`EpochGuard`, `ReadTransaction`, `TrieRoot`,
// …) STAY so `persistent_artrie::mvcc::*` call-sites keep resolving.
//
// use crate::persistent_artrie::nodes::PersistentNode;
//
// impl TrieRoot for PersistentNode<i64> {
//     type Key = u8;
//     type Value = i64;
//     fn is_final(&self) -> bool { PersistentNode::is_final(self) }
//     fn find_child(&self, key: u8) -> Option<Arc<Self>> {
//         PersistentNode::find_child(self, key).and_then(|child| child.as_in_mem().map(Arc::clone))
//     }
//     fn get_value(&self) -> Option<i64> { PersistentNode::get_value(self) }
// }
