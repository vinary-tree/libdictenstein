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
//!
//! # G4 — genericized over `V`, increment is now PATH-COPY CAS
//!
//! The overlay node (`super::nodes::PersistentNode<V>`) carries an **immutable**
//! `Option<V>` value (G4 — was an in-place `AtomicU64`). The membership block is
//! generic `<V: DictionaryValue, S>` and its proven two-phase `try_set_final`
//! finalization (plus the prefix single-arbiter fix) is unchanged — only the
//! `PersistentNode`/`AtomicNodePtr` names gain the `<V>` parameter.
//!
//! The **counter** half is `V = i64`-specific (byte tries persist `i64`; the
//! lock-free n-gram counter accumulates a `u64` count bounded by
//! `LOCKFREE_COUNTER_MAX = i64::MAX as u64`, stored in the overlay leaf as the
//! trie's own `i64` value). Its increment is a **path-copy CAS** — mirroring char
//! `lockfree_cas.rs::try_increment_cas` (`build_value_path_recursive`): read the
//! leaf's count from the published snapshot, build a new leaf
//! `old.as_final().with_value(new_count_as_i64)`, path-copy the root→leaf spine,
//! CAS-publish the root. The wait-free in-place `fetch_add` is gone (arbitrary
//! `V` cannot live in an atomic); the root CAS is the single linearization point,
//! so no increment is lost (a loser re-reads the higher count and folds its
//! delta onto the winner's). This is the same single-phase model the vocab
//! overlay already uses and the char overlay proved via the loom race test.

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
///
/// G4: generic over `V` so the `Inserted` node matches the trie's
/// `lockfree_root: AtomicNodePtr<V>`. A membership trie (`V=()`) is unchanged; a
/// counter trie (`V=i64`) carries the valued leaf back to the caller.
enum LockfreeInsertResult<V = ()> {
    /// Term was newly inserted - contains the node to finalize
    Inserted(Arc<super::nodes::PersistentNode<V>>),
    /// Term already exists in the trie
    AlreadyExists,
    /// CAS conflict - another thread modified the tree, retry needed
    Conflict,
}

// Manual `Debug` so `V` need not be `Debug` (the `DictionaryValue` bound omits
// it). `V: Clone` so the node's own manual `Debug` (on `impl<V: Clone>`) applies.
impl<V: Clone> std::fmt::Debug for LockfreeInsertResult<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockfreeInsertResult::Inserted(_) => f.write_str("LockfreeInsertResult::Inserted(..)"),
            LockfreeInsertResult::AlreadyExists => {
                f.write_str("LockfreeInsertResult::AlreadyExists")
            }
            LockfreeInsertResult::Conflict => f.write_str("LockfreeInsertResult::Conflict"),
        }
    }
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
        let root_node = Arc::new(PersistentNode::<V>::new());
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
        root: &super::nodes::AtomicNodePtr<V>,
        term: &[u8],
    ) -> LockfreeInsertResult<V> {
        use super::nodes::PersistentNode;

        let current_root = match root.load() {
            Some(node) => node,
            None => {
                let new_root = Arc::new(PersistentNode::<V>::new());
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
        node: &Arc<super::nodes::PersistentNode<V>>,
        term: &[u8],
        depth: usize,
    ) -> std::result::Result<
        (
            Arc<super::nodes::PersistentNode<V>>,
            Arc<super::nodes::PersistentNode<V>>,
        ),
        (),
    > {
        use super::nodes::persistent_node::Child;

        if depth == term.len() {
            if node.is_final() {
                return Err(()); // Already a complete term
            }
            // Return the EXISTING node (shared Arc) as the leaf to finalize so
            // `insert_cas`'s `try_set_final` is the SINGLE atomic arbiter across
            // racing inserters. Do NOT pre-finalize (the old `node.as_final()`):
            // that made `try_set_final` see an already-final node and wrongly
            // report a *new* prefix term (e.g. "a" after "ab") as a duplicate,
            // returning `false` AND skipping the lock-free cache so
            // `merge_lockfree_to_persistent` (cache-only) silently dropped it.
            // (Mirror of the char-overlay fix.)
            return Ok((Arc::clone(node), Arc::clone(node)));
        }

        let key = term[depth];

        match node.find_child(key) {
            Some(child_ptr) => {
                // In-memory child: path-copy into it. An on-disk child means this
                // path lives in the persistent trie, which the lock-free overlay
                // cannot fault in here — treat it (and the impossible null filler)
                // as a conflict to force a re-check. Zero `unsafe`: `as_in_mem`
                // borrows the owned child `Arc` and `Child::InMem` re-wraps the
                // path-copied replacement.
                if let Some(child_arc) = child_ptr.as_in_mem() {
                    let child_arc = Arc::clone(child_arc);
                    let (new_child, leaf) =
                        self.build_path_recursive(&child_arc, term, depth + 1)?;
                    let new_node = Arc::new(node.with_child(key, Child::InMem(new_child)));
                    Ok((new_node, leaf))
                } else {
                    Err(())
                }
            }
            None => {
                let (new_subtree, leaf) = self.create_lockfree_path(&term[depth + 1..]);
                let new_node = Arc::new(node.with_child(key, Child::InMem(new_subtree)));

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
        Arc<super::nodes::PersistentNode<V>>,
        Arc<super::nodes::PersistentNode<V>>,
    ) {
        use super::nodes::persistent_node::{Child, PersistentNode};

        let leaf = Arc::new(PersistentNode::<V>::new());

        if term.is_empty() {
            return (leaf.clone(), leaf);
        }

        let mut current = leaf.clone();

        for &b in term.iter().rev() {
            // Each parent owns its child by `Arc` (no raw-pointer smuggling).
            let parent = PersistentNode::<V>::new().with_child(b, Child::InMem(current));
            current = Arc::new(parent);
        }

        (current, leaf)
    }

    /// Attempt to insert a path using CAS. Called from `insert_cas` retry loop.
    fn insert_lockfree_recursive(
        &self,
        root: &super::nodes::AtomicNodePtr<V>,
        current: &Arc<super::nodes::PersistentNode<V>>,
        term: &[u8],
        _depth: usize,
    ) -> LockfreeInsertResult<V> {
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
        node: &Arc<super::nodes::PersistentNode<V>>,
        term: &[u8],
        depth: usize,
    ) -> bool {
        if depth >= term.len() {
            return node.is_final();
        }

        let key = term[depth];
        if let Some(child_ptr) = node.find_child(key) {
            // On-disk references can't be traversed in the lock-free overlay;
            // in-memory children are borrowed and recursed into (owned `Arc`).
            if let Some(child_arc) = child_ptr.as_in_mem() {
                return self.find_in_lockfree_trie(&Arc::clone(child_arc), term, depth + 1);
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

    /// Find the leaf node for a key in the lock-free trie.
    ///
    /// Generic helper shared by the membership block and the `<i64>` counter
    /// block (its calls resolve at `V = i64` — same code, different impl).
    fn find_leaf_lockfree(
        &self,
        root: &super::nodes::AtomicNodePtr<V>,
        key: &[u8],
    ) -> Option<Arc<super::nodes::PersistentNode<V>>> {
        let current = root.load()?;
        self.find_leaf_recursive(&current, key, 0)
    }

    /// Recursive helper for `find_leaf_lockfree`.
    fn find_leaf_recursive(
        &self,
        node: &Arc<super::nodes::PersistentNode<V>>,
        key: &[u8],
        depth: usize,
    ) -> Option<Arc<super::nodes::PersistentNode<V>>> {
        if depth == key.len() {
            return if node.is_final() {
                Some(Arc::clone(node))
            } else {
                None
            };
        }

        let child_ptr = node.find_child(key[depth])?;
        // Can't traverse disk refs in the lock-free overlay; `as_in_mem` returns
        // `None` for an on-disk child, short-circuiting via `?` (owned `Arc`).
        let child_arc = child_ptr.as_in_mem()?;
        self.find_leaf_recursive(&Arc::clone(child_arc), key, depth + 1)
    }
}

// ============================================================================
// Counter (valued) overlay methods — `V = i64` ONLY.
// ============================================================================
//
// G4: the lock-free overlay node now carries an **immutable** `Option<V>` value
// (was an in-place `AtomicU64`). The wait-free `fetch_add` increment is therefore
// gone; an increment becomes a **path-copy CAS** (read the leaf's value, build a
// new leaf with `old_leaf.as_final().with_value(new_val)`, path-copy the
// root→leaf spine, CAS-publish the root — exactly the single-phase model the
// vocab overlay (`persistent_vocab_artrie::lockfree_cas`) and the char overlay
// (`persistent_artrie_char::lockfree_cas`) already use).
//
// Byte tries persist `i64`, so the lock-free counter overlay lives in a
// `V = i64` impl block: the overlay leaf stores the running count as the trie's
// own `i64` value, while the increment accumulates a `u64` count bounded by
// `LOCKFREE_COUNTER_MAX = i64::MAX as u64` (the i64 persistence domain) and the
// public API exposes `u64`. The generic membership block above remains `<V>` and
// its proven `try_set_final` two-phase finalization is untouched. Cross-block
// calls to the generic helpers (`find_leaf_lockfree`, `find_leaf_recursive`,
// `try_insert_lockfree_path`) resolve at `V = i64` — same code, different impl.
impl<S: BlockStorage> PersistentARTrie<i64, S> {
    /// Lock-free read of a value from the lock-free trie overlay.
    ///
    /// Returns the accumulated count if the key is present in the lock-free layer
    /// with a value set. Does not check the persistent layer — callers should
    /// check both layers and sum for n-gram counting. The leaf stores the count
    /// as the trie's `i64` value; it is non-negative (bounded at insert by
    /// `LOCKFREE_COUNTER_MAX`), so the widen to `u64` is lossless.
    #[inline]
    pub fn get_lockfree(&self, key: &[u8]) -> Option<u64> {
        let lockfree_root = self.lockfree_root.as_ref()?;
        let _epoch = self.epoch_manager.enter_read();
        self.find_leaf_lockfree(lockfree_root, key)
            .and_then(|leaf| leaf.get_value())
            .map(|v| v as u64)
    }

    /// Checked lock-free increment: create path if needed, then add `delta`.
    ///
    /// **G4 path-copy CAS** (the wait-free in-place `fetch_add` is gone — the
    /// node's value is now an immutable `Option<i64>`). Each attempt:
    ///   1. loads the overlay root (a published, immutable snapshot);
    ///   2. reads the current count `cur` at `key` (0 if the leaf is absent or
    ///      has no value), overflow-checks `cur.checked_add(delta)` against
    ///      `LOCKFREE_COUNTER_MAX`;
    ///   3. builds the new leaf `old_leaf.as_final().with_value(cur + delta)` and
    ///      path-copies the root→leaf spine splicing in that leaf;
    ///   4. CAS-publishes the new root via `lockfree_root.compare_exchange`.
    /// On CAS failure another writer published a newer root, so we bump
    /// `cas_retries` and retry — re-reading the (now higher) count, so **no
    /// increment is lost** (the loser folds its delta onto the winner's value).
    ///
    /// Mirrors char `lockfree_cas.rs::try_increment_cas` verbatim modulo
    /// `&str`→`&[u8]` (no decode needed for byte keys) and the leaf value type
    /// (`i64` instead of `u64`). The root CAS is the single linearization point,
    /// formally checked by the char loom race test.
    pub fn try_increment_cas(&self, key: &[u8], delta: u64) -> Result<u64> {
        use super::nodes::persistent_node::PersistentNode;
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

        // Path-copy CAS retry loop (single-phase: the root CAS is the sole
        // visibility arbiter — the new leaf's value is published atomically with
        // the new root, so a stale reader never sees a torn count).
        loop {
            // (1) Load the current published root (initializing it if null — the
            // same null-init dance the membership path uses).
            let root = match lockfree_root.load() {
                Some(r) => r,
                None => {
                    let new_root = Arc::new(PersistentNode::<i64>::new());
                    let _ = lockfree_root.try_init(new_root);
                    continue;
                }
            };

            // (2) Read the current count at `key` from THIS snapshot. The leaf
            // stores a non-negative `i64`; widen to `u64` for the running sum.
            let cur = self
                .find_leaf_recursive(&root, key, 0)
                .and_then(|leaf| leaf.get_value())
                .map(|v| v as u64)
                .unwrap_or(0);

            // (3) Overflow-check against the i64 persistence domain.
            let new_val = match cur.checked_add(delta) {
                Some(v) if v <= LOCKFREE_COUNTER_MAX => v,
                _ => {
                    return Err(Self::lockfree_increment_overflow_error(
                        key,
                        Some(cur),
                        delta,
                    ))
                }
            };

            // (4) Build a new root with the value-carrying leaf spliced in. The
            // count is bounded by `LOCKFREE_COUNTER_MAX = i64::MAX as u64`, so the
            // narrow to `i64` is lossless.
            let new_root = match self.build_value_path_recursive(&root, key, 0, new_val as i64) {
                Some(r) => r,
                None => {
                    // An on-disk child blocked the path-copy (cannot fault in the
                    // overlay). Treat as a transient conflict and retry from a
                    // fresh root load — mirrors the membership `Conflict` arm.
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };

            // (5) CAS-publish. On success the new value is now visible. On
            // failure another writer won; re-read the higher count and retry so
            // this delta is not lost (it is folded onto the winner's value).
            match lockfree_root.compare_exchange(&root, new_root) {
                Ok(_) => return Ok(new_val),
                Err(_actual) => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    /// Path-copy the `root`→leaf spine for `key`, finalizing the leaf with
    /// `value`. Returns a new root `Arc` (the published-version candidate) or
    /// `None` if an on-disk child blocks the copy (cannot be faulted in here).
    ///
    /// Mirrors the membership `build_path_recursive`, but instead of returning the
    /// shared leaf for a later `try_set_final`, it bakes `as_final().with_value`
    /// into the leaf so finalization+value publish atomically with the root CAS
    /// (single-phase). For an existing path this replaces the leaf's value
    /// (last-writer = the CAS winner); for a new path it creates the spine.
    /// (Verbatim port of char `build_value_path_recursive` with `u32`→`u8` keys
    /// and `u64`→`i64` leaf value.)
    fn build_value_path_recursive(
        &self,
        node: &Arc<super::nodes::PersistentNode<i64>>,
        key: &[u8],
        depth: usize,
        value: i64,
    ) -> Option<Arc<super::nodes::PersistentNode<i64>>> {
        use super::nodes::persistent_node::{Child, PersistentNode};

        if depth == key.len() {
            // Reached the leaf: bake finality + the new value into a fresh copy.
            return Some(Arc::new(node.as_final().with_value(value)));
        }

        let k = key[depth];
        match node.find_child(k) {
            Some(child) => {
                // In-memory child: path-copy into it. On-disk → cannot fault in.
                let child_arc = child.as_in_mem()?;
                let child_arc = Arc::clone(child_arc);
                let new_child =
                    self.build_value_path_recursive(&child_arc, key, depth + 1, value)?;
                Some(Arc::new(node.with_child(k, Child::InMem(new_child))))
            }
            None => {
                // Child absent: build the remaining spine bottom-up, valued leaf
                // at the bottom.
                let leaf = Arc::new(PersistentNode::<i64>::new().as_final().with_value(value));
                let mut current = leaf;
                for &b in key[depth + 1..].iter().rev() {
                    let parent = PersistentNode::<i64>::new().with_child(b, Child::InMem(current));
                    current = Arc::new(parent);
                }
                Some(Arc::new(node.with_child(k, Child::InMem(current))))
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

    /// Merge lock-free values into the persistent trie by summing.
    ///
    /// Unlike `merge_lockfree_to_persistent()` which does boolean insert,
    /// this method walks the lock-free trie overlay, collects all
    /// `(key, value)` entries, and adds each value to the persistent trie.
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
            root.store(Arc::new(PersistentNode::<i64>::new()));
        }

        Ok(merged_count)
    }

    fn prepare_lockfree_value_merge(
        &self,
        entries: &[(Vec<u8>, u64)],
    ) -> Result<(Vec<(Vec<u8>, i64)>, Vec<(Vec<u8>, i64)>)> {
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

            wal_entries.push((key.clone(), delta_i64));
            prepared_values.push((key.clone(), new_value));
        }

        Ok((wal_entries, prepared_values))
    }

    fn current_i64_for_lockfree_merge(&self, term: &[u8]) -> Result<i64> {
        // The persistent value is the trie's own `i64`; read it directly (the
        // running sum is bounded by `LOCKFREE_COUNTER_MAX = i64::MAX`).
        Ok(self.get_value_impl(term).unwrap_or(0))
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
    /// The leaf stores a non-negative `i64` count; widen to `u64` for the merge.
    fn collect_lockfree_entries_recursive(
        node: &Arc<super::nodes::PersistentNode<i64>>,
        key_buf: &mut Vec<u8>,
        entries: &mut Vec<(Vec<u8>, u64)>,
    ) {
        if node.is_final() {
            if let Some(value) = node.get_value() {
                entries.push((key_buf.clone(), value as u64));
            }
        }

        for (&child_key, child_ptr) in node.iter_children() {
            // Skip on-disk refs in the lock-free overlay; recurse into in-memory
            // children (borrowed owned `Arc`, no `unsafe`).
            if let Some(child_arc) = child_ptr.as_in_mem() {
                let child_arc = Arc::clone(child_arc);
                key_buf.push(child_key);
                Self::collect_lockfree_entries_recursive(&child_arc, key_buf, entries);
                key_buf.pop();
            }
        }
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

#[cfg(test)]
mod reclaim_tests {
    //! Phase-A leak-detection tests for the byte lock-free overlay (the
    //! `Child`-enum fix). Mirror of the char overlay's `reclaim_tests`: prove that
    //! superseded path-copied node versions are reclaimed via ordinary `Arc`
    //! refcounting (owned `Child::InMem` children), not leaked as the old
    //! `Arc::into_raw`-through-`SwizzledPtr` smuggling did. The witness is
    //! `Arc::strong_count` on a retained leaf: after the overlay is dropped, only
    //! the test's handle may reference it (count == 1).

    use crate::persistent_artrie::nodes::persistent_node::PersistentNode;
    use crate::persistent_artrie::PersistentARTrie;
    use std::sync::Arc;

    fn lockfree_trie(prefix: &str) -> (tempfile::TempDir, PersistentARTrie<()>) {
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp");
        let path = dir.path().join("overlay.part");
        let mut trie = PersistentARTrie::<()>::create(&path).expect("create trie");
        trie.enable_lockfree();
        (dir, trie)
    }

    /// Walk the live overlay root down a byte path, returning an owned `Arc`
    /// clone of the node reached (every edge must be an in-memory child).
    fn walk_to(trie: &PersistentARTrie<()>, path: &[u8]) -> Arc<PersistentNode> {
        let mut node = trie
            .lockfree_root
            .as_ref()
            .expect("lock-free enabled")
            .load()
            .expect("non-null overlay root");
        for &b in path {
            let next = node
                .find_child(b)
                .unwrap_or_else(|| panic!("missing child {b} while walking {path:?}"))
                .as_in_mem()
                .unwrap_or_else(|| panic!("child {b} is on-disk while walking {path:?}"))
                .clone();
            node = next;
        }
        node
    }

    #[test]
    fn superseded_overlay_nodes_are_reclaimed_not_leaked() {
        let (_dir, trie) = lockfree_trie("byte-overlay-reclaim");

        for term in [b"ab", b"ac", b"ad", b"ae"] {
            trie.insert_cas(term);
        }

        let held_leaf = walk_to(&trie, b"ab");
        assert!(
            Arc::strong_count(&held_leaf) >= 2,
            "the live overlay and our handle must both reference the leaf; got {}",
            Arc::strong_count(&held_leaf)
        );

        drop(trie);

        assert_eq!(
            Arc::strong_count(&held_leaf),
            1,
            "after dropping the trie only our handle may reference the leaf; \
             strong_count {} > 1 means a superseded node version leaked a child \
             reference (the bug the Child leak-fix closes)",
            Arc::strong_count(&held_leaf)
        );
    }
}
