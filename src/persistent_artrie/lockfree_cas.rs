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
//! The **counter** half is `V = u64`-specific (byte tries persist a `u64`
//! counter, matching char; the lock-free n-gram counter accumulates a `u64` count
//! bounded by `LOCKFREE_COUNTER_MAX = u64::MAX`, stored in the overlay leaf as the
//! trie's own `u64` value). Its increment is a **path-copy CAS** — mirroring char
//! `lockfree_cas.rs::try_increment_cas` (`build_value_path_recursive`): read the
//! leaf's count from the published snapshot, build a new leaf
//! `old.as_final().with_value(new_count)`, path-copy the root→leaf spine,
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
use crate::persistent_artrie_core::counter_codec;
use crate::persistent_artrie_core::key_encoding::ByteKey;
use crate::persistent_artrie_core::overlay::durable_write::DurableOverlayWrite;
// Phase 5 (byte fault-in): the read-path single-level fault-in walk `find_leaf_faulting`
// is the K-generic default of `OverlayEvictable<ByteKey, V, S>` (lifted in Phase 4).
// Bringing the trait into module scope routes the byte read/counter `self.find_leaf_faulting(..)`
// calls below to the shared default (byte's `OverlayFaulter<ByteKey, V>` loader supplies
// the OnDisk-child load). Behavior mirrors char EXACTLY.
use crate::persistent_artrie_core::overlay::evict::OverlayEvictable;
use crate::value::DictionaryValue;

/// Default bound on read/write fault-in install-CAS retries before falling back to a
/// single read-only walk (the byte twin of char's `DEFAULT_MAX_FAULTIN_RETRIES`;
/// design §3 liveness bound — the byte OE8-twin regression-guards termination).
/// Generous: each retry rebases off a freshly-published root, so contention is the
/// only reason to loop, and the fallback is correct (durable) anyway.
pub(crate) const DEFAULT_MAX_FAULTIN_RETRIES: usize = 16;

// The byte counter is now a full `u64` (matching char). Overflow is detected by
// the i128-domain range check in `counter_codec` (`i128_to_counter_leaf::<u64>`
// rejects `> u64::MAX`) plus `checked_add` on the running `u64` sum — the prior
// `i64::MAX` cap (and the now-vacuous `delta > MAX` / `v <= MAX` u64 tautologies)
// are gone. The const is retained as the documented counter-domain ceiling (referred
// to by the surrounding docs); `counter_codec` is the live enforcer, so the value is
// no longer read in code.
#[allow(dead_code)]
const LOCKFREE_COUNTER_MAX: u64 = u64::MAX;

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
/// counter trie (`V=u64`) carries the valued leaf back to the caller.
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
    /// trie.install_overlay();
    /// trie.insert_cas(b"hello");  // Now works concurrently
    /// ```
    pub(crate) fn install_overlay(&mut self) {
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

        // S4 / M2d (byte twin of char's `install_overlay` regime stamp): stamp the
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
        // REVERSIBLE/opt-in: every `install_overlay` caller is opt-in/test today (no
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
                    log::warn!("install_overlay: could not stamp Overlay regime: {:?}", e);
                }
            }
        }
    }

    /// **F5 — install a PRE-BUILT overlay root** (the dense→overlay walk-converter's
    /// output) as the live lock-free overlay, instead of [`Self::install_overlay`]'s
    /// EMPTY root (the byte twin of char's `install_prebuilt_overlay_root_inherent`).
    /// Sets `lockfree_root = Some(AtomicNodePtr::new(root))` + a fresh empty lookup
    /// cache. Idempotent (only installs if NOT already enabled). Does NOT stamp the WAL
    /// regime (the generic [`LockFreeOverlay::install_prebuilt_overlay_root`] does that
    /// AFTER this seam) and does NOT touch the owned tree (F5 adds ALONGSIDE). NO new
    /// `unsafe`.
    pub(crate) fn install_prebuilt_overlay_root_inherent(
        &mut self,
        root: Arc<super::nodes::persistent_node::PersistentNode<V>>,
    ) {
        use super::nodes::atomic_ptr::AtomicNodePtr;
        use dashmap::DashMap;
        if self.lockfree_root.is_some() {
            return; // Already enabled — never clobber a live overlay.
        }
        self.lockfree_root = Some(AtomicNodePtr::new(root));
        self.lockfree_cache = Some(DashMap::new());
    }

    /// **F5 — NO-WAL overlay remove of the NON-EMPTY term `term`** (the
    /// `overlay_try_remove_path` seam for the data-loss-critical reopen WAL-tail
    /// applier — byte twin of char's `overlay_remove_no_wal`). Clear membership via the
    /// EXISTING single-arbiter [`Self::try_remove_lockfree_path`] (path-copy + root
    /// CAS) in a bounded-retry loop, and invalidate the positive cache. NO WAL, NO
    /// commit-rank, NO watermark — the Remove is ALREADY durable in the WAL being
    /// replayed. NEVER called with an empty slice (the generic `overlay_remove` handles
    /// "" via the root publisher). Byte's `try_remove_lockfree_path` has no fault-in
    /// I/O arm, so the loop only retries on `Conflict`.
    pub(crate) fn overlay_remove_no_wal(&self, term: &[u8]) {
        use std::sync::atomic::Ordering;
        debug_assert!(
            !term.is_empty(),
            "overlay_remove_no_wal: empty term handled by root publisher"
        );
        let lockfree_root = match self.lockfree_root.as_ref() {
            Some(r) => r,
            None => return,
        };
        let _epoch = self.epoch_manager.enter_read();
        loop {
            match self.try_remove_lockfree_path(lockfree_root, term) {
                LockfreeRemoveResult::Removed | LockfreeRemoveResult::AlreadyAbsent => {
                    if let Some(ref cache) = self.lockfree_cache {
                        cache.remove(term);
                    }
                    return;
                }
                LockfreeRemoveResult::Conflict => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    // **F7 — `reestablish_overlay_dispatch` DELETED.** The per-term owned→overlay
    // reestablish dispatch (membership/counter/value folds) is superseded by the KEPT
    // structural converter `LockFreeOverlay::reestablish_overlay_from_owned`
    // (`build_overlay_root_from_owned`), which the F7 reopen converter + the legacy-loader
    // oracle + compaction now use. Same overlay, strictly more correct (keeps a term-only
    // counter member the counter fold dropped).

    /// Lock-free insert using CAS operations.
    ///
    /// This method inserts a term into the lock-free trie structure without
    /// acquiring any locks. Multiple threads can call this concurrently.
    pub(crate) fn insert_cas(&self, term: &[u8]) -> bool {
        use std::sync::atomic::Ordering;

        let lockfree_root = self
            .lockfree_root
            .as_ref()
            .expect("Lock-free mode not enabled. Call install_overlay() first.");
        let lockfree_cache = self
            .lockfree_cache
            .as_ref()
            .expect("Lock-free mode not enabled. Call install_overlay() first.");

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

        // **G5.3' P1 (FIX 2):** resolve this spine edge through the shared
        // [`cas_walk::resolve_or_fault`] in [`FaultMode::Fault`], then map the RICH
        // [`ChildResolution`] to byte NON-DURABLE-INSERT's per-cell behavior (see the
        // mapping table in `cas_walk::resolve_or_fault`):
        //   InMem / Faulted → DESCEND (path-copy; a faulted OnDisk prefix splices
        //     `Child::InMem` so the single root CAS stays the SOLE arbiter — no new
        //     commit point);
        //   FaultFailed / Null → `Err(())` ⇒ `LockfreeInsertResult::AlreadyExists`,
        //     which `insert_cas` treats as TERMINAL (caches, returns false) — NOT a
        //     retry. This is the PRESERVED byte non-durable-insert mapping (turning it
        //     into a retry would LIVELOCK on a permanently-failing fault);
        //   Absent → build the remaining spine bottom-up (`create_lockfree_path`).
        use crate::persistent_artrie_core::overlay::cas_walk::{
            resolve_or_fault, ChildResolution, FaultMode,
        };
        match resolve_or_fault::<ByteKey, V, _>(node, key, FaultMode::Fault, |p| {
            self.load_overlay_node_from_disk(p)
        }) {
            ChildResolution::InMem(child_arc) | ChildResolution::Faulted(child_arc) => {
                let (new_child, leaf) = self.build_path_recursive(&child_arc, term, depth + 1)?;
                let new_node = Arc::new(node.with_child(key, Child::InMem(new_child)));
                Ok((new_node, leaf))
            }
            // TERMINAL `AlreadyExists` (PRESERVED — not a retry; else livelock).
            ChildResolution::FaultFailed(_) | ChildResolution::Null => Err(()),
            ChildResolution::Absent => {
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
    ///
    /// **G5.3' P1:** a thin shim over the shared generic [`cas_walk::create_spine`]
    /// (SAME reverse-iteration bottom-up build order — format-preserving) with a
    /// NON-final leaf-maker (the non-durable path: the caller's `try_set_final`
    /// arbitrates). `&self` is no longer read (spine building needs no trie state).
    fn create_lockfree_path(
        &self,
        term: &[u8],
    ) -> (
        Arc<super::nodes::PersistentNode<V>>,
        Arc<super::nodes::PersistentNode<V>>,
    ) {
        use super::nodes::persistent_node::PersistentNode;
        crate::persistent_artrie_core::overlay::cas_walk::create_spine::<ByteKey, V, _>(
            term,
            || Arc::new(PersistentNode::<V>::new()),
        )
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
            // READ-PATH FAULT-IN (design §3.2, byte twin of char's `contains_lockfree`):
            // route through `find_leaf_faulting` so a term under an EVICTED (OnDisk)
            // prefix is faulted back and reported present instead of spuriously absent
            // (the silent read-loss the design closes). On an I/O error fall back to the
            // non-faulting `find_in_lockfree_trie` (best-effort; liveness-only). `term`
            // is already `&[u8]` (= `&[ByteKey::Unit]`); no key conversion needed.
            match self.find_leaf_faulting(root, term, DEFAULT_MAX_FAULTIN_RETRIES) {
                Ok(found) => return found.is_some(),
                Err(_) => {
                    if let Some(root_node) = root.load() {
                        return Self::find_in_lockfree_trie(&root_node, term, 0);
                    }
                    return false;
                }
            }
        }

        false
    }

    /// Navigate the lock-free trie to find a term.
    ///
    /// **G5.3' P1:** a thin shim over the shared generic
    /// [`cas_walk::find_in_lockfree_trie`] (`PersistentNode<V>` IS
    /// `OverlayNode<ByteKey, V>`, a type alias, so the delegation is type-identical
    /// and behavior-identical). `&self` is no longer read (in-memory-only walk).
    fn find_in_lockfree_trie(
        node: &Arc<super::nodes::PersistentNode<V>>,
        term: &[u8],
        depth: usize,
    ) -> bool {
        crate::persistent_artrie_core::overlay::cas_walk::find_in_lockfree_trie::<ByteKey, V>(
            node, term, depth,
        )
    }

    /// Get the number of CAS retries (for monitoring contention).
    #[inline]
    pub fn cas_retry_count(&self) -> u64 {
        self.cas_retries.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Find the leaf node for a key in the lock-free trie.
    ///
    /// Generic helper shared by the membership block and the `<u64>` counter
    /// block (its calls resolve at `V = u64` — same code, different impl).
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
    ///
    /// **G5.3' P1:** a thin shim over the shared generic
    /// [`cas_walk::find_leaf_recursive`]. The `pub(crate)` NAME + the `&self`
    /// receiver are PRESERVED (the value seams call it as `self.find_leaf_recursive`);
    /// `&self` is no longer read. Behavior-identical (`PersistentNode<V>` IS
    /// `OverlayNode<ByteKey, V>`, a type alias).
    pub(crate) fn find_leaf_recursive(
        &self,
        node: &Arc<super::nodes::PersistentNode<V>>,
        key: &[u8],
        depth: usize,
    ) -> Option<Arc<super::nodes::PersistentNode<V>>> {
        crate::persistent_artrie_core::overlay::cas_walk::find_leaf_recursive::<ByteKey, V>(
            node, key, depth,
        )
    }

    // ====================================================================
    // M2b — Order-A DURABLE membership write path (the byte twin of char's
    // insert_cas_durable / remove_cas_durable). SEPARATE from the non-durable
    // `insert_cas` above (which stays byte-identical to the M2a baseline — the
    // two-phase `try_set_final` arbiter is untouched). The durable path is
    // SINGLE-PHASE: the leaf is published FINAL inside the root CAS, so the root
    // CAS is the SOLE linearization point and the claimed `commit_seq` generation
    // == visibility order. Opt-in (`install_overlay` + a synchronous policy); NOT
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
    /// Requires `install_overlay()` and a synchronous durability policy
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
                "Lock-free mode not enabled. Call install_overlay() first.".to_string(),
            )
        })?;
        let lockfree_cache = self.lockfree_cache.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call install_overlay() first.".to_string(),
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
            use crate::persistent_artrie_core::overlay::flip::{
                LockFreeOverlay, RootPublishOutcome,
            };
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
            match self.publish_root_cas_ranked(|r| Arc::new(r.as_final()), |r| r.is_final())? {
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

        // Present-hoist — DELIBERATELY NON-FAULTING (`find_leaf_lockfree` walks only the
        // in-memory overlay). Phase 5 added byte fault-in (`find_leaf_faulting`) to the
        // read/write/counter paths, but this present-hoist MUST stay non-faulting: a
        // faulting read BEFORE the WAL append, racing a checkpoint/eviction holding the
        // arena/buffer lock, is char's documented "75-minute hang" lock-ordering
        // inversion (see `find_leaf_faulting`'s doc + memory
        // `feedback_production-deadlock-is-costly`). A false-absent here only skips a
        // no-op-insert fast path (the term-under-an-evicted-prefix case still inserts
        // correctly below via the write-path fault-in + root CAS), so it never loses a
        // write. If the term is already present IN MEMORY this is a no-op insert: return
        // WITHOUT appending, so it contributes NO record to replay (the idempotent arm
        // NO-RANKs, so a record left here would be an unranked orphan dropped under the
        // Overlay regime).
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

        // Step 2 + 3: the visibility CAS loop (publishing a FINAL leaf inside the
        // root CAS — single-phase, the SOLE LP, NO `try_set_final`) + the Order-A
        // commit-rank/watermark tail are now the SHARED GENERIC
        // [`OverlayCasWalk::drive_insert_cas`] (G5.3' P5), byte-identical to char's
        // routing. FIX 1: the driver claims the generation PER ITERATION via
        // `claim_generation` (the durable global `commit_seq`, NEVER the walk's
        // `root.version()`; the byte `try_insert_path_attempt` hook DROPS the
        // per-attempt `LockfreeDurableInsertResult::Inserted(version)` at the
        // `InsertAttempt` boundary), caches the term present on both arms, and binds
        // the caller-claimed generation. The read epoch (entered above for the
        // non-faulting present-hoist) spans the call. byte's durable insert has no
        // `IoError` arm (fault-fail → Conflict). REC 3: the NON-durable `insert_cas`
        // two-phase `try_set_final` arbiter is a SEPARATE method (NOT routed here).
        <Self as crate::persistent_artrie_core::overlay::cas_walk::OverlayCasWalk<ByteKey, V, S>>::drive_insert_cas(
            self, term, lsn,
        )
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
        <Self as DurableOverlayWrite<ByteKey, V, S>>::durable_policy_gate(
            self,
            "remove_cas_durable",
            "remove",
        )?;

        let lockfree_root = self.lockfree_root.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call install_overlay() first.".to_string(),
            )
        })?;
        let lockfree_cache = self.lockfree_cache.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call install_overlay() first.".to_string(),
            )
        })?;

        if term.is_empty() {
            // Empty-string support (H4): "" is the root. Order-A durable remove via the
            // fresh-root-CAS RANKED un-publisher (`as_non_final` on a FRESH root, NOT an
            // in-place clear of the shared root — last-writer-wins with concurrent
            // inserts via the single root CAS, like every non-empty remove).
            use crate::persistent_artrie_core::overlay::flip::{
                LockFreeOverlay, RootPublishOutcome,
            };
            let _epoch = self.epoch_manager.enter_read();
            // Absent fast-path (pre-WAL, no LSN burn): root not final ⇒ nothing to remove.
            if !self.overlay_root_node().map_or(false, |r| r.is_final()) {
                lockfree_cache.remove(term);
                return Ok(false);
            }
            // ORDER A — step 1: append + sync the Remove{""} record DURABLE.
            let lsn = self.append_to_wal_returning_lsn(WalRecord::Remove { term: Vec::new() })?;
            // Step 2: fresh-root-CAS un-publish (`as_non_final`), RANKED.
            match self.publish_root_cas_ranked(|r| Arc::new(r.as_non_final()), |r| !r.is_final())? {
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

        // Step 2 + 3: the visibility CAS loop + the Order-A commit-rank/watermark
        // tail are now the SHARED GENERIC [`OverlayCasWalk::drive_remove_cas`]
        // (G5.3' P4), byte-identical to char's routing. FIX 1: the driver claims the
        // generation PER ITERATION via `claim_generation` (the durable global
        // `commit_seq`, NEVER the walk's `root.version()`; byte's `LockfreeRemoveResult`
        // never carried a version anyway), invalidates the positive cache FIRST on
        // every state-changing arm, and binds the caller-claimed generation. The read
        // epoch (entered above for the absent fast-path) spans the call. byte's
        // overlay remove has no `IoError` arm (fault-fail → Conflict).
        <Self as crate::persistent_artrie_core::overlay::cas_walk::OverlayCasWalk<ByteKey, V, S>>::drive_remove_cas(
            self, term, lsn,
        )
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
        // **G5.3' P1 (FIX 2):** resolve through the shared
        // [`cas_walk::resolve_or_fault`], then map to byte DURABLE-INSERT's per-cell
        // behavior (see the mapping table):
        //   InMem / Faulted → DESCEND (fault-in splices `Child::InMem` so the single
        //     root CAS in `try_insert_lockfree_path_durable` stays the SOLE arbiter);
        //   FaultFailed / Null → `Conflict` (transient → the caller retries from a
        //     fresh root load — DURABLE insert, NOT the non-durable terminal arm);
        //   Absent → build the remaining FINAL spine (`create_lockfree_path_final`).
        use crate::persistent_artrie_core::overlay::cas_walk::{
            resolve_or_fault, ChildResolution, FaultMode,
        };
        match resolve_or_fault::<ByteKey, V, _>(node, key, FaultMode::Fault, |p| {
            self.load_overlay_node_from_disk(p)
        }) {
            ChildResolution::InMem(child_arc) | ChildResolution::Faulted(child_arc) => {
                let new_child = self.build_final_path_recursive(&child_arc, term, depth + 1)?;
                Ok(Arc::new(node.with_child(key, Child::InMem(new_child))))
            }
            // Transient conflict → retry from a fresh root load (NOT terminal).
            ChildResolution::FaultFailed(_) | ChildResolution::Null => {
                Err(DurableBuildError::Conflict)
            }
            ChildResolution::Absent => {
                let (new_subtree, _leaf) = self.create_lockfree_path_final(&term[depth + 1..]);
                Ok(Arc::new(node.with_child(key, Child::InMem(new_subtree))))
            }
        }
    }

    /// Build a new path for the remaining bytes with the leaf published FINAL
    /// (single-phase durable path). The byte twin of char's
    /// `create_lockfree_path(finalize=true)`.
    ///
    /// **G5.3' P1:** a thin shim over the shared generic [`cas_walk::create_spine`]
    /// (SAME reverse-iteration bottom-up build order — format-preserving) with a
    /// FINAL leaf-maker (the durable single-phase path: the leaf is published final
    /// inside the root CAS, the sole LP — NO caller-level `try_set_final`). `&self`
    /// is no longer read.
    fn create_lockfree_path_final(
        &self,
        term: &[u8],
    ) -> (
        Arc<super::nodes::PersistentNode<V>>,
        Arc<super::nodes::PersistentNode<V>>,
    ) {
        use super::nodes::persistent_node::PersistentNode;
        crate::persistent_artrie_core::overlay::cas_walk::create_spine::<ByteKey, V, _>(
            term,
            || Arc::new(PersistentNode::<V>::new().as_final()),
        )
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
        // **G5.3' P1 (FIX 2):** resolve through the shared
        // [`cas_walk::resolve_or_fault`], then map to byte DURABLE-REMOVE's per-cell
        // behavior (see the mapping table — note the ASYMMETRY between FaultFailed
        // and Null/Absent that the rich resolution preserves):
        //   InMem / Faulted → DESCEND (fault-in splices `Child::InMem` so the single
        //     root CAS stays the SOLE arbiter for the 1→0 clear — DATA-CORRECTNESS:
        //     WITHOUT the fault-in, removing a term under an evicted prefix would
        //     SILENTLY DROP the acknowledged remove — a LOST REMOVE);
        //   FaultFailed → `Conflict` (transient → retry; NOT `AlreadyExists`, which
        //     would DROP the remove);
        //   Null / Absent → `AlreadyExists` (= "already absent" — the no-op-spine
        //     outcome; no publication).
        use crate::persistent_artrie_core::overlay::cas_walk::{
            resolve_or_fault, ChildResolution, FaultMode,
        };
        match resolve_or_fault::<ByteKey, V, _>(node, key, FaultMode::Fault, |p| {
            self.load_overlay_node_from_disk(p)
        }) {
            ChildResolution::InMem(child_arc) | ChildResolution::Faulted(child_arc) => {
                let new_child = self.build_remove_path_recursive(&child_arc, term, depth + 1)?;
                Ok(Arc::new(node.with_child(key, Child::InMem(new_child))))
            }
            // Transient conflict → retry (NOT a dropped remove).
            ChildResolution::FaultFailed(_) => Err(DurableBuildError::Conflict),
            // Null filler / missing edge ⇒ absent on this snapshot (no no-op spine).
            ChildResolution::Null | ChildResolution::Absent => {
                Err(DurableBuildError::AlreadyExists)
            }
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
        // **G5.3' P1:** a thin shim over the shared generic
        // [`cas_walk::build_value_spine`] in [`FaultMode::Fault`]. The byte value
        // path DOES fault an evicted OnDisk prefix in (the §3.3/§4 write-path
        // fault-in — WITHOUT it the counter step-4 spun and an evicted counter reset
        // to `0+delta`); an I/O error / null collapses to `None` (the counter inner
        // treats it as a transient conflict + retries), the prior `as_in_mem()? ` /
        // `.ok()? ` semantics. (REC 4 doc-fix: the byte value path is NOT
        // non-faulting — the stale "byte does NOT fault OnDisk children" comment that
        // was here is removed; both variants' value paths now fault, the shared
        // primitive's `FaultMode::Fault`.) The `pub(crate)` NAME + `&self`-syntax call
        // sites are PRESERVED; `PersistentNode<V>` IS `OverlayNode<ByteKey, V>` (a
        // type alias), so descent + bottom-up build order are byte-identical.
        use crate::persistent_artrie_core::overlay::cas_walk::{build_value_spine, FaultMode};
        build_value_spine::<ByteKey, V, _>(node, key, depth, value, FaultMode::Fault, |p| {
            self.load_overlay_node_from_disk(p)
        })
    }
}

// ============================================================================
// G5.3' P4/P5 — byte seam impl of the shared OverlayCasWalk skeleton.
//
// Supplies the four durable hooks the shared `drive_remove_cas` (P4) +
// `drive_insert_cas` (P5) defaults call. `claim_generation` (= `claim_commit_seq`)
// is INHERITED — the FIX-1 generation source (the durable global `commit_seq`, NOT
// the walk's `root.version()`). The hooks DROP `try_insert_lockfree_path_durable`'s
// per-attempt `root.version()` (and byte's durable insert never had a leaf to
// `try_set_final` — single-phase) at the `InsertAttempt`/`RemoveAttempt` boundary.
//
// REC 3: the byte NON-DURABLE `insert_cas` two-phase publish (CAS a non-final spine
// THEN the caller-level `try_set_final` arbiter — `insert_lockfree_recursive` +
// `LockfreeInsertResult::Inserted(leaf)`) is NOT routed here — it keeps its own
// loop (its DESCENT already shares `resolve_or_fault`/`create_spine` from P1). Only
// byte's DURABLE single-phase insert/remove use these hooks.
//
// byte's `try_remove_lockfree_path` has NO `IoError` arm (its
// `build_remove_path_recursive` maps a fault-fail to `Conflict`), and its durable
// `try_insert_lockfree_path_durable` likewise maps fault-fail → `Conflict`, so byte
// never constructs `Remove/InsertAttempt::IoError` — that arm is char-only here.
// ============================================================================
impl<V: DictionaryValue, S: BlockStorage>
    crate::persistent_artrie_core::overlay::cas_walk::OverlayCasWalk<ByteKey, V, S>
    for PersistentARTrie<V, S>
{
    fn try_remove_path_attempt(
        &self,
        key_bytes: &[u8],
    ) -> crate::persistent_artrie_core::overlay::cas_walk::RemoveAttempt {
        use crate::persistent_artrie_core::overlay::cas_walk::RemoveAttempt;
        let lockfree_root = match self.lockfree_root.as_ref() {
            Some(r) => r,
            None => return RemoveAttempt::AlreadyAbsent,
        };
        // byte keys ARE the raw bytes (no decode). ONE single-arbiter path-copy +
        // root CAS. byte's `LockfreeRemoveResult` carries no version (no FIX-1
        // dropping needed) and no IoError arm.
        match self.try_remove_lockfree_path(lockfree_root, key_bytes) {
            LockfreeRemoveResult::Removed => RemoveAttempt::Removed,
            LockfreeRemoveResult::AlreadyAbsent => RemoveAttempt::AlreadyAbsent,
            LockfreeRemoveResult::Conflict => RemoveAttempt::Conflict,
        }
    }

    fn invalidate_positive_cache(&self, key_bytes: &[u8]) {
        if let Some(ref cache) = self.lockfree_cache {
            // byte's positive cache is keyed by the raw byte term (identity).
            cache.remove(key_bytes);
        }
    }

    fn try_insert_path_attempt(
        &self,
        key_bytes: &[u8],
    ) -> crate::persistent_artrie_core::overlay::cas_walk::InsertAttempt {
        use crate::persistent_artrie_core::overlay::cas_walk::InsertAttempt;
        let lockfree_root = match self.lockfree_root.as_ref() {
            Some(r) => r,
            // Defensive (the durable caller enable-checks); never reached.
            None => return InsertAttempt::Conflict,
        };
        // DURABLE single-phase: a FRESH FINAL leaf published inside the root CAS (the
        // SOLE LP — REC 3, no caller-level `try_set_final`). FIX 1: the
        // `Inserted(_root_generation)` per-attempt version is DROPPED here (the
        // skeleton ranks the caller-claimed `commit_seq`). byte's durable insert has
        // no IoError arm (fault-fail → Conflict).
        match self.try_insert_lockfree_path_durable(lockfree_root, key_bytes) {
            LockfreeDurableInsertResult::Inserted(_root_generation) => InsertAttempt::Inserted,
            LockfreeDurableInsertResult::AlreadyExists => InsertAttempt::AlreadyExists,
            LockfreeDurableInsertResult::Conflict => InsertAttempt::Conflict,
        }
    }

    fn mark_positive_cache(&self, key_bytes: &[u8]) {
        if let Some(ref cache) = self.lockfree_cache {
            cache.insert(key_bytes.to_vec(), true);
        }
    }
}

// ============================================================================
// Counter (valued) overlay methods — `V = u64` ONLY.
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
// Byte tries now persist a full `u64` counter (the u64 restoration — matching
// char), so the lock-free counter overlay lives in a `V = u64` impl block: the
// overlay leaf stores the running count as the trie's own `u64` value, the
// increment accumulates a `u64` count bounded by `LOCKFREE_COUNTER_MAX =
// u64::MAX`, and the public API exposes `u64`. Every counter-leaf read/write
// routes through `counter_codec` (i128 substrate, range-checked) so an increment
// above `i64::MAX` is neither spuriously rejected nor silently wrapped. The
// generic membership block above remains `<V>` and its proven `try_set_final`
// two-phase finalization is untouched. Cross-block calls to the generic helpers
// (`find_leaf_lockfree`, `find_leaf_recursive`, `try_insert_lockfree_path`)
// resolve at `V = u64` — same code, different impl.
impl<S: BlockStorage> PersistentARTrie<u64, S> {
    /// Lock-free read of a value from the lock-free trie overlay.
    ///
    /// Returns the accumulated count if the key is present in the lock-free layer
    /// with a value set. Does not check the persistent layer — callers should
    /// check both layers and sum for n-gram counting. The leaf stores the count
    /// as the trie's own `u64` value, so it is returned directly (no conversion).
    #[inline]
    pub fn get_lockfree(&self, key: &[u8]) -> Option<u64> {
        let lockfree_root = self.lockfree_root.as_ref()?;
        let _epoch = self.epoch_manager.enter_read();

        // READ-PATH FAULT-IN (design §3.2, byte twin of char's `get_lockfree`): fault
        // an evicted (OnDisk) prefix back in so the value is the durable value, not a
        // spurious `None` (the silent counter-reset bug the design closes). On an I/O
        // error fall through to the non-faulting walk below (best-effort).
        match self.find_leaf_faulting(lockfree_root, key, DEFAULT_MAX_FAULTIN_RETRIES) {
            Ok(found) => return found.and_then(|leaf| leaf.get_value()),
            Err(_) => {}
        }

        self.find_leaf_lockfree(lockfree_root, key)
            .and_then(|leaf| leaf.get_value())
    }

    /// Checked lock-free increment: create path if needed, then add `delta`.
    ///
    /// **G4 path-copy CAS** (the wait-free in-place `fetch_add` is gone — the
    /// node's value is now an immutable `Option<u64>`). Each attempt:
    ///   1. loads the overlay root (a published, immutable snapshot);
    ///   2. reads the current count `cur` at `key` (0 if the leaf is absent or
    ///      has no value) as a `u64`, then computes `cur + delta` in the
    ///      `counter_codec` **i128** substrate (range-checked into `[0, u64::MAX]`),
    ///      so an increment above `i64::MAX` is neither spuriously rejected nor
    ///      silently wrapped;
    ///   3. builds the new leaf `old_leaf.as_final().with_value(cur + delta)` and
    ///      path-copies the root→leaf spine splicing in that leaf;
    ///   4. CAS-publishes the new root via `lockfree_root.compare_exchange`.
    /// On CAS failure another writer published a newer root, so we bump
    /// `cas_retries` and retry — re-reading the (now higher) count, so **no
    /// increment is lost** (the loser folds its delta onto the winner's value).
    ///
    /// Mirrors char `lockfree_cas.rs::try_increment_cas` modulo `&str`→`&[u8]`
    /// (no decode needed for byte keys); the leaf value type is `u64` for both.
    /// The root CAS is the single linearization point, formally checked by the
    /// char loom race test.
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
            .expect("Lock-free mode not enabled. Call install_overlay() first.");

        // Empty-string support (H4): the empty key "" IS the root; the loop below
        // reads the root counter via `find_leaf_recursive(root, b"", 0)` (returns the
        // root iff final → its value, else 0) and republishes via
        // `build_value_path_recursive(root, b"", 0, ..)` which at depth 0 produces a
        // FRESH `as_final().with_value` root (fresh-root-CAS, NOT in-place) — so the
        // root counter RMW is the depth-0 case of the general loop. No rejection.
        // (The former `delta > LOCKFREE_COUNTER_MAX` early-return is gone — vacuous on
        // u64; a true `cur + delta` overflow past u64::MAX is caught below by the
        // i128-domain range check in `counter_codec`.)

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
                    let new_root = Arc::new(PersistentNode::<u64>::new());
                    let _ = lockfree_root.try_init(new_root);
                    continue;
                }
            };

            // (2) Read the current count at `key`. COUNTER BOTH-HALVES, READ HALF
            // (design §3.3, byte twin of char's `try_increment_cas_inner` step-2):
            // route through `find_leaf_faulting` so a term under an EVICTED (OnDisk)
            // prefix faults its durable value back in — WITHOUT this, an evicted
            // counter silently reads 0 and RESETS to `0 + delta` (the data-loss bug).
            // The fault-in may publish a newer root; the path-copy CAS below is against
            // THIS snapshot `root`, so a fault that advanced the root simply makes that
            // CAS lose → we retry from the now-faulted root (this is also the read half
            // of the write-path OnDisk fix — step 4's `build_value_path_recursive` faults
            // the spine in). On an I/O error reading the durable image, fall back to this
            // snapshot (non-faulting). The leaf stores the running count as the trie's
            // own `u64` value.
            let cur = match self.find_leaf_faulting(lockfree_root, key, DEFAULT_MAX_FAULTIN_RETRIES)
            {
                Ok(found) => found.and_then(|leaf| leaf.get_value()).unwrap_or(0u64),
                Err(_) => self
                    .find_leaf_recursive(&root, key, 0)
                    .and_then(|leaf| leaf.get_value())
                    .unwrap_or(0u64),
            };

            // (3) Compute `cur + delta` in the i128 substrate, range-checked into
            // `[0, u64::MAX]` — an increment above `i64::MAX` is honored, and a true
            // u64 overflow is rejected LOUD (never silently wrapped). `delta` widens
            // losslessly to i128; `cur as i128` is exact.
            let new_val =
                match counter_codec::i128_to_counter_value::<u64>(cur as i128 + delta as i128) {
                    Some(v) => v,
                    None => {
                        return Err(Self::lockfree_increment_overflow_error(
                            key,
                            Some(cur),
                            delta,
                        ))
                    }
                };

            // (4) Build a new root with the value-carrying `u64` leaf spliced in.
            let new_root = match self.build_value_path_recursive(&root, key, 0, new_val) {
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

    /// Lock-free increment: create path if needed, then atomically add delta.
    ///
    /// Panics if the checked counter domain would be exceeded (a true u64 overflow).
    /// Use [`Self::try_increment_cas`] to handle overflow as a recoverable error.
    pub fn increment_cas(&self, key: &[u8], delta: u64) -> u64 {
        self.try_increment_cas(key, delta)
            .unwrap_or_else(|error| panic!("lock-free increment_cas failed: {}", error))
    }

    /// **M2b — Order-A durable** lock-free increment (`V = u64`), the byte twin of
    /// char's `try_increment_cas_durable`. Establishes `visible ⊆ durable-prefix`
    /// for a counter delta: the delta `BatchIncrement` WAL record is appended AND
    /// synced DURABLE BEFORE the visibility-publishing root CAS, and the committed
    /// watermark advances only after the CAS lands. A crash loses no acknowledged
    /// increment — the durable delta replays (deltas are commutative, so recovery
    /// SUMS them regardless of commit order); an un-acknowledged one was never
    /// durable. The visibility step REUSES the proven path-copy
    /// [`Self::try_increment_cas_inner`] verbatim.
    ///
    /// `delta` is `u64` (the byte overlay counter domain is now a full `u64`,
    /// matching char — the C4 [`DurableOverlayWrite::bound_increment_delta`] seam
    /// chunks a delta above `i64::MAX` into commutative WAL deltas rather than
    /// rejecting it). Requires `install_overlay()` and a synchronous durability
    /// policy (`Immediate`/`GroupCommit`). Returns the new accumulated count.
    ///
    /// Thin wrapper over the SHARED GENERIC Order-A increment template
    /// [`DurableOverlayWrite::try_increment_cas_durable_default`] — the default owns
    /// the data-loss-critical skeleton (gate, empty short-circuit, the C4
    /// value-domain bound via the seam, the append→publish→commit-rank→mark ORDER);
    /// this wrapper supplies only the key-byte boundary + the empty-key return value.
    pub fn try_increment_cas_durable(&self, key: &[u8], delta: u64) -> Result<u64> {
        // The durable increment operates on UTF-8 keys (so the `&str` the trait
        // default threads to `bound_increment_delta` / `increment_publish_inner`
        // round-trips losslessly to `key` via `as_bytes`). Reject non-UTF-8 loudly
        // rather than lossily.
        let key_str = std::str::from_utf8(key).map_err(|_| {
            PersistentARTrieError::InvalidOperation(
                "try_increment_cas_durable requires a UTF-8 key on the byte durable \
                 increment path"
                    .to_string(),
            )
        })?;
        <Self as DurableOverlayWrite<ByteKey, u64, S>>::try_increment_cas_durable_default(
            self, key_str, key, delta,
        )
    }

    /// **M2b — Order-A durable VALUED insert** (`V = u64`), the byte twin of char's
    /// `insert_cas_with_value_durable`. The valued analogue of
    /// [`Self::insert_cas_durable`] (membership only): bakes a `u64` value into the
    /// leaf via [`Self::build_value_path_recursive`] (single-phase — finality + value
    /// publish atomically with the root CAS).
    ///
    /// **Insert semantics (NOT upsert):** if the term is already present this is a
    /// no-op returning `Ok(false)` with NO WAL record (the value is NOT overwritten).
    ///
    /// Order-A: the `Insert{value}` WAL record is appended+synced DURABLE before the
    /// visibility CAS; the watermark advances only after the CAS lands (+ the
    /// CommitRank). Requires a synchronous durability policy and `install_overlay()`.
    /// Returns `Ok(true)` iff this call newly inserted the term. The whole `u64`
    /// range is representable (the value is published via the path-copy value seam,
    /// not a delta-based i64 WAL record), so there is no value-domain reject — a
    /// `value` up to `u64::MAX` round-trips through the leaf.
    pub fn insert_cas_with_value_durable(&self, term: &[u8], value: u64) -> Result<bool> {
        // **Flip F0 (G5): thin caller of the SHARED GENERIC value-write default**
        // (gate → present-hoist → append `Insert` DURABLE → value seam publish
        // (insert-once) → rank-or-burn), shared verbatim with the arbitrary-`V`
        // path. The former `value < 0` C4 guard is gone (vacuous on `u64`); the full
        // u64 leaf is range-checked by `counter_codec` on the publish path. Empty
        // `""` flows through the value seam's RANKED depth-0 publish (no special case).
        <Self as DurableOverlayWrite<ByteKey, u64, S>>::insert_cas_with_value_durable_default(
            self, term, value,
        )
    }

    /// **M2b — Order-A durable UPSERT** (`V = u64`), the byte twin of char's
    /// `upsert_cas_durable`. Like [`Self::insert_cas_with_value_durable`] but UPSERT:
    /// the value is ALWAYS written (last-writer-wins = the root-CAS winner), whether
    /// or not the term already existed. Returns `Ok(true)` iff the term was newly
    /// inserted (`false` = updated an existing term). The full `u64` range is
    /// representable (value-seam publish, not an i64 delta).
    pub fn upsert_cas_durable(&self, term: &[u8], value: u64) -> Result<bool> {
        // **Flip F0 (G5): thin caller of the SHARED GENERIC value-write default**
        // (gate → advisory existed-probe → append `Upsert` DURABLE → value seam
        // publish in Upsert (always-write) mode → rank). The former `value < 0` C4
        // guard is gone (vacuous on `u64`). Empty `""` flows through the value seam's
        // RANKED depth-0 publish (no special case).
        <Self as DurableOverlayWrite<ByteKey, u64, S>>::upsert_cas_durable_default(
            self, term, value,
        )
    }

    // L3.3c: removed — `prepare_lockfree_value_merge` / `current_i128_for_lockfree_merge`
    // / `collect_lockfree_entries_recursive` were the owned lock-free→persistent merge
    // helpers (`merge_lockfree_values_to_persistent`'s machinery, gone with the owned
    // tree); `current_i128_for_lockfree_merge` read the deleted owned `get_value_impl`.

    fn lockfree_increment_overflow_error(
        key: &[u8],
        current: Option<u64>,
        delta: u64,
    ) -> PersistentARTrieError {
        PersistentARTrieError::InvalidOperation(format!(
            "lock-free increment overflow for term {:?}: current {:?} + {} exceeds u64 counter domain",
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
        trie.install_overlay();
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
    /// Explicitly enables the overlay write mode (`install_overlay` +
    /// `set_overlay_write_mode(LockFreeOverlay)`) so the durable path is exercised as
    /// the M4 flip will use it.
    #[test]
    fn insert_cas_durable_survives_reopen_without_checkpoint() {
        let dir = scratch("byte-order-a-durable");
        let path = dir.path().join("t.part");
        let terms: [&[u8]; 6] = [
            b"apple", b"apricot", b"banana", b"band", b"bandana", b"cherry",
        ];

        {
            let mut trie = PersistentARTrie::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.install_overlay();
            // `inserted_count` tracks committed inserts as a u64 (NOT an `as`-cast of
            // the enumerate index) so this membership test stays free of the forbidden
            // counter-codec gate tokens (the watermark/LSN math is not a counter leaf).
            let mut inserted_count: u64 = 0;
            for t in terms.iter() {
                assert!(
                    trie.insert_cas_durable(t).expect("durable insert"),
                    "{:?} is a new term",
                    String::from_utf8_lossy(t)
                );
                inserted_count += 1;
                // The committed watermark advances to cover each appended LSN (LSNs
                // start at 1; each durable insert burns 2 LSNs — the Insert + its
                // CommitRank — so after N inserts the watermark is ≥ 2*N ≥ N).
                assert!(
                    trie.committed_watermark.watermark() >= inserted_count,
                    "watermark must cover {} committed LSNs, got {}",
                    inserted_count,
                    trie.committed_watermark.watermark()
                );
            }
            // A duplicate returns Ok(false) and does not regress the watermark.
            assert!(!trie
                .insert_cas_durable(b"apple")
                .expect("dup durable insert"));
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
        let plan: [(&[u8], u64, u64); 4] = [
            (b"apple", 3, 1),
            (b"apricot", 2, 10),
            (b"band", 1, 7),
            (b"cherry", 4, 25),
        ];

        {
            let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.install_overlay();
            for (key, steps, delta) in plan {
                let mut last = 0u64;
                for _ in 0..steps {
                    last = trie
                        .try_increment_cas_durable(key, delta)
                        .expect("durable increment");
                }
                assert_eq!(
                    last,
                    steps * delta,
                    "live overlay count for {:?}",
                    String::from_utf8_lossy(key)
                );
            }
            // DROP WITHOUT CHECKPOINT — durability rests entirely on the WAL.
        }

        // Reopen: the summed deltas must replay into the recovered tree.
        let trie = PersistentARTrie::<u64>::open(&path).expect("reopen");
        for (key, steps, delta) in plan {
            let key_str = std::str::from_utf8(key).expect("ascii");
            assert_eq!(
                MappedDictionary::get_value(&trie, key_str),
                Some(steps * delta),
                "durably-incremented {:?} lost/wrong after reopen-without-checkpoint (Order-A increment broken)",
                key_str
            );
        }
        assert_eq!(
            MappedDictionary::get_value(&trie, "never-incremented"),
            None
        );
    }

    /// **THE #41 BYTE WITNESS (valued insert + upsert).** `insert_cas_with_value_durable`
    /// + `upsert_cas_durable` acknowledged writes survive a reopen WITH NO CHECKPOINT.
    #[test]
    fn valued_durable_writes_survive_reopen_without_checkpoint() {
        let dir = scratch("byte-order-a-valued-durable");
        let path = dir.path().join("t.part");
        {
            let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.install_overlay();
            assert!(trie
                .insert_cas_with_value_durable(b"alpha", 11)
                .expect("valued insert"));
            // Insert semantics: a second valued insert of an existing term is a no-op.
            assert!(!trie
                .insert_cas_with_value_durable(b"alpha", 99)
                .expect("dup valued insert"));
            // Upsert ALWAYS writes (last-writer-wins): newly inserts "beta", updates "alpha".
            assert!(trie.upsert_cas_durable(b"beta", 22).expect("upsert new"));
            assert!(!trie
                .upsert_cas_durable(b"alpha", 33)
                .expect("upsert existing"));
            // DROP WITHOUT CHECKPOINT.
        }
        let trie = PersistentARTrie::<u64>::open(&path).expect("reopen");
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
            let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.install_overlay();
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
        let trie = PersistentARTrie::<u64>::open(&path).expect("reopen");
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
            trie.install_overlay();
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
            assert!(!trie
                .remove_cas_durable(b"apple")
                .expect("idempotent remove"));
            assert!(!trie
                .remove_cas_durable(b"never-present")
                .expect("absent remove"));
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

    /// **C4 VALUE-DOMAIN REJECT (the byte u64 counter domain).** After the u64
    /// restoration the byte counter is a full `u64`, so the old "negative i64 value"
    /// reject is reframed to the surviving u64 value-domain rejects, each of which
    /// must FAIL LOUD (not wrap, not panic):
    ///
    /// 1. the durable add-only increment seam (`bound_increment_delta`) rejects a
    ///    SINGLE delta > `i64::MAX` (the WAL increment domain is one i64 chunk per
    ///    durable call — a magnitude above i64::MAX is reachable only via the merge
    ///    chunker / multiple calls, not a single durable delta);
    /// 2. a below-zero DECREMENT (the PUBLIC `increment` takes a signed `i64`; a
    ///    negative delta routes to the value-CAS path, which rejects a result < 0);
    /// 3. a u64 OVERFLOW on the value-CAS path (`u64::MAX + positive`) is rejected.
    #[test]
    fn c4_negative_value_is_rejected_not_wrapped() {
        let dir = scratch("byte-order-a-c4-negative");
        let path = dir.path().join("t.part");
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        trie.install_overlay();

        // (1) A single durable delta above i64::MAX must be rejected by the seam
        // (one i64 WAL chunk cannot carry it), not wrapped. `(u64::MAX / 2) + 1`
        // equals the first u64 that overflows an i64, written WITHOUT a numeric cast
        // to keep the counter-codec gate clean.
        let over_i64_delta: u64 = (u64::MAX / 2) + 1;
        let inc = trie.try_increment_cas_durable(b"big", over_i64_delta);
        assert!(
            inc.is_err(),
            "a single durable delta > i64::MAX must be rejected (C4 i64-WAL-chunk bound), got {:?}",
            inc
        );
        assert_eq!(
            MappedDictionary::get_value(&trie, "big"),
            None,
            "the rejected over-i64 delta left no durable record"
        );

        // (2) A below-zero decrement via the PUBLIC signed `increment` must be
        // rejected (the value-CAS path refuses a result < 0), not wrapped.
        assert_eq!(
            trie.increment_bytes(b"ctr", 5).expect("seed +5"),
            5,
            "seed the counter to 5"
        );
        let dec = trie.increment_bytes(b"ctr", -10);
        assert!(
            dec.is_err(),
            "a decrement below zero must be rejected (no u64 underflow wrap), got {:?}",
            dec
        );
        assert_eq!(
            trie.get_value_bytes(b"ctr"),
            Some(5),
            "the rejected below-zero decrement left the counter unchanged"
        );

        // (3) A u64 overflow on the value-CAS path must be rejected (not wrapped).
        trie.upsert_cas_durable(b"max", u64::MAX)
            .expect("set u64::MAX");
        let over = trie.increment_bytes(b"max", 1);
        assert!(
            over.is_err(),
            "incrementing past u64::MAX must be rejected (no wrap), got {:?}",
            over
        );
        assert_eq!(
            trie.get_value_bytes(b"max"),
            Some(u64::MAX),
            "the rejected u64 overflow left the counter at u64::MAX"
        );

        // A non-negative increment still works (the bound passes 0 and positives).
        assert_eq!(
            trie.try_increment_cas_durable(b"pos", 0)
                .expect("zero delta"),
            0
        );
        assert_eq!(
            trie.try_increment_cas_durable(b"pos", 5)
                .expect("pos delta"),
            5
        );
    }

    /// The durable entry points reject a non-synchronous durability policy (an
    /// acknowledged write can only be guaranteed durable under `Immediate`/`GroupCommit`).
    #[test]
    fn durable_writes_reject_non_synchronous_policy() {
        let dir = scratch("byte-order-a-reject");
        let path = dir.path().join("t.part");
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::None);
        trie.install_overlay();
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
            trie.install_overlay();
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
            trie.install_overlay();
            // Stamps the WAL header regime = Overlay.
            // RANKED survivor: insert_cas_durable appends Insert + CommitRank (acked).
            assert!(
                trie.insert_cas_durable(b"survivor")
                    .expect("durable insert"),
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
            trie.install_overlay();
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
            // NO install_overlay / NO set_overlay_write_mode ⇒ the WAL header regime
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
            let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.install_overlay();
            // 3 ranked +7 deltas ⇒ 21.
            for _ in 0..3 {
                trie.try_increment_cas_durable(b"ctr", 7)
                    .expect("durable increment");
            }
            // DROP WITHOUT CHECKPOINT.
        }
        let recovered = PersistentARTrie::<u64>::open(&path).expect("reopen");
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
    use crate::Dictionary;

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp")
    }

    /// **(a) create → durable VALUED write → reopen, no loss / no double-count.** A
    /// fresh `create::<u64>()` create-flips (`route_overlay()==true`); durable valued
    /// writes + a checkpoint; reopen MUST survive with exact counts via the overlay
    /// (the AUTOMATIC open-flip + reestablish in `open`). The membership (`V=()`) twin
    /// is also covered.
    #[test]
    fn m4b_create_durable_valued_write_reopen_survives() {
        // Counters (V = u64).
        {
            let dir = scratch("byte-m4b-rw-u64");
            let path = dir.path().join("t.part");
            let entries: Vec<(Vec<u8>, u64)> = (0..40u64)
                .map(|i| (format!("k{i:03}").into_bytes(), i + 1))
                .collect();
            {
                let trie = PersistentARTrie::<u64>::create(&path).expect("create<u64>");
                trie.set_durability_policy(DurabilityPolicy::Immediate);
                assert!(trie.route_overlay(), "fresh create<u64> is overlay-routed");
                for (k, d) in &entries {
                    // Durable valued insert (overlay path). Distinct values per key so a
                    // double or drop is detectable.
                    assert!(
                        trie.insert_cas_with_value_durable(k, *d)
                            .expect("durable valued insert"),
                        "first valued insert of {k:?} must be newly inserted"
                    );
                }
                trie.checkpoint().expect("overlay checkpoint (route-split)");
            }
            let recovered = PersistentARTrie::<u64>::open(&path).expect("reopen<u64>");
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
            let terms: Vec<Vec<u8>> = (0..40u32)
                .map(|i| format!("term{i:03}").into_bytes())
                .collect();
            {
                let trie = PersistentARTrie::<()>::create(&path).expect("create<()>");
                trie.set_durability_policy(DurabilityPolicy::Immediate);
                assert!(trie.route_overlay(), "fresh create<()> is overlay-routed");
                for t in &terms {
                    assert!(
                        trie.insert_cas_durable(t)
                            .expect("durable membership insert"),
                        "first durable insert of {t:?} must be newly inserted"
                    );
                }
                trie.checkpoint().expect("overlay checkpoint");
            }
            let recovered = PersistentARTrie::<()>::open(&path).expect("reopen<()>");
            assert!(
                recovered.route_overlay(),
                "() Overlay file reopens overlay-routed"
            );
            for t in &terms {
                assert!(
                    recovered.contains_bytes(t),
                    "membership lost {t:?} across create→write→checkpoint→reopen"
                );
            }
        }
    }

    /// **(c) `compact()` SUCCEEDS under the overlay (F6).** A fresh `create::<u64>()`
    /// flips; `compact()` sources the snapshot from the overlay (enumeration AND
    /// values), rebuilds a dense image via the CX serializer, and reopens overlay-routed
    /// — data and routing survive.
    #[test]
    fn m4b_compact_succeeds_under_overlay() {
        let dir = scratch("byte-m4b-compact-overlay");
        let path = dir.path().join("t.part");
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create<u64>");
        assert!(trie.route_overlay(), "fresh create<u64> is overlay-routed");
        trie.increment_bytes(b"seed", 1).expect("seed");
        trie.upsert_bytes(b"alpha", 1).expect("upsert alpha");
        trie.upsert_bytes(b"beta", 2).expect("upsert beta");
        trie.compact(CompactionConfig::default(), |_| {})
            .expect("F6: compact succeeds under the overlay");
        assert!(
            trie.route_overlay(),
            "F6: compact must PRESERVE the overlay regime (reopen overlay-routed)"
        );
        assert_eq!(
            trie.get_value_bytes(b"seed"),
            Some(1),
            "F6: data preserved across overlay compaction"
        );
        assert_eq!(trie.get_value_bytes(b"alpha"), Some(1));
        assert_eq!(trie.get_value_bytes(b"beta"), Some(2));
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
            assert_eq!(
                trie.overlay_len(),
                N as usize,
                "all N terms resident pre-checkpoint"
            );
            // Checkpoint folds the overlay into the durable image (so the reopen reads
            // them from the image into the owned tree, then reestablishes).
            trie.checkpoint()
                .expect("overlay checkpoint of the 100k partition");
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
        assert!(
            recovered.contains_bytes(format!("a{:05x}", 0u32).as_bytes()),
            "first term lost"
        );
        assert!(
            recovered.contains_bytes(format!("a{:05x}", N / 2).as_bytes()),
            "middle term lost"
        );
        assert!(
            recovered.contains_bytes(format!("a{:05x}", N - 1).as_bytes()),
            "LAST term lost (H2 tail-drop witness)"
        );
    }

    /// **(d′) reestablish-survival for COUNTERS (u64) with exact summed values.** The
    /// value-carrying twin of (d): a moderately-sized flipped u64 overlay, durable
    /// valued writes + checkpoint, reopen → `reestablish_overlay_counter` must reproduce
    /// EVERY (term, count) exactly (the `owned_units_with_values_under` uncapped walk).
    #[test]
    fn m4b_reestablish_survival_counter_values() {
        let dir = scratch("byte-m4b-reestablish-ctr");
        let path = dir.path().join("t.part");
        let entries: Vec<(Vec<u8>, u64)> = (0..500u64)
            .map(|i| (format!("c{i:04}").into_bytes(), (i % 97) + 1))
            .collect();
        {
            let trie = PersistentARTrie::<u64>::create(&path).expect("create<u64>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            assert!(trie.route_overlay());
            for (k, d) in &entries {
                trie.insert_cas_with_value_durable(k, *d)
                    .expect("durable valued insert");
            }
            trie.checkpoint().expect("overlay checkpoint");
        }
        let recovered = PersistentARTrie::<u64>::open(&path).expect("reopen<u64>");
        assert!(
            recovered.route_overlay(),
            "counter Overlay file reopens overlay-routed"
        );
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
            let trie = PersistentARTrie::<u64>::create(&path).expect("create<u64>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            assert!(trie.route_overlay());
            assert!(
                trie.insert_cas_with_value_durable(b"", 42)
                    .expect("valued insert \"\""),
                "valued insert of \"\" must be newly inserted"
            );
            // A couple of non-empty terms so "" coexists with children.
            trie.insert_cas_with_value_durable(b"a", 1).expect("a");
            trie.insert_cas_with_value_durable(b"bc", 2).expect("bc");
            assert_eq!(
                trie.get_value_bytes(b""),
                Some(42),
                "\"\" readable pre-checkpoint"
            );
            trie.checkpoint().expect("overlay checkpoint");
        }
        let recovered = PersistentARTrie::<u64>::open(&path).expect("reopen<u64>");
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
            let trie = PersistentARTrie::<u64>::create(&path).expect("create<u64>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.insert_cas_with_value_durable(b"", 7)
                .expect("valued insert \"\"");
            // NO checkpoint — durability rests on WAL replay.
        }
        let recovered = PersistentARTrie::<u64>::open(&path).expect("reopen<u64>");
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
            assert!(trie
                .insert_cas_durable(b"")
                .expect("membership insert \"\""));
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
            let trie = PersistentARTrie::<u64>::create(&path).expect("create<u64>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            let mut last = 0;
            for _ in 0..5 {
                last = trie
                    .try_increment_cas_durable(b"", 3)
                    .expect("increment \"\"");
            }
            assert_eq!(last, 15, "5×3 increments of \"\" accumulate to 15");
            trie.checkpoint().expect("overlay checkpoint");
        }
        let recovered = PersistentARTrie::<u64>::open(&path).expect("reopen<u64>");
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
            assert!(
                trie.remove_cas_durable(b"").expect("remove \"\""),
                "remove cleared \"\""
            );
            assert!(!trie.contains_bytes(b""), "\"\" absent after remove");
            trie.checkpoint().expect("overlay checkpoint");
        }
        let recovered = PersistentARTrie::<()>::open(&path).expect("reopen<()>");
        assert!(
            !recovered.contains_bytes(b""),
            "empty-term must stay REMOVED across checkpoint → reopen (remove symmetry)"
        );
    }

    /// **back-compat — a value-less root reopens with `get_value("")` == None.** A
    /// trie with only non-empty terms (no "" written) has a value-less root; it must
    /// reopen unchanged with the empty term absent (the value-less path is unperturbed).
    #[test]
    fn empty_string_absent_value_less_root_back_compat() {
        let dir = scratch("byte-es-backcompat");
        let path = dir.path().join("t.part");
        {
            let trie = PersistentARTrie::<u64>::create(&path).expect("create<u64>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.insert_cas_with_value_durable(b"alpha", 1)
                .expect("alpha");
            trie.insert_cas_with_value_durable(b"beta", 2)
                .expect("beta");
            trie.checkpoint().expect("overlay checkpoint");
        }
        let recovered = PersistentARTrie::<u64>::open(&path).expect("reopen<u64>");
        assert_eq!(
            recovered.get_value_bytes(b""),
            None,
            "absent \"\" must read None"
        );
        assert!(
            !recovered.contains_bytes(b""),
            "absent \"\" must not be a member"
        );
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
        let threads: usize = 4;
        let per_thread: usize = 25;
        // Expected final count as the native u64 counter type. Declared with u64
        // literals (NOT a numeric cast of the usize loop bounds) so no numeric cast
        // appears in this file — keeps the v6 counter-codec gate clean.
        let threads_u64: u64 = 4;
        let per_thread_u64: u64 = 25;
        let expected_sum: u64 = threads_u64 * per_thread_u64;
        {
            let trie = PersistentARTrie::<u64>::create(&path).expect("create<u64>");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            let trie = Arc::new(trie);
            let mut handles = Vec::with_capacity(threads);
            for _ in 0..threads {
                let t = Arc::clone(&trie);
                handles.push(std::thread::spawn(move || {
                    for _ in 0..per_thread {
                        t.try_increment_cas_durable(b"", 1)
                            .expect("concurrent increment \"\"");
                    }
                }));
            }
            for h in handles {
                h.join().expect("join");
            }
            assert_eq!(
                trie.get_value_bytes(b""),
                Some(expected_sum),
                "concurrent \"\" increments lost an update (fresh-root-CAS RMW must not)"
            );
            // All thread clones dropped on join → sole owner; unwrap for &mut checkpoint.
            let trie = Arc::try_unwrap(trie)
                .ok()
                .expect("sole Arc owner after joins");
            trie.checkpoint().expect("overlay checkpoint");
        }
        let recovered = PersistentARTrie::<u64>::open(&path).expect("reopen<u64>");
        assert_eq!(
            recovered.get_value_bytes(b""),
            Some(expected_sum),
            "concurrent \"\" count lost across reopen"
        );
    }

    /// **compaction preserves "" (H8).** Writes a valued "" + non-empty terms to the
    /// overlay, compacts (the CX serializer enumerates "" from the overlay snapshot),
    /// and confirms "" + its value survive the rebuild + atomic file replace.
    #[test]
    fn empty_string_survives_compaction() {
        let dir = scratch("byte-es-compaction");
        let path = dir.path().join("t.part");
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create<u64>");
        trie.upsert_bytes(b"", 42).expect("valued \"\"");
        trie.upsert_bytes(b"alpha", 1).expect("alpha");
        trie.upsert_bytes(b"beta", 2).expect("beta");
        trie.compact(CompactionConfig::default(), |_| {})
            .expect("compact");
        assert_eq!(
            trie.get_value_bytes(b""),
            Some(42),
            "\"\" value lost in compaction (H8)"
        );
        assert_eq!(trie.get_value_bytes(b"alpha"), Some(1));
        assert_eq!(trie.get_value_bytes(b"beta"), Some(2));
    }
}
