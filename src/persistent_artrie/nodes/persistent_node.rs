//! Byte overlay node — a `<ByteKey>` instantiation of the shared `OverlayNode`.
//!
//! G4 unification: the byte lock-free overlay node (`u8`/ASCII / arbitrary-byte
//! keys) was token-for-token identical to the char overlay node modulo the
//! key-unit type, `MAX_PREFIX_LEN`, the inline zero filler, and prose. Both are
//! now a single generic `persistent_artrie::core::overlay::OverlayNode<K, V>`; the
//! byte node is its `<ByteKey>` alias. This file is a **pure re-export** — every
//! method has the same signature and (after monomorphization) the same machine
//! code as the prior in-place byte node, so the loom/proptest/TLA correspondence
//! over this node is unchanged by construction. The exhaustive node unit tests
//! (both `<ByteKey>` and `<CharKey>` instantiations) live in the shared module
//! `persistent_artrie::core::overlay::node`.
//!
//! The prior in-place byte node body (the duplicate of char's node) is removed in
//! favor of this alias — its logic now lives once in `core::overlay::node`. The
//! `Send`/`Sync` auto-derivation and the `Child` leak-fix carry over verbatim.
//!
//! `MAX_PREFIX_LEN` is preserved as a module constant (= 12 = `ByteKey::MAX_PREFIX_LEN`)
//! for any external referent.

use crate::persistent_artrie::core::key_encoding::{ByteKey, KeyEncoding};

// Re-export the shared flags so `persistent_node::flags::*` call-sites resolve.
pub use crate::persistent_artrie::core::overlay::flags;

/// The byte overlay node (u8/ASCII keys). Now an alias of the shared generic
/// `OverlayNode<ByteKey, V>` (default `V = ()` for membership).
pub type PersistentNode<V = ()> = crate::persistent_artrie::core::overlay::OverlayNode<ByteKey, V>;

/// The byte child slot. Alias of the shared `Child<ByteKey, V>`.
pub type Child<V = ()> = crate::persistent_artrie::core::overlay::Child<ByteKey, V>;

/// Maximum path-compression prefix length for byte overlay nodes (12 B).
/// Mirrors `ByteKey::MAX_PREFIX_LEN`; kept as a module const for external referents.
pub const MAX_PREFIX_LEN: usize = <ByteKey as KeyEncoding>::MAX_PREFIX_LEN;

#[cfg(test)]
mod tests {
    //! Alias-smoke tests: prove the `PersistentNode`/`Child` aliases resolve to the
    //! shared `OverlayNode<ByteKey, _>` and behave identically. The exhaustive node
    //! coverage (tier transitions, all-byte-values, value carry, etc.) lives in
    //! `persistent_artrie::core::overlay::node::tests`, which exercises BOTH the
    //! `<ByteKey>` and `<CharKey>` instantiations of this same code.

    use super::*;
    use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
    use crate::persistent_artrie::NodeType;

    // Pin the default `<()>` membership instantiation so bare `::new()` resolves.
    type PersistentNode = super::PersistentNode<()>;

    #[test]
    fn alias_resolves_and_basic_ops_work() {
        let node = PersistentNode::new();
        assert_eq!(node.num_children(), 0);
        assert!(!node.is_final());

        let child = Child::OnDisk(SwizzledPtr::on_disk(1, 100, NodeType::Node4));
        let node2 = node.with_child(b'a', child);
        assert_eq!(node2.num_children(), 1);
        assert!(node2.has_child(b'a'));
        // Original unchanged (persistent).
        assert_eq!(node.num_children(), 0);
    }

    #[test]
    fn alias_max_prefix_len_is_byte_value() {
        assert_eq!(MAX_PREFIX_LEN, 12);
        let prefix = b"abcdefghijklmnop"; // 16 > 12 ⇒ truncated to 12
        let node = PersistentNode::with_prefix(prefix);
        assert_eq!(node.prefix_len(), 12);
        assert_eq!(node.prefix(), b"abcdefghijkl");
    }
}
