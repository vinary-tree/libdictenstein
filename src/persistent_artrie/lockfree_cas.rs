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
use crate::persistent_artrie_core::key_encoding::ByteKey;
use crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite;
use crate::value::DictionaryValue;

const LOCKFREE_COUNTER_MAX: u64 = i64::MAX as u64;

/// Outcome of a single durable single-phase membership insert attempt (M2b — the
/// byte twin of char's durable `LockfreeInsertResult`). The leaf is published FINAL
/// inside the root CAS, so `Inserted` means OUR root CAS won (this op newly
/// published the term); it carries the published-root version (the Order-A commit
/// generation, kept for parity — the durable wrapper ranks the claimed `commit_seq`).
/// No `V` parameter: the durable membership path never hands a leaf back for a
/// separate `try_set_final` (the root CAS fully arbitrates), so there is no node to
/// carry.
enum LockfreeDurableInsertResult {
    /// The term was newly published FINAL via the winning root CAS. Carries the
    /// published-root version.
    Inserted(u64),
    /// The term is already present on this snapshot (the leaf is already final). No
    /// spine was published.
    AlreadyExists,
    /// CAS failed due to a concurrent modification — re-find and retry.
    Conflict,
}

/// Outcome of a single durable membership-clear attempt (M2b — the byte twin of
/// char's `LockfreeRemoveResult`). The new root (with the freshly-cleared non-final
/// leaf) is installed inside `try_remove_lockfree_path`'s own CAS, so these variants
/// carry no node.
enum LockfreeRemoveResult {
    /// The term was present and cleared: a new root with the freshly-cleared
    /// (non-final) leaf was published via the root CAS.
    Removed,
    /// The term is absent on this snapshot (reached full depth non-final, or a
    /// missing/null spine edge). No spine was published.
    AlreadyAbsent,
    /// CAS failed due to a concurrent modification — re-find and retry.
    Conflict,
}

/// Error outcomes of the durable single-phase build path-copy (M2b). `AlreadyExists`
/// is reused by the remove path as "already absent" (the no-op spine outcome — no
/// publication). `Conflict` carries an OnDisk-child-blocked-the-copy retry (byte
/// overlay has no fault-in pre-M4; an opt-in M2b trie never evicts).
enum DurableBuildError {
    /// Insert: the term already exists. Remove: the term is already absent. Either
    /// way, no no-op spine is published.
    AlreadyExists,
    /// An OnDisk (or null filler) child blocked the in-memory path-copy — transient,
    /// the caller retries from a fresh root load.
    Conflict,
}

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

        // S4 / M2d (byte twin of char's `enable_lockfree` regime stamp): stamp the
        // WAL header to the Overlay regime on an EMPTY WAL so crash recovery DROPS
        // the idempotent NO-RANK two-append-window orphans the durable producers may
        // leave (else, under Owned, an unranked orphan is kept-@-lsn and could
        // RESURRECT a removed term — red-team H3). This is the WRITE-side companion
        // that makes the regime reach disk so the M2d regime-aware recovery
        // (`replay_records_lww`) actually takes its Overlay branch; without it the
        // threading is inert and H3 stays open. SAFE here ONLY on an EMPTY WAL
        // (`current_lsn() == 1` ⇒ no records appended) — an in-place restamp of a
        // non-empty file is torn-write-unsafe and would drop pre-existing Owned
        // records (the non-empty case needs a rotation, deferred to the M4 flip).
        // REVERSIBLE/opt-in: every `enable_lockfree` caller is opt-in/test today (no
        // production/default ctor reaches it); a PRODUCTION caller would make this
        // the irreversible M4 flip.
        if let Some(ref writer) = self.wal_writer {
            // EMPTY-WAL guard: use the WRITER's authoritative next-LSN (incremented
            // by EVERY append — owned insert/remove/upsert AND the durable producers),
            // NOT the trie's `self.next_lsn` (which owned-tree mutations do NOT
            // update; a stale `==1` there would wrongly stamp a trie that already
            // holds owned records, silently DROPPING them on reopen under Overlay).
            if writer.current_lsn() == 1 {
                if let Err(e) = writer.set_overlay_regime() {
                    log::warn!("enable_lockfree: could not stamp Overlay regime: {:?}", e);
                }
            }
        }
    }

    /// **M4b (the V-3 twin) — the compile-safe reestablish dispatch.** The byte twin
    /// of char's `reestablish_overlay_dispatch` (persistent_artrie_char/lockfree_cas.rs).
    /// Routes a recovered Overlay-regime reopen's owned→overlay bootstrap to the
    /// CORRECT reestablish fold by the runtime value type:
    /// - `V == i64` ⇒ [`LockFreeOverlay::reestablish_overlay_counter`] (the trait
    ///   DEFAULT — rebuilds the immutable overlay from the recovered owned `(term,
    ///   value)` pairs; the byte counter monomorph).
    /// - `V == ()` ⇒ [`LockFreeOverlay::reestablish_overlay_membership`] (the trait
    ///   DEFAULT — re-publishes each recovered owned term, no values).
    /// - ineligible `V` ⇒ a strict no-op (`overlay_eligible_v()` is false, so no
    ///   overlay was ever installed by the create-flip / open-flip — nothing to
    ///   reestablish).
    ///
    /// Both folds are `LockFreeOverlay<ByteKey, V, S>` defaults visible for ANY `V`
    /// (byte implements the trait on `impl<V, S>`), so — unlike char's u64-inherent
    /// `reestablish_overlay_after_recovery`, which lives only in `impl<u64, S>` and
    /// needs a SAFE `Any` downcast — byte can call the defaults DIRECTLY for the
    /// monomorph selected by `TypeId`. The counter fold's value seam
    /// (`value_as_counter`/`overlay_publish_counter`) is itself a SAFE `Any` no-op for
    /// non-i64 `V`, so the `TypeId` gate is the (redundant) honest selector. `S:
    /// 'static` keeps parity with char's signature (and is satisfied by both byte
    /// storage backends).
    ///
    /// # D1 (CRITICAL — total-loss guard)
    ///
    /// Runs with `route_overlay()` ALREADY TRUE (every call site flips/sets the
    /// overlay mode BEFORE dispatching). The folds read the recovered OWNED tree via
    /// the UN-routed `owned_*` seams (`overlay_write_mode.rs`: `owned_first_units` /
    /// `owned_units_under` / `owned_units_with_values_under`, all over the UNCAPPED
    /// `unrouted_*` walks of `self.root`), publish to the overlay, and clear the owned
    /// tree LAST (RES-7). A routed read here would see the EMPTY overlay ⇒ publish
    /// nothing ⇒ clear owned = irreversible total loss; the un-routed seams are the
    /// guard byte inherits from the shared trait + the M2a seam impls.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn reestablish_overlay_dispatch(&mut self) -> Result<()>
    where
        S: 'static,
    {
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
        use std::any::TypeId;
        if TypeId::of::<V>() == TypeId::of::<i64>() {
            return <Self as LockFreeOverlay<ByteKey, V, S>>::reestablish_overlay_counter(self);
        }
        if TypeId::of::<V>() == TypeId::of::<()>() {
            return <Self as LockFreeOverlay<ByteKey, V, S>>::reestablish_overlay_membership(self);
        }
        // G5/F1: ARBITRARY eligible `V` → the third (value) reestablish fold. Gated
        // on `overlay_eligible_v()` so it is INERT until the F2 eligibility flip.
        if Self::overlay_eligible_v() {
            return <Self as LockFreeOverlay<ByteKey, V, S>>::reestablish_overlay_value(self);
        }
        Ok(())
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
            // Empty-string support (H4): "" is the root; publish membership via the
            // fresh-root-CAS root publisher (NOT in-place `try_set_final` — a concurrent
            // non-empty insert's `with_child` root-copy snapshots flags and would
            // discard an in-place finalize). Non-durable (no WAL). Returns whether THIS
            // call newly finalized the root.
            use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
            let _epoch = self.epoch_manager.enter_read();
            let inserted = self.overlay_publish_root_membership().unwrap_or(false);
            if inserted {
                lockfree_cache.insert(Vec::new(), true);
            }
            return inserted;
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
    ///
    /// **M3 reject (BROKEN-BY-DESIGN, audit §B #6).** Under `route_overlay()` this
    /// drains the overlay CACHE into the owned tree and CLEARS the cache — but the
    /// overlay IS the durable production state, so draining it to the owned tree
    /// (which the overlay read/checkpoint path does not observe) DESTROYS durable
    /// state. Reject; overlay→owned reconciliation is not a flip-mode operation.
    pub fn merge_lockfree_to_persistent(&mut self) -> Result<usize> {
        if self.route_overlay() {
            return Err(PersistentARTrieError::InvalidOperation(
                "merge_lockfree_to_persistent is not valid under the lock-free overlay write \
                 mode (the overlay is the durable production state; draining it into the owned \
                 tree would destroy durable state); use OverlayWriteMode::OwnedTree"
                    .to_string(),
            ));
        }
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
    pub(crate) fn find_leaf_lockfree(
        &self,
        root: &super::nodes::AtomicNodePtr<V>,
        key: &[u8],
    ) -> Option<Arc<super::nodes::PersistentNode<V>>> {
        let current = root.load()?;
        self.find_leaf_recursive(&current, key, 0)
    }

    /// Recursive helper for `find_leaf_lockfree`. `pub(crate)` so the value seams
    /// ([`DurableOverlayWrite::value_publish_inner`] in `overlay_write_mode`) can do
    /// the in-loop InsertOnce/CAS pre-check on the freshly-loaded root.
    pub(crate) fn find_leaf_recursive(
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

    // ====================================================================
    // M2b — Order-A DURABLE membership write path (the byte twin of char's
    // insert_cas_durable / remove_cas_durable). SEPARATE from the non-durable
    // `insert_cas` above (which stays byte-identical to the M2a baseline — the
    // two-phase `try_set_final` arbiter is untouched). The durable path is
    // SINGLE-PHASE: the leaf is published FINAL inside the root CAS, so the root
    // CAS is the SOLE linearization point and the claimed `commit_seq` generation
    // == visibility order. Opt-in (`enable_lockfree` + a synchronous policy); NOT
    // routed from production `insert`/`remove` until M4.
    // ====================================================================

    /// **Order-A durable** lock-free insert (membership). Unlike [`Self::insert_cas`]
    /// (no WAL), this establishes `visible ⊆ durable-prefix`: the `Insert` WAL record
    /// is appended AND synced DURABLE BEFORE the visibility-publishing root CAS, and
    /// the committed watermark advances only once the CAS lands. A crash loses no
    /// acknowledged write — in-WAL replays, not-in-WAL was never acknowledged. The
    /// byte twin of char's `insert_cas_durable`; the gate + commit-rank/watermark
    /// tail route through the SHARED GENERIC [`DurableOverlayWrite`] defaults.
    ///
    /// Requires `enable_lockfree()` and a synchronous durability policy
    /// (`Immediate`/`GroupCommit`). Returns `Ok(true)` iff this call newly inserted
    /// the term.
    ///
    /// # Safety boundary (pre-flip)
    ///
    /// WAL-only-safe: an acknowledged write survives a crash/reopen with NO
    /// checkpoint (durability rests on WAL replay). It is NOT yet safe to mix with
    /// the owned-tree [`checkpoint()`](Self::checkpoint) (which captures the OWNED
    /// tree, not the overlay) — that is the M4 flip. Use WAL-only until then.
    pub fn insert_cas_durable(&self, term: &[u8]) -> Result<bool> {
        use std::sync::atomic::Ordering;

        // **M1:** the Order-A durability gate is the SHARED GENERIC default
        // [`DurableOverlayWrite::durable_policy_gate`] (byte-exact message via the
        // `(method, noun)` reconstruction). The present-hoist + CAS-publish loop
        // below stay INHERENT (byte-node-building seams); only the gate + the
        // commit-rank/watermark tail route through the shared skeleton.
        <Self as DurableOverlayWrite<ByteKey, V, S>>::durable_policy_gate(
            self,
            "insert_cas_durable",
            "write",
        )?;

        let lockfree_root = self.lockfree_root.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
            )
        })?;
        let lockfree_cache = self.lockfree_cache.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
            )
        })?;

        // Fast path: already durably present (cached by a prior acknowledged op).
        if lockfree_cache.contains_key(term) {
            return Ok(false);
        }
        if term.is_empty() {
            // Empty-string support (H4): "" is the root. Order-A durable membership via
            // the fresh-root-CAS RANKED publisher (NOT `try_insert_lockfree_path_durable`,
            // which finalizes in-place — a concurrent non-empty insert's `with_child`
            // root-copy snapshots flags and would discard an in-place finalize).
            use crate::persistent_artrie_core::overlay::flip::{LockFreeOverlay, RootPublishOutcome};
            let _epoch = self.epoch_manager.enter_read();
            // Present-hoist (pre-WAL, no LSN burn): root already final ⇒ no-op insert.
            if self.overlay_root_node().map_or(false, |r| r.is_final()) {
                lockfree_cache.insert(Vec::new(), true);
                return Ok(false);
            }
            // ORDER A — step 1: append + sync the Insert{""} record DURABLE.
            let lsn = self.append_to_wal_returning_lsn(WalRecord::Insert {
                term: Vec::new(),
                value: None,
            })?;
            // Step 2: fresh-root-CAS publish (`as_final`), RANKED (generation bound to
            // the winning CAS iteration, NOT claimed once-before — split-LP safe).
            match self
                .publish_root_cas_ranked(|r| Arc::new(r.as_final()), |r| r.is_final())?
            {
                RootPublishOutcome::Published(generation) => {
                    lockfree_cache.insert(Vec::new(), true);
                    // Step 3: bind the commit rank durable + advance the watermark.
                    self.commit_rank_and_mark(lsn, b"", generation)?;
                    return Ok(true);
                }
                RootPublishOutcome::AlreadyInState => {
                    // A concurrent insert finalized the root first: idempotent NO-RANK
                    // (ranking a no-op resurrects) + `mark_committed` for liveness (the
                    // Overlay-regime replay drops the unranked record).
                    lockfree_cache.insert(Vec::new(), true);
                    self.mark_committed_burned(lsn);
                    return Ok(false);
                }
            }
        }

        // Present-hoist (NON-FAULTING — byte has no overlay fault-in; `find_leaf_lockfree`
        // walks the in-memory overlay). If the term is already present IN MEMORY this
        // is a no-op insert: return WITHOUT appending, so it contributes NO record to
        // replay (the idempotent arm NO-RANKs, so a record left here would be an
        // unranked orphan dropped under the Overlay regime).
        let _epoch = self.epoch_manager.enter_read();
        if self.find_leaf_lockfree(lockfree_root, term).is_some() {
            lockfree_cache.insert(term.to_vec(), true);
            return Ok(false);
        }

        // ORDER A — step 1: append + sync the WAL record DURABLE, before any
        // visibility. The returned LSN is durable-per-policy here. One append covers
        // every CAS retry — we never re-append (that would burn LSNs and punch holes
        // in the watermark).
        let lsn = self.append_to_wal_returning_lsn(WalRecord::Insert {
            term: term.to_vec(),
            value: None,
        })?;

        // Step 2: the visibility CAS loop. The single root CAS (publishing a FINAL
        // leaf — single-phase, finalize=true) is the SOLE visibility arbiter.
        loop {
            // commit_seq CLAIM (loop-top, re-claimed per iteration) — monotone in the
            // global root-CAS order, durable across restart. The insert/increment
            // paths claim from the SAME `self.commit_seq`.
            let commit_seq = self.commit_seq.fetch_add(1, Ordering::AcqRel) + 1;
            match self.try_insert_lockfree_path_durable(lockfree_root, term) {
                LockfreeDurableInsertResult::Inserted(generation_root) => {
                    let _ = generation_root; // the published root (kept for parity)
                    let generation = commit_seq;
                    lockfree_cache.insert(term.to_vec(), true);
                    // Step 2.5 + 3: bind the commit rank durable, then advance the
                    // watermark over BOTH LSNs — the SHARED GENERIC committed-arm tail.
                    self.commit_rank_and_mark(lsn, term, generation)?;
                    return Ok(true);
                }
                LockfreeDurableInsertResult::AlreadyExists => {
                    // Idempotent arm: NO-RANK (the present-hoist already returned for
                    // present-in-memory terms; reaching here means a concurrent insert
                    // won the race). Our already-appended `Insert@lsn` acked NO new
                    // membership; we do NOT rank it (ranking a no-op resurrects), but we
                    // STILL `mark_committed(lsn)` for LIVENESS (cover the burned LSN or
                    // the contiguous watermark stalls; the Overlay-regime replay drops
                    // the unranked record so it cannot resurrect).
                    lockfree_cache.insert(term.to_vec(), true);
                    self.mark_committed_burned(lsn);
                    return Ok(false);
                }
                LockfreeDurableInsertResult::Conflict => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    /// **Order-A durable** lock-free REMOVE (the byte twin of char's
    /// `remove_cas_durable` / "R-B"). Clears a term's membership in the overlay
    /// durably: the `Remove` WAL record is appended AND synced DURABLE BEFORE the
    /// visibility-publishing root CAS, and the committed watermark advances only once
    /// the CAS lands. A crash loses no acknowledged remove. The cleared leaf is a
    /// FRESH [`OverlayNode::as_non_final`] copy spliced into a NEW spine and published
    /// ONLY via the root CAS (never an in-place clear of a shared node — the root-CAS
    /// total order linearizes inserts and removes together, last-writer-wins).
    ///
    /// # Cache invalidation (DATA-CORRECTNESS)
    ///
    /// `contains_lockfree` trusts the positive `lockfree_cache` FIRST, so a remove
    /// that cleared the trie but left a stale cache entry would read present forever.
    /// This method `lockfree_cache.remove(term)` on EVERY state-changing arm BEFORE
    /// `mark_committed`.
    ///
    /// Returns `Ok(true)` iff this call cleared a previously-present term.
    pub fn remove_cas_durable(&self, term: &[u8]) -> Result<bool> {
        use std::sync::atomic::Ordering;

        <Self as DurableOverlayWrite<ByteKey, V, S>>::durable_policy_gate(
            self,
            "remove_cas_durable",
            "remove",
        )?;

        let lockfree_root = self.lockfree_root.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
            )
        })?;
        let lockfree_cache = self.lockfree_cache.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
            )
        })?;

        if term.is_empty() {
            // Empty-string support (H4): "" is the root. Order-A durable remove via the
            // fresh-root-CAS RANKED un-publisher (`as_non_final` on a FRESH root, NOT an
            // in-place clear of the shared root — last-writer-wins with concurrent
            // inserts via the single root CAS, like every non-empty remove).
            use crate::persistent_artrie_core::overlay::flip::{LockFreeOverlay, RootPublishOutcome};
            let _epoch = self.epoch_manager.enter_read();
            // Absent fast-path (pre-WAL, no LSN burn): root not final ⇒ nothing to remove.
            if !self.overlay_root_node().map_or(false, |r| r.is_final()) {
                lockfree_cache.remove(term);
                return Ok(false);
            }
            // ORDER A — step 1: append + sync the Remove{""} record DURABLE.
            let lsn = self.append_to_wal_returning_lsn(WalRecord::Remove { term: Vec::new() })?;
            // Step 2: fresh-root-CAS un-publish (`as_non_final`), RANKED.
            match self
                .publish_root_cas_ranked(|r| Arc::new(r.as_non_final()), |r| !r.is_final())?
            {
                RootPublishOutcome::Published(generation) => {
                    // CACHE INVALIDATION FIRST (before mark): "" is no longer present.
                    lockfree_cache.remove(term);
                    self.commit_rank_and_mark(lsn, b"", generation)?;
                    return Ok(true);
                }
                RootPublishOutcome::AlreadyInState => {
                    // A concurrent remove cleared the root first: idempotent NO-RANK +
                    // mark_committed for liveness.
                    lockfree_cache.remove(term);
                    self.mark_committed_burned(lsn);
                    return Ok(false);
                }
            }
        }

        // ── ABSENT FAST-PATH + WAL AVOIDANCE ── A no-op remove must NOT burn an LSN
        // / punch a watermark hole. Consult the TRIE (not just the positive cache: a
        // cache MISS is not trie-ABSENT — the cache can be empty after a recovery
        // rebuild while the term is live in the overlay).
        let _epoch = self.epoch_manager.enter_read();
        if self.find_leaf_lockfree(lockfree_root, term).is_none() {
            // Genuinely absent → no WAL record. Invalidate the positive cache
            // defensively (a stale entry without a matching final trie node would
            // otherwise read present forever).
            lockfree_cache.remove(term);
            return Ok(false);
        }

        // ORDER A — step 1: append + sync the Remove record DURABLE, before any
        // visibility. One append covers every CAS retry.
        let lsn = self.append_to_wal_returning_lsn(WalRecord::Remove {
            term: term.to_vec(),
        })?;

        // Step 2: the visibility CAS loop. The single root CAS inside
        // `try_remove_lockfree_path` is the SOLE visibility arbiter.
        loop {
            let commit_seq = self.commit_seq.fetch_add(1, Ordering::AcqRel) + 1;
            match self.try_remove_lockfree_path(lockfree_root, term) {
                LockfreeRemoveResult::Removed => {
                    let generation = commit_seq;
                    // CACHE INVALIDATION (FIRST, before mark_committed): the term is no
                    // longer in the trie, so it must not read present via the cache.
                    lockfree_cache.remove(term);
                    self.commit_rank_and_mark(lsn, term, generation)?;
                    return Ok(true);
                }
                LockfreeRemoveResult::AlreadyAbsent => {
                    // Idempotent arm: NO-RANK (raced — a concurrent remove cleared the
                    // term between our present-check and the CAS). Still mark for
                    // LIVENESS + invalidate the cache.
                    lockfree_cache.remove(term);
                    self.mark_committed_burned(lsn);
                    return Ok(false);
                }
                LockfreeRemoveResult::Conflict => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    /// Durable single-phase insert path-copy + root CAS (membership). Publishes a
    /// FRESH FINAL leaf inside the root CAS (the SOLE linearization point), so —
    /// unlike the non-durable [`Self::try_insert_lockfree_path`] (two-phase
    /// `try_set_final`) — reaching `Inserted` means OUR root CAS won and this op
    /// newly published the term (a racer loses the CAS, retries, sees
    /// `AlreadyExists`). Returns the published-root version on success (kept for
    /// parity with char; the durable wrapper ranks the claimed `commit_seq`).
    fn try_insert_lockfree_path_durable(
        &self,
        root: &super::nodes::AtomicNodePtr<V>,
        term: &[u8],
    ) -> LockfreeDurableInsertResult {
        use super::nodes::PersistentNode;

        let current_root = match root.load() {
            Some(node) => node,
            None => {
                let new_root = Arc::new(PersistentNode::<V>::new());
                match root.try_init(new_root) {
                    Ok(()) => return self.try_insert_lockfree_path_durable(root, term),
                    Err(actual) => actual,
                }
            }
        };

        match self.build_final_path_recursive(&current_root, term, 0) {
            Ok(new_root) => {
                let root_generation = new_root.version();
                match root.compare_exchange(&current_root, new_root) {
                    Ok(_) => LockfreeDurableInsertResult::Inserted(root_generation),
                    Err(_actual) => LockfreeDurableInsertResult::Conflict,
                }
            }
            Err(DurableBuildError::AlreadyExists) => LockfreeDurableInsertResult::AlreadyExists,
            // An OnDisk child blocks the overlay path-copy (byte has no overlay
            // fault-in pre-M4; opt-in M2b never evicts). Treat as a transient
            // conflict so the caller retries from a fresh root load.
            Err(DurableBuildError::Conflict) => LockfreeDurableInsertResult::Conflict,
        }
    }

    /// Recursively build a NEW tree with `term`'s leaf published FINAL (single-phase,
    /// the durable path). On the way down it path-copies the existing spine; at the
    /// terminal depth it returns `Err(AlreadyExists)` if the leaf is already final
    /// (no no-op spine), else bakes `as_final()` into a FRESH leaf copy. The byte twin
    /// of char's `build_path_recursive(finalize=true)`.
    fn build_final_path_recursive(
        &self,
        node: &Arc<super::nodes::PersistentNode<V>>,
        term: &[u8],
        depth: usize,
    ) -> std::result::Result<Arc<super::nodes::PersistentNode<V>>, DurableBuildError> {
        use super::nodes::persistent_node::Child;

        if depth == term.len() {
            if node.is_final() {
                return Err(DurableBuildError::AlreadyExists);
            }
            // FRESH FINAL leaf, published only via the root CAS (single-phase).
            return Ok(Arc::new(node.as_final()));
        }

        let key = term[depth];
        match node.find_child(key) {
            Some(child) => {
                if let Some(child_arc) = child.as_in_mem() {
                    let child_arc = Arc::clone(child_arc);
                    let new_child = self.build_final_path_recursive(&child_arc, term, depth + 1)?;
                    Ok(Arc::new(node.with_child(key, Child::InMem(new_child))))
                } else {
                    // On-disk (or null filler) child: cannot fault in here (byte
                    // overlay, pre-M4). Transient conflict → retry.
                    Err(DurableBuildError::Conflict)
                }
            }
            None => {
                // Child absent: build the remaining spine bottom-up, FINAL leaf at the
                // bottom (single-phase).
                let (new_subtree, _leaf) = self.create_lockfree_path_final(&term[depth + 1..]);
                Ok(Arc::new(node.with_child(key, Child::InMem(new_subtree))))
            }
        }
    }

    /// Build a new path for the remaining bytes with the leaf published FINAL
    /// (single-phase durable path). The byte twin of char's
    /// `create_lockfree_path(finalize=true)`.
    fn create_lockfree_path_final(
        &self,
        term: &[u8],
    ) -> (
        Arc<super::nodes::PersistentNode<V>>,
        Arc<super::nodes::PersistentNode<V>>,
    ) {
        use super::nodes::persistent_node::{Child, PersistentNode};

        let leaf = Arc::new(PersistentNode::<V>::new().as_final());
        if term.is_empty() {
            return (leaf.clone(), leaf);
        }
        let mut current = leaf.clone();
        for &b in term.iter().rev() {
            let parent = PersistentNode::<V>::new().with_child(b, Child::InMem(current));
            current = Arc::new(parent);
        }
        (current, leaf)
    }

    /// Attempt to clear a term's membership in the overlay via a single path-copy +
    /// root CAS (the byte twin of char's `try_remove_lockfree_path`). The cleared leaf
    /// is a FRESH `as_non_final` copy spliced into a NEW spine published ONLY via the
    /// root CAS (the SOLE visibility arbiter for the 1→0 transition).
    fn try_remove_lockfree_path(
        &self,
        root: &super::nodes::AtomicNodePtr<V>,
        term: &[u8],
    ) -> LockfreeRemoveResult {
        let current_root = match root.load() {
            Some(node) => node,
            None => return LockfreeRemoveResult::AlreadyAbsent, // empty overlay
        };

        match self.build_remove_path_recursive(&current_root, term, 0) {
            Ok(new_root) => match root.compare_exchange(&current_root, new_root) {
                Ok(_) => LockfreeRemoveResult::Removed,
                Err(_actual) => LockfreeRemoveResult::Conflict,
            },
            Err(DurableBuildError::AlreadyExists) => LockfreeRemoveResult::AlreadyAbsent,
            Err(DurableBuildError::Conflict) => LockfreeRemoveResult::Conflict,
        }
    }

    /// Recursively build a NEW tree with `term`'s leaf cleared (non-final) — the dual
    /// of [`Self::build_final_path_recursive`]. At the terminal depth it clears
    /// finality on a FRESH `as_non_final` copy (NOT a shared node — the root CAS is
    /// the sole arbiter for 1→0); on the way up it path-copies each ancestor.
    /// `Err(AlreadyExists)` (reused as "already absent") if the leaf is already
    /// non-final or a spine edge is missing — no no-op spine is published.
    fn build_remove_path_recursive(
        &self,
        node: &Arc<super::nodes::PersistentNode<V>>,
        term: &[u8],
        depth: usize,
    ) -> std::result::Result<Arc<super::nodes::PersistentNode<V>>, DurableBuildError> {
        use super::nodes::persistent_node::Child;

        if depth == term.len() {
            if !node.is_final() {
                // Already absent — do NOT publish a no-op spine.
                return Err(DurableBuildError::AlreadyExists);
            }
            // FRESH cleared leaf (as_non_final); the subtree is RETAINED (remove "cat"
            // keeps "cats"). The 1→0 transition goes through a fresh copy + root CAS,
            // never an in-place clear of the shared node.
            return Ok(Arc::new(node.as_non_final()));
        }

        let key = term[depth];
        match node.find_child(key) {
            Some(child) => {
                if let Some(child_arc) = child.as_in_mem() {
                    let child_arc = Arc::clone(child_arc);
                    let new_child = self.build_remove_path_recursive(&child_arc, term, depth + 1)?;
                    Ok(Arc::new(node.with_child(key, Child::InMem(new_child))))
                } else {
                    // On-disk / null filler ⇒ absent on this snapshot (byte overlay
                    // has no fault-in pre-M4).
                    Err(DurableBuildError::AlreadyExists)
                }
            }
            // Missing edge ⇒ the term is absent on this snapshot.
            None => Err(DurableBuildError::AlreadyExists),
        }
    }

    /// Path-copy the `root`→leaf spine for `key`, finalizing the leaf with `value`.
    /// Returns a new root `Arc` or `None` if an OnDisk child blocks the copy (byte
    /// overlay has no write-path fault-in). **G5/F0: GENERIC over `V`** (relocated
    /// here from the `<i64,S>` block; the only `V`-ness is `value`). Shared by the
    /// value seams (insert/upsert/CAS — [`value_publish_inner`]) AND the i64 counter
    /// inner. Empty `key` (depth 0 == len 0) is the RANKED empty-term root publish.
    pub(crate) fn build_value_path_recursive(
        &self,
        node: &Arc<super::nodes::PersistentNode<V>>,
        key: &[u8],
        depth: usize,
        value: V,
    ) -> Option<Arc<super::nodes::PersistentNode<V>>> {
        use super::nodes::persistent_node::{Child, PersistentNode};

        // ITERATIVE (was recursive — recursion depth == key length, which overflows the
        // stack for very long keys because the overlay spine is UN-path-compressed, one
        // node per byte). Descend from `depth` collecting the (parent, byte) spine, then
        // rebuild it bottom-up. Same path-copy / absent-spine / valued-leaf semantics as
        // the prior recursion; byte does NOT fault OnDisk children in on the write path
        // (an OnDisk child returns `None`, exactly as the prior `as_in_mem()?` did).
        let mut spine: Vec<(Arc<PersistentNode<V>>, u8)> =
            Vec::with_capacity(key.len().saturating_sub(depth));
        let mut current = Arc::clone(node);
        let mut d = depth;
        loop {
            if d == key.len() {
                // Reached the leaf: bake finality + value into a fresh copy, then rebuild
                // every ancestor bottom-up (the path copy).
                let mut new_node = Arc::new(current.as_final().with_value(value));
                for (parent, b) in spine.into_iter().rev() {
                    new_node = Arc::new(parent.with_child(b, Child::InMem(new_node)));
                }
                return Some(new_node);
            }

            let k = key[d];
            match current.find_child(k) {
                Some(child) => {
                    // In-memory child: descend (path-copy on the way back up). On-disk →
                    // cannot fault in on the write path; return None (the caller retries).
                    let child_arc = match child.as_in_mem() {
                        Some(c) => Arc::clone(c),
                        None => return None,
                    };
                    spine.push((current, k));
                    current = child_arc;
                    d += 1;
                }
                None => {
                    // Child absent: build the remaining spine bottom-up (valued leaf),
                    // splice at `k`, then rebuild the collected spine.
                    let leaf = Arc::new(PersistentNode::<V>::new().as_final().with_value(value));
                    let mut sub = leaf;
                    for &b in key[d + 1..].iter().rev() {
                        sub = Arc::new(PersistentNode::<V>::new().with_child(b, Child::InMem(sub)));
                    }
                    let mut new_node = Arc::new(current.with_child(k, Child::InMem(sub)));
                    for (parent, b) in spine.into_iter().rev() {
                        new_node = Arc::new(parent.with_child(b, Child::InMem(new_node)));
                    }
                    return Some(new_node);
                }
            }
        }
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
    ///
    /// Thin wrapper over [`Self::try_increment_cas_inner`] that drops the commit
    /// generation, preserving the public signature for the existing callers (the
    /// non-durable / `increment_cas` paths and tests do not rank, so they ignore
    /// the generation) — mirrors char's `try_increment_cas`.
    pub fn try_increment_cas(&self, key: &[u8], delta: u64) -> Result<u64> {
        self.try_increment_cas_inner(key, delta).map(|(v, _)| v)
    }

    /// **M2b — the generation-returning increment publish inner.** Like
    /// [`Self::try_increment_cas`] but ALSO returns the durable global `commit_seq`
    /// of the WINNING CAS (the Order-A commit GENERATION), so the durable wrapper
    /// ([`DurableOverlayWrite::try_increment_cas_durable_default`]) can rank the
    /// delta in the same generation domain as the overwrite producers (closes hazard
    /// D — a `V=i64` key touched by both a ranked overwrite and an unranked
    /// increment would otherwise cross-domain mis-sort). The byte twin of char's
    /// `try_increment_cas_inner`.
    ///
    /// The `commit_seq` claim is taken at the CAS-retry loop-top and RE-CLAIMED each
    /// iteration so a Conflict-retry discards the lost claim and takes a fresh
    /// (higher) one; every write serializes at the single root CAS ⇒ the winning
    /// iteration's claim is strictly monotone in the global root-CAS order AND
    /// durable across restart (seeded from `max(floor, scan)` on open). The
    /// generation is returned ONLY from the `Ok` arm (a losing iteration discards
    /// its claim, so no stale generation leaks).
    pub(super) fn try_increment_cas_inner(&self, key: &[u8], delta: u64) -> Result<(u64, u64)> {
        use super::nodes::persistent_node::PersistentNode;
        use std::sync::atomic::Ordering;

        let lockfree_root = self
            .lockfree_root
            .as_ref()
            .expect("Lock-free mode not enabled. Call enable_lockfree() first.");

        // Empty-string support (H4): the empty key "" IS the root; the loop below
        // reads the root counter via `find_leaf_recursive(root, b"", 0)` (returns the
        // root iff final → its value, else 0) and republishes via
        // `build_value_path_recursive(root, b"", 0, ..)` which at depth 0 produces a
        // FRESH `as_final().with_value` root (fresh-root-CAS, NOT in-place) — so the
        // root counter RMW is the depth-0 case of the general loop. No rejection.
        if delta > LOCKFREE_COUNTER_MAX {
            return Err(Self::lockfree_increment_overflow_error(key, None, delta));
        }

        let _epoch = self.epoch_manager.enter_read();

        // Path-copy CAS retry loop (single-phase: the root CAS is the sole
        // visibility arbiter — the new leaf's value is published atomically with
        // the new root, so a stale reader never sees a torn count).
        loop {
            // commit_seq CLAIM (loop-top, re-claimed per iteration) — see char's
            // `try_increment_cas_inner`. The durable wrapper ranks the winning claim;
            // the non-durable caller discards it (a harmless gap in the global
            // counter). Monotone in the global root-CAS order, durable across restart.
            let commit_seq = self.commit_seq.fetch_add(1, Ordering::AcqRel) + 1;

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
            // GENERATION: the durable global `commit_seq` claimed at this iteration's
            // loop-top (NOT `new_root.version()`). Returned ONLY from the winning
            // `Ok` arm so a losing iteration never leaks a stale rank; the durable
            // wrapper ranks it.
            let generation = commit_seq;
            match lockfree_root.compare_exchange(&root, new_root) {
                Ok(_) => return Ok((new_val, generation)),
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
    /// Lock-free increment: create path if needed, then atomically add delta.
    ///
    /// Panics if the checked counter domain would be exceeded. Use
    /// [`Self::try_increment_cas`] to handle overflow as a recoverable error.
    pub fn increment_cas(&self, key: &[u8], delta: u64) -> u64 {
        self.try_increment_cas(key, delta)
            .unwrap_or_else(|error| panic!("lock-free increment_cas failed: {}", error))
    }

    /// **M2b — Order-A durable** lock-free increment (`V = i64`), the byte twin of
    /// char's `try_increment_cas_durable`. Establishes `visible ⊆ durable-prefix`
    /// for a counter delta: the delta `BatchIncrement` WAL record is appended AND
    /// synced DURABLE BEFORE the visibility-publishing root CAS, and the committed
    /// watermark advances only after the CAS lands. A crash loses no acknowledged
    /// increment — the durable delta replays (deltas are commutative, so recovery
    /// SUMS them regardless of commit order); an un-acknowledged one was never
    /// durable. The visibility step REUSES the proven path-copy
    /// [`Self::try_increment_cas_inner`] verbatim.
    ///
    /// `delta` is `i64` (the byte overlay counter domain); a NEGATIVE delta is
    /// rejected by the C4 [`DurableOverlayWrite::bound_increment_delta`] seam
    /// (counters are non-negative). Requires `enable_lockfree()` and a synchronous
    /// durability policy (`Immediate`/`GroupCommit`). Returns the new accumulated
    /// count on success.
    ///
    /// Thin wrapper over the SHARED GENERIC Order-A increment template
    /// [`DurableOverlayWrite::try_increment_cas_durable_default`] — the default owns
    /// the data-loss-critical skeleton (gate, empty short-circuit, the C4
    /// value-domain bound via the seam, the append→publish→commit-rank→mark ORDER);
    /// this wrapper supplies only the key-byte boundary + the empty-key return value.
    pub fn try_increment_cas_durable(&self, key: &[u8], delta: i64) -> Result<i64> {
        // The durable increment operates on UTF-8 keys (so the `&str` the trait
        // default threads to `bound_increment_delta` / `increment_publish_inner`
        // round-trips losslessly to `key` via `as_bytes`). Reject non-UTF-8 loudly
        // rather than lossily.
        let key_str = std::str::from_utf8(key).map_err(|_| {
            PersistentARTrieError::InvalidOperation(
                "try_increment_cas_durable requires a UTF-8 key on the byte durable \
                 increment path".to_string(),
            )
        })?;
        <Self as DurableOverlayWrite<ByteKey, i64, S>>::try_increment_cas_durable_default(
            self, key_str, key, delta,
        )
    }

    /// **M2b — Order-A durable VALUED insert** (`V = i64`), the byte twin of char's
    /// `insert_cas_with_value_durable`. The valued analogue of
    /// [`Self::insert_cas_durable`] (membership only): bakes an `i64` value into the
    /// leaf via [`Self::build_value_path_recursive`] (single-phase — finality + value
    /// publish atomically with the root CAS).
    ///
    /// **Insert semantics (NOT upsert):** if the term is already present this is a
    /// no-op returning `Ok(false)` with NO WAL record (the value is NOT overwritten).
    ///
    /// Order-A: the `Insert{value}` WAL record is appended+synced DURABLE before the
    /// visibility CAS; the watermark advances only after the CAS lands (+ the
    /// CommitRank). Requires a synchronous durability policy and `enable_lockfree()`.
    /// Returns `Ok(true)` iff this call newly inserted the term.
    pub fn insert_cas_with_value_durable(&self, term: &[u8], value: i64) -> Result<bool> {
        // **Flip F0 (G5): thin caller of the SHARED GENERIC value-write default.**
        // The C4 i64-specific guard (the byte overlay counter domain is a
        // non-negative i64) stays here; EVERYTHING else is the generic Order-A
        // [`DurableOverlayWrite::insert_cas_with_value_durable_default`] (gate →
        // present-hoist → append `Insert` DURABLE → value seam publish (insert-once)
        // → rank-or-burn), shared verbatim with the arbitrary-`V` path. Empty `""`
        // flows through the value seam's RANKED depth-0 publish (no special case).
        if value < 0 {
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "insert_cas_with_value_durable value for byte term {:?} must be non-negative \
                 (the overlay counter domain is a non-negative i64); got {}",
                String::from_utf8_lossy(term),
                value
            )));
        }
        <Self as DurableOverlayWrite<ByteKey, i64, S>>::insert_cas_with_value_durable_default(
            self, term, value,
        )
    }

    /// **M2b — Order-A durable UPSERT** (`V = i64`), the byte twin of char's
    /// `upsert_cas_durable`. Like [`Self::insert_cas_with_value_durable`] but UPSERT:
    /// the value is ALWAYS written (last-writer-wins = the root-CAS winner), whether
    /// or not the term already existed. Returns `Ok(true)` iff the term was newly
    /// inserted (`false` = updated an existing term).
    pub fn upsert_cas_durable(&self, term: &[u8], value: i64) -> Result<bool> {
        // **Flip F0 (G5): thin caller of the SHARED GENERIC value-write default.**
        // C4 i64-specific guard, then the generic
        // [`DurableOverlayWrite::upsert_cas_durable_default`] (gate → advisory
        // existed-probe → append `Upsert` DURABLE → value seam publish in Upsert
        // (always-write) mode → rank). Empty `""` flows through the value seam's
        // RANKED depth-0 publish (no special case).
        if value < 0 {
            return Err(PersistentARTrieError::InvalidOperation(format!(
                "upsert_cas_durable value for byte term {:?} must be non-negative \
                 (the overlay counter domain is a non-negative i64); got {}",
                String::from_utf8_lossy(term),
                value
            )));
        }
        <Self as DurableOverlayWrite<ByteKey, i64, S>>::upsert_cas_durable_default(self, term, value)
    }

    /// Merge lock-free values into the persistent trie by summing.
    ///
    /// Unlike `merge_lockfree_to_persistent()` which does boolean insert,
    /// this method walks the lock-free trie overlay, collects all
    /// `(key, value)` entries, and adds each value to the persistent trie.
    ///
    /// **M3 reject (BROKEN-BY-DESIGN, audit §B #5).** Under `route_overlay()` this
    /// drains the overlay (`collect_lockfree_entries_recursive`) into the owned tree
    /// AND clears the overlay root — destroying the durable production state (the
    /// overlay IS that state under the flip). Reject.
    pub fn merge_lockfree_values_to_persistent(&mut self) -> Result<usize> {
        use super::nodes::PersistentNode;

        if self.route_overlay() {
            return Err(PersistentARTrieError::InvalidOperation(
                "merge_lockfree_values_to_persistent is not valid under the lock-free overlay \
                 write mode (it drains and clears the overlay, which is the durable production \
                 state); use OverlayWriteMode::OwnedTree"
                    .to_string(),
            ));
        }

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

#[cfg(test)]
mod durable_write_tests {
    //! **M2b — Order-A durable write path (the byte twin of char's
    //! `durable_write_tests`).**
    //!
    //! The headline durability property (the #41-closed witness, byte twin): a term
    //! written via `insert_cas_durable` / `try_increment_cas_durable` and acknowledged
    //! survives a reopen **with no checkpoint at all** — durability rests entirely on
    //! the WAL record synced BEFORE the write became visible (Order A). On reopen the
    //! WAL replays the record into the recovered (owned) tree. Scratch is real disk
    //! (`target/test-tmp`), never `/tmp` (tmpfs — the disk-backed-test gotcha).

    use crate::persistent_artrie::PersistentARTrie;
    use crate::persistent_artrie_core::durability::DurabilityPolicy;
    use crate::persistent_artrie_core::overlay::write_mode::OverlayWriteMode;
    use crate::{Dictionary, MappedDictionary};
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp")
    }

    /// **THE #41 BYTE WITNESS (membership).** Terms inserted durably + acknowledged
    /// survive a reopen WITHOUT a checkpoint (pure WAL replay = durability proven).
    /// Explicitly enables the overlay write mode (`enable_lockfree` +
    /// `set_overlay_write_mode(LockFreeOverlay)`) so the durable path is exercised as
    /// the M4 flip will use it.
    #[test]
    fn insert_cas_durable_survives_reopen_without_checkpoint() {
        let dir = scratch("byte-order-a-durable");
        let path = dir.path().join("t.part");
        let terms: [&[u8]; 6] = [b"apple", b"apricot", b"banana", b"band", b"bandana", b"cherry"];

        {
            let mut trie = PersistentARTrie::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
            for (i, t) in terms.iter().enumerate() {
                assert!(
                    trie.insert_cas_durable(t).expect("durable insert"),
                    "{:?} is a new term",
                    String::from_utf8_lossy(t)
                );
                // The committed watermark advances to cover each appended LSN (LSNs
                // start at 1; each durable insert burns 2 LSNs — the Insert + its
                // CommitRank — so after i+1 inserts the watermark is ≥ 2*(i+1)).
                assert!(
                    trie.committed_watermark.watermark() >= (i as u64 + 1),
                    "watermark must cover {} committed LSNs, got {}",
                    i + 1,
                    trie.committed_watermark.watermark()
                );
            }
            // A duplicate returns Ok(false) and does not regress the watermark.
            assert!(!trie.insert_cas_durable(b"apple").expect("dup durable insert"));
            // DROP WITHOUT CHECKPOINT — durability rests entirely on the WAL.
        }

        // Reopen: every durably-logged insert must replay into the recovered tree.
        let trie = PersistentARTrie::<()>::open(&path).expect("reopen");
        for t in terms {
            let term_str = std::str::from_utf8(t).expect("ascii");
            assert!(
                Dictionary::contains(&trie, term_str),
                "durably-inserted term {:?} lost after reopen-without-checkpoint (Order-A broken)",
                term_str
            );
        }
        assert!(!Dictionary::contains(&trie, "never-inserted"));
    }

    /// **THE #41 BYTE WITNESS (counter).** Each durably-acknowledged delta survives a
    /// reopen WITH NO CHECKPOINT, replayed from the delta-based `BatchIncrement` WAL
    /// records (deltas are commutative, so recovery SUMS them). The reopened counts
    /// equal the summed deltas.
    #[test]
    fn try_increment_cas_durable_survives_reopen_without_checkpoint() {
        let dir = scratch("byte-order-a-incr-durable");
        let path = dir.path().join("t.part");
        // (key, number of +delta steps, delta) → expected = steps*delta.
        let plan: [(&[u8], i64, i64); 4] = [
            (b"apple", 3, 1),
            (b"apricot", 2, 10),
            (b"band", 1, 7),
            (b"cherry", 4, 25),
        ];

        {
            let mut trie = PersistentARTrie::<i64>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
            for (key, steps, delta) in plan {
                let mut last = 0;
                for _ in 0..steps {
                    last = trie
                        .try_increment_cas_durable(key, delta)
                        .expect("durable increment");
                }
                assert_eq!(last, steps * delta, "live overlay count for {:?}", String::from_utf8_lossy(key));
            }
            // DROP WITHOUT CHECKPOINT — durability rests entirely on the WAL.
        }

        // Reopen: the summed deltas must replay into the recovered tree.
        let trie = PersistentARTrie::<i64>::open(&path).expect("reopen");
        for (key, steps, delta) in plan {
            let key_str = std::str::from_utf8(key).expect("ascii");
            assert_eq!(
                MappedDictionary::get_value(&trie, key_str),
                Some(steps * delta),
                "durably-incremented {:?} lost/wrong after reopen-without-checkpoint (Order-A increment broken)",
                key_str
            );
        }
        assert_eq!(MappedDictionary::get_value(&trie, "never-incremented"), None);
    }

    /// **THE #41 BYTE WITNESS (valued insert + upsert).** `insert_cas_with_value_durable`
    /// + `upsert_cas_durable` acknowledged writes survive a reopen WITH NO CHECKPOINT.
    #[test]
    fn valued_durable_writes_survive_reopen_without_checkpoint() {
        let dir = scratch("byte-order-a-valued-durable");
        let path = dir.path().join("t.part");
        {
            let mut trie = PersistentARTrie::<i64>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
            assert!(trie.insert_cas_with_value_durable(b"alpha", 11).expect("valued insert"));
            // Insert semantics: a second valued insert of an existing term is a no-op.
            assert!(!trie.insert_cas_with_value_durable(b"alpha", 99).expect("dup valued insert"));
            // Upsert ALWAYS writes (last-writer-wins): newly inserts "beta", updates "alpha".
            assert!(trie.upsert_cas_durable(b"beta", 22).expect("upsert new"));
            assert!(!trie.upsert_cas_durable(b"alpha", 33).expect("upsert existing"));
            // DROP WITHOUT CHECKPOINT.
        }
        let trie = PersistentARTrie::<i64>::open(&path).expect("reopen");
        // "alpha": inserted 11, the dup insert was a no-op (11 retained), then upsert→33.
        assert_eq!(
            MappedDictionary::get_value(&trie, "alpha"),
            Some(33),
            "alpha must recover as the last upsert value (33)"
        );
        assert_eq!(
            MappedDictionary::get_value(&trie, "beta"),
            Some(22),
            "beta must recover as its upsert value (22)"
        );
    }

    /// **THE D-VAL GATE (M4a).** A valued overlay write, then a CHECKPOINT, then a
    /// reopen — the i64 value must survive THROUGH THE CHECKPOINT IMAGE, not WAL
    /// replay: `checkpoint()` advances `checkpoint_lsn`, so recovery skips the WAL
    /// deltas `≤ checkpoint_lsn` as "already folded into the image", and reopen loads
    /// the serialized image. This is exactly the red-team's D-VAL scenario: the overlay
    /// capture emits valued ART nodes whose value `serialize_child_to_disk_with_path`
    /// previously DROPPED (`let _ = value;`) and `disk_load` reloaded as `None` — a
    /// silent total counter-value loss. M4a appends the value to the node record (the
    /// `HAS_VALUE` flag) and reads it back, so it round-trips. WITHOUT M4a this FAILS.
    #[test]
    fn m4a_valued_overlay_checkpoint_reopen_preserves_value_through_image() {
        let dir = scratch("byte-m4a-dval-checkpoint");
        let path = dir.path().join("t.part");
        {
            let mut trie = PersistentARTrie::<i64>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
            assert!(trie
                .insert_cas_with_value_durable(b"counter", 42)
                .expect("valued insert"));
            assert!(trie
                .insert_cas_with_value_durable(b"other", 7)
                .expect("valued insert 2"));
            assert_eq!(
                trie.get_lockfree(b"counter"),
                Some(42),
                "value present in the overlay pre-checkpoint"
            );
            // CHECKPOINT: capture the overlay → serialize the valued ART nodes (the
            // D-VAL serialization path). Then DROP — the WAL deltas are skip-marked
            // `≤ checkpoint_lsn`, so the reopen below reads the IMAGE, not WAL replay.
            trie.checkpoint().expect("overlay checkpoint");
        }
        let trie = PersistentARTrie::<i64>::open(&path).expect("reopen");
        assert_eq!(
            MappedDictionary::get_value(&trie, "counter"),
            Some(42),
            "D-VAL: the i64 value MUST survive the checkpoint-image round-trip (M4a)"
        );
        assert_eq!(
            MappedDictionary::get_value(&trie, "other"),
            Some(7),
            "D-VAL: the second i64 value must survive too"
        );
    }

    /// **THE #41 BYTE WITNESS (remove).** A durable remove clears a present term and
    /// the clear survives a reopen WITH NO CHECKPOINT (the `Remove` record replays over
    /// the recovered tree), while a co-inserted, never-removed sibling survives.
    #[test]
    fn remove_cas_durable_clears_and_survives_reopen_without_checkpoint() {
        let dir = scratch("byte-order-a-remove");
        let path = dir.path().join("t.part");
        {
            let mut trie = PersistentARTrie::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
            // "apple"/"apricot" share the "ap" prefix; removing "apple" retains "apricot".
            assert!(trie.insert_cas_durable(b"apple").expect("durable insert"));
            assert!(trie.insert_cas_durable(b"apricot").expect("durable insert"));
            assert!(trie.contains_lockfree(b"apple"));

            let wm_before = trie.committed_watermark.watermark();
            assert!(
                trie.remove_cas_durable(b"apple").expect("durable remove"),
                "removing a present term returns Ok(true)"
            );
            assert!(
                !trie.contains_lockfree(b"apple"),
                "removed term must read ABSENT — stale positive cache would resurrect it"
            );
            assert!(
                trie.contains_lockfree(b"apricot"),
                "the shared-prefix sibling must survive the remove (subtree retained)"
            );
            assert!(
                trie.committed_watermark.watermark() > wm_before,
                "the durable Remove must advance the committed watermark"
            );

            // A no-op remove (already absent) must NOT append a WAL record / move the
            // watermark.
            let wm_noop = trie.committed_watermark.watermark();
            assert!(!trie.remove_cas_durable(b"apple").expect("idempotent remove"));
            assert!(!trie.remove_cas_durable(b"never-present").expect("absent remove"));
            assert_eq!(
                trie.committed_watermark.watermark(),
                wm_noop,
                "a no-op remove must NOT append a WAL record / advance the watermark"
            );
            // DROP WITHOUT CHECKPOINT.
        }
        let trie = PersistentARTrie::<()>::open(&path).expect("reopen");
        assert!(
            !Dictionary::contains(&trie, "apple"),
            "durably-removed term \"apple\" reappeared after reopen (Order-A remove broken)"
        );
        assert!(
            Dictionary::contains(&trie, "apricot"),
            "co-inserted, never-removed term \"apricot\" lost after reopen"
        );
    }

    /// **C4 NEGATIVE-REJECT (the byte i64 value-domain guard).** A negative `i64`
    /// delta/value is REJECTED (not wrapped/panicked) — the overlay counter domain is
    /// a non-negative i64. Covers the increment, valued-insert, and upsert durable
    /// paths.
    #[test]
    fn c4_negative_value_is_rejected_not_wrapped() {
        let dir = scratch("byte-order-a-c4-negative");
        let path = dir.path().join("t.part");
        let mut trie = PersistentARTrie::<i64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        trie.enable_lockfree();
        trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);

        // A negative increment delta must be rejected (the C4 bound), not wrapped.
        let inc = trie.try_increment_cas_durable(b"neg", -1);
        assert!(
            inc.is_err(),
            "a negative i64 increment delta must be rejected (C4), got {:?}",
            inc
        );
        // A negative valued-insert value must be rejected.
        assert!(
            trie.insert_cas_with_value_durable(b"neg", -5).is_err(),
            "a negative i64 insert value must be rejected (C4)"
        );
        // A negative upsert value must be rejected.
        assert!(
            trie.upsert_cas_durable(b"neg", -7).is_err(),
            "a negative i64 upsert value must be rejected (C4)"
        );
        // None of the rejected writes left a durable record: the key never became
        // present, and a reopen sees nothing (no panic, no wrap, no partial state).
        assert_eq!(MappedDictionary::get_value(&trie, "neg"), None);
        // A non-negative increment still works (the bound passes 0 and positives).
        assert_eq!(trie.try_increment_cas_durable(b"pos", 0).expect("zero delta"), 0);
        assert_eq!(trie.try_increment_cas_durable(b"pos", 5).expect("pos delta"), 5);
    }

    /// The durable entry points reject a non-synchronous durability policy (an
    /// acknowledged write can only be guaranteed durable under `Immediate`/`GroupCommit`).
    #[test]
    fn durable_writes_reject_non_synchronous_policy() {
        let dir = scratch("byte-order-a-reject");
        let path = dir.path().join("t.part");
        let mut trie = PersistentARTrie::<i64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::None);
        trie.enable_lockfree();
        trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
        assert!(
            trie.insert_cas_durable(b"x").is_err(),
            "insert_cas_durable must reject a non-synchronous policy"
        );
        assert!(
            trie.try_increment_cas_durable(b"x", 1).is_err(),
            "try_increment_cas_durable must reject a non-synchronous policy"
        );
        trie.set_durability_policy(DurabilityPolicy::Periodic);
        assert!(
            trie.remove_cas_durable(b"x").is_err(),
            "remove_cas_durable must reject a non-synchronous policy"
        );
        assert!(
            trie.upsert_cas_durable(b"x", 1).is_err(),
            "upsert_cas_durable must reject a non-synchronous policy"
        );
    }

    /// Concurrent soak: many threads durably-insert disjoint keys under shared-prefix
    /// CAS contention (WAL-only — no checkpoint). EVERY acknowledged key MUST survive a
    /// reopen via WAL replay — the #41-closed property under concurrency.
    #[test]
    fn concurrent_durable_writers_all_survive_reopen() {
        let dir = scratch("byte-order-a-soak");
        let path = dir.path().join("t.part");
        let n_threads = 6;
        let per_thread = 100;

        let acknowledged: Vec<Vec<u8>> = {
            let mut trie = PersistentARTrie::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
            let trie = Arc::new(trie);
            let barrier = Arc::new(Barrier::new(n_threads));

            let handles: Vec<_> = (0..n_threads)
                .map(|t| {
                    let trie = Arc::clone(&trie);
                    let barrier = Arc::clone(&barrier);
                    thread::spawn(move || {
                        barrier.wait();
                        let mut acked = Vec::with_capacity(per_thread);
                        for i in 0..per_thread {
                            // Shared "p" prefix → CAS contention on the spine.
                            let key = format!("p{t}_{i:04}").into_bytes();
                            if trie.insert_cas_durable(&key).expect("durable insert") {
                                acked.push(key);
                            }
                        }
                        acked
                    })
                })
                .collect();
            let mut all = Vec::new();
            for h in handles {
                all.extend(h.join().expect("durable writer thread"));
            }
            all
            // DROP WITHOUT CHECKPOINT.
        };

        // Reopen: every acknowledged key survives via WAL replay.
        let trie = PersistentARTrie::<()>::open(&path).expect("reopen");
        for key in &acknowledged {
            let key_str = std::str::from_utf8(key).expect("ascii");
            assert!(
                Dictionary::contains(&trie, key_str),
                "concurrently durably-inserted key {:?} lost after reopen (Order-A broken under concurrency)",
                key_str
            );
        }
        assert_eq!(
            acknowledged.len(),
            n_threads * per_thread,
            "all disjoint keys should have been newly inserted (one ack each)"
        );
    }
}

#[cfg(test)]
mod m2d_regime_aware_recovery_tests {
    //! **M2d — byte regime-aware crash-recovery (the byte twin of char's A2
    //! end-to-end / s5_12 Test-A gate).** Byte now EMITS `WalRecord::CommitRank`
    //! (M2b), so post-recovery same-term last-writer-wins MUST order by commit
    //! GENERATION (not physical LSN) and an Overlay-regime UNRANKED orphan MUST be
    //! DROPPED — else recovery resurrects a dropped term (red-team defect H3).
    //! These tests exercise the FULL byte `open` recovery sink (sink 1) on a REAL
    //! on-disk WAL, proving (a) the Overlay orphan-drop closes H3 and (b) a
    //! rank-less Owned WAL replays in-order byte-for-byte (the back-compat proof).
    //!
    //! NOTE (no M4 here): byte's `open` does NOT create-flip / reestablish, so the
    //! recovered state lands in the OWNED tree and is read post-reopen via
    //! `Dictionary::contains` / `MappedDictionary::get_value` (the owned readers) —
    //! M2d threads the RECONCILE only, not the read route. Scratch is real disk
    //! (`target/test-tmp`), never `/tmp` (tmpfs — the disk-backed-test gotcha).

    use crate::persistent_artrie::PersistentARTrie;
    use crate::persistent_artrie_core::durability::DurabilityPolicy;
    use crate::persistent_artrie_core::overlay::write_mode::OverlayWriteMode;
    use crate::persistent_artrie_core::wal::WalRecord;
    use crate::{Dictionary, MappedDictionary};

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp")
    }

    /// **Test A — the A2 end-to-end PRIMARY gate (byte twin of char's
    /// `s5_12_test_a_overlay_reopen_drops_unranked_orphan_keeps_ranked`).** An
    /// Overlay-regime WAL with a RANKED survivor (`insert_cas_durable` ⇒ durable
    /// Insert + CommitRank, acked) and a durable UNRANKED orphan (a raw Insert with
    /// NO following CommitRank — exactly the two-append-window crash state) ⇒ a real
    /// reopen DROPS the orphan and KEEPS the survivor (the SHARED regime-aware
    /// reconcile threaded through byte's `open` sink, end-to-end on a real WAL).
    #[test]
    fn test_a_overlay_reopen_drops_unranked_orphan_keeps_ranked() {
        let dir = scratch("byte-m2d-test-a");
        let path = dir.path().join("t.part");
        {
            let mut trie = PersistentARTrie::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            // Stamps the WAL header regime = Overlay.
            trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
            // RANKED survivor: insert_cas_durable appends Insert + CommitRank (acked).
            assert!(
                trie.insert_cas_durable(b"survivor").expect("durable insert"),
                "survivor is a new term"
            );
            // Durable UNRANKED orphan: a raw Insert with NO following CommitRank —
            // the two-append-window crash state recovery must DROP under Overlay.
            trie.append_to_wal_returning_lsn(WalRecord::Insert {
                term: b"orphan".to_vec(),
                value: None,
            })
            .expect("append durable orphan");
            // DROP WITHOUT CHECKPOINT.
        }
        // Reopen: the Overlay-regime replay (the SHARED reconcile) DROPS the orphan.
        let recovered = PersistentARTrie::<()>::open(&path).expect("reopen");
        assert!(
            Dictionary::contains(&recovered, "survivor"),
            "the ranked survivor must survive reopen"
        );
        assert!(
            !Dictionary::contains(&recovered, "orphan"),
            "the unranked orphan must be DROPPED on Overlay reopen (A2/H3, end-to-end)"
        );
    }

    /// **Test B — no-resurrection (same-term, the H3 data-loss scenario).** Under
    /// Overlay: durably insert then durably remove a term `t` (both RANKED), then
    /// leave a raw UNRANKED Insert-`t` orphan (the redundant idempotent producer
    /// append on a present-hoist miss). A reopen MUST keep `t` ABSENT — the orphan,
    /// being unranked, is DROPPED under Overlay, so it cannot out-sort the ranked
    /// remove and resurrect `t`. (Under the OLD dumb in-order replay the orphan's
    /// high LSN would sort LAST ⇒ `t` resurrected = the bug H3 closes.) End-to-end
    /// twin of core's `overlay_drops_unranked_orphan_no_resurrection`.
    #[test]
    fn test_b_overlay_reopen_unranked_orphan_does_not_resurrect_removed_term() {
        let dir = scratch("byte-m2d-test-b");
        let path = dir.path().join("t.part");
        {
            let mut trie = PersistentARTrie::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
            // RANKED insert then RANKED remove of the SAME term `t`.
            assert!(trie.insert_cas_durable(b"t").expect("durable insert"));
            assert!(
                trie.remove_cas_durable(b"t").expect("durable remove"),
                "removing a present term returns Ok(true)"
            );
            // A co-present, never-removed sibling (RANKED) — must survive.
            assert!(trie.insert_cas_durable(b"keep").expect("durable insert"));
            // Durable UNRANKED orphan re-inserting `t` (no CommitRank): under Owned
            // its high LSN would sort AFTER the remove ⇒ resurrection; under Overlay
            // it is DROPPED.
            trie.append_to_wal_returning_lsn(WalRecord::Insert {
                term: b"t".to_vec(),
                value: None,
            })
            .expect("append durable orphan");
            // DROP WITHOUT CHECKPOINT.
        }
        let recovered = PersistentARTrie::<()>::open(&path).expect("reopen");
        assert!(
            !Dictionary::contains(&recovered, "t"),
            "Overlay must DROP the unranked orphan ⇒ the durably-removed term stays ABSENT (H3)"
        );
        assert!(
            Dictionary::contains(&recovered, "keep"),
            "the ranked, never-removed sibling must survive reopen"
        );
    }

    /// **Test C — rank-less Owned back-compat (the INERT proof).** A WAL written via
    /// the ordinary (non-durable) `insert` API carries NO `CommitRank` and stays the
    /// default `Owned` regime, so byte's `open` takes the `Owned` branch of
    /// `replay_records_lww` — the LITERAL pre-M2d in-order replay of the
    /// transaction-filtered `RecoveryManager` ops. Every inserted term must recover,
    /// exactly as before M2d (the SHARED reconcile is never consulted here).
    #[test]
    fn test_c_owned_rankless_wal_replays_in_order_unchanged() {
        let dir = scratch("byte-m2d-test-c");
        let path = dir.path().join("t.part");
        let terms: [&str; 6] = ["apple", "apricot", "banana", "band", "bandana", "cherry"];
        {
            let trie = PersistentARTrie::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            // NO enable_lockfree / NO set_overlay_write_mode ⇒ the WAL header regime
            // stays Owned (rank-less).
            for t in terms {
                trie.insert(t);
            }
            // DROP WITHOUT CHECKPOINT — durability rests on the (Owned) WAL.
        }
        let recovered = PersistentARTrie::<()>::open(&path).expect("reopen");
        for t in terms {
            assert!(
                Dictionary::contains(&recovered, t),
                "Owned rank-less WAL must replay {t:?} in-order (back-compat regression)"
            );
        }
        assert!(!Dictionary::contains(&recovered, "never-inserted"));
    }

    /// **Test D — Owned in-order last-writer-wins (remove-then-reinsert).** The
    /// end-to-end twin of core's `rankless_wal_applies_in_lsn_order`: under the
    /// `Owned` branch the per-term final state is decided by the HIGHEST-LSN op
    /// (in-order replay). `a` is removed then re-inserted (final = PRESENT); `gone`
    /// is inserted then removed (final = ABSENT). This proves the Owned branch keeps
    /// the pre-M2d LSN-ordered semantics — it must NOT borrow the Overlay
    /// orphan-drop (every op here is unranked, and under Owned unranked ⇒ KEEP).
    #[test]
    fn test_d_owned_rankless_in_order_lww_decides_final_state() {
        let dir = scratch("byte-m2d-test-d");
        let path = dir.path().join("t.part");
        {
            let trie = PersistentARTrie::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.insert("a"); // a: inserted…
            trie.insert("gone"); // gone: inserted…
            assert!(trie.remove("a"), "remove present a");
            assert!(trie.remove("gone"), "remove present gone");
            trie.insert("a"); // …a re-inserted at the highest LSN ⇒ final PRESENT.
            // gone stays removed ⇒ final ABSENT.
        }
        let recovered = PersistentARTrie::<()>::open(&path).expect("reopen");
        assert!(
            Dictionary::contains(&recovered, "a"),
            "Owned in-order replay: a's last op is insert ⇒ PRESENT"
        );
        assert!(
            !Dictionary::contains(&recovered, "gone"),
            "Owned in-order replay: gone's last op is remove ⇒ ABSENT"
        );
    }

    /// **Test E — Overlay counter survives (ranked deltas kept, no over/under-count).**
    /// The existing M2b durable-increment reopen test already covers the happy path;
    /// this asserts specifically that the new Overlay reconcile does NOT drop the
    /// RANKED increment data records (each durable increment is Insert/BatchIncrement
    /// + CommitRank), i.e. the orphan-DROP rule fires ONLY for unranked records. A
    /// ranked durable counter must recover its exact summed value.
    #[test]
    fn test_e_overlay_reopen_keeps_ranked_counter_value() {
        let dir = scratch("byte-m2d-test-e");
        let path = dir.path().join("t.part");
        {
            let mut trie = PersistentARTrie::<i64>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
            // 3 ranked +7 deltas ⇒ 21.
            for _ in 0..3 {
                trie.try_increment_cas_durable(b"ctr", 7)
                    .expect("durable increment");
            }
            // DROP WITHOUT CHECKPOINT.
        }
        let recovered = PersistentARTrie::<i64>::open(&path).expect("reopen");
        assert_eq!(
            MappedDictionary::get_value(&recovered, "ctr"),
            Some(21),
            "the ranked durable counter must recover its exact summed value (no orphan-drop of ranked records)"
        );
    }
}

#[cfg(test)]
mod m4b_flip_gate_tests {
    //! **M4b — the IRREVERSIBLE byte create-flip gate (the byte twin of char's
    //! `s5_12_*` gate).** These tests pin the production behavior of the flip that
    //! made the lock-free overlay byte's DEFAULT for `V ∈ {(), i64}`:
    //!
    //! - the create-flip routes a fresh eligible-V `create()` to the overlay and a
    //!   create→durable-write→reopen survives (via the AUTOMATIC open-flip +
    //!   reestablish in `open`'s D-SINK-2);
    //! - an old/Owned-regime file (and an ineligible-V `<String>`) reopens OWNED, with
    //!   `route_overlay()==false`, data intact, NO flip (back-compat);
    //! - `compact()` rejects under the overlay;
    //! - the reestablish SINK round-trips EVERY recovered term back into the overlay on
    //!   reopen, INCLUDING a >100k-term first-byte partition (the H2 uncapped-enumerator
    //!   witness — `owned_first_units` / `owned_units_with_values_under` must not cap).
    //!
    //! Scratch is real disk (`target/test-tmp`), never `/tmp` (tmpfs — the
    //! disk-backed-test gotcha).

    use crate::persistent_artrie::{CompactionConfig, PersistentARTrie};
    use crate::persistent_artrie_core::durability::DurabilityPolicy;
    use crate::{Dictionary, MappedDictionary};

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp")
    }

    /// **(a) create → durable VALUED write → reopen, no loss / no double-count.** A
    /// fresh `create::<i64>()` create-flips (`route_overlay()==true`); durable valued
    /// writes + a checkpoint; reopen MUST survive with exact counts via the overlay
    /// (the AUTOMATIC open-flip + reestablish in `open`). The membership (`V=()`) twin
    /// is also covered.
    #[test]
    fn m4b_create_durable_valued_write_reopen_survives() {
        // Counters (V = i64).
        {
            let dir = scratch("byte-m4b-rw-i64");
            let path = dir.path().join("t.part");
            let entries: Vec<(Vec<u8>, i64)> =
                (0..40i64).map(|i| (format!("k{i:03}").into_bytes(), i + 1)).collect();
            {
                let trie = PersistentARTrie::<i64>::create(&path).expect("create<i64>");
                trie.set_durability_policy(DurabilityPolicy::Immediate);
                assert!(trie.route_overlay(), "fresh create<i64> is overlay-routed");
                for (k, d) in &entries {
                    // Durable valued insert (overlay path). Distinct values per key so a
                    // double or drop is detectable.
                    assert!(
                        trie.insert_cas_with_value_durable(k, *d).expect("durable valued insert"),
                        "first valued insert of {k:?} must be newly inserted"
                    );
                }
                trie.checkpoint().expect("overlay checkpoint (route-split)");
            }
            let recovered = PersistentARTrie::<i64>::open(&path).expect("reopen<i64>");
            assert!(
                recovered.route_overlay(),
                "an Overlay-regime file must reopen overlay-routed (D-SINK-2)"
            );
            for (k, d) in &entries {
                assert_eq!(
                    recovered.get_value_bytes(k),
                    Some(*d),
                    "counter {k:?} wrong after reopen (loss or double-count)"
                );
            }
        }
        // Membership (V = ()).
        {
            let dir = scratch("byte-m4b-rw-unit");
            let path = dir.path().join("t.part");
            let terms: Vec<Vec<u8>> =
                (0..40u32).map(|i| format!("term{i:03}").into_bytes()).collect();
            {
                let trie = PersistentARTrie::<()>::create(&path).expect("create<()>");
                trie.set_durability_policy(DurabilityPolicy::Immediate);
                assert!(trie.route_overlay(), "fresh create<()> is overlay-routed");
                for t in &terms {
                    assert!(
                        trie.insert_cas_durable(t).expect("durable membership insert"),
                        "first durable insert of {t:?} must be newly inserted"
                    );
                }
                trie.checkpoint().expect("overlay checkpoint");
            }
            let recovered = PersistentARTrie::<()>::open(&path).expect("reopen<()>");
            assert!(recovered.route_overlay(), "() Overlay file reopens overlay-routed");
            for t in &terms {
                assert!(
                    recovered.contains_bytes(t),
                    "membership lost {t:?} across create→write→checkpoint→reopen"
                );
            }
        }
    }

    /// **(b) old-Owned file stays OWNED on reopen (back-compat) + ineligible V never
    /// flips.** A `<String>` (arbitrary V ⇒ never flips) trie that was created,
    /// written, and checkpointed produces an OWNED-regime file; reopening it must keep
    /// it OWNED (`route_overlay()==false`), data intact via the OWNED read path, NO
    /// flip. Also pins that an eligible-V trie that is `kill_switch_to_owned()`'d after
    /// create stays Owned across reopen (the restamp-Owned path).
    #[test]
    fn m4b_old_owned_file_stays_owned_on_reopen() {
        // Arbitrary V = String: an Owned-regime file reopens Owned with data intact.
        //
        // Arbitrary-V overlay routing is the default, so a fresh `create::<String>()`
        // create-flips; kill-switch it to the Owned regime (the same pattern as the
        // i64 block below) to exercise the "an Owned-regime file stays owned on
        // reopen" path.
        {
            let dir = scratch("byte-m4b-owned-string");
            let path = dir.path().join("t.part");
            let entries: Vec<(String, String)> = (0..30u32)
                .map(|i| (format!("w{i:03}"), format!("v{i:03}")))
                .collect();
            {
                let trie = PersistentARTrie::<String>::create(&path).expect("create<String>");
                trie.kill_switch_to_owned();
                assert!(!trie.route_overlay(), "String trie is on the owned path");
                for (k, v) in &entries {
                    trie.insert_with_value(k, v.clone());
                }
                trie.checkpoint().expect("owned checkpoint");
            }
            let recovered = PersistentARTrie::<String>::open(&path).expect("reopen<String>");
            assert!(
                !recovered.route_overlay(),
                "an Owned-regime file must STAY owned on reopen (no flip, back-compat)"
            );
            for (k, v) in &entries {
                assert_eq!(
                    MappedDictionary::get_value(&recovered, k),
                    Some(v.clone()),
                    "owned data lost for {k:?} across reopen"
                );
            }
        }
        // Eligible V = i64, kill-switched to Owned after create: reopens Owned.
        {
            let dir = scratch("byte-m4b-owned-i64-ks");
            let path = dir.path().join("t.part");
            {
                let trie = PersistentARTrie::<i64>::create(&path).expect("create<i64>");
                // kill-switch restamps the fresh WAL Owned ⇒ reopen stays Owned.
                trie.kill_switch_to_owned();
                assert!(!trie.route_overlay(), "kill-switch reverts to owned");
                trie.upsert_bytes(b"alpha", 7).expect("owned upsert");
                trie.upsert_bytes(b"beta", 11).expect("owned upsert");
                trie.checkpoint().expect("owned checkpoint");
            }
            let recovered = PersistentARTrie::<i64>::open(&path).expect("reopen<i64>");
            assert!(
                !recovered.route_overlay(),
                "a kill-switched (Owned-regime) i64 file must STAY owned on reopen"
            );
            assert_eq!(recovered.get_value_bytes(b"alpha"), Some(7), "owned alpha intact");
            assert_eq!(recovered.get_value_bytes(b"beta"), Some(11), "owned beta intact");
        }
    }

    /// **(c) `compact()` SUCCEEDS under the overlay (F6).** A fresh `create::<i64>()`
    /// flips; `compact()` now sources the snapshot from the overlay (enumeration AND
    /// values), rebuilds a dense owned image, and RE-FLIPS to preserve the overlay
    /// regime — data and routing survive. The kill-switched owned twin succeeds via the
    /// proven owned path.
    #[test]
    fn m4b_compact_succeeds_under_overlay_and_owned() {
        // Overlay (flipped): compact succeeds, preserves data, STAYS overlay.
        {
            let dir = scratch("byte-m4b-compact-overlay");
            let path = dir.path().join("t.part");
            let mut trie = PersistentARTrie::<i64>::create(&path).expect("create<i64>");
            assert!(trie.route_overlay(), "fresh create<i64> is overlay-routed");
            trie.increment_bytes(b"seed", 1).expect("seed");
            trie.compact(CompactionConfig::default(), |_| {})
                .expect("F6: compact succeeds under the overlay");
            assert!(
                trie.route_overlay(),
                "F6: compact must PRESERVE the overlay regime (re-flip after reopen)"
            );
            assert_eq!(
                trie.get_value_bytes(b"seed"),
                Some(1),
                "F6: data preserved across overlay compaction"
            );
        }
        // Owned (kill-switched): compact succeeds.
        {
            let dir = scratch("byte-m4b-compact-owned");
            let path = dir.path().join("t.part");
            let mut trie = PersistentARTrie::<i64>::create(&path).expect("create<i64>");
            trie.kill_switch_to_owned();
            trie.upsert_bytes(b"alpha", 1).expect("owned upsert");
            trie.upsert_bytes(b"beta", 2).expect("owned upsert");
            trie.compact(CompactionConfig::default(), |_| {})
                .expect("owned compact succeeds");
            assert_eq!(trie.get_value_bytes(b"alpha"), Some(1));
            assert_eq!(trie.get_value_bytes(b"beta"), Some(2));
        }
    }

    /// **(d) reestablish-survival across reopen, INCLUDING a >100k-term first-byte
    /// partition (the H2 uncapped-enumerator witness).** Build a flipped overlay trie
    /// with a LARGE single-first-byte partition (every key starts with `b'a'`, so they
    /// all fall in ONE `owned_first_units` partition — the worst case for the streaming
    /// reestablish), durably write + checkpoint, then reopen. On reopen the checkpoint
    /// image loads into the OWNED tree and `open`'s D-SINK-2 reestablishes owned→overlay
    /// via `reestablish_overlay_membership` (the `owned_first_units` /
    /// `owned_units_under` UNCAPPED walks). EVERY term must survive in the overlay — a
    /// capped enumerator would silently drop the tail (H2).
    #[test]
    fn m4b_reestablish_survival_incl_100k_first_byte_partition() {
        let dir = scratch("byte-m4b-reestablish-100k");
        let path = dir.path().join("t.part");
        // >100k terms ALL under the single first byte `a` (one partition). Use a fixed
        // 5-hex-digit suffix so every key is distinct and shares the `a` first byte.
        const N: u32 = 100_001;
        {
            let trie = PersistentARTrie::<()>::create(&path).expect("create<()>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            assert!(trie.route_overlay(), "fresh create<()> is overlay-routed");
            // Batch the durable inserts through the overlay membership path. Insert via
            // the no-WAL `insert_cas` + a SINGLE checkpoint would not be durable across
            // reopen for the image, so checkpoint AFTER to fold the overlay into the
            // image (the reopen reads the image, then reestablishes it).
            for i in 0..N {
                let key = format!("a{i:05x}");
                trie.insert_cas(key.as_bytes());
            }
            assert_eq!(trie.overlay_len(), N as usize, "all N terms resident pre-checkpoint");
            // Checkpoint folds the overlay into the durable image (so the reopen reads
            // them from the image into the owned tree, then reestablishes).
            trie.checkpoint().expect("overlay checkpoint of the 100k partition");
        }
        let recovered = PersistentARTrie::<()>::open(&path).expect("reopen<()>");
        assert!(
            recovered.route_overlay(),
            "the >100k Overlay file must reopen overlay-routed (reestablished)"
        );
        // EVERY term must have round-tripped through the reestablish (the H2 witness:
        // the uncapped first-byte partition enumerator must not drop the tail).
        assert_eq!(
            recovered.overlay_len(),
            N as usize,
            "reestablish must reproduce ALL {N} terms (H2: no capped-enumerator tail drop)"
        );
        // Spot-check the first, a middle, and the LAST term (the tail is the H2 risk).
        assert!(recovered.contains_bytes(format!("a{:05x}", 0u32).as_bytes()), "first term lost");
        assert!(
            recovered.contains_bytes(format!("a{:05x}", N / 2).as_bytes()),
            "middle term lost"
        );
        assert!(
            recovered.contains_bytes(format!("a{:05x}", N - 1).as_bytes()),
            "LAST term lost (H2 tail-drop witness)"
        );
    }

    /// **(d′) reestablish-survival for COUNTERS (i64) with exact summed values.** The
    /// value-carrying twin of (d): a moderately-sized flipped i64 overlay, durable
    /// valued writes + checkpoint, reopen → `reestablish_overlay_counter` must reproduce
    /// EVERY (term, count) exactly (the `owned_units_with_values_under` uncapped walk).
    #[test]
    fn m4b_reestablish_survival_counter_values() {
        let dir = scratch("byte-m4b-reestablish-ctr");
        let path = dir.path().join("t.part");
        let entries: Vec<(Vec<u8>, i64)> =
            (0..500i64).map(|i| (format!("c{i:04}").into_bytes(), (i % 97) + 1)).collect();
        {
            let trie = PersistentARTrie::<i64>::create(&path).expect("create<i64>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            assert!(trie.route_overlay());
            for (k, d) in &entries {
                trie.insert_cas_with_value_durable(k, *d).expect("durable valued insert");
            }
            trie.checkpoint().expect("overlay checkpoint");
        }
        let recovered = PersistentARTrie::<i64>::open(&path).expect("reopen<i64>");
        assert!(recovered.route_overlay(), "counter Overlay file reopens overlay-routed");
        assert_eq!(
            recovered.overlay_len(),
            entries.len(),
            "reestablish must reproduce ALL counter terms"
        );
        for (k, d) in &entries {
            assert_eq!(
                recovered.get_value_bytes(k),
                Some(*d),
                "counter value lost/wrong for {k:?} after reestablish"
            );
        }
    }

    // ======================================================================
    // EMPTY-STRING ("") DECISIVE MATRIX (empty-string support P2).
    // The empty term is now a FULL first-class key carrying a value, round-tripping
    // write → WAL → checkpoint → reopen (checkpoint-reopen AND pure-WAL-replay) →
    // read, on the overlay (production) path AND the owned (kill-switched) path.
    // ======================================================================

    /// **valued "" — overlay checkpoint → reopen.** The headline: an `i64` value on
    /// the empty term survives a checkpoint + reopen via the overlay root (H4 write +
    /// H2 capture + H1 serialize/load + H3 reestablish + H5 read).
    #[test]
    fn empty_string_valued_overlay_checkpoint_reopen() {
        let dir = scratch("byte-es-valued-ckpt");
        let path = dir.path().join("t.part");
        {
            let trie = PersistentARTrie::<i64>::create(&path).expect("create<i64>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            assert!(trie.route_overlay());
            assert!(
                trie.insert_cas_with_value_durable(b"", 42).expect("valued insert \"\""),
                "valued insert of \"\" must be newly inserted"
            );
            // A couple of non-empty terms so "" coexists with children.
            trie.insert_cas_with_value_durable(b"a", 1).expect("a");
            trie.insert_cas_with_value_durable(b"bc", 2).expect("bc");
            assert_eq!(trie.get_value_bytes(b""), Some(42), "\"\" readable pre-checkpoint");
            trie.checkpoint().expect("overlay checkpoint");
        }
        let recovered = PersistentARTrie::<i64>::open(&path).expect("reopen<i64>");
        assert!(recovered.route_overlay());
        assert_eq!(
            recovered.get_value_bytes(b""),
            Some(42),
            "empty-term value lost across checkpoint → reopen"
        );
        assert_eq!(recovered.get_value_bytes(b"a"), Some(1), "child 'a' lost");
        assert_eq!(recovered.get_value_bytes(b"bc"), Some(2), "child 'bc' lost");
    }

    /// **valued "" — pure WAL replay (NO checkpoint).** Order-A durability: an
    /// acknowledged valued "" write survives reopen with no checkpoint (WAL replay).
    #[test]
    fn empty_string_valued_pure_wal_replay() {
        let dir = scratch("byte-es-valued-wal");
        let path = dir.path().join("t.part");
        {
            let trie = PersistentARTrie::<i64>::create(&path).expect("create<i64>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.insert_cas_with_value_durable(b"", 7).expect("valued insert \"\"");
            // NO checkpoint — durability rests on WAL replay.
        }
        let recovered = PersistentARTrie::<i64>::open(&path).expect("reopen<i64>");
        assert_eq!(
            recovered.get_value_bytes(b""),
            Some(7),
            "empty-term value lost on pure-WAL-replay reopen (Order-A durability)"
        );
    }

    /// **membership "" — overlay checkpoint → reopen (H3).** The red-team's
    /// membership-reopen case: `insert("")` (V=()) → reopen → `contains("")` true (the
    /// reestablish membership fold republishes "" to the root, not drops it).
    #[test]
    fn empty_string_membership_overlay_reopen() {
        let dir = scratch("byte-es-membership");
        let path = dir.path().join("t.part");
        {
            let trie = PersistentARTrie::<()>::create(&path).expect("create<()>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            assert!(trie.insert_cas_durable(b"").expect("membership insert \"\""));
            trie.insert_cas_durable(b"x").expect("x");
            assert!(trie.contains_bytes(b""), "\"\" member pre-checkpoint");
            trie.checkpoint().expect("overlay checkpoint");
        }
        let recovered = PersistentARTrie::<()>::open(&path).expect("reopen<()>");
        assert!(
            recovered.contains_bytes(b""),
            "empty-term MEMBERSHIP lost across checkpoint → reopen (H3)"
        );
        assert!(recovered.contains_bytes(b"x"), "child 'x' membership lost");
    }

    /// **increment "" — overlay checkpoint → reopen (the unranked-drop fix).**
    /// `try_increment_cas_durable("")` ×N accumulates a RANKED durable root counter
    /// (not the old dropped-as-unranked 0).
    #[test]
    fn empty_string_increment_reopen() {
        let dir = scratch("byte-es-increment");
        let path = dir.path().join("t.part");
        {
            let trie = PersistentARTrie::<i64>::create(&path).expect("create<i64>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            let mut last = 0;
            for _ in 0..5 {
                last = trie.try_increment_cas_durable(b"", 3).expect("increment \"\"");
            }
            assert_eq!(last, 15, "5×3 increments of \"\" accumulate to 15");
            trie.checkpoint().expect("overlay checkpoint");
        }
        let recovered = PersistentARTrie::<i64>::open(&path).expect("reopen<i64>");
        assert_eq!(
            recovered.get_value_bytes(b""),
            Some(15),
            "empty-term counter lost/wrong across checkpoint → reopen (unranked-drop fix)"
        );
    }

    /// **remove "" — symmetry.** A durably-inserted "" is durably removable;
    /// `contains("")` is false after reopen.
    #[test]
    fn empty_string_remove_reopen() {
        let dir = scratch("byte-es-remove");
        let path = dir.path().join("t.part");
        {
            let trie = PersistentARTrie::<()>::create(&path).expect("create<()>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            assert!(trie.insert_cas_durable(b"").expect("insert \"\""));
            assert!(trie.contains_bytes(b""), "\"\" present after insert");
            assert!(trie.remove_cas_durable(b"").expect("remove \"\""), "remove cleared \"\"");
            assert!(!trie.contains_bytes(b""), "\"\" absent after remove");
            trie.checkpoint().expect("overlay checkpoint");
        }
        let recovered = PersistentARTrie::<()>::open(&path).expect("reopen<()>");
        assert!(
            !recovered.contains_bytes(b""),
            "empty-term must stay REMOVED across checkpoint → reopen (remove symmetry)"
        );
    }

    /// **owned-regime valued "" — checkpoint → reopen (P0/H1 serialize+load).** A
    /// kill-switched-to-owned trie writes "" via the owned path; the value survives
    /// via P0's `serialize_root`/load fix (the owned file reopens owned).
    #[test]
    fn empty_string_valued_owned_regime_reopen() {
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
        let dir = scratch("byte-es-owned");
        let path = dir.path().join("t.part");
        {
            let trie = PersistentARTrie::<i64>::create(&path).expect("create<i64>");
            trie.kill_switch_to_owned();
            assert!(!trie.route_overlay(), "kill-switched → owned");
            trie.upsert_bytes(b"", 88).expect("owned valued upsert \"\"");
            trie.upsert_bytes(b"k", 9).expect("owned upsert k");
            assert_eq!(trie.get_value_bytes(b""), Some(88));
            trie.checkpoint().expect("owned checkpoint");
        }
        let recovered = PersistentARTrie::<i64>::open(&path).expect("reopen<i64>");
        assert_eq!(
            recovered.get_value_bytes(b""),
            Some(88),
            "owned-regime empty-term value lost across checkpoint → reopen (P0/H1)"
        );
        assert_eq!(recovered.get_value_bytes(b"k"), Some(9));
    }

    /// **back-compat — a value-less root reopens with `get_value("")` == None.** A
    /// trie with only non-empty terms (no "" written) has a value-less root; it must
    /// reopen unchanged with the empty term absent (the value-less path is unperturbed).
    #[test]
    fn empty_string_absent_value_less_root_back_compat() {
        let dir = scratch("byte-es-backcompat");
        let path = dir.path().join("t.part");
        {
            let trie = PersistentARTrie::<i64>::create(&path).expect("create<i64>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.insert_cas_with_value_durable(b"alpha", 1).expect("alpha");
            trie.insert_cas_with_value_durable(b"beta", 2).expect("beta");
            trie.checkpoint().expect("overlay checkpoint");
        }
        let recovered = PersistentARTrie::<i64>::open(&path).expect("reopen<i64>");
        assert_eq!(recovered.get_value_bytes(b""), None, "absent \"\" must read None");
        assert!(!recovered.contains_bytes(b""), "absent \"\" must not be a member");
        assert_eq!(recovered.get_value_bytes(b"alpha"), Some(1));
        assert_eq!(recovered.get_value_bytes(b"beta"), Some(2));
    }

    /// **concurrent increment of "" (R1 — the root-value race).** N threads each
    /// increment "" durably; the final count must equal the sum (no lost update —
    /// the fresh-root-CAS RMW rebases on conflict).
    #[test]
    fn empty_string_concurrent_increment_race() {
        use std::sync::Arc;
        let dir = scratch("byte-es-concurrent");
        let path = dir.path().join("t.part");
        let threads = 4;
        let per_thread = 25;
        {
            let trie = PersistentARTrie::<i64>::create(&path).expect("create<i64>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            let trie = Arc::new(trie);
            let mut handles = Vec::with_capacity(threads);
            for _ in 0..threads {
                let t = Arc::clone(&trie);
                handles.push(std::thread::spawn(move || {
                    for _ in 0..per_thread {
                        t.try_increment_cas_durable(b"", 1).expect("concurrent increment \"\"");
                    }
                }));
            }
            for h in handles {
                h.join().expect("join");
            }
            assert_eq!(
                trie.get_value_bytes(b""),
                Some((threads * per_thread) as i64),
                "concurrent \"\" increments lost an update (fresh-root-CAS RMW must not)"
            );
            // All thread clones dropped on join → sole owner; unwrap for &mut checkpoint.
            let trie = Arc::try_unwrap(trie).ok().expect("sole Arc owner after joins");
            trie.checkpoint().expect("overlay checkpoint");
        }
        let recovered = PersistentARTrie::<i64>::open(&path).expect("reopen<i64>");
        assert_eq!(
            recovered.get_value_bytes(b""),
            Some((threads * per_thread) as i64),
            "concurrent \"\" count lost across reopen"
        );
    }

    /// **compaction preserves "" (H8 — owned-mode).** `compact()` rejects under the
    /// overlay, so this kill-switches to owned, writes a valued "" + non-empty terms,
    /// compacts (which rebuilds from the owned iterator — H8 verified it enumerates
    /// ""), and confirms "" + its value survive the rebuild + atomic file replace.
    #[test]
    fn empty_string_survives_compaction() {
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
        let dir = scratch("byte-es-compaction");
        let path = dir.path().join("t.part");
        let mut trie = PersistentARTrie::<i64>::create(&path).expect("create<i64>");
        trie.kill_switch_to_owned();
        assert!(!trie.route_overlay(), "compact() requires owned mode");
        trie.upsert_bytes(b"", 42).expect("owned valued \"\"");
        trie.upsert_bytes(b"alpha", 1).expect("alpha");
        trie.upsert_bytes(b"beta", 2).expect("beta");
        trie.compact(CompactionConfig::default(), |_| {}).expect("compact");
        assert_eq!(trie.get_value_bytes(b""), Some(42), "\"\" value lost in compaction (H8)");
        assert_eq!(trie.get_value_bytes(b"alpha"), Some(1));
        assert_eq!(trie.get_value_bytes(b"beta"), Some(2));
    }

    /// **H7 — empty-term value survives a root bucket→ART split (owned-mode /
    /// WAL-replay-reachable path).** Insert a valued "" then enough distinct terms to
    /// overflow the root bucket (forcing `convert_bucket_to_art`), and confirm "" keeps
    /// its value — `convert_bucket_to_art` now threads `result.final_value` (was the
    /// `value: None` drop the H7 fix closed).
    #[test]
    fn empty_string_value_survives_bucket_to_art_split() {
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
        let dir = scratch("byte-es-split");
        let path = dir.path().join("t.part");
        let trie = PersistentARTrie::<i64>::create(&path).expect("create<i64>");
        trie.kill_switch_to_owned();
        trie.upsert_bytes(b"", 99).expect("owned valued \"\"");
        // Enough distinct terms to overflow the root bucket → ART split.
        for i in 0..256u32 {
            trie.upsert_bytes(format!("k{i:04}").as_bytes(), i as i64 + 1)
                .expect("term");
        }
        assert_eq!(
            trie.get_value_bytes(b""),
            Some(99),
            "\"\" value lost in root bucket→ART split (H7 convert_bucket_to_art)"
        );
        // Spot-check a few non-empty terms survived the split too.
        assert_eq!(trie.get_value_bytes(b"k0000"), Some(1));
        assert_eq!(trie.get_value_bytes(b"k0255"), Some(256));
    }
}
