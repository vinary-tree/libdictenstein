//! Char CAS-style node pointer — a `<CharKey>` instantiation of the shared
//! `persistent_artrie_core::overlay::AtomicNodePtr`.
//!
//! G4 unification: the char and byte `AtomicNodePtr` wrappers were identical
//! modulo the node type they wrapped. Both are now the single generic
//! `overlay::AtomicNodePtr<K, V>`; the char pointer is its `<CharKey>` alias.
//! This is a **pure re-export** — the exhaustive pointer unit tests (both
//! `<CharKey>` and `<ByteKey>` instantiations) live in
//! `persistent_artrie_core::overlay::atomic_ptr`.

use crate::persistent_artrie_core::key_encoding::CharKey;

/// The char CAS-style node pointer. Alias of the shared
/// `overlay::AtomicNodePtr<CharKey, V>` (default `V = ()` for membership).
pub type AtomicNodePtr<V = ()> = crate::persistent_artrie_core::overlay::AtomicNodePtr<CharKey, V>;

#[cfg(test)]
mod tests {
    //! Alias-smoke test: prove the `AtomicNodePtr` alias resolves to the shared
    //! `overlay::AtomicNodePtr<CharKey, _>` and behaves. Exhaustive CAS coverage
    //! lives in `persistent_artrie_core::overlay::atomic_ptr::tests`.

    use std::sync::Arc;

    // Pin the default `<()>` membership instantiation.
    type AtomicNodePtr = super::AtomicNodePtr<()>;
    type PersistentCharNode = super::super::persistent_node::PersistentCharNode<()>;

    #[test]
    fn alias_resolves_load_and_cas() {
        let node1 = Arc::new(PersistentCharNode::new());
        let ptr = AtomicNodePtr::new(Arc::clone(&node1));
        assert_eq!(ptr.load().expect("load").num_children(), 0);

        let node2 = Arc::new(node1.as_final());
        assert!(ptr.compare_exchange(&node1, node2).is_ok());
        assert!(ptr.load().expect("load").is_final());
    }
}
