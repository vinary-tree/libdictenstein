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

use log::warn;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::Result;
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

    fn preflight_existing_terminal_is_final(&self, term: &str) -> Result<Option<bool>> {
        // **F4:** `&self` taking the OR write lock — the lock is the exclusivity
        // anchor that replaces the old `&mut self` for the raw-pointer walk (no NEW
        // unsafe; the same raw-pointer pattern, now under the OR guard).
        let mut root_guard = self.root.write();
        let root = match &mut *root_guard {
            CharTrieRoot::Node(node) => node.as_mut(),
            CharTrieRoot::Empty => return Ok(None),
        };

        let mut current = root as *mut CharTrieNodeInner<V>;
        let mut terminal_is_final = root.is_final();
        for c in term.chars() {
            // Safety: current is valid and we hold exclusive access via the OR write
            // guard (single owned writer).
            let node = unsafe { &mut *current };
            let Some(ptr) = node.node.find_child(c as u32) else {
                return Ok(None);
            };
            if ptr.is_null() {
                return Ok(None);
            }
            let child = self.resolve_swizzled_ptr_mut(ptr)?;
            terminal_is_final = child.is_final();
            current = child as *mut CharTrieNodeInner<V>;
        }

        Ok(Some(terminal_is_final))
    }

    pub(super) fn preflight_insert_no_wal(&self, term: &str) -> Result<bool> {
        match self.preflight_existing_terminal_is_final(term)? {
            Some(is_final) => Ok(!is_final),
            None => Ok(true),
        }
    }

    pub(super) fn preflight_insert_with_value_no_wal(&self, term: &str) -> Result<()> {
        let _ = self.preflight_existing_terminal_is_final(term)?;
        Ok(())
    }

    pub(super) fn preflight_remove_no_wal(&self, term: &str) -> Result<bool> {
        match self.preflight_existing_terminal_is_final(term)? {
            Some(is_final) => Ok(is_final),
            None => Ok(false),
        }
    }

    /// Insert a term (internal, no WAL logging)
    ///
    /// **F4:** `&self` taking the OR write lock once. The guard is held for the
    /// whole raw-pointer walk (its target owns the nodes the pointers reference);
    /// the lock provides the single-owned-writer exclusivity that replaces the old
    /// `&mut self` for the unsafe walk (no NEW unsafe).
    pub(super) fn try_insert_impl_no_wal(&self, term: &str) -> Result<bool> {
        let mut root_guard = self.root.write();
        // Ensure we have a root node
        if matches!(&*root_guard, CharTrieRoot::Empty) {
            *root_guard = CharTrieRoot::Node(Box::new(CharTrieNodeInner::new()));
        }

        // Navigate to the insertion point using raw pointer for traversal.
        let root = match &mut *root_guard {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => unreachable!(),
        };

        let mut current = root;
        for c in term.chars() {
            // Safety: current is valid; exclusivity via the held OR write guard.
            let node = unsafe { &mut *current };
            current = self.get_or_create_child_lazy_ptr(node, c)?;
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if already final
        if node.is_final() {
            return Ok(false);
        }

        // Mark as final
        node.set_final(true);
        self.len.fetch_add(1, AtomicOrdering::Relaxed);
        self.dirty.store(true, AtomicOrdering::Release);
        Ok(true)
    }

    /// Insert a term with value (internal, no WAL logging)
    pub(super) fn try_insert_impl_no_wal_with_value(&self, term: &str, value: V) -> Result<bool> {
        // **F4:** `&self` + OR write guard (see `try_insert_impl_no_wal`).
        let mut root_guard = self.root.write();
        // Ensure we have a root node
        if matches!(&*root_guard, CharTrieRoot::Empty) {
            *root_guard = CharTrieRoot::Node(Box::new(CharTrieNodeInner::new()));
        }

        // Navigate to the insertion point using raw pointer for traversal
        let root = match &mut *root_guard {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => unreachable!(),
        };

        let mut current = root;
        for c in term.chars() {
            // Safety: current is valid; exclusivity via the held OR write guard.
            let node = unsafe { &mut *current };
            current = self.get_or_create_child_lazy_ptr(node, c)?;
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if already final
        if node.is_final() {
            // Update value if already exists
            node.value = Some(value);
            return Ok(false);
        }

        // Mark as final with value
        node.set_final(true);
        node.value = Some(value);
        self.len.fetch_add(1, AtomicOrdering::Relaxed);
        self.dirty.store(true, AtomicOrdering::Release);
        Ok(true)
    }

    /// Insert a term with value (internal, no WAL logging)

    /// Remove a term (internal, no WAL logging)
    pub(super) fn try_remove_impl_no_wal(&self, term: &str) -> Result<bool> {
        // **F4:** `&self` + OR write guard (see `try_insert_impl_no_wal`).
        let mut root_guard = self.root.write();
        let root = match &mut *root_guard {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => return Ok(false),
        };

        // Navigate to the node using raw pointer for traversal
        let chars: Vec<char> = term.chars().collect();
        let mut current = root;
        for &c in &chars {
            // Safety: current is valid; exclusivity via the held OR write guard.
            let node = unsafe { &*current };
            match self.get_child_mut_lazy(node, c) {
                Ok(Some(child)) => current = child as *mut CharTrieNodeInner<V>,
                Ok(None) => return Ok(false), // Term not found
                Err(error) => return Err(error),
            }
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if this node is final
        if !node.is_final() {
            return Ok(false);
        }

        // Mark as not final
        node.set_final(false);
        node.value = None;
        self.len.fetch_sub(1, AtomicOrdering::Relaxed);
        self.dirty.store(true, AtomicOrdering::Release);
        Ok(true)
    }

    pub(super) fn insert_impl_no_wal(&self, term: &str) -> bool {
        self.try_insert_impl_no_wal(term).unwrap_or_else(|error| {
            warn!(
                "I/O error during lazy loading in insert replay: {:?}",
                error
            );
            false
        })
    }

    #[allow(dead_code)] // L1.3: production-dead (the recovery appliers that called it are gone); retained for the in-crate owned white-box tests + L2/L3 owned-staging; removed with the owned path at L3.3
    pub(super) fn insert_impl_no_wal_with_value(&self, term: &str, value: V) -> bool {
        self.try_insert_impl_no_wal_with_value(term, value)
            .unwrap_or_else(|error| {
                warn!(
                    "I/O error during lazy loading in value insert replay: {:?}",
                    error
                );
                false
            })
    }

    #[allow(dead_code)] // L1.3: production-dead (the recovery appliers that called it are gone); retained for the in-crate owned white-box tests + L2/L3 owned-staging; removed with the owned path at L3.3
    pub(super) fn remove_impl_no_wal(&self, term: &str) -> bool {
        self.try_remove_impl_no_wal(term).unwrap_or_else(|error| {
            warn!(
                "I/O error during lazy loading in remove replay: {:?}",
                error
            );
            false
        })
    }
}
