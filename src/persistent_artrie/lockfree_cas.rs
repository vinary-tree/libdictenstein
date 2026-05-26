//! Lock-free CAS-based methods for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (the entire `// Lock-Free CAS Methods`
//! cluster, lines ~2178-2767, ~590 LOC) as part of the Phase-5
//! decomposition. The cluster forms a coherent feature: an overlay
//! `AtomicNodePtr`-backed trie that lets concurrent writers insert /
//! increment without taking a write lock, plus the eventual merge path
//! into the persistent trie.
//!
//! All methods in this file are `pub` on `PersistentARTrie<V, S>` (or
//! private helpers used only by the cluster) and read/write the
//! `pub(crate)` fields directly — the layered storage state stays in
//! `dict_impl.rs`'s `struct PersistentARTrie` definition, this sibling
//! file just contains the lock-free `impl` methods.

#![cfg(feature = "persistent-artrie")]

use std::sync::Arc;

use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;
use super::error::{PersistentARTrieError, Result};
use super::wal::WalRecord;
use crate::value::DictionaryValue;

const LOCKFREE_COUNTER_MAX: u64 = i64::MAX as u64;

/// Result of a lock-free insert attempt.
///
/// Used by `insert_cas()` to communicate the outcome of a CAS operation.
#[derive(Debug)]
enum LockfreeInsertResult {
    /// Term was newly inserted - contains the node to finalize
    Inserted(Arc<super::nodes::PersistentNode>),
    /// Term already exists in the trie
    AlreadyExists,
    /// CAS conflict - another thread modified the tree, retry needed
    Conflict,
}

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Enable lock-free mode for concurrent inserts.
    ///
    /// This initializes the lock-free infrastructure including:
    /// - An `AtomicNodePtr` root for CAS-based tree modifications
    /// - A `DashMap` cache for fast lookups
    ///
    /// # Example
    ///
    /// ```text
    /// let mut trie = PersistentARTrie::<()>::create("trie.part")?;
    /// trie.enable_lockfree();
    /// trie.insert_cas(b"hello");  // Now works concurrently
    /// ```
    pub fn enable_lockfree(&mut self) {
        use super::nodes::atomic_ptr::AtomicNodePtr;
        use super::nodes::persistent_node::PersistentNode;
        use dashmap::DashMap;

        if self.lockfree_root.is_some() {
            return; // Already enabled
        }

        // Initialize with an empty root node
        let root_node = Arc::new(PersistentNode::new());
        self.lockfree_root = Some(AtomicNodePtr::new(root_node));
        self.lockfree_cache = Some(DashMap::new());
    }

    /// Lock-free insert using CAS operations.
    ///
    /// This method inserts a term into the lock-free trie structure without
    /// acquiring any locks. Multiple threads can call this concurrently.
    pub fn insert_cas(&self, term: &[u8]) -> bool {
        use std::sync::atomic::Ordering;

        let lockfree_root = self
            .lockfree_root
            .as_ref()
            .expect("Lock-free mode not enabled. Call enable_lockfree() first.");
        let lockfree_cache = self
            .lockfree_cache
            .as_ref()
            .expect("Lock-free mode not enabled. Call enable_lockfree() first.");

        // Fast path: check cache first
        if lockfree_cache.contains_key(term) {
            return false;
        }

        if term.is_empty() {
            return false;
        }

        // Enter the read epoch for safe memory access
        let _epoch = self.epoch_manager.enter_read();

        // CAS retry loop
        loop {
            match self.try_insert_lockfree_path(lockfree_root, term) {
                LockfreeInsertResult::Inserted(node) => {
                    // We inserted a new path - try to claim it as final
                    if node.try_set_final() {
                        // We won the race to finalize this node
                        lockfree_cache.insert(term.to_vec(), true);
                        return true;
                    } else {
                        // Another thread finalized it - the term already exists
                        return false;
                    }
                }
                LockfreeInsertResult::AlreadyExists => {
                    // Term already exists in the trie
                    lockfree_cache.insert(term.to_vec(), true);
                    return false;
                }
                LockfreeInsertResult::Conflict => {
                    // CAS failed due to concurrent modification - retry
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    /// Attempt to insert a path in the lock-free trie.
    fn try_insert_lockfree_path(
        &self,
        root: &super::nodes::AtomicNodePtr,
        term: &[u8],
    ) -> LockfreeInsertResult {
        use super::nodes::PersistentNode;

        let current_root = match root.load() {
            Some(node) => node,
            None => {
                let new_root = Arc::new(PersistentNode::new());
                match root.try_init(new_root) {
                    Ok(()) => return self.try_insert_lockfree_path(root, term),
                    Err(actual) => actual,
                }
            }
        };

        self.insert_lockfree_recursive(root, &current_root, term, 0)
    }

    /// Recursively build a new tree with the path inserted.
    ///
    /// Builds the path from leaf to root: recurses down to the target depth,
    /// creates the leaf node, then on the way back up creates new versions
    /// of each parent with updated child pointers.
    fn build_path_recursive(
        &self,
        node: &Arc<super::nodes::PersistentNode>,
        term: &[u8],
        depth: usize,
    ) -> std::result::Result<
        (
            Arc<super::nodes::PersistentNode>,
            Arc<super::nodes::PersistentNode>,
        ),
        (),
    > {
        use super::nodes::PersistentNode;
        use super::swizzled_ptr::SwizzledPtr;

        if depth == term.len() {
            if node.is_final() {
                return Err(()); // Already exists
            }
            let final_node = Arc::new(node.as_final());
            return Ok((final_node.clone(), final_node));
        }

        let key = term[depth];

        match node.find_child(key) {
            Some(child_ptr) => {
                if child_ptr.is_on_disk() {
                    return Err(());
                }

                if let Some(ptr) = child_ptr.as_ptr::<PersistentNode>() {
                    // SAFETY: lock-free child pointers are created from
                    // Arc::into_raw in this module. Incrementing the strong
                    // count before Arc::from_raw creates a temporary owned
                    // Arc for traversal while leaving the published child
                    // pointer valid for other readers.
                    let child = unsafe {
                        Arc::increment_strong_count(ptr);
                        Arc::from_raw(ptr)
                    };

                    let (new_child, leaf) = self.build_path_recursive(&child, term, depth + 1)?;

                    let new_child_ptr = SwizzledPtr::in_memory(Arc::into_raw(new_child));
                    let new_node = Arc::new(node.with_child(key, new_child_ptr));

                    Ok((new_node, leaf))
                } else {
                    Err(())
                }
            }
            None => {
                let (new_subtree, leaf) = self.create_lockfree_path(&term[depth + 1..]);
                let new_child_ptr = SwizzledPtr::in_memory(Arc::into_raw(new_subtree));
                let new_node = Arc::new(node.with_child(key, new_child_ptr));

                Ok((new_node, leaf))
            }
        }
    }

    /// Create a new path for the remaining bytes.
    ///
    /// Builds the path bottom-up: creates the final leaf node first, then
    /// wraps each byte as a parent going up to the start.
    fn create_lockfree_path(
        &self,
        term: &[u8],
    ) -> (
        Arc<super::nodes::PersistentNode>,
        Arc<super::nodes::PersistentNode>,
    ) {
        use super::nodes::PersistentNode;
        use super::swizzled_ptr::SwizzledPtr;

        let leaf = Arc::new(PersistentNode::new());

        if term.is_empty() {
            return (leaf.clone(), leaf);
        }

        let mut current = leaf.clone();

        for &b in term.iter().rev() {
            let child_ptr = SwizzledPtr::in_memory(Arc::into_raw(current));
            let parent = PersistentNode::new().with_child(b, child_ptr);
            current = Arc::new(parent);
        }

        (current, leaf)
    }

    /// Attempt to insert a path using CAS. Called from `insert_cas` retry loop.
    fn insert_lockfree_recursive(
        &self,
        root: &super::nodes::AtomicNodePtr,
        current: &Arc<super::nodes::PersistentNode>,
        term: &[u8],
        _depth: usize,
    ) -> LockfreeInsertResult {
        match self.build_path_recursive(current, term, 0) {
            Ok((new_root, leaf)) => match root.compare_exchange(current, new_root) {
                Ok(_) => LockfreeInsertResult::Inserted(leaf),
                Err(_actual) => LockfreeInsertResult::Conflict,
            },
            Err(()) => LockfreeInsertResult::AlreadyExists,
        }
    }

    /// Check if a term exists in the lock-free trie.
    ///
    /// Fast, lock-free lookup that checks the cache first.
    pub fn contains_lockfree(&self, term: &[u8]) -> bool {
        if let Some(ref cache) = self.lockfree_cache {
            if cache.contains_key(term) {
                return true;
            }
        }

        if let Some(ref root) = self.lockfree_root {
            if let Some(root_node) = root.load() {
                return self.find_in_lockfree_trie(&root_node, term, 0);
            }
        }

        false
    }

    /// Navigate the lock-free trie to find a term.
    fn find_in_lockfree_trie(
        &self,
        node: &Arc<super::nodes::PersistentNode>,
        term: &[u8],
        depth: usize,
    ) -> bool {
        use super::nodes::PersistentNode;

        if depth >= term.len() {
            return node.is_final();
        }

        let key = term[depth];
        if let Some(child_ptr) = node.find_child(key) {
            if child_ptr.is_on_disk() {
                return false;
            }

            if let Some(ptr) = child_ptr.as_ptr::<PersistentNode>() {
                // SAFETY: see build_path_recursive. The raw child pointer is
                // an Arc allocation published by Arc::into_raw; bumping the
                // strong count before from_raw keeps this traversal's Arc
                // independent of the published pointer.
                let child = unsafe {
                    Arc::increment_strong_count(ptr);
                    Arc::from_raw(ptr)
                };
                return self.find_in_lockfree_trie(&child, term, depth + 1);
            }
        }

        false
    }

    /// Merge lock-free entries into the persistent trie.
    ///
    /// Takes entries from the lock-free cache and inserts them into the
    /// persistent trie structure. Call this during checkpoints or before
    /// saving to ensure all entries are persisted.
    pub fn merge_lockfree_to_persistent(&mut self) -> Result<usize> {
        let entries: Vec<Vec<u8>> = match &self.lockfree_cache {
            Some(cache) => cache.iter().map(|e| e.key().clone()).collect(),
            None => return Ok(0),
        };

        let mut count = 0;
        for term in entries {
            if self.insert_impl(&term, None) {
                count += 1;
            }
        }

        if let Some(ref cache) = self.lockfree_cache {
            cache.clear();
        }

        Ok(count)
    }

    /// Get the number of CAS retries (for monitoring contention).
    #[inline]
    pub fn cas_retry_count(&self) -> u64 {
        self.cas_retries.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Merge lock-free values into the persistent trie by summing.
    ///
    /// Unlike `merge_lockfree_to_persistent()` which does boolean insert,
    /// this method walks the lock-free trie overlay, collects all
    /// `(key, value)` entries, and adds each value to the persistent trie
    /// via `increment_bytes`.
    pub fn merge_lockfree_values_to_persistent(&mut self) -> Result<usize> {
        use super::nodes::PersistentNode;

        let entries = {
            let lockfree_root = match self.lockfree_root.as_ref() {
                Some(root) => root,
                None => return Ok(0),
            };

            let root_node = match lockfree_root.load() {
                Some(node) => node,
                None => return Ok(0),
            };

            let mut entries: Vec<(Vec<u8>, u64)> = Vec::new();
            let mut key_buf: Vec<u8> = Vec::new();
            Self::collect_lockfree_entries_recursive(&root_node, &mut key_buf, &mut entries);
            entries
        };

        if entries.is_empty() {
            return Ok(0);
        }

        let (wal_entries, prepared_values) = self.prepare_lockfree_value_merge(&entries)?;
        let merged_count = wal_entries.len();

        self.append_mutation_wal_record(
            WalRecord::BatchIncrement {
                entries: wal_entries,
            },
            "lockfree_value_merge",
        )?;

        for (key, value) in prepared_values {
            self.upsert_impl_no_wal(&key, value);
        }

        if let Some(ref cache) = self.lockfree_cache {
            cache.clear();
        }
        if let Some(ref root) = self.lockfree_root {
            root.store(Arc::new(PersistentNode::new()));
        }

        Ok(merged_count)
    }

    fn prepare_lockfree_value_merge(
        &self,
        entries: &[(Vec<u8>, u64)],
    ) -> Result<(Vec<(Vec<u8>, i64)>, Vec<(Vec<u8>, V)>)> {
        let mut wal_entries = Vec::with_capacity(entries.len());
        let mut prepared_values = Vec::with_capacity(entries.len());

        for (key, delta) in entries {
            let delta_i64 = Self::lockfree_delta_to_i64(key, *delta)?;
            let current = self.current_i64_for_lockfree_merge(key)?;
            let new_value = current.checked_add(delta_i64).ok_or_else(|| {
                PersistentARTrieError::InvalidOperation(format!(
                    "lock-free merge increment overflow for term {:?}: {} + {} exceeds i64 range",
                    String::from_utf8_lossy(key),
                    current,
                    delta_i64
                ))
            })?;
            let value = Self::value_from_i64_for_lockfree_merge(new_value)?;

            wal_entries.push((key.clone(), delta_i64));
            prepared_values.push((key.clone(), value));
        }

        Ok((wal_entries, prepared_values))
    }

    fn current_i64_for_lockfree_merge(&self, term: &[u8]) -> Result<i64> {
        match self.get_value_impl(term) {
            Some(value) => {
                let bytes =
                    crate::serialization::bincode_compat::serialize(&value).map_err(|e| {
                        PersistentARTrieError::internal(format!("Serialization error: {}", e))
                    })?;
                if bytes.len() == 8 {
                    Ok(i64::from_le_bytes(
                        bytes.try_into().expect("checked len == 8"),
                    ))
                } else {
                    crate::serialization::bincode_compat::deserialize::<i64>(&bytes).map_err(|e| {
                        PersistentARTrieError::internal(format!(
                            "Value cannot be interpreted as i64: {}",
                            e
                        ))
                    })
                }
            }
            None => Ok(0),
        }
    }

    fn value_from_i64_for_lockfree_merge(value: i64) -> Result<V> {
        let value_bytes = crate::serialization::bincode_compat::serialize(&value)
            .map_err(|e| PersistentARTrieError::internal(format!("Serialization error: {}", e)))?;
        crate::serialization::bincode_compat::deserialize(&value_bytes).map_err(|e| {
            PersistentARTrieError::internal(format!("Cannot create value from i64: {}", e))
        })
    }

    fn lockfree_delta_to_i64(term: &[u8], delta: u64) -> Result<i64> {
        i64::try_from(delta).map_err(|_| {
            PersistentARTrieError::InvalidOperation(format!(
                "lock-free counter value for term {:?} exceeds i64 persistence domain: {}",
                String::from_utf8_lossy(term),
                delta
            ))
        })
    }

    /// Recursively collect all (key, value) entries from the lock-free trie.
    fn collect_lockfree_entries_recursive(
        node: &Arc<super::nodes::PersistentNode>,
        key_buf: &mut Vec<u8>,
        entries: &mut Vec<(Vec<u8>, u64)>,
    ) {
        use super::nodes::PersistentNode;

        if node.is_final() {
            if let Some(value) = node.get_value() {
                entries.push((key_buf.clone(), value));
            }
        }

        for (&child_key, child_ptr) in node.iter_children() {
            if child_ptr.is_on_disk() {
                continue;
            }
            if let Some(ptr) = child_ptr.as_ptr::<PersistentNode>() {
                // SAFETY: lock-free child pointers are Arc allocations that
                // remain published through SwizzledPtr raw values. The strong
                // count is incremented before reconstructing this temporary
                // Arc so collection owns a valid traversal reference.
                let child = unsafe {
                    Arc::increment_strong_count(ptr);
                    Arc::from_raw(ptr)
                };
                key_buf.push(child_key);
                Self::collect_lockfree_entries_recursive(&child, key_buf, entries);
                key_buf.pop();
            }
        }
    }

    /// Find the leaf node for a key in the lock-free trie.
    fn find_leaf_lockfree(
        &self,
        root: &super::nodes::AtomicNodePtr,
        key: &[u8],
    ) -> Option<Arc<super::nodes::PersistentNode>> {
        let current = root.load()?;
        self.find_leaf_recursive(&current, key, 0)
    }

    /// Recursive helper for `find_leaf_lockfree`.
    fn find_leaf_recursive(
        &self,
        node: &Arc<super::nodes::PersistentNode>,
        key: &[u8],
        depth: usize,
    ) -> Option<Arc<super::nodes::PersistentNode>> {
        use super::nodes::PersistentNode;

        if depth == key.len() {
            return if node.is_final() {
                Some(Arc::clone(node))
            } else {
                None
            };
        }

        let child_ptr = node.find_child(key[depth])?;
        if child_ptr.is_on_disk() {
            return None;
        }

        let ptr = child_ptr.as_ptr::<PersistentNode>()?;
        // SAFETY: lock-free child pointers are Arc allocations published by
        // Arc::into_raw in this module. Increment before from_raw so the
        // returned Arc is an owned traversal reference.
        let child = unsafe {
            Arc::increment_strong_count(ptr);
            Arc::from_raw(ptr)
        };
        self.find_leaf_recursive(&child, key, depth + 1)
    }

    /// Lock-free read of a value from the lock-free trie overlay.
    #[inline]
    pub fn get_lockfree(&self, key: &[u8]) -> Option<u64> {
        let lockfree_root = self.lockfree_root.as_ref()?;
        let _epoch = self.epoch_manager.enter_read();
        self.find_leaf_lockfree(lockfree_root, key)
            .and_then(|leaf| leaf.get_value())
    }

    /// Checked lock-free increment: create path if needed, then atomically add delta.
    ///
    /// For existing keys: single `fetch_add` on the leaf (wait-free).
    /// For new keys: CAS retry loop to create path, then set initial value.
    pub fn try_increment_cas(&self, key: &[u8], delta: u64) -> Result<u64> {
        use std::sync::atomic::Ordering;

        let lockfree_root = self
            .lockfree_root
            .as_ref()
            .expect("Lock-free mode not enabled. Call enable_lockfree() first.");

        if key.is_empty() {
            return Ok(0);
        }

        if delta > LOCKFREE_COUNTER_MAX {
            return Err(Self::lockfree_increment_overflow_error(key, None, delta));
        }

        let _epoch = self.epoch_manager.enter_read();

        if let Some(leaf) = self.find_leaf_lockfree(lockfree_root, key) {
            return leaf
                .try_increment_value(delta, LOCKFREE_COUNTER_MAX)
                .ok_or_else(|| {
                    Self::lockfree_increment_overflow_error(key, leaf.get_value(), delta)
                });
        }

        loop {
            match self.try_insert_lockfree_path(lockfree_root, key) {
                LockfreeInsertResult::Inserted(leaf) => {
                    leaf.try_set_final();
                    return leaf
                        .try_increment_value(delta, LOCKFREE_COUNTER_MAX)
                        .ok_or_else(|| {
                            Self::lockfree_increment_overflow_error(key, leaf.get_value(), delta)
                        });
                }
                LockfreeInsertResult::AlreadyExists => {
                    if let Some(leaf) = self.find_leaf_lockfree(lockfree_root, key) {
                        return leaf
                            .try_increment_value(delta, LOCKFREE_COUNTER_MAX)
                            .ok_or_else(|| {
                                Self::lockfree_increment_overflow_error(
                                    key,
                                    leaf.get_value(),
                                    delta,
                                )
                            });
                    }
                    continue;
                }
                LockfreeInsertResult::Conflict => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    /// Lock-free increment: create path if needed, then atomically add delta.
    ///
    /// Panics if the checked counter domain would be exceeded. Use
    /// [`Self::try_increment_cas`] to handle overflow as a recoverable error.
    pub fn increment_cas(&self, key: &[u8], delta: u64) -> u64 {
        self.try_increment_cas(key, delta)
            .unwrap_or_else(|error| panic!("lock-free increment_cas failed: {}", error))
    }

    fn lockfree_increment_overflow_error(
        key: &[u8],
        current: Option<u64>,
        delta: u64,
    ) -> PersistentARTrieError {
        PersistentARTrieError::InvalidOperation(format!(
            "lock-free increment overflow for term {:?}: current {:?} + {} exceeds i64 persistence domain",
            String::from_utf8_lossy(key),
            current,
            delta
        ))
    }
}
