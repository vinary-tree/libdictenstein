//! Char overlay node — a `<CharKey>` instantiation of the shared `OverlayNode`.
//!
//! G4 unification: the char lock-free overlay node (`u32`/Unicode-code-point
//! keys) was token-for-token identical to the byte overlay node modulo the
//! key-unit type, `MAX_PREFIX_LEN`, the inline zero filler, and prose. Both are
//! now a single generic `persistent_artrie_core::overlay::OverlayNode<K, V>`; the
//! char node is its `<CharKey>` alias. This file is a **pure re-export** — every
//! method has the same signature and (after monomorphization) the same machine
//! code as before, so the loom/proptest/TLA correspondence over this node is
//! unchanged by construction. The exhaustive node unit tests (both `<CharKey>`
//! and `<ByteKey>` instantiations) live in the shared module
//! `persistent_artrie_core::overlay::node`.
//!
//! `MAX_PREFIX_LEN` is preserved as a module constant (= 6 = `CharKey::MAX_PREFIX_LEN`)
//! for any external referent.

use crate::persistent_artrie_core::key_encoding::{CharKey, KeyEncoding};

// Re-export the shared flags so `persistent_node::flags::*` call-sites resolve.
pub use crate::persistent_artrie_core::overlay::flags;

/// The char overlay node (Unicode code-point keys). Now an alias of the shared
/// generic `OverlayNode<CharKey, V>` (default `V = ()` for membership).
pub type PersistentCharNode<V = ()> =
    crate::persistent_artrie_core::overlay::OverlayNode<CharKey, V>;

/// The char child slot. Alias of the shared `Child<CharKey, V>`.
pub type Child<V = ()> = crate::persistent_artrie_core::overlay::Child<CharKey, V>;

/// Maximum path-compression prefix length for char overlay nodes (6 chars = 24 B).
/// Mirrors `CharKey::MAX_PREFIX_LEN`; kept as a module const for external referents.
pub const MAX_PREFIX_LEN: usize = <CharKey as KeyEncoding>::MAX_PREFIX_LEN;

#[cfg(test)]
mod tests {
    //! Alias-smoke tests: prove the `PersistentCharNode`/`Child` aliases resolve to
    //! the shared `OverlayNode<CharKey, _>` and behave identically. The exhaustive
    //! node coverage (tier transitions, Unicode keys, value carry, etc.) lives in
    //! `persistent_artrie_core::overlay::node::tests`, which exercises BOTH the
    //! `<CharKey>` and `<ByteKey>` instantiations of this same code.

    use super::*;
    use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
    use crate::persistent_artrie::NodeType;

    // Pin the default `<()>` membership instantiation so bare `::new()` resolves.
    type PersistentCharNode = super::PersistentCharNode<()>;

    #[test]
    fn alias_resolves_and_basic_ops_work() {
        let node = PersistentCharNode::new();
        assert_eq!(node.num_children(), 0);
        assert!(!node.is_final());

        let child = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::CharNode4));
        let node2 = node.with_child('a' as u32, child);
        assert_eq!(node2.num_children(), 1);
        assert!(node2.has_child('a' as u32));
        // Original unchanged (persistent).
        assert_eq!(node.num_children(), 0);
    }

    #[test]
    fn alias_max_prefix_len_is_char_value() {
        assert_eq!(MAX_PREFIX_LEN, 6);
        let prefix: Vec<u32> = "abcdefghi".chars().map(|c| c as u32).collect();
        let node = PersistentCharNode::with_prefix(&prefix);
        assert_eq!(node.prefix_len(), 6);
    }
}
