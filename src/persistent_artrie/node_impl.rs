//! Dictionary Node alias for Persistent ART (byte variant).
//!
//! **G5.1 (DRY unification).** The byte overlay-backed `DictionaryNode` handle is now
//! a thin alias of the shared, key-encoding-generic
//! [`OverlayDictionaryNode<ByteKey, V>`](crate::persistent_artrie_core::overlay::OverlayDictionaryNode)
//! that lives in `persistent_artrie_core`. The byte and char handles were
//! token-for-token identical modulo the key encoding (`ByteKey` vs `CharKey`) and the
//! public unit they presented (`u8` vs `char`); G5.1 collapses both into the one
//! generic handle. The public name `PersistentARTrieNode<V>` is PRESERVED (downstream
//! `liblevenshtein` / `libgrammstein` depend on it), and `DictionaryNode::Unit = u8`
//! is preserved exactly (`K::Token = u8` for `ByteKey`, identity).
//!
//! The former bespoke byte `NodeInner::Overlay` one-arm wrapper enum + its
//! hand-written `DictionaryNode` / `MappedDictionaryNode` / `Debug` impls + the
//! `new_overlay` / `overlay_child_node` constructors were deleted; they now live once,
//! generically, in `overlay::dict_node`. The byte `root()` constructors call the
//! shared `OverlayDictionaryNode::from_overlay_root` directly.
//!
//! # Thread Safety
//!
//! The handle holds an owned `Arc<OverlayNode>` snapshot (immutable + reference-
//! counted, so descent needs no pin and no `unsafe`) plus an optional SAFE
//! `OverlayFaulter`; `Send`/`Sync` AUTO-DERIVE (the faulter trait object carries a
//! `Send + Sync` supertrait bound). No raw pointers, zero `unsafe`.

use crate::persistent_artrie_core::key_encoding::ByteKey;
use crate::persistent_artrie_core::overlay::OverlayDictionaryNode;

/// A node in the Persistent ART, backed by a lock-free overlay snapshot.
///
/// **G5.1:** an alias of the shared generic [`OverlayDictionaryNode<ByteKey, V>`]. It
/// implements `DictionaryNode<Unit = u8>` + `MappedDictionaryNode<Value = V>` for
/// integration with the Levenshtein transducer.
pub type PersistentARTrieNode<V = ()> = OverlayDictionaryNode<ByteKey, V>;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::persistent_artrie_core::key_encoding::ByteKey;
    use crate::persistent_artrie_core::overlay::OverlayNode;
    use crate::DictionaryNode;

    use super::PersistentARTrieNode;

    #[test]
    fn overlay_root_is_navigable() {
        // Smoke test the overlay-backed node directly: a fresh overlay root is
        // non-final, childless, and has no transitions.
        let root: Arc<OverlayNode<ByteKey, ()>> = Arc::new(OverlayNode::new());
        let node: PersistentARTrieNode<()> = PersistentARTrieNode::from_overlay_root(root, None);
        assert!(!node.is_final());
        assert_eq!(node.edge_count(), Some(0));
        assert!(node.transition(b'a').is_none());
        assert_eq!(node.edges().count(), 0);
    }
}
