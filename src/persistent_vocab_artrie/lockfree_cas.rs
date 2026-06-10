//! Lock-free CAS-based vocabulary inserts for `PersistentVocabARTrie`.
//!
//! Split out of vocab `dict_impl.rs` (lines ~850-1158, ~309 LOC) as
//! a Phase-6 vocab sub-module, mirroring the byte and char
//! `lockfree_cas` splits. Methods covered:
//!
//! - `enable_lockfree` / `is_lockfree_enabled`
//! - `insert_cas` — CAS-driven concurrent insert with index allocation
//! - `cas_retries` — observability counter
//! - `merge_lockfree_to_persistent` — promote lockfree overlay into
//!   the persistent trie

use std::sync::atomic::Ordering;
use std::sync::Arc;

use dashmap::DashMap;

use crate::persistent_artrie::error::Result;
use crate::persistent_artrie_char::nodes::persistent_node::Child as ChildGeneric;
use crate::persistent_artrie_char::nodes::{
    AtomicNodePtr as AtomicNodePtrGeneric, PersistentCharNode as PersistentCharNodeGeneric,
};

// Vocab overlay node value = `u64` vocabulary index (G1: char overlay node is now
// generic; the vocab instantiates it at `V = u64`).
type Child = ChildGeneric<u64>;
type AtomicNodePtr = AtomicNodePtrGeneric<u64>;
type PersistentCharNode = PersistentCharNodeGeneric<u64>;

impl<S: crate::persistent_artrie::block_storage::BlockStorage>
    super::dict_impl::PersistentVocabARTrie<S>
{
    // =========================================================================
    // Lock-Free CAS Insert (per plan Phase 5)
    // =========================================================================

    /// Enable lock-free mode for CAS-based concurrent inserts.
    ///
    /// This initializes the lock-free infrastructure using `PersistentCharNode`
    /// with `im::Vector` for structural sharing. Once enabled, `insert_cas()`
    /// can be called from multiple threads without locks.
    ///
    /// # Returns
    ///
    /// `true` if lock-free mode was newly enabled, `false` if already enabled.
    pub fn enable_lockfree(&mut self) -> bool {
        if self.lockfree_root.is_some() {
            return false;
        }

        // Initialize lock-free root
        let root = Arc::new(PersistentCharNode::new());
        self.lockfree_root = Some(AtomicNodePtr::new(root));
        self.lockfree_cache = Some(DashMap::new());

        // NB the WAL Overlay-regime stamp is intentionally NOT done here (unlike char,
        // whose `enable_lockfree` IS the flip). In V1 the owned tree is still the
        // default, so the WAL stays in the owned/LSN-order regime until the production
        // FLIP (V4 — `flip_to_overlay` → `wal_stamp_overlay_regime`), at which point the
        // insert is ALSO routed to the overlay (Order-A, emitting CommitRank). Stamping
        // the Overlay regime here would make the still-active owned inserts' UNRANKED
        // WAL records get dropped on reopen (Overlay-regime recovery drops unranked).

        true
    }

    /// Check if lock-free mode is enabled.
    #[inline]
    pub fn is_lockfree_enabled(&self) -> bool {
        self.lockfree_root.is_some()
    }

    /// Insert a term using lock-free CAS operations.
    ///
    /// This method is thread-safe and can be called from multiple threads
    /// concurrently without external synchronization. It uses `PersistentCharNode`
    /// with `im::Vector` for structural sharing and CAS for atomic updates.
    ///
    /// # Panics
    ///
    /// Panics if lock-free mode is not enabled. Call `enable_lockfree()` first.
    ///
    /// # Returns
    ///
    /// The vocabulary index for the term (existing or newly assigned).
    ///
    /// # Example
    ///
    /// ```text
    /// use std::sync::Arc;
    /// use std::thread;
    ///
    /// let mut vocab = PersistentVocabARTrie::create("vocab.vocab")?;
    /// vocab.enable_lockfree();
    ///
    /// let vocab = Arc::new(vocab);
    /// let handles: Vec<_> = (0..8).map(|t| {
    ///     let v = Arc::clone(&vocab);
    ///     thread::spawn(move || {
    ///         for i in 0..1000 {
    ///             v.insert_cas(&format!("thread{}_{}", t, i));
    ///         }
    ///     })
    /// }).collect();
    ///
    /// for h in handles { h.join().unwrap(); }
    /// ```
    pub fn insert_cas(&self, term: &str) -> u64 {
        let lockfree_root = self
            .lockfree_root
            .as_ref()
            .expect("Lock-free mode not enabled. Call enable_lockfree() first.");
        let lockfree_cache = self
            .lockfree_cache
            .as_ref()
            .expect("Lock-free cache not initialized");

        // Fast path: check cache
        if let Some(entry) = lockfree_cache.get(term) {
            return *entry;
        }

        // Convert term to character codes
        let chars: Vec<u32> = term.chars().map(|c| c as u32).collect();

        // Check if exists in lock-free trie
        if let Some(root) = lockfree_root.load() {
            if let Some(idx) = self.find_in_lockfree_trie(&root, &chars) {
                lockfree_cache.insert(term.to_string(), idx);
                return idx;
            }
        }

        // Also check the persistent trie (for terms inserted before lock-free was enabled)
        if let Some(idx) = self.get_index(term) {
            lockfree_cache.insert(term.to_string(), idx);
            return idx;
        }

        // Atomically claim the next index
        let index = self.next_index.fetch_add(1, Ordering::AcqRel);

        // CAS loop to insert into lock-free trie
        loop {
            let root = match lockfree_root.load() {
                Some(r) => r,
                None => {
                    // Root is null - initialize it
                    let new_root = Arc::new(PersistentCharNode::new());
                    if lockfree_root.try_init(new_root).is_ok() {
                        continue; // Root initialized, retry insert
                    }
                    continue; // Someone else initialized, retry
                }
            };

            match self.try_insert_lockfree_path(&root, &chars, index) {
                Ok(new_root) => {
                    // CAS the root to the new version
                    match lockfree_root.compare_exchange(&root, new_root) {
                        Ok(_) => {
                            // Success! Update cache and counts
                            lockfree_cache.insert(term.to_string(), index);
                            self.entry_count.fetch_add(1, Ordering::AcqRel);
                            self.dirty.store(true, Ordering::Release);

                            // Update bloom filter if present
                            // Note: BloomFilter insertion is not thread-safe,
                            // but false negatives are acceptable for bloom filters
                            // (we'll just do an extra lookup)

                            return index;
                        }
                        Err(actual) => {
                            // CAS failed - someone else modified the root
                            self.cas_retries.fetch_add(1, Ordering::Relaxed);

                            // Check if the term was inserted by another thread
                            if let Some(existing_idx) = self.find_in_lockfree_trie(&actual, &chars)
                            {
                                lockfree_cache.insert(term.to_string(), existing_idx);
                                return existing_idx;
                            }

                            // Retry with the new root
                            continue;
                        }
                    }
                }
                Err(existing_idx) => {
                    // Term already exists
                    lockfree_cache.insert(term.to_string(), existing_idx);
                    return existing_idx;
                }
            }
        }
    }

    /// Try to create a new root with the term inserted (lock-free version).
    ///
    /// Returns `Ok(new_root)` if successful, `Err(existing_idx)` if term already exists.
    pub(super) fn try_insert_lockfree_path(
        &self,
        root: &Arc<PersistentCharNode>,
        chars: &[u32],
        index: u64,
    ) -> std::result::Result<Arc<PersistentCharNode>, u64> {
        if chars.is_empty() {
            // Empty term - mark root as final
            if root.is_final() {
                return Err(root.get_value().unwrap_or(0));
            }
            let new_root = root.as_final().with_value(index);
            return Ok(Arc::new(new_root));
        }

        // Recursively create the path
        self.insert_lockfree_recursive(root, chars, 0, index)
    }

    /// Recursively create new nodes along the path (lock-free version).
    fn insert_lockfree_recursive(
        &self,
        node: &Arc<PersistentCharNode>,
        chars: &[u32],
        depth: usize,
        index: u64,
    ) -> std::result::Result<Arc<PersistentCharNode>, u64> {
        if depth == chars.len() {
            // Reached the end - mark as final
            if node.is_final() {
                return Err(node.get_value().unwrap_or(0));
            }
            let new_node = node.as_final().with_value(index);
            return Ok(Arc::new(new_node));
        }

        let c = chars[depth];

        match node.find_child(c) {
            Some(child) => {
                // Child exists - recurse
                if child.is_null() {
                    return Err(0); // Shouldn't happen
                }

                match child.as_in_mem() {
                    Some(child_arc) => {
                        let child_arc = Arc::clone(child_arc);

                        // Recurse into child
                        let new_child =
                            self.insert_lockfree_recursive(&child_arc, chars, depth + 1, index)?;

                        // Create new node owning the updated child by `Arc`
                        // (no raw-pointer smuggling).
                        let new_node = node.with_child(c, Child::InMem(new_child));
                        Ok(Arc::new(new_node))
                    }
                    None => {
                        // On-disk child - not supported in lock-free mode yet
                        Err(0)
                    }
                }
            }
            None => {
                // Child doesn't exist - create new path
                let new_child = self.create_lockfree_path(&chars[depth + 1..], index);
                let new_node = node.with_child(c, Child::InMem(new_child));
                Ok(Arc::new(new_node))
            }
        }
    }

    /// Create a new path from the remaining characters (lock-free version).
    fn create_lockfree_path(&self, chars: &[u32], index: u64) -> Arc<PersistentCharNode> {
        if chars.is_empty() {
            // Create final node with value
            let node = PersistentCharNode::new().as_final().with_value(index);
            return Arc::new(node);
        }

        // Build path bottom-up
        let mut current = Arc::new(PersistentCharNode::new().as_final().with_value(index));

        for &c in chars.iter().rev() {
            // Each parent owns its child by `Arc` (no raw-pointer smuggling).
            let parent = PersistentCharNode::new().with_child(c, Child::InMem(current));
            current = Arc::new(parent);
        }

        current
    }

    /// Find a term in the lock-free trie, returning its index if found.
    pub(super) fn find_in_lockfree_trie(
        &self,
        root: &Arc<PersistentCharNode>,
        chars: &[u32],
    ) -> Option<u64> {
        let mut current = root.clone();

        for &c in chars {
            match current.find_child(c) {
                Some(child) => {
                    if child.is_null() {
                        return None;
                    }
                    // On-disk children are not traversable here → `None`.
                    match child.as_in_mem() {
                        Some(child_arc) => current = Arc::clone(child_arc),
                        None => return None,
                    }
                }
                None => return None,
            }
        }

        current.get_value()
    }

    /// Get CAS retry statistics for monitoring lock contention.
    #[inline]
    pub fn cas_retries(&self) -> u64 {
        self.cas_retries.load(Ordering::Relaxed)
    }

    /// Merge lock-free trie entries into the persistent trie.
    ///
    /// This should be called before checkpointing to ensure all lock-free
    /// inserts are persisted. The lock-free trie remains valid after merge.
    ///
    /// # Returns
    ///
    /// Number of entries merged.
    pub fn merge_lockfree_to_persistent(&mut self) -> Result<usize> {
        // Collect entries first to avoid borrow conflict
        let entries: Vec<(String, u64)> = match &self.lockfree_cache {
            Some(cache) => cache
                .iter()
                .map(|e| (e.key().clone(), *e.value()))
                .collect(),
            None => return Ok(0),
        };

        let mut count = 0;
        for (term, index) in entries {
            // Insert into persistent trie if not already there
            if self.get_index(&term).is_none() {
                // Use insert_with_index to add to persistent trie
                if self.insert_with_index(&term, index)? {
                    count += 1;
                }
            }
        }

        Ok(count)
    }
}
