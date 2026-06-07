//! Byte CAS-style node pointer — a `<ByteKey>` instantiation of the shared
//! `persistent_artrie_core::overlay::AtomicNodePtr`.
//!
//! G4 unification: the byte and char `AtomicNodePtr` wrappers were identical
//! modulo the node type they wrapped. Both are now the single generic
//! `overlay::AtomicNodePtr<K, V>`; the byte pointer is its `<ByteKey>` alias.
//! This is a **pure re-export** — the exhaustive pointer unit tests (both
//! `<ByteKey>` and `<CharKey>` instantiations) live in
//! `persistent_artrie_core::overlay::atomic_ptr`.

use crate::persistent_artrie_core::key_encoding::ByteKey;

/// The byte CAS-style node pointer. Alias of the shared
/// `overlay::AtomicNodePtr<ByteKey, V>` (default `V = ()` for membership).
pub type AtomicNodePtr<V = ()> = crate::persistent_artrie_core::overlay::AtomicNodePtr<ByteKey, V>;

#[cfg(test)]
mod tests {
    //! Alias-smoke test: prove the `AtomicNodePtr` alias resolves to the shared
    //! `overlay::AtomicNodePtr<ByteKey, _>` and behaves. Exhaustive CAS coverage
    //! lives in `persistent_artrie_core::overlay::atomic_ptr::tests`.

    
    use std::sync::Arc;

    // Pin the default `<()>` membership instantiation.
    type AtomicNodePtr = super::AtomicNodePtr<()>;
    type PersistentNode = super::super::persistent_node::PersistentNode<()>;

    #[test]
    fn alias_resolves_load_and_cas() {
        let node1 = Arc::new(PersistentNode::new());
        let ptr = AtomicNodePtr::new(Arc::clone(&node1));
        assert_eq!(ptr.load().expect("load").num_children(), 0);

        let node2 = Arc::new(node1.as_final());
        assert!(ptr.compare_exchange(&node1, node2).is_ok());
        assert!(ptr.load().expect("load").is_final());
    }
}
