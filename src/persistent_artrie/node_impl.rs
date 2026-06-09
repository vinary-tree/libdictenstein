//! Dictionary Node Implementation for Persistent ART
//!
//! This module provides the `PersistentARTrieNode` type that implements the
//! `DictionaryNode` trait, enabling the Persistent Adaptive Radix Trie to work
//! with the Levenshtein automata transducer.
//!
//! **L3.3c — overlay-only.** The owned tree is gone, so the node has exactly one
//! representation: a snapshot of the lock-free **overlay** (`OverlayNode<ByteKey, V>`).
//! `Dictionary::root()` constructs it via [`PersistentARTrieNode::new_overlay`]. The
//! overlay is un-path-compressed (one node per key byte, no buckets, no consumed
//! prefixes), so traversal is a direct child lookup. The former owned `NodeInner`
//! variants (`ArtNode` / `Bucket` / `Root` / `Empty`), their constructors, the
//! `BucketPosition` cursor, and the owned bucket/ART edge walks were deleted with the
//! owned tree.
//!
//! # Thread Safety
//!
//! All operations are thread-safe through immutable access patterns. The node holds an
//! owned `Arc<OverlayNode>` snapshot (immutable + reference-counted, so descent needs no
//! pin and no `unsafe`) and clones cheaply.

use std::sync::Arc;

use crate::persistent_artrie_core::key_encoding::ByteKey;
use crate::persistent_artrie_core::overlay::{Child, OverlayFaulter, OverlayNode};
use crate::{value::DictionaryValue, DictionaryNode, MappedDictionaryNode};

/// A node in the Persistent ART, backed by a lock-free overlay snapshot.
///
/// This type implements `DictionaryNode` for integration with the Levenshtein transducer.
#[derive(Clone, Debug)]
pub struct PersistentARTrieNode<V: DictionaryValue = ()> {
    /// The inner node representation
    inner: Arc<NodeInner<V>>,
}

/// Inner representation of a node.
///
/// `Debug` is hand-written (below) so the `Overlay` arm — which carries an
/// `Arc<dyn OverlayFaulter<..>>` that is not itself `Debug` — can be summarized
/// rather than derived.
enum NodeInner<V: DictionaryValue> {
    /// A node in the **lock-free overlay** (returned by `root()`). Navigates the
    /// overlay lazily for the zipper / transducer / fuzzy graph walk. Holds an owned
    /// `Arc<OverlayNode>` snapshot (immutable + reference-counted, so descent needs no
    /// pin and no `unsafe`), plus an optional SAFE [`OverlayFaulter`] to resolve
    /// `Child::OnDisk` overlay children (faulted in on demand, never dropped). The
    /// overlay is un-path-compressed: one node per key byte, no buckets, no consumed
    /// prefixes.
    Overlay {
        /// The owned overlay node snapshot (keeps its in-memory subtree alive).
        node: Arc<OverlayNode<ByteKey, V>>,
        /// Fault-in capability for `Child::OnDisk` overlay children, or `None` for a
        /// resident-only walk (an owned-trie `root()`, where eviction — hence an
        /// OnDisk overlay child — is impossible). `Arc<dyn ..>` (owned), so it keeps
        /// the trie alive for the walk and clones cheaply when the node clones. No
        /// raw pointer, no `unsafe`.
        faulter: Option<Arc<dyn OverlayFaulter<ByteKey, V>>>,
    },
}

impl<V: DictionaryValue> std::fmt::Debug for NodeInner<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeInner::Overlay { node, faulter } => f
                .debug_struct("Overlay")
                .field("node", node)
                .field("has_faulter", &faulter.is_some())
                .finish(),
        }
    }
}

impl<V: DictionaryValue> PersistentARTrieNode<V> {
    /// Create an **overlay-backed** node (the `root()` node). Navigates the lock-free
    /// overlay lazily. `faulter` is the SAFE fault-in capability for `Child::OnDisk`
    /// overlay children (or `None` for a resident-only walk).
    pub(crate) fn new_overlay(
        node: Arc<OverlayNode<ByteKey, V>>,
        faulter: Option<Arc<dyn OverlayFaulter<ByteKey, V>>>,
    ) -> Self {
        Self {
            inner: Arc::new(NodeInner::Overlay { node, faulter }),
        }
    }

    /// Resolve an overlay child slot into a child overlay node, faulting an
    /// `Child::OnDisk` slot in via `faulter` (never dropping it). Returns `None`
    /// for a null/absent slot, or an OnDisk slot that cannot be faulted in (no
    /// faulter / I/O error) — the same conservative degrade the production
    /// point-read uses (liveness-only, never a fabricated term).
    fn overlay_child_node(
        child: &Child<ByteKey, V>,
        faulter: &Option<Arc<dyn OverlayFaulter<ByteKey, V>>>,
    ) -> Option<Self> {
        if let Some(child_arc) = child.as_in_mem() {
            return Some(Self::new_overlay(Arc::clone(child_arc), faulter.clone()));
        }
        if let Some(on_disk) = child.as_on_disk() {
            if !on_disk.is_null() {
                let loaded = faulter.as_ref()?.fault_overlay_slot(on_disk)?;
                return Some(Self::new_overlay(loaded, faulter.clone()));
            }
        }
        None
    }

    /// Get the associated value (for mapped dictionaries).
    ///
    /// Returns a borrow; the overlay arm cannot (its `get_value()` yields an owned
    /// `Option<V>`), so the overlay value is exposed via the owned-returning
    /// [`MappedDictionaryNode::value`] impl below — this borrow accessor returns
    /// `None` (callers that need the overlay value use the `MappedDictionaryNode`
    /// trait method, which is what the transducer drives).
    pub fn value(&self) -> Option<&V> {
        match &*self.inner {
            NodeInner::Overlay { .. } => None,
        }
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentARTrieNode<V> {
    type Unit = u8;

    fn is_final(&self) -> bool {
        match &*self.inner {
            NodeInner::Overlay { node, .. } => node.is_final(),
        }
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        match &*self.inner {
            NodeInner::Overlay { node, faulter } => {
                // Overlay is un-path-compressed: one byte per edge. Find the child
                // for `label`; InMem ⇒ wrap directly, OnDisk ⇒ fault in (never drop).
                let child = node.find_child(label)?;
                Self::overlay_child_node(child, faulter)
            }
        }
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        match &*self.inner {
            NodeInner::Overlay { node, faulter } => {
                // One edge per overlay child slot (InMem direct, OnDisk faulted in —
                // never dropped). Preallocated to the known child count.
                let mut edges = Vec::with_capacity(node.num_children());
                for (&edge, child) in node.iter_children() {
                    if let Some(child_node) = Self::overlay_child_node(child, faulter) {
                        edges.push((edge, child_node));
                    }
                }
                Box::new(edges.into_iter())
            }
        }
    }

    fn has_edge(&self, label: Self::Unit) -> bool {
        self.transition(label).is_some()
    }

    fn edge_count(&self) -> Option<usize> {
        match &*self.inner {
            NodeInner::Overlay { node, .. } => Some(node.num_children()),
        }
    }
}

impl<V: DictionaryValue> MappedDictionaryNode for PersistentARTrieNode<V> {
    type Value = V;

    /// The value at this node if it terminates a term. Reads the overlay leaf's
    /// `Option<V>` directly (unlocking liblevenshtein's value-aware transducer queries
    /// over a byte trie). `None` for a non-final / value-less node.
    fn value(&self) -> Option<V> {
        match &*self.inner {
            NodeInner::Overlay { node, .. } => node.get_value(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // L3.3c: removed — test_empty_node / test_root_node / test_bucket_node /
    // test_bucket_with_empty_suffix / test_art_node_final / test_clone exercised the
    // deleted owned `PersistentARTrieNode` representation (the `Empty` / `Root` /
    // `Bucket` / `ArtNode` `NodeInner` variants + their `new_root` / `new_bucket` /
    // `new_art_node` / `empty` constructors + the `StringBucket`-backed edge walk).
    // Overlay-node traversal is exercised end-to-end by the transducer / zipper
    // correspondence suites (`overlay_routing_tests`, `persistent_prefix_correspondence`,
    // etc.) which drive `Dictionary::root()` over a real overlay.

    #[test]
    fn overlay_root_is_navigable() {
        // Smoke test the overlay-backed node directly: a fresh overlay root is
        // non-final, childless, and has no transitions.
        let root: Arc<OverlayNode<ByteKey, ()>> = Arc::new(OverlayNode::new());
        let node: PersistentARTrieNode<()> = PersistentARTrieNode::new_overlay(root, None);
        assert!(!node.is_final());
        assert_eq!(node.edge_count(), Some(0));
        assert!(node.transition(b'a').is_none());
        assert_eq!(node.edges().count(), 0);
    }
}
