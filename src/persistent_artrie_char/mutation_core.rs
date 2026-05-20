//! Core mutation implementations for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~143-273, ~131 LOC)
//! as the twenty-fourth and final Phase-6 char sub-module.
//! These are the pub(super) primitives that the public mutation
//! API (`mutation_api`) and recovery replay (`mmap_ctor` /
//! `io_uring_ctor`) call without WAL logging:
//!
//! - `insert_impl_no_wal` — insert without value
//! - `insert_impl_no_wal_with_value` — insert with value
//! - `remove_impl_no_wal` — remove
//!
//! These manage in-memory `CharTrieNodeInner<V>` directly, including
//! the path-compression / node-growth logic.

use std::sync::atomic::Ordering as AtomicOrdering;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::value::DictionaryValue;

use super::types::{CharTrieNodeInner, CharTrieRoot};

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// Load root from disk given the root descriptor pointer
    ///
    /// This function:
    /// 1. Reads the root descriptor block
    /// 2. Loads arena block IDs and populates the arena manager
    /// 3. Loads the root node (which can now read from arenas)
    ///
    /// # Arguments
    /// * `buffer_manager` - The buffer manager for disk I/O
    /// * `root_desc_ptr` - Pointer to the root descriptor block
    /// * `eager_depth` - Controls loading strategy:
    ///   - `None`: Fully lazy loading (only root node loaded)
    ///   - `Some(0)`: Same as None (lazy loading)
    ///   - `Some(n)`: Load n levels eagerly, rest lazy
    ///   - `Some(usize::MAX)`: Fully eager loading (all levels)

    /// Insert a term (internal, no WAL logging)
    pub(super) fn insert_impl_no_wal(&mut self, term: &str) -> bool {
        // Ensure we have a root node
        if matches!(self.root, CharTrieRoot::Empty) {
            self.root = CharTrieRoot::Node(Box::new(CharTrieNodeInner::new()));
        }

        // Navigate to the insertion point using raw pointer for traversal
        // This is safe because we maintain exclusive access through &mut self
        let root = match &mut self.root {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => unreachable!(),
        };

        let mut current = root;
        for c in term.chars() {
            // Safety: current is valid and we have exclusive access through &mut self
            let node = unsafe { &mut *current };
            current = self.get_or_create_child_lazy_ptr(node, c)
                .expect("I/O error during lazy loading in insert");
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if already final
        if node.is_final() {
            return false;
        }

        // Mark as final
        node.set_final(true);
        self.len.fetch_add(1, AtomicOrdering::Relaxed);
        self.dirty.store(true, AtomicOrdering::Release);
        true
    }


    /// Insert a term with value (internal, no WAL logging)
    pub(super) fn insert_impl_no_wal_with_value(&mut self, term: &str, value: V) -> bool {
        // Ensure we have a root node
        if matches!(self.root, CharTrieRoot::Empty) {
            self.root = CharTrieRoot::Node(Box::new(CharTrieNodeInner::new()));
        }

        // Navigate to the insertion point using raw pointer for traversal
        let root = match &mut self.root {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => unreachable!(),
        };

        let mut current = root;
        for c in term.chars() {
            // Safety: current is valid and we have exclusive access through &mut self
            let node = unsafe { &mut *current };
            current = self.get_or_create_child_lazy_ptr(node, c)
                .expect("I/O error during lazy loading in insert");
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if already final
        if node.is_final() {
            // Update value if already exists
            node.value = Some(value);
            return false;
        }

        // Mark as final with value
        node.set_final(true);
        node.value = Some(value);
        self.len.fetch_add(1, AtomicOrdering::Relaxed);
        self.dirty.store(true, AtomicOrdering::Release);
        true
    }

    /// Insert a term with value (internal, no WAL logging)

    /// Remove a term (internal, no WAL logging)
    pub(super) fn remove_impl_no_wal(&mut self, term: &str) -> bool {
        let root = match &mut self.root {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => return false,
        };

        // Navigate to the node using raw pointer for traversal
        let chars: Vec<char> = term.chars().collect();
        let mut current = root;
        for &c in &chars {
            // Safety: current is valid and we have exclusive access through &mut self
            let node = unsafe { &*current };
            match self.get_child_mut_lazy(node, c) {
                Ok(Some(child)) => current = child as *mut CharTrieNodeInner<V>,
                Ok(None) => return false, // Term not found
                Err(_) => return false, // I/O error during lazy load
            }
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if this node is final
        if !node.is_final() {
            return false;
        }

        // Mark as not final
        node.set_final(false);
        node.value = None;
        self.len.fetch_sub(1, AtomicOrdering::Relaxed);
        self.dirty.store(true, AtomicOrdering::Release);
        true
    }
}
