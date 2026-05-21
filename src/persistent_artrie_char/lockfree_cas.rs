//! Lock-free CAS-based insert/contains methods for `PersistentARTrieChar<V>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~470-1148, ~679 LOC)
//! as a Phase-6 char sub-module, mirroring the byte
//! `super::lockfree_cas` split. Methods covered:
//!
//! - `enable_lockfree` — set up `AtomicNodePtr` root + DashMap cache
//! - `insert_cas` / `contains_lockfree` — CAS-driven concurrent ops
//! - `get_lockfree` / `increment_cas` / `cas_retry_count`
//! - `merge_lockfree_to_persistent` / `merge_lockfree_values_to_persistent`
//! - Private DFS helpers: `try_insert_lockfree_path`, `build_path_recursive`,
//!   `create_lockfree_path`, `insert_lockfree_recursive`,
//!   `find_in_lockfree_trie`, `find_leaf_lockfree`, `find_leaf_recursive`,
//!   `merge_lockfree_zipper`, `chars_to_utf8_bytes`

use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::Arc;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::WalRecord;
use crate::value::DictionaryValue;

use super::dict_impl_char::LockfreeInsertResult;
use super::types::{CharTrieNodeInner, CharTrieRoot};

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    // ==================== Lock-Free CAS Methods (Phase 4) ====================

    /// Enable lock-free mode for this trie.
    ///
    /// This initializes the lock-free infrastructure including:
    /// - An `AtomicNodePtr` root for CAS-based tree modifications
    /// - A `DashMap` cache for fast lookups
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut trie = PersistentARTrieChar::<()>::create("trie.artc")?;
    /// trie.enable_lockfree();
    /// trie.insert_cas("hello");  // Now works concurrently
    /// ```
    pub fn enable_lockfree(&mut self) {
        use super::nodes::atomic_ptr::AtomicNodePtr;
        use super::nodes::persistent_node::PersistentCharNode;
        use dashmap::DashMap;

        if self.lockfree_root.is_some() {
            return; // Already enabled
        }

        // Initialize with an empty root node
        let root_node = Arc::new(PersistentCharNode::new());
        self.lockfree_root = Some(AtomicNodePtr::new(root_node));
        self.lockfree_cache = Some(DashMap::new());
    }

    /// Lock-free insert using CAS operations.
    ///
    /// This method inserts a term into the lock-free trie structure without
    /// acquiring any locks. Multiple threads can call this concurrently.
    ///
    /// # Arguments
    ///
    /// * `term` - The term to insert
    ///
    /// # Returns
    ///
    /// `true` if the term was newly inserted, `false` if it already existed.
    ///
    /// # Panics
    ///
    /// Panics if `enable_lockfree()` was not called first.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut trie = PersistentARTrieChar::<()>::create("trie.artc")?;
    /// trie.enable_lockfree();
    ///
    /// let inserted = trie.insert_cas("hello");
    /// assert!(inserted);
    ///
    /// let inserted2 = trie.insert_cas("hello");
    /// assert!(!inserted2);  // Already exists
    /// ```
    pub fn insert_cas(&self, term: &str) -> bool {
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

        // Convert term to Unicode code points
        let chars: Vec<u32> = term.chars().map(|c| c as u32).collect();
        if chars.is_empty() {
            return false;
        }

        // Enter the read epoch for safe memory access
        let _epoch = self.epoch_manager.enter_read();

        // CAS retry loop
        loop {
            match self.try_insert_lockfree_path(lockfree_root, &chars) {
                LockfreeInsertResult::Inserted(node) => {
                    // We inserted a new path - try to claim it as final
                    if node.try_set_final() {
                        // We won the race to finalize this node
                        lockfree_cache.insert(term.to_string(), true);
                        return true;
                    } else {
                        // Another thread finalized it - the term already exists
                        return false;
                    }
                }
                LockfreeInsertResult::AlreadyExists => {
                    // Term already exists in the trie
                    lockfree_cache.insert(term.to_string(), true);
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
    ///
    /// Returns the result of the insertion attempt.
    fn try_insert_lockfree_path(
        &self,
        root: &super::nodes::atomic_ptr::AtomicNodePtr,
        chars: &[u32],
    ) -> LockfreeInsertResult {
        use super::nodes::persistent_node::PersistentCharNode;

        // Load current root
        let current_root = match root.load() {
            Some(node) => node,
            None => {
                // Root is null - try to initialize it
                let new_root = Arc::new(PersistentCharNode::new());
                match root.try_init(new_root) {
                    Ok(()) => return self.try_insert_lockfree_path(root, chars),
                    Err(actual) => actual,
                }
            }
        };

        // Navigate/create path to the target node
        self.insert_lockfree_recursive(root, &current_root, chars, 0)
    }

    /// Recursively build a new tree with the path inserted.
    ///
    /// This method builds the path from leaf to root: it recurses down to the
    /// target depth, creates the leaf node, then on the way back up creates
    /// new versions of each parent with updated child pointers.
    ///
    /// # Returns
    ///
    /// - `Ok(new_node, leaf)` - New version of this node with path inserted, plus leaf node
    /// - `Err(())` - Term already exists (node is already final at target depth)
    fn build_path_recursive(
        &self,
        node: &Arc<super::nodes::persistent_node::PersistentCharNode>,
        chars: &[u32],
        depth: usize,
    ) -> std::result::Result<
        (
            Arc<super::nodes::persistent_node::PersistentCharNode>,
            Arc<super::nodes::persistent_node::PersistentCharNode>,
        ),
        (),
    > {
        use super::nodes::persistent_node::PersistentCharNode;
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

        if depth == chars.len() {
            // Reached target depth - mark as final
            if node.is_final() {
                return Err(()); // Already exists
            }
            let final_node = Arc::new(node.as_final());
            return Ok((final_node.clone(), final_node));
        }

        let key = chars[depth];

        match node.find_child(key) {
            Some(child_ptr) => {
                // Child exists - check if it's on disk
                if child_ptr.is_on_disk() {
                    // On-disk child means this path exists in persistent trie
                    // For lock-free overlay, we can't easily check this
                    // Mark as conflict to force re-check via cache/persistent lookup
                    return Err(());
                }

                // In-memory child - traverse into it
                if let Some(ptr) = child_ptr.as_ptr::<PersistentCharNode>() {
                    let child = unsafe {
                        Arc::increment_strong_count(ptr);
                        Arc::from_raw(ptr)
                    };

                    // Recursively build path in child
                    let (new_child, leaf) = self.build_path_recursive(&child, chars, depth + 1)?;

                    // Create new version of this node with updated child pointer
                    let new_child_ptr = SwizzledPtr::in_memory(Arc::into_raw(new_child));
                    let new_node = Arc::new(node.with_child(key, new_child_ptr));

                    Ok((new_node, leaf))
                } else {
                    // Null pointer shouldn't happen
                    Err(())
                }
            }
            None => {
                // Child doesn't exist - create entire remaining path
                let (new_subtree, leaf) = self.create_lockfree_path(&chars[depth + 1..]);
                let new_child_ptr = SwizzledPtr::in_memory(Arc::into_raw(new_subtree));
                let new_node = Arc::new(node.with_child(key, new_child_ptr));

                Ok((new_node, leaf))
            }
        }
    }

    /// Create a new path for the remaining characters.
    ///
    /// Builds the path bottom-up: creates the final leaf node first,
    /// then wraps each character as a parent going up to the start.
    ///
    /// # Returns
    ///
    /// A tuple of (subtree_root, leaf_node) where:
    /// - subtree_root is the top of the new path (to be attached as a child)
    /// - leaf_node is the final node (to have try_set_final called on it)
    fn create_lockfree_path(
        &self,
        chars: &[u32],
    ) -> (
        Arc<super::nodes::persistent_node::PersistentCharNode>,
        Arc<super::nodes::persistent_node::PersistentCharNode>,
    ) {
        use super::nodes::persistent_node::PersistentCharNode;
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

        // Create the final leaf node (not marked final yet - caller will try_set_final)
        let leaf = Arc::new(PersistentCharNode::new());

        if chars.is_empty() {
            // No more characters - leaf is also the root
            return (leaf.clone(), leaf);
        }

        // Build path bottom-up
        let mut current = leaf.clone();

        for &c in chars.iter().rev() {
            let child_ptr = SwizzledPtr::in_memory(Arc::into_raw(current));
            let parent = PersistentCharNode::new().with_child(c, child_ptr);
            current = Arc::new(parent);
        }

        (current, leaf)
    }

    /// Attempt to insert a path using CAS. Called from insert_cas retry loop.
    fn insert_lockfree_recursive(
        &self,
        root: &super::nodes::atomic_ptr::AtomicNodePtr,
        current: &Arc<super::nodes::persistent_node::PersistentCharNode>,
        chars: &[u32],
        _depth: usize, // Kept for API compatibility
    ) -> LockfreeInsertResult {
        // Build the new tree structure with the path inserted
        match self.build_path_recursive(current, chars, 0) {
            Ok((new_root, leaf)) => {
                // Try to CAS the root to the new version
                match root.compare_exchange(current, new_root) {
                    Ok(_) => {
                        // Successfully updated the tree
                        LockfreeInsertResult::Inserted(leaf)
                    }
                    Err(_actual) => {
                        // CAS failed - another thread modified the tree
                        LockfreeInsertResult::Conflict
                    }
                }
            }
            Err(()) => {
                // Term already exists or on-disk reference found
                LockfreeInsertResult::AlreadyExists
            }
        }
    }

    /// Check if a term exists in the lock-free trie.
    ///
    /// This is a fast, lock-free lookup that checks the cache first.
    pub fn contains_lockfree(&self, term: &str) -> bool {
        if let Some(ref cache) = self.lockfree_cache {
            if cache.contains_key(term) {
                return true;
            }
        }

        // Fall back to checking the lock-free trie structure
        if let Some(ref root) = self.lockfree_root {
            if let Some(root_node) = root.load() {
                let chars: Vec<u32> = term.chars().map(|c| c as u32).collect();
                return self.find_in_lockfree_trie(&root_node, &chars, 0);
            }
        }

        false
    }

    /// Navigate the lock-free trie to find a term.
    fn find_in_lockfree_trie(
        &self,
        node: &Arc<super::nodes::persistent_node::PersistentCharNode>,
        chars: &[u32],
        depth: usize,
    ) -> bool {
        use super::nodes::persistent_node::PersistentCharNode;

        if depth >= chars.len() {
            return node.is_final();
        }

        let key = chars[depth];
        if let Some(child_ptr) = node.find_child(key) {
            if child_ptr.is_on_disk() {
                // On-disk reference - can't traverse in lock-free overlay
                // The persistent trie would need to be checked
                return false;
            }

            // In-memory child - traverse into it
            if let Some(ptr) = child_ptr.as_ptr::<PersistentCharNode>() {
                let child = unsafe {
                    Arc::increment_strong_count(ptr);
                    Arc::from_raw(ptr)
                };
                return self.find_in_lockfree_trie(&child, chars, depth + 1);
            }
        }

        false
    }

    /// Merge lock-free entries into the persistent trie.
    ///
    /// This method takes entries from the lock-free cache and inserts them
    /// into the persistent trie structure. Call this during checkpoints or
    /// before saving to ensure all entries are persisted.
    ///
    /// # Returns
    ///
    /// The number of entries merged.
    pub fn merge_lockfree_to_persistent(&mut self) -> Result<usize> {
        // Collect entries first to avoid borrow conflict
        let entries: Vec<String> = match &self.lockfree_cache {
            Some(cache) => cache.iter().map(|e| e.key().clone()).collect(),
            None => return Ok(0),
        };

        let mut count = 0;
        for term in entries {
            if self.insert_impl_no_wal(&term) {
                count += 1;
            }
        }

        // Clear the cache after merging
        if let Some(ref cache) = self.lockfree_cache {
            cache.clear();
        }

        Ok(count)
    }

    /// Find the leaf node for a key in the lock-free trie.
    ///
    /// Navigates the lock-free trie overlay and returns the leaf node if the
    /// full path exists and the leaf is final. Unlike `find_in_lockfree_trie`
    /// which returns a `bool`, this returns the node itself so the caller can
    /// read or atomically modify its value.
    fn find_leaf_lockfree(
        &self,
        root: &super::nodes::atomic_ptr::AtomicNodePtr,
        chars: &[u32],
    ) -> Option<Arc<super::nodes::persistent_node::PersistentCharNode>> {
        let current = root.load()?;
        self.find_leaf_recursive(&current, chars, 0)
    }

    /// Recursive helper for `find_leaf_lockfree`.
    fn find_leaf_recursive(
        &self,
        node: &Arc<super::nodes::persistent_node::PersistentCharNode>,
        chars: &[u32],
        depth: usize,
    ) -> Option<Arc<super::nodes::persistent_node::PersistentCharNode>> {
        use super::nodes::persistent_node::PersistentCharNode;

        if depth == chars.len() {
            return if node.is_final() {
                Some(Arc::clone(node))
            } else {
                None
            };
        }

        let child_ptr = node.find_child(chars[depth])?;
        if child_ptr.is_on_disk() {
            return None; // Can't traverse disk refs in lock-free overlay
        }

        let ptr = child_ptr.as_ptr::<PersistentCharNode>()?;
        let child = unsafe {
            Arc::increment_strong_count(ptr);
            Arc::from_raw(ptr)
        };
        self.find_leaf_recursive(&child, chars, depth + 1)
    }

    /// Lock-free read of a value from the lock-free trie overlay.
    ///
    /// Returns the value if the key is found in the lock-free layer with a value
    /// set. Does not check the persistent layer — callers should check both layers
    /// and sum the results for n-gram counting.
    ///
    /// # Arguments
    ///
    /// * `key` - The string key to look up
    ///
    /// # Returns
    ///
    /// `Some(value)` if found in the lock-free layer, `None` otherwise.
    #[inline]
    pub fn get_lockfree(&self, key: &str) -> Option<u64> {
        let lockfree_root = self.lockfree_root.as_ref()?;
        let _epoch = self.epoch_manager.enter_read();
        let chars: Vec<u32> = key.chars().map(|c| c as u32).collect();
        self.find_leaf_lockfree(lockfree_root, &chars)
            .and_then(|leaf| leaf.get_value())
    }

    /// Lock-free increment: create path if needed, then atomically add delta.
    ///
    /// For existing keys: single `fetch_add` on the leaf (wait-free).
    /// For new keys: CAS retry loop to create path, then set initial value.
    ///
    /// This is the primary method for n-gram counting. Workers call this
    /// concurrently without any exclusive locks — only a shared read lock is
    /// needed since this method takes `&self`. Contention only occurs when two
    /// threads simultaneously create the *same new path* (rare in practice
    /// since n-gram keys are distributed across the alphabet).
    ///
    /// # Arguments
    ///
    /// * `key` - The string key (e.g., Latin-1 encoded n-gram)
    /// * `delta` - The count to add
    ///
    /// # Returns
    ///
    /// The new accumulated value after increment.
    ///
    /// # Panics
    ///
    /// Panics if `enable_lockfree()` was not called first.
    pub fn increment_cas(&self, key: &str, delta: u64) -> u64 {
        use std::sync::atomic::Ordering;

        let lockfree_root = self
            .lockfree_root
            .as_ref()
            .expect("Lock-free mode not enabled. Call enable_lockfree() first.");

        let chars: Vec<u32> = key.chars().map(|c| c as u32).collect();
        if chars.is_empty() {
            return 0;
        }

        let _epoch = self.epoch_manager.enter_read();

        // Fast path: find existing leaf and increment atomically (wait-free)
        if let Some(leaf) = self.find_leaf_lockfree(lockfree_root, &chars) {
            return leaf.increment_value(delta);
        }

        // Slow path: create path, then increment
        loop {
            match self.try_insert_lockfree_path(lockfree_root, &chars) {
                LockfreeInsertResult::Inserted(leaf) => {
                    // New path created — claim it as final and set initial value
                    leaf.try_set_final();
                    return leaf.increment_value(delta);
                }
                LockfreeInsertResult::AlreadyExists => {
                    // Path exists but we didn't find the leaf earlier — retry find
                    if let Some(leaf) = self.find_leaf_lockfree(lockfree_root, &chars) {
                        return leaf.increment_value(delta);
                    }
                    // Unusual: exists flag but no leaf found. Retry full path.
                    continue;
                }
                LockfreeInsertResult::Conflict => {
                    // CAS failed — another thread modified the tree, retry
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    /// Merge lock-free values into the persistent trie by summing.
    ///
    /// Unlike `merge_lockfree_to_persistent()` which does boolean insert,
    /// this method adds the accumulated lock-free values to the persistent
    /// trie's existing values via `increment()`.
    ///
    /// # Returns
    ///
    /// The number of entries merged.
    pub fn merge_lockfree_values_to_persistent(&mut self) -> Result<usize> {
        use super::nodes::persistent_node::PersistentCharNode;

        // Load the lock-free root into an independent Arc (does not borrow self)
        let root_node = match self.lockfree_root.as_ref() {
            Some(root) => match root.load() {
                Some(node) => node,
                None => return Ok(0),
            },
            None => return Ok(0),
        };

        // Ensure we have a persistent root node for the zipper to descend into
        if matches!(self.root, CharTrieRoot::Empty) {
            self.root = CharTrieRoot::Node(Box::new(CharTrieNodeInner::new()));
        }
        let persistent_root = match &mut self.root {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => unreachable!(),
        };

        // Zipper merge: walk the lock-free trie and the persistent trie in
        // lockstep, merging values directly at co-positioned nodes.  Avoids:
        //   - intermediate Vec buffer of all entries
        //   - String allocation / UTF-8 encode+decode per entry
        //   - redundant root-to-leaf persistent trie traversal per entry
        let mut key_buf: Vec<u32> = Vec::new();
        let count = self.merge_lockfree_zipper(&root_node, persistent_root, &mut key_buf)?;

        // Clear the lock-free layer
        if let Some(ref cache) = self.lockfree_cache {
            cache.clear();
        }
        if let Some(ref root) = self.lockfree_root {
            root.store(Arc::new(PersistentCharNode::new()));
        }

        Ok(count)
    }

    /// Recursive zipper that walks the lock-free overlay and the persistent trie
    /// in parallel, merging accumulated deltas directly at each co-positioned node.
    ///
    /// Both tree pointers advance together — no redundant traversal from the root.
    /// UTF-8 encoding is deferred to the single WAL-write point per entry.
    ///
    /// # Safety contract (same as `insert_impl_no_wal_with_value`)
    ///
    /// `persistent_node` must be a valid pointer into this trie's node tree.
    /// The caller must ensure no other mutable references to the persistent trie
    /// exist for the duration of this call.
    fn merge_lockfree_zipper(
        &self,
        lockfree_node: &Arc<super::nodes::persistent_node::PersistentCharNode>,
        persistent_node: *mut CharTrieNodeInner<V>,
        key_buf: &mut Vec<u32>,
    ) -> Result<usize> {
        use super::nodes::persistent_node::PersistentCharNode;

        let mut count = 0;

        // If this lock-free node has an accumulated value, merge it into the
        // co-positioned persistent node
        if lockfree_node.is_final() {
            if let Some(delta) = lockfree_node.get_value() {
                // Safety: persistent_node is valid per caller contract
                let node = unsafe { &mut *persistent_node };

                // Read current value from the persistent node
                let current: i64 = if node.is_final() {
                    if let Some(v) = node.value.as_ref() {
                        let bytes = bincode::serialize(v).map_err(|e| {
                            PersistentARTrieError::internal(format!(
                                "Failed to serialize value: {}",
                                e
                            ))
                        })?;
                        if bytes.len() == 8 {
                            i64::from_le_bytes(bytes.try_into().expect("checked len == 8"))
                        } else {
                            bincode::deserialize::<i64>(&bytes).map_err(|e| {
                                PersistentARTrieError::internal(format!(
                                    "Failed to deserialize as i64: {}",
                                    e
                                ))
                            })?
                        }
                    } else {
                        0
                    }
                } else {
                    0
                };

                let new_value = current + delta as i64;

                // Serialize the new value back to V
                let value_bytes = bincode::serialize(&new_value).map_err(|e| {
                    PersistentARTrieError::internal(format!("Failed to serialize new value: {}", e))
                })?;
                let v: V = bincode::deserialize(&value_bytes).map_err(|e| {
                    PersistentARTrieError::internal(format!("Failed to deserialize as V: {}", e))
                })?;

                // WAL record — the only point that needs UTF-8 encoding
                let record = WalRecord::Increment {
                    term: Self::chars_to_utf8_bytes(key_buf),
                    delta: delta as i64,
                    result: new_value,
                };
                self.append_to_wal(record)?;

                // Update the persistent node in place
                if !node.is_final() {
                    node.set_final(true);
                    self.len.fetch_add(1, AtomicOrdering::Relaxed);
                }
                node.value = Some(v);
                self.dirty.store(true, AtomicOrdering::Release);

                count += 1;
            }
        }

        // Recurse into lock-free children, advancing both tree pointers
        for (&child_key, child_ptr) in lockfree_node.iter_children() {
            if child_ptr.is_on_disk() {
                continue; // Skip disk refs in lock-free overlay
            }
            if let Some(ptr) = child_ptr.as_ptr::<PersistentCharNode>() {
                let child = unsafe {
                    Arc::increment_strong_count(ptr);
                    Arc::from_raw(ptr)
                };

                // Advance the persistent trie to the matching child (create if needed)
                let persistent_child = {
                    let node = unsafe { &mut *persistent_node };
                    self.get_or_create_child_lazy_u32_ptr(node, child_key)?
                };

                key_buf.push(child_key);
                count += self.merge_lockfree_zipper(&child, persistent_child, key_buf)?;
                key_buf.pop();
            }
        }

        Ok(count)
    }

    /// Encode u32 character codes to UTF-8 bytes without intermediate String.
    ///
    /// Used by the merge zipper to produce WAL record payloads from the `&[u32]`
    /// key buffer maintained during traversal.
    fn chars_to_utf8_bytes(chars: &[u32]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(chars.len() * 2);
        let mut encode_buf = [0u8; 4];
        for &code in chars {
            if let Some(c) = char::from_u32(code) {
                let encoded = c.encode_utf8(&mut encode_buf);
                buf.extend_from_slice(encoded.as_bytes());
            }
        }
        buf
    }

    /// Get the number of CAS retries (for monitoring contention).
    pub fn cas_retry_count(&self) -> u64 {
        self.cas_retries.load(std::sync::atomic::Ordering::Relaxed)
    }

    // ==================== End Lock-Free CAS Methods ====================
}
