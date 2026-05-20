//! DFS iterators for `PersistentVocabARTrie<S>`.
//!
//! Split out of vocab `dict_impl.rs` (lines ~225-320, ~96 LOC) as
//! the twelfth Phase-6 vocab sub-module. Types covered:
//!
//! - `VocabTermIterator<'a>` — DFS iterator over all terms
//! - `VocabPrefixIterator<'a>` — DFS iterator over terms with a prefix
//!
//! Both iterators are `pub(super)` so the sibling `path_query.rs`
//! that calls their `::new` constructors compiles.

use crate::persistent_artrie::block_storage::BlockStorage;

use super::dict_impl::PersistentVocabARTrie;
use super::types::{VocabTrieNode, VocabTrieRoot};

/// Iterator over all terms in a PersistentVocabARTrie.
pub(super) struct VocabTermIterator<'a> {
    /// Stack of (node, path, edge_index) for DFS traversal
    stack: Vec<(&'a VocabTrieNode, Vec<char>, usize)>,
}

impl<'a> VocabTermIterator<'a> {
    pub(super) fn new<S: BlockStorage>(trie: &'a PersistentVocabARTrie<S>) -> Self {
        let mut iter = Self { stack: Vec::new() };
        if let VocabTrieRoot::Node(ref root) = trie.root {
            iter.stack.push((root.as_ref(), Vec::new(), 0));
        }
        iter
    }
}

impl Iterator for VocabTermIterator<'_> {
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((node, path, edge_idx)) = self.stack.pop() {
            // Collect children to a vec so we can index them
            let children: Vec<_> = node.iter_children().collect();

            if edge_idx < children.len() {
                let (label, child) = children[edge_idx];
                let mut new_path = path.clone();
                new_path.push(label);

                // Push current node back with next edge index
                self.stack.push((node, path, edge_idx + 1));
                // Push child to visit
                self.stack.push((child, new_path, 0));
            } else if node.is_final() && !path.is_empty() {
                // All children visited, and this is a final node
                return Some(path.into_iter().collect());
            }
        }
        None
    }
}

/// Iterator over terms with a specific prefix.
pub(super) struct VocabPrefixIterator<'a> {
    /// Stack of (node, path, edge_index) for DFS traversal
    stack: Vec<(&'a VocabTrieNode, Vec<char>, usize)>,
    /// The prefix (already navigated to)
    #[allow(dead_code)]
    prefix: Vec<char>,
}

impl<'a> VocabPrefixIterator<'a> {
    pub(super) fn new<S: BlockStorage>(trie: &'a PersistentVocabARTrie<S>, prefix: Vec<char>) -> Self {
        let mut iter = Self {
            stack: Vec::new(),
            prefix: prefix.clone(),
        };

        // Navigate to prefix node
        if let VocabTrieRoot::Node(ref root) = trie.root {
            let mut current = root.as_ref();
            for &c in &prefix {
                match current.get_child(c) {
                    Some(child) => current = child,
                    None => return iter, // Prefix doesn't exist
                }
            }
            // Start DFS from prefix node
            iter.stack.push((current, prefix, 0));
        }
        iter
    }
}

impl Iterator for VocabPrefixIterator<'_> {
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((node, path, edge_idx)) = self.stack.pop() {
            let children: Vec<_> = node.iter_children().collect();

            if edge_idx < children.len() {
                let (label, child) = children[edge_idx];
                let mut new_path = path.clone();
                new_path.push(label);

                self.stack.push((node, path, edge_idx + 1));
                self.stack.push((child, new_path, 0));
            } else if node.is_final() {
                // Return the term (path includes the prefix)
                return Some(path.into_iter().collect());
            }
        }
        None
    }
}
