//! Public query API for `PersistentVocabARTrie<S>`.
//!
//! Split out of vocab `dict_impl.rs` (lines ~928-1057, ~130 LOC) as
//! a Phase-6 vocab sub-module. Methods covered:
//!
//! - `get_index` (term → u64 index)
//! - `get_term` (u64 index → reconstructed term via parent-pointer
//!   backtracking + reverse-index cache)
//! - `contains` / `contains_index`
//! - `len` / `is_empty`
//! - `reserve_node_map`
//! - `start_index` / `next_index`

use std::sync::atomic::Ordering;

use crate::persistent_artrie::block_storage::BlockStorage;

use super::types::{NodeRef, VocabTrieRoot};

impl<S: BlockStorage> super::dict_impl::PersistentVocabARTrie<S> {
    pub fn get_index(&self, term: &str) -> Option<u64> {
        // Flip routing: under route_overlay() the lock-free overlay is the live rep.
        if self.route_overlay() {
            return self.get_index_lockfree(term);
        }
        let chars: Vec<char> = term.chars().collect();

        match &self.root {
            VocabTrieRoot::Empty => None,
            VocabTrieRoot::Node(root) => {
                let mut current = root.as_ref();

                for &c in &chars {
                    match current.get_child(c) {
                        Some(child) => current = child,
                        None => return None,
                    }
                }

                if current.is_final() {
                    current.get_value()
                } else {
                    None
                }
            }
        }
    }

    /// Get the term for a vocabulary index.
    ///
    /// # Performance
    ///
    /// - O(1) if cached (LRU cache hit)
    /// - O(k) if not cached (parent pointer backtracking, where k = term length)
    pub fn get_term(&self, index: u64) -> Option<String> {
        // Flip routing: under route_overlay() the reverse map (id->term) is the live
        // reverse index (the owned reverse_index/parent-pointer machinery is deleted at V6).
        if self.route_overlay() {
            return self
                .reverse_term_map
                .as_ref()
                .and_then(|m| m.get(&index).map(|e| e.value().clone()));
        }
        // Check cache first
        if let Some(term) = self.reverse_cache.get(index) {
            return Some(term);
        }

        // Look up in reverse index
        let node_ref = {
            let reverse_index = self.reverse_index.as_ref()?;
            reverse_index.get(index)?
        };

        // Reconstruct term via parent pointer backtracking
        let term = self.reconstruct_term(node_ref)?;

        // Cache for future lookups
        self.reverse_cache.put(index, term.clone());

        Some(term)
    }

    /// Reconstruct a term by backtracking parent pointers.
    fn reconstruct_term(&self, node_ref: NodeRef) -> Option<String> {
        let node_ptr = *self.node_map.get(&node_ref)?;
        let node = unsafe { &*node_ptr };

        let mut chars: Vec<char> = Vec::new();
        let mut current = node;

        // Walk up the tree
        while !current.parent.is_null() {
            if let Some(c) = char::from_u32(current.parent_edge) {
                chars.push(c);
            }
            match self.node_map.get(&current.parent) {
                Some(&ptr) => current = unsafe { &*ptr },
                None => break,
            }
        }

        // Reverse to get correct order
        chars.reverse();
        Some(chars.into_iter().collect())
    }

    /// Check if a term exists in the vocabulary.
    #[inline]
    pub fn contains(&self, term: &str) -> bool {
        self.get_index(term).is_some()
    }

    /// Check if an index exists in the vocabulary.
    #[inline]
    pub fn contains_index(&self, index: u64) -> bool {
        self.get_term(index).is_some()
    }

    /// Get the number of vocabulary entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.entry_count.load(Ordering::Acquire)
    }

    /// Check if the vocabulary is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Pre-allocate capacity in the internal node map.
    ///
    /// Call this before bulk insertions (e.g., merging lock-free vocabulary)
    /// to avoid HashMap resize doubling spikes. During resize, both the old
    /// and new backing arrays coexist in memory simultaneously — for a
    /// 5.8M-word vocabulary, this can cause a ~6.4 GB transient spike.
    ///
    /// # Arguments
    ///
    /// * `additional` - Number of additional nodes to reserve space for.
    ///   A good estimate is `estimated_terms * 8` (average trie depth).
    pub fn reserve_node_map(&mut self, additional: usize) {
        self.node_map.reserve(additional);
    }

    /// Get the starting index.
    #[inline]
    pub fn start_index(&self) -> u64 {
        self.start_index
    }

    /// Get the next index to be assigned.
    #[inline]
    pub fn next_index(&self) -> u64 {
        self.next_index.load(Ordering::Acquire)
    }
}
