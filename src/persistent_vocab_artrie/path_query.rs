//! Path-based query API for `PersistentVocabARTrie<S>`.
//!
//! Split out of vocab `dict_impl.rs` (lines ~701-772, ~73 LOC) as
//! a Phase-6 vocab sub-module. Methods covered:
//!
//! - `get_root_children` — list children of the root (label + is_final)
//! - `get_children_at_path` — same, but for the node at an arbitrary path
//! - `is_final_at_path` — predicate over the node at a path
//! - `iter_terms` / `iter_terms_with_prefix` — DFS iterators (the
//!   underlying `VocabTermIterator` / `VocabPrefixIterator` impls live
//!   in `dict_impl.rs`)

use crate::persistent_artrie::block_storage::BlockStorage;

use super::iterators::{VocabPrefixIterator, VocabTermIterator};
use super::types::VocabTrieRoot;

impl<S: BlockStorage> super::dict_impl::PersistentVocabARTrie<S> {
    /// Get root children information for Dictionary trait implementation.
    ///
    /// Returns a vector of (label, is_final) pairs for all children of the root node.
    pub fn get_root_children(&self) -> Vec<(char, bool)> {
        match &self.root {
            VocabTrieRoot::Empty => Vec::new(),
            VocabTrieRoot::Node(root) => root
                .iter_children()
                .map(|(c, child)| (c, child.is_final()))
                .collect(),
        }
    }

    /// Get children of a node at the given path.
    ///
    /// Returns a vector of (label, is_final) pairs for all children.
    pub fn get_children_at_path(&self, path: &[char]) -> Vec<(char, bool)> {
        match &self.root {
            VocabTrieRoot::Empty => Vec::new(),
            VocabTrieRoot::Node(root) => {
                let mut current = root.as_ref();
                for &c in path {
                    match current.get_child(c) {
                        Some(child) => current = child,
                        None => return Vec::new(),
                    }
                }
                current
                    .iter_children()
                    .map(|(c, child)| (c, child.is_final()))
                    .collect()
            }
        }
    }

    /// Check if the node at the given path is final.
    pub fn is_final_at_path(&self, path: &[char]) -> bool {
        match &self.root {
            VocabTrieRoot::Empty => false,
            VocabTrieRoot::Node(root) => {
                if path.is_empty() {
                    return root.is_final();
                }
                let mut current = root.as_ref();
                for &c in path {
                    match current.get_child(c) {
                        Some(child) => current = child,
                        None => return false,
                    }
                }
                current.is_final()
            }
        }
    }

    /// Iterate over all terms in the vocabulary.
    ///
    /// This performs a depth-first traversal of the trie to enumerate all terms.
    /// Note: For large vocabularies, consider using the reverse index lookup
    /// via `get_term(index)` for specific indices instead.
    pub fn iter_terms(&self) -> impl Iterator<Item = String> + '_ {
        VocabTermIterator::new(self)
    }

    /// Iterate over terms with the given prefix.
    ///
    /// Returns an iterator over all terms that start with the given prefix.
    pub fn iter_terms_with_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> impl Iterator<Item = String> + 'a {
        let prefix_chars: Vec<char> = prefix.chars().collect();
        VocabPrefixIterator::new(self, prefix_chars)
    }
}
