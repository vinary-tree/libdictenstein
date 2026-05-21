//! Generic `ScdawgNode<U, V>` shared between byte and char SCDAWG variants.
//!
//! Carries the forward/left edge lists, suffix link, length/depth
//! bookkeeping, optional value, and parent linkage that the byte and
//! char variants previously duplicated.

use crate::value::DictionaryValue;
use crate::CharUnit;
use smallvec::SmallVec;

/// Sentinel meaning "no node" (used for suffix_link/parent on the root).
pub const NIL: usize = usize::MAX;

/// State in the compact suffix DAWG.
///
/// Per Blumer et al. 1987, each state represents an endpos equivalence
/// class refined by the SCDAWG "left-extension uniqueness" property.
/// Forward edges allow appending characters; left edges (computed in
/// the post-construction `compute_left_edges()` pass) allow prepending.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serialization", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    all(feature = "serialization", not(feature = "persistent-artrie")),
    serde(bound(
        serialize = "U: serde::Serialize, V: serde::Serialize",
        deserialize = "U: serde::Deserialize<'de>, V: serde::Deserialize<'de>",
    ))
)]
#[cfg_attr(
    all(feature = "serialization", feature = "persistent-artrie"),
    serde(bound(
        serialize = "U: serde::Serialize, V: serde::Serialize",
        deserialize = "U: serde::de::DeserializeOwned, V: serde::de::DeserializeOwned",
    ))
)]
pub struct ScdawgNode<U: CharUnit, V: DictionaryValue = ()> {
    /// Forward (right-extension) edges: label → target node.
    pub forward_edges: SmallVec<[(U, usize); 4]>,

    /// Suffix link: longest proper suffix in a different equivalence
    /// class. [`NIL`] for the root.
    pub suffix_link: usize,

    /// Left (prepend) edges: derived from suffix links after
    /// construction (`compute_left_edges()`).
    pub left_edges: SmallVec<[(U, usize); 2]>,

    /// Maximum length of strings in this equivalence class.
    pub length: usize,

    /// True if this state ends some indexed term.
    pub is_final: bool,

    /// (term_index, position_in_term) pairs for multi-string support.
    pub term_ends: SmallVec<[(usize, usize); 2]>,

    /// Optional value associated with final states.
    pub value: Option<V>,

    /// Parent node in the canonical (longest) path. [`NIL`] for root.
    pub parent: usize,

    /// Edge label from parent to this node (last unit of canonical path).
    pub parent_label: U,

    /// First unit of the canonical longest string represented by this
    /// node (used to compute left-extension edges).
    pub first_char: U,

    /// Depth from root (canonical-path edge count).
    pub depth: usize,
}

impl<U: CharUnit, V: DictionaryValue> ScdawgNode<U, V> {
    /// Construct the root node.
    pub fn root() -> Self {
        Self {
            forward_edges: SmallVec::new(),
            suffix_link: NIL,
            left_edges: SmallVec::new(),
            length: 0,
            is_final: false,
            term_ends: SmallVec::new(),
            value: None,
            parent: NIL,
            parent_label: U::default(),
            first_char: U::default(),
            depth: 0,
        }
    }

    /// Construct a new non-root node.
    pub fn new(length: usize, suffix_link: usize, first_char: U) -> Self {
        Self {
            forward_edges: SmallVec::new(),
            suffix_link,
            left_edges: SmallVec::new(),
            length,
            is_final: false,
            term_ends: SmallVec::new(),
            value: None,
            parent: NIL,
            parent_label: U::default(),
            first_char,
            depth: 0,
        }
    }

    /// Find a forward edge by label. Uses binary search.
    #[inline(always)]
    pub fn get_edge(&self, label: U) -> Option<usize> {
        match self.forward_edges.binary_search_by_key(&label, |(l, _)| *l) {
            Ok(idx) => Some(self.forward_edges[idx].1),
            Err(_) => None,
        }
    }

    /// Add or update a forward edge, maintaining sorted order.
    #[inline(always)]
    pub fn set_edge(&mut self, label: U, target: usize) {
        match self.forward_edges.binary_search_by_key(&label, |(l, _)| *l) {
            Ok(idx) => self.forward_edges[idx].1 = target,
            Err(idx) => self.forward_edges.insert(idx, (label, target)),
        }
    }

    /// True if this is the root node.
    ///
    /// Matches the original `Scdawg::ScdawgNode::is_root` semantics —
    /// the root has no parent and represents the empty string (length
    /// = 0).
    #[inline]
    pub fn is_root(&self) -> bool {
        self.parent == NIL && self.length == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scdawg_node_byte_smoke() {
        let mut node: ScdawgNode<u8, ()> = ScdawgNode::root();
        node.set_edge(b'a', 1);
        node.set_edge(b'b', 2);
        assert_eq!(node.get_edge(b'a'), Some(1));
        assert_eq!(node.get_edge(b'b'), Some(2));
        assert_eq!(node.get_edge(b'z'), None);
        assert!(node.is_root());
    }

    #[test]
    fn scdawg_node_char_smoke() {
        let mut node: ScdawgNode<char, u32> = ScdawgNode::new(3, 0, 'a');
        node.set_edge('é', 5);
        node.set_edge('中', 7);
        assert_eq!(node.get_edge('é'), Some(5));
        assert_eq!(node.get_edge('中'), Some(7));
        // length=3 means this is not root (root has length 0).
        assert!(!node.is_root());
        assert_eq!(node.first_char, 'a');
        assert_eq!(node.length, 3);
    }
}
