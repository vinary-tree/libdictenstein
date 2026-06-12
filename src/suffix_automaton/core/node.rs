//! Generic `SuffixNode<U, V>` shared between byte and char suffix
//! automaton variants.
//!
//! Type parameters:
//! - `U`: edge label type (`u8` or `char`). Must implement [`CharUnit`].
//! - `V`: per-final-state value type. Must implement
//!   [`crate::value::DictionaryValue`].

use crate::value::DictionaryValue;
use crate::CharUnit;

/// State in the suffix automaton.
///
/// Each state represents an endpos equivalence class of substrings of
/// the indexed corpus. The state holds outgoing labeled transitions,
/// a suffix link to the longest-proper-suffix class, the length of the
/// longest string in this class, plus a final-state flag and optional
/// value.
#[derive(Debug, Clone)]
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
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
pub struct SuffixNode<U: CharUnit, V: DictionaryValue = ()> {
    /// Outgoing edges: (label, target state index), sorted by label.
    pub edges: Vec<(U, usize)>,

    /// Suffix link: points to state representing the longest proper
    /// suffix in a different endpos equivalence class. None for the
    /// root.
    pub suffix_link: Option<usize>,

    /// Length of the longest string in this equivalence class.
    pub max_length: usize,

    /// True if this state represents an end-of-string position.
    pub is_final: bool,

    /// Optional value associated with this state (only meaningful for
    /// final states).
    pub value: Option<V>,
}

impl<U: CharUnit, V: DictionaryValue> SuffixNode<U, V> {
    /// Create a new root node.
    pub fn root() -> Self {
        Self {
            edges: Vec::new(),
            suffix_link: None,
            max_length: 0,
            is_final: false,
            value: None,
        }
    }

    /// Create a new non-root node with the given max-length bound.
    pub fn new(max_length: usize) -> Self {
        Self {
            edges: Vec::new(),
            suffix_link: None,
            max_length,
            is_final: false,
            value: None,
        }
    }

    /// Find an edge by label.
    ///
    /// Uses linear search for small edge counts, binary search for
    /// larger. Threshold at 16 edges based on DAWG benchmarks.
    pub fn find_edge(&self, label: U) -> Option<usize> {
        if self.edges.len() < 16 {
            self.edges
                .iter()
                .find(|(u, _)| *u == label)
                .map(|(_, t)| *t)
        } else {
            self.edges
                .binary_search_by_key(&label, |(u, _)| *u)
                .ok()
                .map(|idx| self.edges[idx].1)
        }
    }

    /// Add an edge, maintaining sorted order. Updates target if an
    /// edge already exists for `label`.
    pub fn add_edge(&mut self, label: U, target: usize) {
        match self.edges.binary_search_by_key(&label, |(u, _)| *u) {
            Ok(idx) => {
                // Edge already exists, update target.
                self.edges[idx].1 = target;
            }
            Err(idx) => {
                self.edges.insert(idx, (label, target));
            }
        }
    }

    /// Update an existing edge's target. Returns false if the edge
    /// wasn't present.
    pub fn update_edge(&mut self, label: U, new_target: usize) -> bool {
        if let Some(idx) = self.edges.iter().position(|(u, _)| *u == label) {
            self.edges[idx].1 = new_target;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_node_byte_smoke() {
        let mut node: SuffixNode<u8, ()> = SuffixNode::root();
        node.add_edge(b'a', 1);
        node.add_edge(b'b', 2);
        assert_eq!(node.find_edge(b'a'), Some(1));
        assert_eq!(node.find_edge(b'b'), Some(2));
        assert_eq!(node.find_edge(b'c'), None);
        assert!(node.update_edge(b'a', 10));
        assert_eq!(node.find_edge(b'a'), Some(10));
        assert!(!node.update_edge(b'z', 99));
    }

    #[test]
    fn suffix_node_char_smoke() {
        let mut node: SuffixNode<char, u32> = SuffixNode::new(5);
        node.add_edge('é', 7);
        node.add_edge('中', 12);
        assert_eq!(node.find_edge('é'), Some(7));
        assert_eq!(node.find_edge('中'), Some(12));
        assert_eq!(node.find_edge('z'), None);
        assert_eq!(node.max_length, 5);
    }
}
