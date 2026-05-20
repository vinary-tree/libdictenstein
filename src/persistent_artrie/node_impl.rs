//! Dictionary Node Implementation for Persistent ART
//!
//! This module provides the `PersistentARTrieNode` type that implements the
//! `DictionaryNode` trait, enabling the Persistent Adaptive Radix Trie to work
//! with the Levenshtein automata transducer.
//!
//! # Architecture
//!
//! The node implementation handles two types of children:
//! 1. **ART Nodes**: Internal nodes with up to 256 children (Node4/16/48/256)
//! 2. **Buckets**: Leaf nodes containing multiple suffixes
//!
//! Traversal follows a hybrid approach:
//! - ART nodes use pointer-based child lookup
//! - Buckets use binary search for suffix lookup
//!
//! # Thread Safety
//!
//! All operations are thread-safe through immutable access patterns. The nodes
//! are designed to be cloned cheaply using `Arc` for shared ownership.

use std::sync::Arc;

use crate::{DictionaryNode, value::DictionaryValue};
use super::bucket::StringBucket;
use super::nodes::{Node, Node4};
use super::transitions::ChildNode;

/// A node in the Persistent ART that can be either an ART internal node or a bucket leaf.
///
/// This type implements `DictionaryNode` for integration with the Levenshtein transducer.
#[derive(Clone, Debug)]
pub struct PersistentARTrieNode<V: DictionaryValue = ()> {
    /// The inner node representation
    inner: Arc<NodeInner<V>>,
    /// Current position within a bucket (for bucket iteration)
    bucket_position: Option<BucketPosition>,
}

/// Position within a bucket for iterating through its suffixes
#[derive(Clone, Debug)]
struct BucketPosition {
    /// Current suffix being traversed
    suffix: Vec<u8>,
    /// Index of current entry in the bucket
    entry_index: usize,
    /// Current position within the suffix
    suffix_offset: usize,
}

/// Inner representation of a node
#[derive(Debug)]
enum NodeInner<V: DictionaryValue> {
    /// An ART internal node
    ArtNode {
        /// The node variant (Node4/16/48/256). Kept for `edge_count` / type
        /// inspection; child traversal uses `children` below, not the
        /// `SwizzledPtr`s inside this `Node` (which are `null` placeholders
        /// in this code path).
        node: Node,
        /// Whether this is a final state
        is_final: bool,
        /// Value if this is a final state (for mapped dictionaries)
        value: Option<V>,
        /// Actual child subtrees (real children — `node`'s `SwizzledPtr`s
        /// are not followed in this trait surface). Wrapped in `Arc` so
        /// cloning a `PersistentARTrieNode` and constructing transition
        /// targets stays cheap.
        children: Arc<Vec<(u8, ChildNode)>>,
    },
    /// A bucket leaf node
    Bucket {
        /// The bucket containing multiple entries
        bucket: StringBucket,
    },
    /// The root node of the trie
    Root {
        /// Root node (always an ART node initially)
        node: Node,
        /// Whether root is final (empty string is in dictionary)
        is_final: bool,
        /// Value for empty string
        value: Option<V>,
        /// Actual children of the root (same role as `ArtNode::children`).
        children: Arc<Vec<(u8, ChildNode)>>,
    },
    /// An empty node (used for non-existent transitions)
    Empty,
}

impl<V: DictionaryValue> PersistentARTrieNode<V> {
    /// Create a new empty root node (no children).
    pub fn new_root() -> Self {
        Self {
            inner: Arc::new(NodeInner::Root {
                node: Node::N4(Box::new(Node4::new())),
                is_final: false,
                value: None,
                children: Arc::new(Vec::new()),
            }),
            bucket_position: None,
        }
    }

    /// Create a new root node with explicit children.
    pub fn new_root_with_children(
        node: Node,
        is_final: bool,
        value: Option<V>,
        children: Vec<(u8, ChildNode)>,
    ) -> Self {
        Self {
            inner: Arc::new(NodeInner::Root {
                node,
                is_final,
                value,
                children: Arc::new(children),
            }),
            bucket_position: None,
        }
    }

    /// Create a new ART node. Used when no actual children are available
    /// (e.g., synthetic nodes in tests); for trie traversal use
    /// [`PersistentARTrieNode::new_art_node_with_children`].
    pub fn new_art_node(node: Node, is_final: bool, value: Option<V>) -> Self {
        Self {
            inner: Arc::new(NodeInner::ArtNode {
                node,
                is_final,
                value,
                children: Arc::new(Vec::new()),
            }),
            bucket_position: None,
        }
    }

    /// Create a new ART node with the given children.
    pub fn new_art_node_with_children(
        node: Node,
        is_final: bool,
        value: Option<V>,
        children: Arc<Vec<(u8, ChildNode)>>,
    ) -> Self {
        Self {
            inner: Arc::new(NodeInner::ArtNode {
                node,
                is_final,
                value,
                children,
            }),
            bucket_position: None,
        }
    }

    /// Create a new bucket node
    pub fn new_bucket(bucket: StringBucket) -> Self {
        Self {
            inner: Arc::new(NodeInner::Bucket { bucket }),
            bucket_position: None,
        }
    }

    /// Create a bucket node at a specific position
    fn new_bucket_at_position(bucket: Arc<NodeInner<V>>, position: BucketPosition) -> Self {
        Self {
            inner: bucket,
            bucket_position: Some(position),
        }
    }

    /// Create an empty node (for non-existent transitions)
    fn empty() -> Self {
        Self {
            inner: Arc::new(NodeInner::Empty),
            bucket_position: None,
        }
    }

    /// Check if this is an empty node
    pub fn is_empty(&self) -> bool {
        matches!(&*self.inner, NodeInner::Empty)
    }

    /// Get the associated value (for mapped dictionaries)
    pub fn value(&self) -> Option<&V> {
        match &*self.inner {
            NodeInner::ArtNode { value, .. } => value.as_ref(),
            NodeInner::Root { value, .. } => value.as_ref(),
            NodeInner::Bucket { bucket } => {
                if let Some(ref pos) = self.bucket_position {
                    if pos.suffix_offset == pos.suffix.len() {
                        // At the end of suffix, check for value
                        if let Some(entry) = bucket.get_entry(pos.entry_index) {
                            if entry.has_value() {
                                // We can't return a reference to the value here
                                // since it would require parsing the bytes
                                return None;
                            }
                        }
                    }
                }
                None
            }
            NodeInner::Empty => None,
        }
    }

    /// Get child edges from an ART node.
    ///
    /// Walks the real `children: Vec<(u8, ChildNode)>` carried by this node;
    /// for in-memory children (`ChildNode::Bucket` and `ChildNode::ArtNode`)
    /// constructs a properly-populated transition target. `ChildNode::DiskRef`
    /// children are SKIPPED — without a buffer-manager handle the
    /// `DictionaryNode` traversal cannot load the on-disk subtree; callers
    /// that need disk-resident traversal must use `PersistentARTrie::contains`
    /// / `get_value` (which go through the dict's own resolution path
    /// via `resolve_disk_ref`).
    fn art_edges(&self, children: &[(u8, ChildNode)]) -> Vec<(u8, Self)> {
        let mut edges = Vec::with_capacity(children.len());
        for (key, child) in children {
            if let Some(node) = self.persistent_node_from_child(child) {
                edges.push((*key, node));
            }
        }
        edges
    }

    /// Construct a `PersistentARTrieNode` representing a `ChildNode`.
    /// Returns `None` if the child is a `DiskRef` (no traversal possible
    /// without disk resolution).
    fn persistent_node_from_child(&self, child: &ChildNode) -> Option<Self> {
        match child {
            ChildNode::Bucket(bucket) => Some(Self::new_bucket(bucket.clone())),
            ChildNode::ArtNode {
                node,
                is_final,
                value: _,
                children,
            } => {
                // Note: `value` here is `Option<Vec<u8>>` (raw serialized
                // bytes). Deserializing into `V` requires the dict's value
                // codec, which is not available at this layer; the trait
                // surface exposes `is_final` for navigation correctness and
                // value bytes are retrieved via the dict's `get_value` API
                // when needed.
                Some(Self::new_art_node_with_children(
                    node.clone(),
                    *is_final,
                    None,
                    Arc::new(children.clone()),
                ))
            }
            ChildNode::DiskRef { .. } => None,
        }
    }

    /// Get edges from a bucket (first bytes of all suffixes)
    fn bucket_edges(&self, bucket: &StringBucket) -> Vec<(u8, Self)> {
        let mut edges = Vec::new();
        let mut seen_bytes = [false; 256];

        for i in 0..bucket.len() {
            if let Some(entry) = bucket.get_entry(i) {
                let suffix = bucket.get_suffix(&entry);
                if !suffix.is_empty() {
                    let first_byte = suffix[0];
                    if !seen_bytes[first_byte as usize] {
                        seen_bytes[first_byte as usize] = true;

                        // Create a child node positioned at this suffix
                        let position = BucketPosition {
                            suffix: suffix.to_vec(),
                            entry_index: i,
                            suffix_offset: 1, // Skip the first byte (edge label)
                        };
                        let child = Self::new_bucket_at_position(self.inner.clone(), position);
                        edges.push((first_byte, child));
                    }
                }
            }
        }

        edges
    }
}

impl<V: DictionaryValue> DictionaryNode for PersistentARTrieNode<V> {
    type Unit = u8;

    fn is_final(&self) -> bool {
        match &*self.inner {
            NodeInner::ArtNode { is_final, .. } => *is_final,
            NodeInner::Root { is_final, .. } => *is_final,
            NodeInner::Bucket { bucket } => {
                if let Some(ref pos) = self.bucket_position {
                    // Final if we've consumed the entire suffix
                    pos.suffix_offset == pos.suffix.len()
                } else {
                    // At bucket root, check for empty suffix
                    bucket.contains(b"")
                }
            }
            NodeInner::Empty => false,
        }
    }

    fn transition(&self, label: Self::Unit) -> Option<Self> {
        match &*self.inner {
            NodeInner::ArtNode { children, .. } | NodeInner::Root { children, .. } => {
                // Linear scan over real children (each node holds at most 256
                // entries; the inner Vec is sorted by insertion order, not by
                // key, so we cannot binary-search without an additional pass).
                for (key, child) in children.iter() {
                    if *key == label {
                        return self.persistent_node_from_child(child);
                    }
                }
                None
            }
            NodeInner::Bucket { bucket } => {
                if let Some(ref pos) = self.bucket_position {
                    // Continue traversing the current suffix
                    if pos.suffix_offset < pos.suffix.len() {
                        if pos.suffix[pos.suffix_offset] == label {
                            let new_pos = BucketPosition {
                                suffix: pos.suffix.clone(),
                                entry_index: pos.entry_index,
                                suffix_offset: pos.suffix_offset + 1,
                            };
                            return Some(Self::new_bucket_at_position(self.inner.clone(), new_pos));
                        }
                    }
                    None
                } else {
                    // Search for matching suffix in bucket
                    for i in 0..bucket.len() {
                        if let Some(entry) = bucket.get_entry(i) {
                            let suffix = bucket.get_suffix(&entry);
                            if !suffix.is_empty() && suffix[0] == label {
                                let position = BucketPosition {
                                    suffix: suffix.to_vec(),
                                    entry_index: i,
                                    suffix_offset: 1,
                                };
                                return Some(Self::new_bucket_at_position(
                                    self.inner.clone(),
                                    position,
                                ));
                            }
                        }
                    }
                    None
                }
            }
            NodeInner::Empty => None,
        }
    }

    fn edges(&self) -> Box<dyn Iterator<Item = (Self::Unit, Self)> + '_> {
        match &*self.inner {
            NodeInner::ArtNode { children, .. } | NodeInner::Root { children, .. } => {
                Box::new(self.art_edges(children).into_iter())
            }
            NodeInner::Bucket { bucket } => {
                if let Some(ref pos) = self.bucket_position {
                    // Within a suffix, there's at most one edge
                    if pos.suffix_offset < pos.suffix.len() {
                        let next_byte = pos.suffix[pos.suffix_offset];
                        let new_pos = BucketPosition {
                            suffix: pos.suffix.clone(),
                            entry_index: pos.entry_index,
                            suffix_offset: pos.suffix_offset + 1,
                        };
                        let child = Self::new_bucket_at_position(self.inner.clone(), new_pos);
                        Box::new(std::iter::once((next_byte, child)))
                    } else {
                        Box::new(std::iter::empty())
                    }
                } else {
                    Box::new(self.bucket_edges(bucket).into_iter())
                }
            }
            NodeInner::Empty => Box::new(std::iter::empty()),
        }
    }

    fn has_edge(&self, label: Self::Unit) -> bool {
        self.transition(label).is_some()
    }

    fn edge_count(&self) -> Option<usize> {
        match &*self.inner {
            NodeInner::ArtNode { node, .. } | NodeInner::Root { node, .. } => {
                Some(node.header().num_children as usize)
            }
            NodeInner::Bucket { bucket } => {
                if self.bucket_position.is_some() {
                    // Within a suffix, there's at most one edge
                    Some(1)
                } else {
                    // Count unique first bytes
                    let mut count = 0;
                    let mut seen = [false; 256];
                    for i in 0..bucket.len() {
                        if let Some(entry) = bucket.get_entry(i) {
                            let suffix = bucket.get_suffix(&entry);
                            if !suffix.is_empty() && !seen[suffix[0] as usize] {
                                seen[suffix[0] as usize] = true;
                                count += 1;
                            }
                        }
                    }
                    Some(count)
                }
            }
            NodeInner::Empty => Some(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_node() {
        let node: PersistentARTrieNode<()> = PersistentARTrieNode::empty();
        assert!(node.is_empty());
        assert!(!node.is_final());
        assert!(node.transition(b'a').is_none());
        assert_eq!(node.edges().count(), 0);
    }

    #[test]
    fn test_root_node() {
        let node: PersistentARTrieNode<()> = PersistentARTrieNode::new_root();
        assert!(!node.is_empty());
        assert!(!node.is_final());
    }

    #[test]
    fn test_bucket_node() {
        let mut bucket = StringBucket::new();
        bucket.insert_key(b"apple").unwrap();
        bucket.insert_key(b"banana").unwrap();
        bucket.insert_key(b"cherry").unwrap();

        let node: PersistentARTrieNode<()> = PersistentARTrieNode::new_bucket(bucket);

        // Should have edges for 'a', 'b', 'c'
        let edges: Vec<_> = node.edges().collect();
        assert_eq!(edges.len(), 3);

        // Transition through 'a' -> 'p' -> 'p' -> 'l' -> 'e'
        let a_node = node.transition(b'a');
        assert!(a_node.is_some());

        let p1_node = a_node.unwrap().transition(b'p');
        assert!(p1_node.is_some());

        let p2_node = p1_node.unwrap().transition(b'p');
        assert!(p2_node.is_some());

        let l_node = p2_node.unwrap().transition(b'l');
        assert!(l_node.is_some());

        let e_node = l_node.unwrap().transition(b'e');
        assert!(e_node.is_some());
        assert!(e_node.unwrap().is_final());
    }

    #[test]
    fn test_bucket_with_empty_suffix() {
        let mut bucket = StringBucket::new();
        bucket.insert_key(b"").unwrap();
        bucket.insert_key(b"test").unwrap();

        let node: PersistentARTrieNode<()> = PersistentARTrieNode::new_bucket(bucket);

        // Should be final at root (empty suffix exists)
        assert!(node.is_final());
    }

    #[test]
    fn test_art_node_final() {
        let mut art = Node4::new();
        art.header.set_final(true);

        let node: PersistentARTrieNode<()> =
            PersistentARTrieNode::new_art_node(Node::N4(Box::new(art)), true, None);

        assert!(node.is_final());
    }

    #[test]
    fn test_clone() {
        let node: PersistentARTrieNode<()> = PersistentARTrieNode::new_root();
        let cloned = node.clone();

        assert!(!cloned.is_empty());
        assert!(!cloned.is_final());
    }
}
