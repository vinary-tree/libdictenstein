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

use std::sync::Arc;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::WalRecord;
use crate::persistent_artrie_core::durability::DurabilityPolicy;
use crate::value::DictionaryValue;

use super::dict_impl_char::LockfreeInsertResult;

const LOCKFREE_COUNTER_MAX: u64 = i64::MAX as u64;

/// **OD4 deterministic-regression rendezvous (test-only).** The two phases a
/// durable lock-free op crosses between Order-A step 1 (WAL append) and the ack:
/// `AfterAppend` fires right after the data record is durable (LSN fixed) and
/// BEFORE the visibility CAS; `AfterCommit` fires right after the winning CAS and
/// BEFORE the `CommitRank` append + return. A test installs a per-thread closure
/// (see `set_commit_rendezvous`) to deterministically stage the s019 interleaving
/// — both threads append first (fixing WAL/LSN order), then one CAS is forced to
/// land before the other (commit/generation order). Production builds never
/// reference this (every call site is `#[cfg(test)]`).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RendezvousPhase {
    /// Step 1 complete: the data record is durable; the CAS has not run.
    AfterAppend,
    /// Step 2 complete: the visibility CAS won; the `CommitRank` is not yet appended.
    AfterCommit,
}

#[cfg(test)]
thread_local! {
    /// Per-thread rendezvous closure consulted by the durable producers. `None`
    /// (the default) ⇒ the producers behave exactly as in production.
    static COMMIT_RENDEZVOUS: std::cell::RefCell<Option<Box<dyn Fn(RendezvousPhase)>>> =
        const { std::cell::RefCell::new(None) };
}

/// Install (or clear, with `None`) this thread's OD4 commit rendezvous closure.
#[cfg(test)]
pub(crate) fn set_commit_rendezvous(hook: Option<Box<dyn Fn(RendezvousPhase)>>) {
    COMMIT_RENDEZVOUS.with(|h| *h.borrow_mut() = hook);
}

/// Fire this thread's rendezvous closure for `phase`, if one is installed.
#[cfg(test)]
fn commit_rendezvous(phase: RendezvousPhase) {
    COMMIT_RENDEZVOUS.with(|h| {
        if let Some(hook) = h.borrow().as_ref() {
            hook(phase);
        }
    });
}

/// Default bound on read/write fault-in install-CAS retries before falling back to
/// a single read-only walk (design §3 liveness bound; OE8 regression-guards it).
/// Generous: each retry rebases off a freshly-published root, so contention is the
/// only reason to loop, and the fallback is correct (durable) anyway.
///
/// **Flip F0:** un-gated to production. Once the production write path routes
/// through the overlay (`route_overlay()`), evicted overlay nodes must be
/// re-readable/writable on every path, so fault-in is unconditional (the g5
/// design anticipated "the flip CONSUMES this primitive").
pub(crate) const DEFAULT_MAX_FAULTIN_RETRIES: usize = 16;

/// Error outcomes of [`PersistentARTrieChar::build_path_recursive`] (membership
/// write path). Replaces the former bare `()` error so the WRITE-PATH FAULT-IN
/// (design §4) OnDisk arm can carry a buffer-manager I/O error out WITHOUT widening
/// the recursive `Err` at every site (smaller blast radius — the design's choice).
enum BuildPathError {
    /// The term already exists (the target node is already final at full depth).
    /// Maps to [`LockfreeInsertResult::AlreadyExists`].
    AlreadyExists,
    /// **R-B (proven overlay DELETE):** the term is ABSENT on this snapshot — the
    /// remove path reached the full depth and the target node is NOT final, or a
    /// spine edge is missing/null. The remove must NOT publish a no-op spine; the
    /// caller returns `Ok(false)` (LSN still durable, watermark must not stall).
    /// Maps to [`LockfreeRemoveResult::AlreadyAbsent`]. Constructed only by the
    /// remove path; the insert path never produces it.
    AlreadyAbsent,
    /// WRITE-PATH FAULT-IN: an I/O error faulting an `OnDisk` prefix node back in.
    /// Maps to [`LockfreeInsertResult::IoError`]. **Flip F0:** un-gated — fault-in
    /// is now a production path, so this variant is always constructible.
    Io(crate::persistent_artrie::error::PersistentARTrieError),
}

/// Outcome of a single [`PersistentARTrieChar::try_remove_lockfree_path`] attempt
/// (R-B membership-clear path). The dual of [`LockfreeInsertResult`]:
/// a `Removed` clears finality on a fresh leaf published via the root CAS, while
/// `AlreadyAbsent` is the no-op (durable-LSN, no spine published) and `Conflict`
/// re-finds on retry. The new root is installed inside `try_remove_lockfree_path`'s
/// own CAS, so — unlike [`LockfreeInsertResult`] which hands its leaf back for a
/// separate `try_set_final` — these variants carry no node and the enum needs no
/// `V` parameter (the 1→0 clear is fully arbitrated by the root CAS before this
/// result is returned).
enum LockfreeRemoveResult {
    /// The term was present and cleared: a new root with the freshly-cleared
    /// (non-final) leaf was published via the root CAS. Carries the
    /// **published-root version** — the Order-A commit GENERATION (design C′,
    /// §3.6), read from the EXACT root the CAS swapped (NOT a re-walk). The root
    /// version is bumped by the spine path-copy on every publication, so it is
    /// strictly monotone in root-CAS order for both insert and remove (the same
    /// generation source the insert path uses — so an insert and the remove it
    /// clobbers never TIE).
    Removed(u64),
    /// The term is absent on this snapshot (reached full depth non-final, or a
    /// missing/null spine edge). No spine was published. Carries the
    /// **observed-root version** (FIX-A / D2.8): `version()` of the `current_root`
    /// this remove walked (or `0` for the empty/null-root early return). This op
    /// took no root CAS, so its commit generation is the causally-bounded observed
    /// version — `<` any strictly-later same-key insert's published version — keeping
    /// the idempotent record correctly ordered in the same `root.version` domain.
    AlreadyAbsent(u64),
    /// CAS failed due to a concurrent modification — re-find and retry.
    Conflict,
    /// WRITE-PATH FAULT-IN (design §3, R-B): a buffer-manager I/O error faulting
    /// an `OnDisk` prefix node back in. The Remove WAL record is ALREADY durable;
    /// surfaced as `Err(e)` (durable-but-visible-only-after-reopen window). **Flip
    /// F0:** un-gated — fault-in is now a production path.
    IoError(crate::persistent_artrie::error::PersistentARTrieError),
}

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
    /// ```text
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
    /// ```text
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
            // Non-durable: `finalize = false` ⇒ the shared non-final leaf +
            // `try_set_final` arbiter below (UNCHANGED behavior).
            match self.try_insert_lockfree_path(lockfree_root, &chars, false) {
                // The non-durable path does not record a commit generation.
                LockfreeInsertResult::Inserted(node, _gen) => {
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
                LockfreeInsertResult::AlreadyExists(_observed_gen) => {
                    // Term already exists in the trie. Non-durable path: no WAL, no
                    // rank, so the observed generation is unused here.
                    lockfree_cache.insert(term.to_string(), true);
                    return false;
                }
                LockfreeInsertResult::Conflict => {
                    // CAS failed due to concurrent modification - retry
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                // WRITE-PATH FAULT-IN I/O error (design §4): could not load an
                // evicted prefix from the durable image. Non-durable best-effort
                // insert: bump the retry counter and report `false` (not acked).
                // The durable image is intact; a later call can retry. (Flip F0:
                // un-gated — fault-in is a production path.)
                LockfreeInsertResult::IoError(_e) => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    return false;
                }
            }
        }
    }

    /// **Order-A durable** lock-free insert (Migration Phase E).
    ///
    /// Unlike [`Self::insert_cas`] (which bypasses the WAL), this establishes the
    /// durability invariant **`visible ⊆ durable-prefix`**: the WAL record is
    /// appended AND synced durable BEFORE the visibility-publishing root CAS, and
    /// the committed watermark is advanced only once the CAS lands. A crash
    /// therefore loses no acknowledged write — in-WAL replays, not-in-WAL was
    /// never acknowledged. (Order B — CAS-then-log — is rejected: it can expose a
    /// visible-but-not-durable write.) The committed watermark is the only safe
    /// `checkpoint_lsn` under out-of-order lock-free commit; the whole protocol is
    /// TLC-verified in `formal-verification/tla+/LockFreeDurableCheckpoint.tla`.
    ///
    /// Requires `enable_lockfree()` and a synchronous durability policy
    /// (`Immediate`/`GroupCommit`) so that "acknowledged ⇒ durable" holds.
    ///
    /// # ⚠️ Safety boundary (pre-flip)
    ///
    /// This is **WAL-only-safe**: durability rests on WAL replay, so an
    /// acknowledged write survives a crash/reopen with **no checkpoint**. It is
    /// NOT yet safe to mix with the *owned-tree* [`checkpoint()`](Self::checkpoint):
    /// that checkpoint captures the **owned** tree (these writes live in the
    /// lock-free **overlay**, not the owned tree) and rotates the WAL by
    /// `self.next_lsn` — so it could archive an overlay-write record that is not in
    /// the checkpoint, losing it. The clean integration (checkpoint captures the
    /// overlay via `capture_snapshot_immutable`, rotating by the committed
    /// watermark) is the **Phase-E flip** — until then, use this in a WAL-only
    /// configuration (no owned checkpoint between writes and recovery), or via the
    /// overlay-as-default path once the flip lands. Increments are durable via the
    /// existing merge path (`merge_lockfree_to_persistent` logs `BatchIncrement`):
    /// per-op Order-A durable increment does not fit the *result-based* `Increment`
    /// WAL record under lock-free CAS (the logged result can be invalidated by a
    /// concurrent commit), so it is intentionally not provided.
    ///
    /// Returns `Ok(true)` iff this call newly inserted the term.
    pub fn insert_cas_durable(&self, term: &str) -> Result<bool> {
        use std::sync::atomic::Ordering;

        // "Acknowledged ⇒ durable" only holds under a synchronous policy.
        match self.durability_policy {
            DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit => {}
            DurabilityPolicy::Periodic | DurabilityPolicy::None => {
                return Err(PersistentARTrieError::InvalidOperation(
                    "insert_cas_durable requires Immediate or GroupCommit durability so an \
                     acknowledged write is guaranteed durable before it becomes visible"
                        .to_string(),
                ));
            }
        }

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

        let chars: Vec<u32> = term.chars().map(|c| c as u32).collect();
        if chars.is_empty() {
            return Ok(false);
        }

        // ORDER A — step 1: append + sync the WAL record DURABLE, before any
        // visibility. The returned LSN is durable-per-policy at this point.
        let lsn = self.append_to_wal_returning_lsn(WalRecord::Insert {
            term: term.as_bytes().to_vec(),
            value: None,
        })?;

        // OD4 (test-only): the data record is durable; the CAS has not run. A
        // regression test rendezvouses here to fix the WAL/LSN order before any
        // CAS lands.
        #[cfg(test)]
        commit_rendezvous(RendezvousPhase::AfterAppend);

        // Step 2: the existing lock-free CAS publication (the visibility point).
        // The single durable append above covers every CAS retry — we never
        // re-append (that would burn LSNs and punch holes in the watermark).
        let _epoch = self.epoch_manager.enter_read();
        loop {
            // Durable (1a, D2.8 §1.2): `finalize = true` ⇒ the leaf is published
            // FINAL inside the root CAS (the SOLE linearization point), so the root
            // CAS — not a later `try_set_final` — arbitrates. Reaching the
            // `Inserted` arm means OUR root CAS won ⇒ this op newly published the
            // term (a racer loses the CAS, retries, and sees `AlreadyExists`).
            match self.try_insert_lockfree_path(lockfree_root, &chars, true) {
                LockfreeInsertResult::Inserted(_node, root_generation) => {
                    // Leaf is already final (built via `as_final`); no try_set_final.
                    let newly = true;
                    // OD4 (test-only): the visibility CAS has won; the CommitRank
                    // is not yet appended. A test rendezvouses here to order this
                    // commit relative to the other op's commit.
                    #[cfg(test)]
                    commit_rendezvous(RendezvousPhase::AfterCommit);
                    // GENERATION (§3.6): the PUBLISHED-root version captured at THIS
                    // op's visibility CAS (`root_generation`, read from the exact root
                    // the CAS swapped — no re-walk). The root version is bumped by every
                    // publication, so it is strictly monotone in root-CAS order for BOTH
                    // insert and remove. (S2/FIX-A keeps the real arms on root.version,
                    // SAME domain as the idempotent observed-version arm below; the
                    // durable global commit_seq stamp lands at S4 with the Overlay
                    // regime + idempotent NO-RANK.)
                    let generation = root_generation;
                    lockfree_cache.insert(term.to_string(), true);
                    // Step 2.5 (NEW, Order-A-preserving): bind the durable data
                    // record (`lsn`) to its commit generation, durable BEFORE ack.
                    let rank_lsn = self.append_commit_rank(lsn, term.as_bytes(), generation)?;
                    // Step 3: the write is now durable AND visible. Advance the
                    // committed watermark to include BOTH the data LSN and the rank
                    // LSN so the contiguous prefix does not stall on the rank record.
                    self.committed_watermark.mark_committed(lsn);
                    self.committed_watermark.mark_committed(rank_lsn);
                    return Ok(newly);
                }
                LockfreeInsertResult::AlreadyExists(observed_gen) => {
                    // FIX-A idempotent arm: the term is already published-present (a
                    // concurrent op finalized it, or it is present-but-cache-cold) — this
                    // op took NO root CAS, so it has no position in the CAS order. Rank
                    // with the OBSERVED-root version (`observed_gen` = `version()` of the
                    // exact root the walk found the term final in). This is causally
                    // bounded: the observed-present root precedes (`≺`) any strictly-later
                    // same-key remove in the CAS chain, so `observed_gen < remove's
                    // generation`, and the idempotent Insert sorts BEFORE a later Remove
                    // on replay ⇒ no resurrection. SAME `root.version` domain as the real
                    // arms (no cross-domain). Uses the OBSERVED root, NOT a second
                    // `lockfree_root.load()` (which could leapfrog past a later remove —
                    // RT-1's bug). At S4 this arm converts to NO-RANK + non-faulting
                    // present-hoist under the Overlay regime.
                    let generation = observed_gen;
                    // OD4 (test-only): this idempotent commit "decided" here.
                    #[cfg(test)]
                    commit_rendezvous(RendezvousPhase::AfterCommit);
                    lockfree_cache.insert(term.to_string(), true);
                    let rank_lsn = self.append_commit_rank(lsn, term.as_bytes(), generation)?;
                    self.committed_watermark.mark_committed(lsn);
                    self.committed_watermark.mark_committed(rank_lsn);
                    return Ok(false);
                }
                LockfreeInsertResult::Conflict => {
                    // Retry visibility only; the WAL record is already durable.
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                // WRITE-PATH FAULT-IN I/O error (design §4): the WAL record is
                // ALREADY durable (Order-A step 1), but we could not fault the
                // evicted prefix in to make the write VISIBLE. Surface the error.
                // This is the documented Order-A "durable-but-visible-only-after-
                // reopen" window, NOT a lost write: recovery replays the logged
                // Insert. We do NOT advance the committed watermark (the write is
                // not yet visible), so the contiguous prefix correctly stalls at
                // this LSN until a later retry (or recovery) completes it. (Flip F0:
                // un-gated to production.)
                LockfreeInsertResult::IoError(e) => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    let _ = lsn;
                    return Err(e);
                }
            }
        }
    }

    /// **Order-A durable** lock-free REMOVE (design "R-B") — the proven mirror of
    /// [`Self::insert_cas_durable`]. Clears a term's membership in the lock-free
    /// overlay durably: the `Remove` WAL record is appended AND synced DURABLE
    /// BEFORE the visibility-publishing root CAS, and the committed watermark
    /// advances only once the CAS lands. A crash therefore loses no acknowledged
    /// remove — an acked remove replays (clears the term on recovery); a
    /// non-acked one was never durable.
    ///
    /// Returns `Ok(true)` iff this call cleared a previously-present term,
    /// `Ok(false)` if the term was already absent.
    ///
    /// # Why monotonicity is dropped here (and why it is still sound)
    ///
    /// The insert path relies on finality being monotone (0→1 only) so the shared
    /// node's in-place `try_set_final` (`fetch_or`) is the single arbiter. Remove
    /// breaks 0→1-only (it does 1→0). R-B keeps the protocol sound by NEVER
    /// clearing a shared node in place: the cleared leaf is a FRESH
    /// [`OverlayNode::as_non_final`] copy spliced into a NEW spine and published
    /// ONLY via the root CAS, so the root-CAS total order linearizes inserts and
    /// removes together (last-writer-wins). The composite linearizability is
    /// machine-checked by the RB2 loom schedules, the RB3 remove-aware proptest,
    /// and the RB4 `LockFreeOverlayRemoveCas.tla` spec (whose `_Unsafe` negative
    /// control proves the in-place-clear alternative violates last-writer-wins).
    ///
    /// # Cache invalidation (DATA-CORRECTNESS — design §3.4)
    ///
    /// `contains_lockfree` trusts the insert-only positive `lockfree_cache` FIRST
    /// and short-circuits `true`. A remove that cleared the trie but left a stale
    /// cache entry would make the term read present FOREVER. So this method
    /// `lockfree_cache.remove(term)` on EVERY state-changing arm (`Removed` and
    /// `AlreadyAbsent`) BEFORE `mark_committed`. The RB3 proptest `Contains`
    /// assertion + an RB2 remove‖contains schedule witness this.
    ///
    /// Requires `enable_lockfree()` and a synchronous durability policy
    /// (`Immediate`/`GroupCommit`), rejected EXACTLY as `insert_cas_durable` does.
    /// Behind the `enable_lockfree` opt-in; NOT routed from production `remove`
    /// (that routing is the later flip's RB6, which depends on fault-in being
    /// un-gated to production — design §6).
    pub fn remove_cas_durable(&self, term: &str) -> Result<bool> {
        use std::sync::atomic::Ordering;

        // "Acknowledged ⇒ durable" only holds under a synchronous policy — reject
        // the others EXACTLY as `insert_cas_durable` does.
        match self.durability_policy {
            DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit => {}
            DurabilityPolicy::Periodic | DurabilityPolicy::None => {
                return Err(PersistentARTrieError::InvalidOperation(
                    "remove_cas_durable requires Immediate or GroupCommit durability so an \
                     acknowledged remove is guaranteed durable before it becomes visible"
                        .to_string(),
                ));
            }
        }

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

        let chars: Vec<u32> = term.chars().map(|c| c as u32).collect();
        if chars.is_empty() {
            return Ok(false);
        }

        // ── ABSENT FAST-PATH + WAL AVOIDANCE (key divergence from insert) ──
        // A no-op remove must NOT burn an LSN / punch a watermark hole (matches
        // the owned `preflight_remove_no_wal`). Consult the TRIE, not just the
        // positive cache: a cache MISS is not the same as trie-ABSENT (the cache
        // can be empty after a recovery rebuild while the term is live in the
        // overlay). When fault-in is compiled in, walk through `find_leaf_faulting`
        // so a term under an evicted (OnDisk) prefix is faulted back and seen
        // present; on I/O error fall back to the non-faulting walk (best-effort).
        let _epoch = self.epoch_manager.enter_read();
        // Flip F0: fault-in un-gated to production. A term under an evicted (OnDisk)
        // prefix is faulted back and seen present; on I/O error fall back to the
        // non-faulting walk (best-effort).
        let present_before = {
            match self.find_leaf_faulting(lockfree_root, &chars, DEFAULT_MAX_FAULTIN_RETRIES) {
                Ok(found) => found.is_some(),
                Err(_) => self.find_leaf_lockfree(lockfree_root, &chars).is_some(),
            }
            // Pre-flip production fallback (commented out, not deleted — F0
            // reversibility): the non-faulting walk that reported a term under an
            // evicted prefix as absent.
            // self.find_leaf_lockfree(lockfree_root, &chars).is_some()
        };
        if !present_before {
            // Genuinely absent → no WAL record (no LSN, no watermark hole).
            // Invalidate the positive cache defensively (a stale entry without a
            // matching final trie node would otherwise read present forever).
            lockfree_cache.remove(term);
            return Ok(false);
        }

        // ORDER A — step 1: append + sync the Remove record DURABLE, before any
        // visibility. The returned LSN is durable-per-policy at this point. One
        // append covers every CAS retry — we never re-append (that would burn
        // LSNs and punch holes in the watermark).
        let lsn = self.append_to_wal_returning_lsn(WalRecord::Remove {
            term: term.as_bytes().to_vec(),
        })?;

        // OD4 (test-only): the Remove record is durable; the CAS has not run.
        #[cfg(test)]
        commit_rendezvous(RendezvousPhase::AfterAppend);

        // Step 2: the visibility CAS loop. The single root CAS inside
        // `try_remove_lockfree_path` is the SOLE visibility arbiter.
        loop {
            match self.try_remove_lockfree_path(lockfree_root, &chars) {
                LockfreeRemoveResult::Removed(root_generation) => {
                    // GENERATION (§3.6): the PUBLISHED-root version captured at THIS
                    // remove's CAS (`root_generation`, from the exact root it swapped),
                    // strictly monotone in root-CAS order — the SAME source the insert
                    // path uses, so a remove and the insert it clobbers never tie.
                    // (S2/FIX-A keeps real arms on root.version; commit_seq stamp at S4.)
                    let generation = root_generation;
                    // OD4 (test-only): the clear CAS has won; CommitRank not yet
                    // appended. A test rendezvouses here to order this commit.
                    #[cfg(test)]
                    commit_rendezvous(RendezvousPhase::AfterCommit);
                    // §3.4 CACHE INVALIDATION (FIRST, before mark_committed): the
                    // term is no longer in the trie, so it must not read present
                    // via the positive cache.
                    lockfree_cache.remove(term);
                    // Step 2.5 (NEW, Order-A-preserving): bind the durable Remove
                    // record (`lsn`) to its commit generation, durable BEFORE ack.
                    let rank_lsn = self.append_commit_rank(lsn, term.as_bytes(), generation)?;
                    self.committed_watermark.mark_committed(lsn);
                    self.committed_watermark.mark_committed(rank_lsn);
                    return Ok(true);
                }
                LockfreeRemoveResult::AlreadyAbsent(observed_gen) => {
                    // Raced: a concurrent remove cleared it between our presence
                    // check and the CAS. The Remove LSN is durable; the watermark
                    // must not stall (mirrors insert's AlreadyExists arm). Still
                    // invalidate the cache (the term is absent now).
                    //
                    // FIX-A idempotent arm: this op took NO root CAS. Rank with the
                    // OBSERVED-root version (`observed_gen` = version of the root this
                    // remove walked and found the term absent in) — causally bounded
                    // (< any strictly-later same-key insert's published version), in the
                    // SAME root.version domain as the real arms, NOT a second load. (At
                    // S4 this converts to NO-RANK under the Overlay regime.)
                    let generation = observed_gen;
                    // OD4 (test-only): this idempotent commit "decided" here.
                    #[cfg(test)]
                    commit_rendezvous(RendezvousPhase::AfterCommit);
                    lockfree_cache.remove(term);
                    let rank_lsn = self.append_commit_rank(lsn, term.as_bytes(), generation)?;
                    self.committed_watermark.mark_committed(lsn);
                    self.committed_watermark.mark_committed(rank_lsn);
                    return Ok(false);
                }
                LockfreeRemoveResult::Conflict => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                // WRITE-PATH FAULT-IN I/O error (fault-in builds only): the Remove
                // record is ALREADY durable (step 1) but we could not fault the
                // evicted prefix in to make the clear VISIBLE. Surface the error;
                // do NOT advance the watermark (the contiguous prefix correctly
                // stalls at this LSN until a later retry / recovery completes it).
                // This is the documented Order-A "durable-but-visible-after-reopen"
                // window — recovery replays the logged Remove, NOT a lost write.
                // (Flip F0: un-gated to production.)
                LockfreeRemoveResult::IoError(e) => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    let _ = lsn;
                    return Err(e);
                }
            }
        }
    }

    /// Attempt to clear a term's membership in the lock-free overlay via a single
    /// path-copy + root CAS (R-B). Dual of [`Self::try_insert_lockfree_path`].
    fn try_remove_lockfree_path(
        &self,
        root: &super::nodes::atomic_ptr::AtomicNodePtr<V>,
        chars: &[u32],
    ) -> LockfreeRemoveResult {
        // Load the current published root. A null/empty overlay has nothing to
        // remove (absent).
        let current_root = match root.load() {
            Some(node) => node,
            // Empty/null overlay: nothing was ever present, so generation 0 (sorts
            // first; an idempotent remove of a never-present term is harmless).
            None => return LockfreeRemoveResult::AlreadyAbsent(0),
        };

        // Build a NEW spine whose leaf is a FRESH cleared copy (as_non_final);
        // the single root CAS below is the SOLE visibility arbiter — no in-place
        // clear of a shared node (design §3.5). The PUBLISHED-ROOT version is the
        // Order-A commit generation (§3.6): the spine path-copy bumped it to
        // `current_root.version + 1`, fixed at this CAS, strictly monotone in
        // root-CAS order — the SAME generation source the insert path reads, so an
        // insert and the remove it clobbers can never TIE.
        match self.build_remove_path_recursive(&current_root, chars, 0) {
            Ok((new_root, _cleared_leaf)) => {
                let root_generation = new_root.version();
                match root.compare_exchange(&current_root, new_root) {
                    Ok(_) => LockfreeRemoveResult::Removed(root_generation),
                    Err(_actual) => LockfreeRemoveResult::Conflict,
                }
            }
            // FIX-A: carry the OBSERVED-root version (`current_root` — the exact root
            // this walk traversed and found the term absent in) so the idempotent
            // caller ranks causally in the same `root.version` domain.
            Err(BuildPathError::AlreadyAbsent) => {
                LockfreeRemoveResult::AlreadyAbsent(current_root.version())
            }
            // `build_remove_path_recursive` never returns `AlreadyExists`; keep the
            // match total by mapping it to absent (the no-op spine outcome).
            Err(BuildPathError::AlreadyExists) => {
                LockfreeRemoveResult::AlreadyAbsent(current_root.version())
            }
            // Flip F0: fault-in I/O error un-gated to production.
            Err(BuildPathError::Io(e)) => LockfreeRemoveResult::IoError(e),
        }
    }

    /// Recursively build a NEW tree with `chars`'s leaf cleared (non-final) — the
    /// dual of [`Self::build_path_recursive`]. On the way down it descends the
    /// existing spine; at `depth == len` it clears finality on a **FRESH**
    /// [`OverlayNode::as_non_final`] copy of the existing leaf (NOT a shared Arc
    /// like insert — the root CAS is the sole arbiter for the 1→0 transition,
    /// §3.5). On the way back up it path-copies each ancestor with the rebuilt
    /// child. Returns the new spine root, or:
    ///   * `Err(AlreadyAbsent)` if the leaf is already non-final (don't publish a
    ///     no-op spine) or a spine edge is missing/null;
    ///   * `Err(Io(_))` (fault-in builds) if loading an evicted `OnDisk` prefix
    ///     fails.
    ///
    /// Returns `(new_spine_root, cleared_leaf)` on success: the rebuilt root the
    /// caller CAS-publishes, AND the FRESH cleared-leaf Arc itself (created at the
    /// base case, passed UNCHANGED up the path-copy). The caller reads the leaf's
    /// `version()` for the Order-A commit generation (§3.6) from this EXACT node —
    /// the one the root CAS publishes — not a re-walk.
    fn build_remove_path_recursive(
        &self,
        node: &Arc<super::nodes::persistent_node::PersistentCharNode<V>>,
        chars: &[u32],
        depth: usize,
    ) -> std::result::Result<
        (
            Arc<super::nodes::persistent_node::PersistentCharNode<V>>,
            Arc<super::nodes::persistent_node::PersistentCharNode<V>>,
        ),
        BuildPathError,
    > {
        use super::nodes::persistent_node::Child;

        if depth == chars.len() {
            // Reached the target depth.
            if !node.is_final() {
                // Already absent — do NOT publish a no-op spine.
                return Err(BuildPathError::AlreadyAbsent);
            }
            // FRESH cleared leaf (as_non_final): a NEW node version, published
            // only via the root CAS. The subtree is RETAINED (remove "cat" keeps
            // "cats"). This is the 1→0 transition the §3.5/§4.4 negative control
            // proves MUST go through a fresh copy + root CAS, never an in-place
            // `fetch_and` on the shared node. At the base, root == leaf.
            let leaf = Arc::new(node.as_non_final());
            return Ok((Arc::clone(&leaf), leaf));
        }

        let key = chars[depth];

        match node.find_child(key) {
            Some(child) => {
                if let Some(child_arc) = child.as_in_mem() {
                    // In-memory child: descend + path-copy. Thread the cleared
                    // leaf up unchanged.
                    let child_arc = Arc::clone(child_arc);
                    let (new_child, leaf) =
                        self.build_remove_path_recursive(&child_arc, chars, depth + 1)?;
                    let new_node = Arc::new(node.with_child(key, Child::InMem(new_child)));
                    Ok((new_node, leaf))
                } else if let Some(on_disk) = child.as_on_disk().filter(|p| !p.is_null()) {
                    // WRITE-PATH FAULT-IN (design §3, R-B): the prefix child was
                    // EVICTED to OnDisk. Fault it in first, then descend, splicing
                    // `Child::InMem(faulted)` — identical in shape to an in-memory
                    // child, so the single root CAS stays the SOLE arbiter (no new
                    // commit point). Flip F0: un-gated to production (RB6 depends on
                    // fault-in being a production path — remove-under-evicted-prefix
                    // needs it).
                    {
                        let loaded = match self.load_overlay_node_from_disk(on_disk) {
                            Ok(n) => n,
                            Err(e) => return Err(BuildPathError::Io(e)),
                        };
                        let (new_child, leaf) =
                            self.build_remove_path_recursive(&loaded, chars, depth + 1)?;
                        let new_node = Arc::new(node.with_child(key, Child::InMem(new_child)));
                        return Ok((new_node, leaf));
                    }
                    // Pre-flip production fallback (commented out, not deleted — F0
                    // reversibility): an OnDisk prefix couldn't be faulted in, so the
                    // overlay remove treated it as absent.
                    // #[cfg(not(any(test, feature = "bench-internals")))]
                    // {
                    //     let _ = on_disk;
                    //     Err(BuildPathError::AlreadyAbsent)
                    // }
                } else {
                    // Null filler (never a real child) ⇒ absent.
                    Err(BuildPathError::AlreadyAbsent)
                }
            }
            // Missing edge ⇒ the term is absent on this snapshot.
            None => Err(BuildPathError::AlreadyAbsent),
        }
    }

    /// Attempt to insert a path in the lock-free trie.
    ///
    /// Returns the result of the insertion attempt.
    fn try_insert_lockfree_path(
        &self,
        root: &super::nodes::atomic_ptr::AtomicNodePtr<V>,
        chars: &[u32],
        finalize: bool,
    ) -> LockfreeInsertResult<V> {
        use super::nodes::persistent_node::PersistentCharNode;

        // Load current root
        let current_root = match root.load() {
            Some(node) => node,
            None => {
                // Root is null - try to initialize it
                let new_root = Arc::new(PersistentCharNode::new());
                match root.try_init(new_root) {
                    Ok(()) => return self.try_insert_lockfree_path(root, chars, finalize),
                    Err(actual) => actual,
                }
            }
        };

        // Navigate/create path to the target node
        self.insert_lockfree_recursive(root, &current_root, chars, 0, finalize)
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
        node: &Arc<super::nodes::persistent_node::PersistentCharNode<V>>,
        chars: &[u32],
        depth: usize,
        finalize: bool,
    ) -> std::result::Result<
        (
            Arc<super::nodes::persistent_node::PersistentCharNode<V>>,
            Arc<super::nodes::persistent_node::PersistentCharNode<V>>,
        ),
        BuildPathError,
    > {
        use super::nodes::persistent_node::Child;

        if depth == chars.len() {
            // Reached the target depth.
            if node.is_final() {
                return Err(BuildPathError::AlreadyExists); // Already a complete term
            }
            // (1a, D2.8 §1.2 / RT-D2-A): the DURABLE path (`finalize == true`)
            // publishes a FRESH FINAL leaf INSIDE the root CAS, so the root CAS in
            // `insert_lockfree_recursive` becomes the SOLE linearization point
            // (matching the value/remove paths) ⇒ generation/commit_seq order ==
            // visibility order. No proper-prefix regression and no double-count:
            // two racers each build a fresh final copy but only ONE root CAS wins;
            // the loser retries, sees `is_final()` above ⇒ `AlreadyExists` ⇒ exactly
            // one publisher. (The old shared-node + `try_set_final` arbiter is kept
            // ONLY for the non-durable path below — it has no replay key to
            // mis-order, so its split linearization point is harmless there.)
            if finalize {
                let final_leaf = Arc::new(node.as_final());
                return Ok((Arc::clone(&final_leaf), final_leaf));
            }
            // Non-durable `insert_cas` (`finalize == false`): return the EXISTING
            // shared node so its later `try_set_final` (`fetch_or`) is the single
            // atomic arbiter — UNCHANGED behavior (the §6.2 no-regression contract;
            // a fresh-final here would make `try_set_final` observe an already-final
            // node and wrongly report a new prefix term, e.g. "d" after "da", as a
            // duplicate — the Phase-A bug — so the non-durable path MUST stay shared).
            return Ok((Arc::clone(node), Arc::clone(node)));
        }

        let key = chars[depth];

        match node.find_child(key) {
            Some(child) => {
                // In-memory child: path-copy into it. `as_in_mem` borrows the owned
                // child `Arc` and `Child::InMem` re-wraps the path-copied
                // replacement (zero `unsafe`).
                if let Some(child_arc) = child.as_in_mem() {
                    let child_arc = Arc::clone(child_arc);

                    // Recursively build path in child
                    let (new_child, leaf) =
                        self.build_path_recursive(&child_arc, chars, depth + 1, finalize)?;

                    // Create new version of this node with the updated child
                    let new_node = Arc::new(node.with_child(key, Child::InMem(new_child)));

                    Ok((new_node, leaf))
                } else if let Some(on_disk) = child.as_on_disk().filter(|p| !p.is_null()) {
                    // WRITE-PATH FAULT-IN (design §4, DATA-LOSS-CRITICAL): the child
                    // was EVICTED to OnDisk. Without faulting it in, a NEW term under
                    // this evicted prefix would return `AlreadyExists` (false) and be
                    // SILENTLY DROPPED (never cached, never merged). FAULT it back in,
                    // then DESCEND, splicing `Child::InMem(faulted+extended)` at `key`
                    // — identical in shape to an in-memory child, so the single root
                    // CAS in `insert_lockfree_recursive` remains the SOLE arbiter (NO
                    // new commit point is introduced here).
                    //
                    // Flip F0: un-gated to production. The flip routes production
                    // inserts through the overlay, so a NEW term under an evicted
                    // prefix MUST fault the prefix in rather than be silently dropped
                    // (the data-loss-critical write-path half, design §4).
                    {
                        let loaded = match self.load_overlay_node_from_disk(on_disk) {
                            Ok(n) => n,
                            Err(e) => return Err(BuildPathError::Io(e)),
                        };
                        let (new_child, leaf) =
                            self.build_path_recursive(&loaded, chars, depth + 1, finalize)?;
                        let new_node = Arc::new(node.with_child(key, Child::InMem(new_child)));
                        return Ok((new_node, leaf));
                    }
                    // Pre-flip production fallback (commented out, not deleted — F0
                    // reversibility): treated an OnDisk child as already-present
                    // (forcing a cache/persistent re-check), which silently dropped a
                    // new term under an evicted prefix.
                    // #[cfg(not(any(test, feature = "bench-internals")))]
                    // {
                    //     let _ = on_disk;
                    //     Err(BuildPathError::AlreadyExists)
                    // }
                } else {
                    // Null filler (never a real child) — conservative AlreadyExists.
                    Err(BuildPathError::AlreadyExists)
                }
            }
            None => {
                // Child doesn't exist - create entire remaining path
                let (new_subtree, leaf) = self.create_lockfree_path(&chars[depth + 1..], finalize);
                let new_node = Arc::new(node.with_child(key, Child::InMem(new_subtree)));

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
        finalize: bool,
    ) -> (
        Arc<super::nodes::persistent_node::PersistentCharNode<V>>,
        Arc<super::nodes::persistent_node::PersistentCharNode<V>>,
    ) {
        use super::nodes::persistent_node::{Child, PersistentCharNode};

        // The leaf: for the durable (1a) path `finalize == true` it is published
        // FINAL inside the root CAS (the sole LP); for the non-durable path it is
        // non-final and the caller's `try_set_final` is the arbiter (unchanged).
        let leaf = Arc::new(if finalize {
            PersistentCharNode::new().as_final()
        } else {
            PersistentCharNode::new()
        });

        if chars.is_empty() {
            // No more characters - leaf is also the root
            return (leaf.clone(), leaf);
        }

        // Build path bottom-up
        let mut current = leaf.clone();

        for &c in chars.iter().rev() {
            // Each parent owns its child by `Arc` (no raw-pointer smuggling).
            let parent = PersistentCharNode::new().with_child(c, Child::InMem(current));
            current = Arc::new(parent);
        }

        (current, leaf)
    }

    /// Attempt to insert a path using CAS. Called from insert_cas retry loop.
    fn insert_lockfree_recursive(
        &self,
        root: &super::nodes::atomic_ptr::AtomicNodePtr<V>,
        current: &Arc<super::nodes::persistent_node::PersistentCharNode<V>>,
        chars: &[u32],
        _depth: usize, // Kept for API compatibility
        finalize: bool,
    ) -> LockfreeInsertResult<V> {
        // Build the new tree structure with the path inserted. The single root CAS
        // below is the SOLE visibility arbiter — write-path fault-in (design §4)
        // happens INSIDE `build_path_recursive` (it rebuilds ONE new spine that
        // splices any faulted prefix InMem), so it adds no second commit point.
        match self.build_path_recursive(current, chars, 0, finalize) {
            Ok((new_root, leaf)) => {
                // The published root's version IS the Order-A commit generation
                // (design C′, §3.6): `with_child` path-copy bumped it to
                // `current.version + 1`, and it is fixed at the CAS, so successive
                // publications are strictly monotone in CAS order. Capture it
                // BEFORE the CAS consumes `new_root`.
                let root_generation = new_root.version();
                // Try to CAS the root to the new version
                match root.compare_exchange(current, new_root) {
                    Ok(_) => {
                        // Successfully updated the tree
                        LockfreeInsertResult::Inserted(leaf, root_generation)
                    }
                    Err(_actual) => {
                        // CAS failed - another thread modified the tree
                        LockfreeInsertResult::Conflict
                    }
                }
            }
            Err(BuildPathError::AlreadyExists) => {
                // Term already exists (or, in the production build, an on-disk
                // reference treated conservatively as present). FIX-A: carry the
                // OBSERVED-root version (`current.version()` — the exact root this
                // walk traversed and found the term final in) so the idempotent
                // caller ranks causally (< any later same-key remove), NOT a second
                // `lockfree_root.load()` (the leapfrog).
                LockfreeInsertResult::AlreadyExists(current.version())
            }
            // R-B `AlreadyAbsent` is produced ONLY by the remove path
            // (`build_remove_path_recursive`); the insert path's
            // `build_path_recursive` never returns it. Treat it conservatively as
            // "already exists" so this arm stays total without inventing a new
            // insert outcome (unreachable in practice for inserts).
            Err(BuildPathError::AlreadyAbsent) => {
                LockfreeInsertResult::AlreadyExists(current.version())
            }
            // WRITE-PATH FAULT-IN I/O error: surface it so the durable caller
            // returns `Err(e)` and the best-effort caller retries / returns false.
            // The durable image is intact (fault-in writes nothing). (Flip F0:
            // un-gated to production.)
            Err(BuildPathError::Io(e)) => LockfreeInsertResult::IoError(e),
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

        // Fall back to checking the lock-free trie structure.
        if let Some(ref root) = self.lockfree_root {
            let chars: Vec<u32> = term.chars().map(|c| c as u32).collect();

            // READ-PATH FAULT-IN (design §3): Flip F0 un-gates this to production.
            // Route through `find_leaf_faulting` so a term under an EVICTED (OnDisk)
            // prefix is faulted back and reported present instead of spuriously
            // absent — production point-reads now follow the overlay. On an I/O
            // error fall back to the non-faulting walk (best-effort; liveness-only).
            {
                match self.find_leaf_faulting(root, &chars, DEFAULT_MAX_FAULTIN_RETRIES) {
                    Ok(found) => return found.is_some(),
                    Err(_) => {
                        if let Some(root_node) = root.load() {
                            return self.find_in_lockfree_trie(&root_node, &chars, 0);
                        }
                        return false;
                    }
                }
            }
            // Pre-flip production fallback (commented out, not deleted — F0
            // reversibility): the non-faulting walk that read a term under an
            // evicted prefix as absent.
            // {
            //     if let Some(root_node) = root.load() {
            //         return self.find_in_lockfree_trie(&root_node, &chars, 0);
            //     }
            // }
        }

        false
    }

    /// Navigate the lock-free trie to find a term.
    fn find_in_lockfree_trie(
        &self,
        node: &Arc<super::nodes::persistent_node::PersistentCharNode<V>>,
        chars: &[u32],
        depth: usize,
    ) -> bool {
        if depth >= chars.len() {
            return node.is_final();
        }

        let key = chars[depth];
        if let Some(child) = node.find_child(key) {
            // On-disk references can't be traversed in the lock-free overlay; the
            // persistent trie would need to be checked instead. In-memory children
            // are borrowed and recursed into (owned `Arc`, no `unsafe`).
            if let Some(child_arc) = child.as_in_mem() {
                return self.find_in_lockfree_trie(&Arc::clone(child_arc), chars, depth + 1);
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
        root: &super::nodes::atomic_ptr::AtomicNodePtr<V>,
        chars: &[u32],
    ) -> Option<Arc<super::nodes::persistent_node::PersistentCharNode<V>>> {
        let current = root.load()?;
        self.find_leaf_recursive(&current, chars, 0)
    }

    /// Recursive helper for `find_leaf_lockfree`.
    fn find_leaf_recursive(
        &self,
        node: &Arc<super::nodes::persistent_node::PersistentCharNode<V>>,
        chars: &[u32],
        depth: usize,
    ) -> Option<Arc<super::nodes::persistent_node::PersistentCharNode<V>>> {
        if depth == chars.len() {
            return if node.is_final() {
                Some(Arc::clone(node))
            } else {
                None
            };
        }

        let child = node.find_child(chars[depth])?;
        // Can't traverse disk refs in the lock-free overlay; `as_in_mem` returns
        // `None` for an on-disk child, short-circuiting via `?` (owned `Arc`).
        let child_arc = child.as_in_mem()?;
        self.find_leaf_recursive(&Arc::clone(child_arc), chars, depth + 1)
    }

    /// **Read-path fault-in (design §3).** Find the leaf for `chars`, FAULTING any
    /// `OnDisk` spine slot back into memory along the way, so a term under an
    /// evicted prefix is no longer reported absent. Returns `Ok(Some(leaf))` iff the
    /// full path exists and the leaf is final, `Ok(None)` if the term is genuinely
    /// absent, and `Err(_)` only on a buffer-manager I/O error while loading an
    /// `OnDisk` node.
    ///
    /// This is the dual of [`Self::evict_overlay_node_at_path`]
    /// (`mod.rs`): where eviction path-copies the spine swapping an in-memory child
    /// for `Child::OnDisk`, fault-in path-copies the spine swapping an `OnDisk`
    /// child for `Child::InMem(loaded)` and CAS-publishes a new root. It mirrors
    /// `resolve_swizzled_ptr`'s settle-and-reread, with an `arc-swap` root CAS
    /// instead of a swizzle.
    ///
    /// Per attempt (bounded by `max_faultin_retries`):
    ///   1. `enter_read()` (epoch parity) and `load()` the published root;
    ///   2. walk `chars` top-down collecting the `(node, edge)` spine. At each edge:
    ///      `None` ⇒ the term is absent (`Ok(None)`); `InMem` ⇒ descend; **`OnDisk`
    ///      ⇒ fault**: `load_overlay_node_from_disk(ptr)`, rebuild the spine
    ///      bottom-up splicing `Child::InMem(loaded)` at that edge (every shallower
    ///      ancestor re-linked `InMem`), then `compare_exchange(&old_root, new_root)`.
    ///      On CAS success: rebase from the just-published root and continue the
    ///      walk (the faulted child is now `InMem`); on CAS failure (a concurrent
    ///      writer/evictor/faulter won): drop our loaded `Arc` (refcount) and rebase
    ///      from a fresh root load — never clobbering the racer (loser-safe);
    ///   3. terminal: leaf-by-`is_final` (as [`Self::find_leaf_recursive`]).
    ///
    /// **Idempotent / loser-safe:** two faulters each load their own `Arc`; exactly
    /// one install CAS wins (`Arc::ptr_eq` arbitration in `AtomicNodePtr`), the loser
    /// drops + re-reads the now-`InMem` child. **Single arbiter:** the `lockfree_root`
    /// slot totally orders every version; every published root has each node
    /// `InMem` XOR `OnDisk` (`LinkedAndOnDiskDisjoint`). **Liveness:** on retry
    /// exhaustion we do ONE final read-only walk of the fresh root — a still-`OnDisk`
    /// slot there reads absent (durable; a later read retries), never spins.
    ///
    /// ZERO new `unsafe`: only `AtomicNodePtr::{load,compare_exchange}` (hazard-
    /// protected), pure node copies, `Arc` clone/drop, and the EXISTING lazy loader
    /// (called through the safe `&self` `load_overlay_node_from_disk` boundary).
    ///
    /// REVERSIBLE BENCH GATE: gated `any(test, bench-internals)` (consumes the
    /// bench/test-gated `load_overlay_node_from_disk`; production read routing is
    /// untouched until the flip — design §6/§8).
    ///
    /// MAINTENANCE COUPLING: mirrors `evict_overlay_node_at_path`; keep in lockstep.
    // Flip F0: un-gated to production (the read/write paths route through this).
    pub(crate) fn find_leaf_faulting(
        &self,
        root_slot: &super::nodes::atomic_ptr::AtomicNodePtr<V>,
        chars: &[u32],
        max_faultin_retries: usize,
    ) -> Result<Option<Arc<super::nodes::persistent_node::PersistentCharNode<V>>>> {
        use super::nodes::persistent_node::{Child, PersistentCharNode};

        // One read-only walk of `root` (no faulting): used for the empty-key leaf
        // and the post-exhaustion liveness fallback. A still-OnDisk slot reads
        // absent (durable; a later call retries) — never spins.
        fn walk_no_fault<V: DictionaryValue>(
            root: &Arc<PersistentCharNode<V>>,
            chars: &[u32],
        ) -> Option<Arc<PersistentCharNode<V>>> {
            let mut current = Arc::clone(root);
            for &edge in chars {
                let child = current.find_child(edge)?;
                let child_arc = child.as_in_mem()?;
                let next = Arc::clone(child_arc);
                current = next;
            }
            if current.is_final() {
                Some(current)
            } else {
                None
            }
        }

        // +1 so we always get at least one fresh-root liveness walk even when
        // `max_faultin_retries == 0`.
        for _attempt in 0..=max_faultin_retries {
            let _epoch = self.epoch_manager.enter_read();

            let old_root = match root_slot.load() {
                Some(r) => r,
                None => return Ok(None), // empty overlay
            };

            // Walk top-down, collecting (node, edge) for a possible rebuild, until
            // we either reach the leaf (all InMem ⇒ answer directly), hit a missing
            // edge (absent), or hit an OnDisk edge (fault + CAS + rebase).
            let mut spine: Vec<(Arc<PersistentCharNode<V>>, u32)> =
                Vec::with_capacity(chars.len());
            let mut current = Arc::clone(&old_root);
            let mut faulted = false;

            let mut idx = 0usize;
            while idx < chars.len() {
                let edge = chars[idx];
                let child = match current.find_child(edge) {
                    Some(c) => c,
                    None => return Ok(None), // genuinely absent on this snapshot
                };
                match child {
                    Child::InMem(child_arc) => {
                        let next = Arc::clone(child_arc);
                        spine.push((Arc::clone(&current), edge));
                        current = next;
                        idx += 1;
                    }
                    Child::OnDisk(ptr) if !ptr.is_null() => {
                        // FAULT: load the OnDisk child back into memory, then rebuild
                        // the spine bottom-up splicing it InMem at THIS edge.
                        let loaded = self.load_overlay_node_from_disk(ptr)?;

                        // The deepest rebuilt node is `current` with its `edge` child
                        // replaced by InMem(loaded); each shallower ancestor in
                        // `spine` is re-linked InMem around the rebuilt child.
                        let mut new_child =
                            Arc::new(current.with_child(edge, Child::InMem(loaded)));
                        for (ancestor, anc_edge) in spine.iter().rev() {
                            new_child =
                                Arc::new(ancestor.with_child(*anc_edge, Child::InMem(new_child)));
                        }

                        // Loser-safe install CAS against the snapshot root.
                        match root_slot.compare_exchange(&old_root, new_child) {
                            Ok(_) => {
                                self.cas_retries
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                            Err(_actual) => {
                                self.cas_retries
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        // Whether we won (published) or lost (a racer advanced the
                        // root, possibly already faulting this node), rebase: break
                        // to the outer loop and re-walk from a fresh root load.
                        faulted = true;
                        break;
                    }
                    // Null filler (never yielded as a real child) ⇒ absent.
                    Child::OnDisk(_) => return Ok(None),
                }
            }

            if faulted {
                // Re-walk from a freshly-published root on the next attempt.
                continue;
            }

            // Reached the terminal depth with an all-InMem spine: answer directly.
            return Ok(if current.is_final() {
                Some(current)
            } else {
                None
            });
        }

        // Retry budget exhausted: ONE final read-only walk of the freshest root.
        // A still-OnDisk slot reads absent (liveness-only; durable, a later read
        // faults it). Never spins.
        let final_root = match root_slot.load() {
            Some(r) => r,
            None => return Ok(None),
        };
        Ok(walk_no_fault(&final_root, chars))
    }

    /// Get the number of CAS retries (for monitoring contention).
    pub fn cas_retry_count(&self) -> u64 {
        self.cas_retries.load(std::sync::atomic::Ordering::Relaxed)
    }

    // ==================== End Lock-Free CAS Methods ====================
}

// ============================================================================
// Counter (valued) overlay methods — `V = u64` ONLY.
// ============================================================================
//
// G1: the lock-free overlay node now carries an **immutable** `Option<V>` value
// (was an in-place `AtomicU64`). The wait-free `fetch_add` increment is therefore
// gone; an increment becomes a **path-copy CAS** (read the leaf's value, build a
// new leaf with `old_leaf.as_final().with_value(new_val)`, path-copy the
// root→leaf spine, CAS-publish the root — exactly the single-phase model the
// vocab overlay (`persistent_vocab_artrie::lockfree_cas`) already uses).
//
// These methods are counter-specific (the lock-free n-gram counter is `u64`), so
// they live in a `V = u64` impl block. The generic membership block above remains
// `<V>` and its proven `try_set_final` two-phase finalization is untouched.
// Cross-block calls to the generic helpers (`find_leaf_lockfree`,
// `try_insert_lockfree_path`) resolve at `V = u64` — same code, different impl.
impl<S: BlockStorage> super::PersistentARTrieChar<u64, S> {
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

        // READ-PATH FAULT-IN (design §3): Flip F0 un-gates this to production.
        // Fault an evicted (OnDisk) prefix back in so the value is the durable
        // value, not a spurious `None`. On I/O error fall through to the
        // non-faulting walk below (best-effort).
        {
            if let Ok(found) = self.find_leaf_faulting(lockfree_root, &chars, DEFAULT_MAX_FAULTIN_RETRIES)
            {
                return found.and_then(|leaf| leaf.get_value());
            }
        }

        self.find_leaf_lockfree(lockfree_root, &chars)
            .and_then(|leaf| leaf.get_value())
    }

    /// Checked lock-free increment: create path if needed, then add `delta`.
    ///
    /// **G1 path-copy CAS** (the wait-free in-place `fetch_add` is gone — the
    /// node's value is now an immutable `Option<u64>`). Each attempt:
    ///   1. loads the overlay root (a published, immutable snapshot);
    ///   2. reads the current count `cur` at `key` (0 if the leaf is absent or
    ///      has no value), overflow-checks `cur.checked_add(delta)` against
    ///      `LOCKFREE_COUNTER_MAX`;
    ///   3. builds the new leaf `old_leaf.as_final().with_value(cur + delta)` and
    ///      path-copies the root→leaf spine splicing in that leaf (reusing the
    ///      membership `build_path_recursive` to materialize the spine, then
    ///      overwriting the leaf's value);
    ///   4. CAS-publishes the new root via `lockfree_root.compare_exchange`.
    /// On CAS failure another writer published a newer root, so we bump
    /// `cas_retries` and retry — re-reading the (now higher) count, so **no
    /// increment is lost** (the loser folds its delta onto the winner's value).
    ///
    /// This is the primary method for n-gram counting. Workers call it
    /// concurrently under only a shared read lock (`&self`). Contention is the CAS
    /// retry on the shared root; for distinct keys the retries are rare.
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
    /// Inner increment: like [`Self::try_increment_cas`] but ALSO returns the
    /// published-root version (the Order-A commit GENERATION, §3.6) of the WINNING
    /// CAS, so the durable wrapper ([`Self::try_increment_cas_durable`]) can rank the
    /// delta in the SAME `root.version` domain as the overwrite producers (closes
    /// hazard D — a `V=u64` key touched by both a ranked overwrite and an unranked
    /// increment would otherwise cross-domain mis-sort). The generation is captured
    /// before the winning CAS and returned ONLY from the `Ok` arm (a losing iteration
    /// discards its `new_root`, so no stale generation leaks).
    fn try_increment_cas_inner(&self, key: &str, delta: u64) -> Result<(u64, u64)> {
        use super::nodes::persistent_node::PersistentCharNode;
        use std::sync::atomic::Ordering;

        let lockfree_root = self
            .lockfree_root
            .as_ref()
            .expect("Lock-free mode not enabled. Call enable_lockfree() first.");

        let chars: Vec<u32> = key.chars().map(|c| c as u32).collect();
        if chars.is_empty() {
            return Ok((0, 0));
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
                    let new_root = Arc::new(PersistentCharNode::<u64>::new());
                    let _ = lockfree_root.try_init(new_root);
                    continue;
                }
            };

            // (2) Read the current count at `key`. READ-PATH FAULT-IN (design §3):
            // when compiled in, fault an evicted (OnDisk) prefix back in FIRST so
            // `cur` is the durable value, not a silent 0 (counter reset). The
            // fault-in may publish a newer root; the subsequent path-copy CAS below
            // is against `root` (this snapshot), so a fault that advanced the root
            // simply makes that CAS lose → we retry from the now-faulted root and
            // descend without reload (also fixes the pre-existing OnDisk infinite
            // spin in the write step, design §4 read half). Flip F0: un-gated to
            // production.
            let cur = match self.find_leaf_faulting(lockfree_root, &chars, DEFAULT_MAX_FAULTIN_RETRIES)
            {
                Ok(found) => found.and_then(|leaf| leaf.get_value()).unwrap_or(0),
                // I/O error reading the durable image: fall back to this snapshot.
                Err(_) => self
                    .find_leaf_recursive(&root, &chars, 0)
                    .and_then(|leaf| leaf.get_value())
                    .unwrap_or(0),
            };
            // Pre-flip production fallback (commented out, not deleted — F0
            // reversibility): the non-faulting read that returned 0 (silent counter
            // reset) for a term under an evicted prefix.
            // let cur = self
            //     .find_leaf_recursive(&root, &chars, 0)
            //     .and_then(|leaf| leaf.get_value())
            //     .unwrap_or(0);

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

            // (4) Build a new root with the value-carrying leaf spliced in.
            let new_root = match self.build_value_path_recursive(&root, &chars, 0, new_val) {
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
            // S3 GENERATION: the published-root version (Order-A commit generation,
            // §3.6) — captured BEFORE the CAS consumes `new_root`, and returned ONLY
            // from the winning `Ok` arm (the `Err` arm discards this `new_root` and
            // re-claims next iteration), so a losing iteration never leaks a stale rank.
            let generation = new_root.version();
            match lockfree_root.compare_exchange(&root, new_root) {
                Ok(_) => return Ok((new_val, generation)),
                Err(_actual) => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    /// Lock-free path-copy increment (non-durable). Thin wrapper over
    /// [`Self::try_increment_cas_inner`] that drops the commit generation, preserving
    /// the public signature for the existing callers (the non-durable / increment_cas
    /// paths and tests do not rank, so they ignore the generation).
    pub fn try_increment_cas(&self, key: &str, delta: u64) -> Result<u64> {
        self.try_increment_cas_inner(key, delta).map(|(v, _)| v)
    }

    /// **Order-A durable** lock-free increment (Migration Phase E) — the counter
    /// analogue of [`Self::insert_cas_durable`].
    ///
    /// Establishes `visible ⊆ durable-prefix` for a counter delta: the WAL record
    /// is appended AND synced DURABLE **before** the visibility-publishing root
    /// CAS, and the committed watermark advances only after the CAS lands. A crash
    /// loses no acknowledged increment — the durable delta replays; an
    /// un-acknowledged one was never durable.
    ///
    /// # Why a DELTA record (single-entry `BatchIncrement`), not a result record
    ///
    /// `insert_cas_durable`'s doc explains that a per-op *result-based* `Increment`
    /// WAL record does NOT fit lock-free CAS — under out-of-order commit the logged
    /// *result* can be invalidated by a concurrent committer, so recovery could
    /// replay a stale absolute count. This method sidesteps that by logging the
    /// **delta** (`BatchIncrement { entries: [(term, delta)] }`, exactly the
    /// delta-based record the merge path uses): deltas are commutative, so recovery
    /// SUMS them regardless of the order they committed in — order-independence is
    /// the whole point of the watermark/Order-A pairing. The append happens ONCE,
    /// before the CAS loop, and covers every CAS retry (we never re-append: that
    /// would double-count the delta and punch a hole in the watermark).
    ///
    /// The visibility step REUSES the proven path-copy [`Self::try_increment_cas`]
    /// verbatim — its CAS-retry / re-read-on-conflict logic is the formally-checked
    /// no-lost-update arbiter (`char_create_vs_increment_race_has_one_leaf_and_total_value`),
    /// so this method adds only the WAL-before-CAS framing around it and does not
    /// touch that logic.
    ///
    /// Requires `enable_lockfree()` and a synchronous durability policy
    /// (`Immediate`/`GroupCommit`), rejected EXACTLY as `insert_cas_durable` does.
    ///
    /// # ⚠️ Safety boundary (pre-flip)
    ///
    /// WAL-only-safe, identical to `insert_cas_durable`: durability rests on WAL
    /// replay (survives reopen with NO checkpoint), but it is NOT yet safe to mix
    /// with the *owned-tree* checkpoint (which captures the owned tree, not the
    /// overlay, and rotates the WAL by `next_lsn`). Use in a WAL-only configuration
    /// until the Phase-E flip routes checkpoints through `capture_snapshot_immutable`
    /// reclaiming by the committed watermark.
    ///
    /// Returns the new accumulated count on success.
    pub fn try_increment_cas_durable(&self, key: &str, delta: u64) -> Result<u64> {
        // "Acknowledged ⇒ durable" only holds under a synchronous policy — reject
        // the others EXACTLY as `insert_cas_durable` does (copy the discipline so
        // the two durable entry points agree).
        match self.durability_policy {
            DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit => {}
            DurabilityPolicy::Periodic | DurabilityPolicy::None => {
                return Err(PersistentARTrieError::InvalidOperation(
                    "try_increment_cas_durable requires Immediate or GroupCommit durability so an \
                     acknowledged increment is guaranteed durable before it becomes visible"
                        .to_string(),
                ));
            }
        }

        // enable_lockfree() must have run (try_increment_cas would otherwise
        // panic; surface it as a recoverable error on the durable path instead).
        if self.lockfree_root.is_none() {
            return Err(PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
            ));
        }

        let chars: Vec<u32> = key.chars().map(|c| c as u32).collect();
        if chars.is_empty() {
            return Ok(0);
        }

        // Bound the delta to the i64 persistence domain BEFORE logging it, so the
        // WAL never records a delta the merge/recovery path cannot represent. This
        // mirrors `try_increment_cas`'s own up-front overflow guard.
        if delta > LOCKFREE_COUNTER_MAX {
            return Err(Self::lockfree_increment_overflow_error(key, None, delta));
        }
        let delta_i64 = i64::try_from(delta).map_err(|_| {
            PersistentARTrieError::InvalidOperation(format!(
                "try_increment_cas_durable delta for term {:?} exceeds i64 persistence domain: {}",
                key, delta
            ))
        })?;

        // ORDER A — step 1: append + sync the DELTA record DURABLE, before any
        // visibility. Single-entry `BatchIncrement` (delta-based, commutative on
        // replay). Returned LSN is durable-per-policy here.
        let lsn = self.append_to_wal_returning_lsn(WalRecord::BatchIncrement {
            entries: vec![(key.as_bytes().to_vec(), delta_i64)],
        })?;

        // Step 2: publish via the PROVEN path-copy increment (its CAS-retry /
        // re-read-on-conflict loop is the formally-checked no-lost-update arbiter;
        // we do not re-append the WAL on its internal retries). On overflow at the
        // accumulated value the increment errors AFTER the durable append — the
        // delta is durably logged but not made visible; this is the documented
        // "durable-but-visible-only-after-reopen" Order-A panic/error window, not a
        // lost write (recovery replays the logged delta).
        // S3 increment-rank: call the INNER to obtain the WINNING published-root
        // version, then bind the durable delta record (`lsn`) to its commit generation
        // (durable BEFORE ack) so it ranks in the SAME `root.version` domain as the
        // overwrite producers (closes hazard D). G-OVF: the inner errors BEFORE any CAS
        // on overflow, so `?` early-returns here leaving the already-appended
        // BatchIncrement UNRANKED — benign (an accumulate-delta replays in lsn order
        // under Owned; an unacked drop under Overlay). The rank append is reached ONLY
        // on the inner's `Ok`.
        let (new_val, generation) = self.try_increment_cas_inner(key, delta)?;
        let rank_lsn = self.append_commit_rank(lsn, key.as_bytes(), generation)?;

        // Step 3: durable AND visible — advance the committed watermark to include
        // BOTH the data LSN and the rank LSN so the contiguous prefix does not stall.
        self.committed_watermark.mark_committed(lsn);
        self.committed_watermark.mark_committed(rank_lsn);
        Ok(new_val)
    }

    /// **Flip F0 — thin Order-A durable VALUED insert** (`V = u64`). The valued
    /// analogue of [`Self::insert_cas_durable`] (which writes membership only,
    /// `value = None`): this bakes a `u64` value into the leaf via
    /// [`Self::build_value_path_recursive`] (single-phase — finality + value
    /// publish atomically with the root CAS).
    ///
    /// **Insert semantics (NOT upsert):** if the term is already present this is a
    /// no-op returning `Ok(false)` with NO WAL record (matches owned
    /// `insert_with_value`, which preflights and skips an existing term — the
    /// value is NOT overwritten). Presence is consulted on the TRIE via
    /// `find_leaf_faulting` (a term under an evicted prefix is faulted back), NOT
    /// just the positive cache.
    ///
    /// Order-A: the `Insert{value}` WAL record is appended+synced DURABLE before
    /// the visibility CAS; the committed watermark advances only after the CAS
    /// lands (+ the CommitRank record, design C′). Requires a synchronous
    /// durability policy and `enable_lockfree()`, rejected exactly as
    /// `insert_cas_durable`.
    ///
    /// Returns `Ok(true)` iff this call newly inserted the term.
    pub fn insert_cas_with_value_durable(&self, term: &str, value: u64) -> Result<bool> {
        use super::nodes::persistent_node::PersistentCharNode;
        use std::sync::atomic::Ordering;

        match self.durability_policy {
            DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit => {}
            DurabilityPolicy::Periodic | DurabilityPolicy::None => {
                return Err(PersistentARTrieError::InvalidOperation(
                    "insert_cas_with_value_durable requires Immediate or GroupCommit durability so an \
                     acknowledged write is guaranteed durable before it becomes visible"
                        .to_string(),
                ));
            }
        }

        let lockfree_root = self.lockfree_root.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
            )
        })?;

        let chars: Vec<u32> = term.chars().map(|c| c as u32).collect();
        if chars.is_empty() {
            return Ok(false);
        }
        if value > LOCKFREE_COUNTER_MAX {
            return Err(Self::lockfree_increment_overflow_error(term, None, value));
        }

        // INSERT (not upsert): if already present, no-op with NO WAL (don't burn an
        // LSN / punch a watermark hole). Consult the trie (fault-in), not the cache.
        {
            let _epoch = self.epoch_manager.enter_read();
            let present_before =
                match self.find_leaf_faulting(lockfree_root, &chars, DEFAULT_MAX_FAULTIN_RETRIES) {
                    Ok(found) => found.is_some(),
                    Err(_) => self.find_leaf_lockfree(lockfree_root, &chars).is_some(),
                };
            if present_before {
                return Ok(false);
            }
        }

        // ORDER A — step 1: append + sync the valued Insert record DURABLE, before
        // any visibility. One append covers every CAS retry (never re-appended).
        let value_bytes = crate::serialization::bincode_compat::serialize(&value).map_err(|e| {
            PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
        })?;
        let lsn = self.append_to_wal_returning_lsn(WalRecord::Insert {
            term: term.as_bytes().to_vec(),
            value: Some(value_bytes),
        })?;

        // Step 2: publish the valued leaf via the single-root-CAS arbiter (the same
        // path-copy `build_value_path_recursive` the proven counter path uses, so
        // no new commit point is introduced).
        let _epoch = self.epoch_manager.enter_read();
        loop {
            let root = match lockfree_root.load() {
                Some(r) => r,
                None => {
                    let new_root = Arc::new(PersistentCharNode::<u64>::new());
                    let _ = lockfree_root.try_init(new_root);
                    continue;
                }
            };
            let new_root = match self.build_value_path_recursive(&root, &chars, 0, value) {
                Some(r) => r,
                // An I/O error faulting an evicted prefix blocked the path-copy: the
                // WAL record is ALREADY durable, but we could not make the write
                // visible. Surface it (Order-A durable-but-visible-after-reopen
                // window — recovery replays the logged Insert); do NOT advance the
                // watermark (the contiguous prefix stalls at this LSN).
                None => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    return Err(PersistentARTrieError::internal(
                        "insert_cas_with_value_durable: could not fault an evicted prefix in to \
                         publish the valued leaf; the Insert record is durable and replays on reopen",
                    ));
                }
            };
            // GENERATION (§3.6): the published-root version captured at THIS op's
            // visibility CAS, monotone in root-CAS order. (S2/FIX-A keeps the real arms
            // on root.version; the durable commit_seq stamp lands at S4 with Overlay.)
            let generation = new_root.version();
            match lockfree_root.compare_exchange(&root, new_root) {
                Ok(_) => {
                    if let Some(ref cache) = self.lockfree_cache {
                        cache.insert(term.to_string(), true);
                    }
                    let rank_lsn = self.append_commit_rank(lsn, term.as_bytes(), generation)?;
                    self.committed_watermark.mark_committed(lsn);
                    self.committed_watermark.mark_committed(rank_lsn);
                    return Ok(true);
                }
                Err(_actual) => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    /// **Flip F0 — thin Order-A durable UPSERT** (`V = u64`). Like
    /// [`Self::insert_cas_with_value_durable`] but with UPSERT semantics: the value
    /// is ALWAYS written (last-writer-wins = the root-CAS winner), whether or not
    /// the term already existed. Mirrors owned `upsert` (which always writes and
    /// returns whether the term was newly inserted).
    ///
    /// Returns `Ok(true)` iff the term was newly inserted (`false` = updated an
    /// existing term).
    pub fn upsert_cas_durable(&self, term: &str, value: u64) -> Result<bool> {
        use super::nodes::persistent_node::PersistentCharNode;
        use std::sync::atomic::Ordering;

        match self.durability_policy {
            DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit => {}
            DurabilityPolicy::Periodic | DurabilityPolicy::None => {
                return Err(PersistentARTrieError::InvalidOperation(
                    "upsert_cas_durable requires Immediate or GroupCommit durability so an \
                     acknowledged write is guaranteed durable before it becomes visible"
                        .to_string(),
                ));
            }
        }

        let lockfree_root = self.lockfree_root.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
            )
        })?;

        let chars: Vec<u32> = term.chars().map(|c| c as u32).collect();
        if chars.is_empty() {
            return Ok(false);
        }
        if value > LOCKFREE_COUNTER_MAX {
            return Err(Self::lockfree_increment_overflow_error(term, None, value));
        }

        // UPSERT returns whether the term was NEWLY inserted: consult the trie
        // (fault-in) BEFORE the write so the return value is correct. This read is
        // advisory for the return flag only (the write is unconditional), so a race
        // is harmless (the CAS is the linearization point).
        let existed = {
            let _epoch = self.epoch_manager.enter_read();
            match self.find_leaf_faulting(lockfree_root, &chars, DEFAULT_MAX_FAULTIN_RETRIES) {
                Ok(found) => found.is_some(),
                Err(_) => self.find_leaf_lockfree(lockfree_root, &chars).is_some(),
            }
        };

        // ORDER A — step 1: append + sync the Upsert record DURABLE.
        let value_bytes = crate::serialization::bincode_compat::serialize(&value).map_err(|e| {
            PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
        })?;
        let lsn = self.append_to_wal_returning_lsn(WalRecord::Upsert {
            term: term.as_bytes().to_vec(),
            value: value_bytes,
        })?;

        // Step 2: publish via the single-root-CAS arbiter (always writes the value).
        let _epoch = self.epoch_manager.enter_read();
        loop {
            let root = match lockfree_root.load() {
                Some(r) => r,
                None => {
                    let new_root = Arc::new(PersistentCharNode::<u64>::new());
                    let _ = lockfree_root.try_init(new_root);
                    continue;
                }
            };
            let new_root = match self.build_value_path_recursive(&root, &chars, 0, value) {
                Some(r) => r,
                None => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    return Err(PersistentARTrieError::internal(
                        "upsert_cas_durable: could not fault an evicted prefix in to publish the \
                         valued leaf; the Upsert record is durable and replays on reopen",
                    ));
                }
            };
            // GENERATION (§3.6): the published-root version (S2/FIX-A; commit_seq at S4).
            let generation = new_root.version();
            match lockfree_root.compare_exchange(&root, new_root) {
                Ok(_) => {
                    if let Some(ref cache) = self.lockfree_cache {
                        cache.insert(term.to_string(), true);
                    }
                    let rank_lsn = self.append_commit_rank(lsn, term.as_bytes(), generation)?;
                    self.committed_watermark.mark_committed(lsn);
                    self.committed_watermark.mark_committed(rank_lsn);
                    return Ok(!existed);
                }
                Err(_actual) => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    /// Path-copy the `root`→leaf spine for `chars`, finalizing the leaf with
    /// `value`. Returns a new root `Arc` (the published-version candidate) or
    /// `None` if an on-disk child blocks the copy (cannot be faulted in here).
    ///
    /// Mirrors the membership `build_path_recursive`, but instead of returning the
    /// shared leaf for a later `try_set_final`, it bakes `as_final().with_value`
    /// into the leaf so finalization+value publish atomically with the root CAS
    /// (single-phase). For an existing path this replaces the leaf's value
    /// (last-writer = the CAS winner); for a new path it creates the spine.
    fn build_value_path_recursive(
        &self,
        node: &Arc<super::nodes::persistent_node::PersistentCharNode<u64>>,
        chars: &[u32],
        depth: usize,
        value: u64,
    ) -> Option<Arc<super::nodes::persistent_node::PersistentCharNode<u64>>> {
        use super::nodes::persistent_node::{Child, PersistentCharNode};

        if depth == chars.len() {
            // Reached the leaf: bake finality + the new value into a fresh copy.
            return Some(Arc::new(node.as_final().with_value(value)));
        }

        let key = chars[depth];
        match node.find_child(key) {
            Some(child) => {
                // In-memory child: path-copy into it.
                if let Some(child_arc) = child.as_in_mem() {
                    let child_arc = Arc::clone(child_arc);
                    let new_child =
                        self.build_value_path_recursive(&child_arc, chars, depth + 1, value)?;
                    return Some(Arc::new(node.with_child(key, Child::InMem(new_child))));
                }
                // WRITE-PATH FAULT-IN (design §4): the child was EVICTED to OnDisk.
                // Fault it back in then descend, splicing it InMem — the single root
                // CAS in `try_increment_cas` stays the sole arbiter. This also fixes
                // the PRE-EXISTING infinite-spin: previously an OnDisk child returned
                // `None` → `try_increment_cas` looped forever re-reading the same
                // OnDisk root. Now we fault it in (the read step already published a
                // faulted root, so on the retry this slot is InMem). On I/O error we
                // return `None` (transient Conflict → bounded by the read step's own
                // fault-in + retries; never an unbounded spin). Flip F0: un-gated to
                // production (fixes the pre-existing OnDisk infinite-spin on the
                // counter write path).
                {
                    let on_disk = child.as_on_disk().filter(|p| !p.is_null())?;
                    let loaded = self.load_overlay_node_from_disk(on_disk).ok()?;
                    let new_child =
                        self.build_value_path_recursive(&loaded, chars, depth + 1, value)?;
                    Some(Arc::new(node.with_child(key, Child::InMem(new_child))))
                }
                // Pre-flip production fallback (commented out, not deleted — F0
                // reversibility): returned `None` for an OnDisk child (which spun
                // the counter write path forever).
                // #[cfg(not(any(test, feature = "bench-internals")))]
                // {
                //     None
                // }
            }
            None => {
                // Child absent: build the remaining spine bottom-up, valued leaf
                // at the bottom.
                let leaf = Arc::new(
                    PersistentCharNode::<u64>::new()
                        .as_final()
                        .with_value(value),
                );
                let mut current = leaf;
                for &c in chars[depth + 1..].iter().rev() {
                    let parent =
                        PersistentCharNode::<u64>::new().with_child(c, Child::InMem(current));
                    current = Arc::new(parent);
                }
                Some(Arc::new(node.with_child(key, Child::InMem(current))))
            }
        }
    }

    /// Lock-free increment: create path if needed, then add `delta`.
    ///
    /// Panics if the checked counter domain would be exceeded. Use
    /// [`Self::try_increment_cas`] to handle overflow as a recoverable error.
    pub fn increment_cas(&self, key: &str, delta: u64) -> u64 {
        self.try_increment_cas(key, delta)
            .unwrap_or_else(|error| panic!("lock-free increment_cas failed: {}", error))
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

        let entries = {
            let root_node = match self.lockfree_root.as_ref() {
                Some(root) => match root.load() {
                    Some(node) => node,
                    None => return Ok(0),
                },
                None => return Ok(0),
            };

            let mut entries = Vec::new();
            let mut key_buf = Vec::new();
            Self::collect_lockfree_value_entries_recursive(&root_node, &mut key_buf, &mut entries)?;
            entries
        };

        if entries.is_empty() {
            return Ok(0);
        }

        let (wal_entries, prepared_values) = self.prepare_lockfree_value_merge(&entries)?;
        let merged_count = wal_entries.len();

        // G-MERGE (S3): this drain-to-owned-tree BatchIncrement is intentionally
        // UNRANKED — unlike `try_increment_cas_durable` (an Order-A concurrent producer
        // that ranks its delta), this is a non-Order-A `&mut self` batch drain whose
        // single record replays in LSN order = its single-threaded commit order, so it
        // needs no CommitRank under the Owned reconcile. It is the ONE remaining
        // unranked durable record. ⚠️ S4/Overlay: an Overlay-regime reconcile DROPS
        // unranked records, so this drain must NOT run on (or be excluded from) an
        // Overlay-regime file, else a legitimately-acked drain is silently dropped.
        self.append_to_wal(WalRecord::BatchIncrement {
            entries: wal_entries,
        })?;

        for (term, value) in prepared_values {
            self.try_insert_impl_no_wal_with_value(&term, value)?;
        }

        // Clear the lock-free layer
        if let Some(ref cache) = self.lockfree_cache {
            cache.clear();
        }
        if let Some(ref root) = self.lockfree_root {
            root.store(Arc::new(PersistentCharNode::<u64>::new()));
        }

        Ok(merged_count)
    }

    fn prepare_lockfree_value_merge(
        &self,
        entries: &[(String, u64)],
    ) -> Result<(Vec<(Vec<u8>, i64)>, Vec<(String, u64)>)> {
        let mut wal_entries = Vec::with_capacity(entries.len());
        let mut prepared_values = Vec::with_capacity(entries.len());

        for (term, delta) in entries {
            let delta_i64 = Self::lockfree_delta_to_i64(term, *delta)?;
            let current = self.current_i64_for_lockfree_merge(term)?;
            let new_value = current.checked_add(delta_i64).ok_or_else(|| {
                PersistentARTrieError::InvalidOperation(format!(
                    "lock-free merge increment overflow for term {:?}: {} + {} exceeds i64 range",
                    term, current, delta_i64
                ))
            })?;
            let value = Self::value_from_i64_for_lockfree_merge(new_value)?;

            wal_entries.push((term.as_bytes().to_vec(), delta_i64));
            prepared_values.push((term.clone(), value));
        }

        Ok((wal_entries, prepared_values))
    }

    fn current_i64_for_lockfree_merge(&self, term: &str) -> Result<i64> {
        // The persistent value is `u64`; widen to `i64` for the running sum
        // (the lock-free domain is bounded by `LOCKFREE_COUNTER_MAX = i64::MAX`).
        // `get` yields `Option<&u64>`, so dereference before the conversion.
        match self.get(term) {
            Some(&value) => i64::try_from(value).map_err(|_| {
                PersistentARTrieError::InvalidOperation(format!(
                    "persistent counter value for term {:?} exceeds i64 merge domain: {}",
                    term, value
                ))
            }),
            None => Ok(0),
        }
    }

    fn value_from_i64_for_lockfree_merge(value: i64) -> Result<u64> {
        u64::try_from(value).map_err(|_| {
            PersistentARTrieError::InvalidOperation(format!(
                "lock-free merged counter value is negative or out of u64 range: {}",
                value
            ))
        })
    }

    fn lockfree_delta_to_i64(term: &str, delta: u64) -> Result<i64> {
        i64::try_from(delta).map_err(|_| {
            PersistentARTrieError::InvalidOperation(format!(
                "lock-free counter value for term {:?} exceeds i64 persistence domain: {}",
                term, delta
            ))
        })
    }

    fn collect_lockfree_value_entries_recursive(
        lockfree_node: &Arc<super::nodes::persistent_node::PersistentCharNode<u64>>,
        key_buf: &mut Vec<u32>,
        entries: &mut Vec<(String, u64)>,
    ) -> Result<usize> {
        let mut count = 0;

        if lockfree_node.is_final() {
            if let Some(delta) = lockfree_node.get_value() {
                entries.push((Self::chars_to_string(key_buf)?, delta));
                count += 1;
            }
        }

        for (&child_key, child) in lockfree_node.iter_children() {
            // Skip on-disk refs in the lock-free overlay; recurse into in-memory
            // children (borrowed owned `Arc`, no `unsafe`).
            if let Some(child_arc) = child.as_in_mem() {
                let child_arc = Arc::clone(child_arc);
                key_buf.push(child_key);
                count +=
                    Self::collect_lockfree_value_entries_recursive(&child_arc, key_buf, entries)?;
                key_buf.pop();
            }
        }

        Ok(count)
    }

    fn chars_to_string(chars: &[u32]) -> Result<String> {
        let mut term = String::with_capacity(chars.len());
        for &code in chars {
            let c = char::from_u32(code).ok_or_else(|| {
                PersistentARTrieError::InvalidOperation(format!(
                    "lock-free overlay contained invalid Unicode scalar value: {}",
                    code
                ))
            })?;
            term.push(c);
        }
        Ok(term)
    }

    fn lockfree_increment_overflow_error(
        key: &str,
        current: Option<u64>,
        delta: u64,
    ) -> PersistentARTrieError {
        PersistentARTrieError::InvalidOperation(format!(
            "lock-free increment overflow for term {:?}: current {:?} + {} exceeds i64 persistence domain",
            key, current, delta
        ))
    }
}

#[cfg(test)]
mod reclaim_tests {
    //! Phase-A leak-detection tests for the lock-free overlay (the `Child`-enum fix).
    //!
    //! These prove that superseded (path-copied) node versions are **reclaimed**
    //! via ordinary `Arc` refcounting — the property the `Child` leak-fix restored.
    //! Before the fix, in-memory children were smuggled through `SwizzledPtr`'s
    //! `u64` via `Arc::into_raw`; because that `u64` has no `Drop`, a dropped node
    //! version never decremented its children, so **every superseded subtree
    //! leaked**. With owned `Child::InMem(Arc<…>)` children, dropping a node
    //! version drops its children's `Arc`s, so a node is freed exactly when no live
    //! version references it.
    //!
    //! The witness is `Arc::strong_count` on a leaf the test retains: after the
    //! whole overlay is dropped, only the test's handle may reference the leaf
    //! (count == 1). Under the old smuggling design, dropped node versions leaked
    //! their `+1` on the leaf, leaving `strong_count > 1` — so these tests FAIL
    //! against the pre-fix code and PASS after it. They live in-crate because the
    //! overlay root (`lockfree_root`) is `pub(crate)`.

    use crate::persistent_artrie_char::nodes::persistent_node::PersistentCharNode;
    use crate::persistent_artrie_char::PersistentARTrieChar;
    use std::sync::Arc;

    /// Build a lock-free overlay trie on the real-disk scratch dir
    /// (`target/test-tmp`) — NEVER `/tmp`, which is tmpfs (RAM) on this host.
    fn lockfree_trie(prefix: &str) -> (tempfile::TempDir, PersistentARTrieChar<()>) {
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp");
        let path = dir.path().join("overlay.artc");
        let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create trie");
        trie.enable_lockfree();
        (dir, trie)
    }

    /// Walk the live overlay root down a code-point path, returning an owned `Arc`
    /// clone of the node reached (every edge must be an in-memory child).
    fn walk_to(trie: &PersistentARTrieChar<()>, path: &str) -> Arc<PersistentCharNode> {
        let mut node = trie
            .lockfree_root
            .as_ref()
            .expect("lock-free enabled")
            .load()
            .expect("non-null overlay root");
        for c in path.chars() {
            let next = node
                .find_child(c as u32)
                .unwrap_or_else(|| panic!("missing child {c:?} while walking {path:?}"))
                .as_in_mem()
                .unwrap_or_else(|| panic!("child {c:?} is on-disk while walking {path:?}"))
                .clone();
            node = next;
        }
        node
    }

    #[test]
    fn superseded_overlay_nodes_are_reclaimed_not_leaked() {
        let (_dir, trie) = lockfree_trie("overlay-reclaim");

        // Each insert shares — and thus path-copies and supersedes — the "a"
        // subtree, creating several superseded node versions that must reclaim.
        for term in ["ab", "ac", "ad", "ae"] {
            trie.insert_cas(term);
        }

        // Own an `Arc` to the "ab" leaf: root -'a'-> n_a -'b'-> leaf.
        let held_leaf = walk_to(&trie, "ab");
        assert!(
            Arc::strong_count(&held_leaf) >= 2,
            "the live overlay and our handle must both reference the leaf; got {}",
            Arc::strong_count(&held_leaf)
        );

        // Dropping the trie drops the overlay root and every node version.
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

    #[test]
    fn many_supersessions_over_a_deep_path_do_not_accumulate_leaks() {
        let (_dir, trie) = lockfree_trie("overlay-reclaim-deep");

        // A deep shared spine "abcd" plus many siblings branching off every level
        // forces repeated multi-level path-copies of the whole spine.
        trie.insert_cas("abcd");
        for sib in [
            "abce", "abcf", "abda", "abea", "acaa", "adaa", "aeaa", "afaa", "agaa", "ahaa",
        ] {
            trie.insert_cas(sib);
        }
        for extra in ["abcda", "abcdb", "abcdc", "abcdd", "abcde"] {
            trie.insert_cas(extra);
        }

        // Own the deep "abcd" leaf, which survived many supersessions of its spine.
        let held_leaf = walk_to(&trie, "abcd");
        assert!(Arc::strong_count(&held_leaf) >= 2);

        drop(trie);

        assert_eq!(
            Arc::strong_count(&held_leaf),
            1,
            "deep leaf over-retained after drop (strong_count {}): a superseded \
             spine version leaked a reference",
            Arc::strong_count(&held_leaf)
        );
    }

    #[test]
    fn reclaim_leaves_the_live_overlay_correct() {
        // Sanity: the reclamation does not corrupt the live structure — every
        // inserted term is still found, and a non-inserted one is not.
        let (_dir, trie) = lockfree_trie("overlay-reclaim-correct");
        let terms = ["ab", "ac", "ad", "ae", "abcd", "abce"];
        for t in terms {
            trie.insert_cas(t);
        }
        for t in terms {
            assert!(trie.contains_lockfree(t), "term {t:?} must be present");
        }
        assert!(!trie.contains_lockfree("zzz"));
        assert!(!trie.contains_lockfree("a"));
    }
}

#[cfg(test)]
mod eviction_primitive_tests {
    //! **Migration Phase D — eviction via CAS + reclamation over immutable nodes.**
    //!
    //! The eviction primitive: CAS-replace an in-memory child slot
    //! (`Child::InMem(Arc<…>)`) with its on-disk reference
    //! (`Child::OnDisk(SwizzledPtr)` — the cached last-checkpoint location), which
    //! drops the in-memory subtree from the published tree. These tests prove its
    //! two safety properties with `Arc::strong_count` witnesses:
    //!
    //! 1. **No leak:** once every root version that referenced the evicted subtree
    //!    drops, the subtree's `Arc` refcount falls to the test's lone handle (the
    //!    owned-`Arc` reclamation from Phase A, now driving eviction).
    //! 2. **No use-after-free:** a concurrent reader holding the PRE-eviction root
    //!    snapshot still safely sees the subtree in memory (the old root keeps it
    //!    alive until that reader drops), exactly as `arc-swap`'s `load_full`
    //!    pins a snapshot.
    //!
    //! Integrating real per-node disk locations (so the `OnDisk` ref points at the
    //! evicted subtree's actual checkpoint slot) and fault-in-on-read are wired
    //! with the Phase-E default flip, where the overlay becomes the read/write
    //! path and faulting is required regardless.

    use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
    use crate::persistent_artrie::NodeType;
    use crate::persistent_artrie_char::nodes::persistent_node::Child;
    use std::sync::Arc;

    // G1: pin the generic overlay node/pointer to the default `<()>` membership
    // instantiation so bare `::new()` resolves (E0283 otherwise).
    type PersistentCharNode =
        crate::persistent_artrie_char::nodes::persistent_node::PersistentCharNode<()>;
    type AtomicNodePtr = crate::persistent_artrie_char::nodes::atomic_ptr::AtomicNodePtr<()>;

    #[test]
    fn evict_in_memory_child_to_on_disk_reclaims_subtree_without_uaf() {
        // Build root -'a'-> n_a (an in-memory subtree: a final node with a child).
        let leaf = Arc::new(PersistentCharNode::new().as_final());
        let n_a = Arc::new(PersistentCharNode::new().with_child(b'x' as u32, Child::InMem(leaf)));
        let root_v0 = Arc::new(
            PersistentCharNode::new().with_child('a' as u32, Child::InMem(Arc::clone(&n_a))),
        );
        let slot = AtomicNodePtr::new(Arc::clone(&root_v0));

        // A concurrent reader's snapshot of the PRE-eviction root.
        let reader_snapshot = slot.load().expect("load root");
        assert!(
            Arc::strong_count(&n_a) >= 2,
            "n_a referenced by the published root plus our handle"
        );

        // EVICT: CAS the root to a version whose 'a' child is an ON-DISK reference
        // (the cached checkpoint location), dropping the in-memory n_a from the
        // published tree.
        let disk_ref = SwizzledPtr::on_disk(7, 4096, NodeType::CharNode4);
        let root_v1 = Arc::new(root_v0.with_child('a' as u32, Child::OnDisk(disk_ref)));
        slot.compare_exchange(&root_v0, root_v1)
            .expect("eviction CAS succeeds (no concurrent writer)");

        // (a) The newly-published root carries an ON-DISK child at 'a'.
        let published = slot.load().expect("load published root");
        assert!(
            published
                .find_child('a' as u32)
                .expect("'a' present")
                .is_on_disk(),
            "the evicted child must be an on-disk reference in the published tree"
        );

        // (b) NO UAF: the reader's pre-eviction snapshot still safely sees n_a in
        // memory (the old root keeps the subtree alive).
        assert!(
            reader_snapshot
                .find_child('a' as u32)
                .expect("'a' in snapshot")
                .as_in_mem()
                .is_some(),
            "the pre-eviction reader must still observe the in-memory subtree"
        );

        // (c) NO LEAK: drop every root version that referenced n_a in memory; the
        // evicted subtree then reclaims down to our lone handle.
        drop(reader_snapshot);
        drop(root_v0);
        assert_eq!(
            Arc::strong_count(&n_a),
            1,
            "evicted in-memory subtree must reclaim once all referencing roots drop; \
             strong_count {} > 1 means eviction leaked the subtree",
            Arc::strong_count(&n_a)
        );
    }
}

#[cfg(test)]
mod durable_write_tests {
    //! **Migration Phase E — Order-A durable write path (`insert_cas_durable`).**
    //!
    //! The headline durability property (the #41-closed witness): a term inserted
    //! via `insert_cas_durable` and acknowledged (`Ok(true)`) survives a reopen
    //! **with no checkpoint at all** — durability rests entirely on the WAL record
    //! that was synced BEFORE the write became visible (Order A). On reopen the
    //! WAL replays the `Insert` into the recovered tree. Scratch is real disk
    //! (`target/test-tmp`), never `/tmp` (tmpfs).

    use crate::persistent_artrie_char::PersistentARTrieChar;
    use crate::persistent_artrie_core::durability::DurabilityPolicy;
    // `MappedDictionary` brings `get_value` into scope for the counter Order-A
    // increment durability witness (`try_increment_cas_durable_*`).
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

    #[test]
    fn insert_cas_durable_survives_reopen_without_checkpoint() {
        let dir = scratch("order-a-durable");
        let path = dir.path().join("t.artc");
        let terms = ["apple", "apricot", "banana", "band", "bandana", "cherry"];

        {
            let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            for (i, t) in terms.iter().enumerate() {
                assert!(
                    trie.insert_cas_durable(t).expect("durable insert"),
                    "{t:?} is a new term"
                );
                // The committed watermark advances to cover each appended LSN
                // (LSNs start at 1, so after i+1 inserts the watermark is ≥ i+1).
                assert!(
                    trie.committed_watermark.watermark() >= (i as u64 + 1),
                    "watermark must cover {} committed LSNs, got {}",
                    i + 1,
                    trie.committed_watermark.watermark()
                );
            }
            // A duplicate returns Ok(false) and does not regress the watermark.
            assert!(!trie
                .insert_cas_durable("apple")
                .expect("dup durable insert"));
            // DROP WITHOUT CHECKPOINT — durability rests entirely on the WAL.
        }

        // Reopen: every durably-logged insert must replay into the recovered tree.
        let trie = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        for t in terms {
            assert!(
                Dictionary::contains(&trie, t),
                "durably-inserted term {t:?} lost after reopen-without-checkpoint (Order-A broken)"
            );
        }
        assert!(!Dictionary::contains(&trie, "never-inserted"));
    }

    #[test]
    fn insert_cas_durable_rejects_non_synchronous_policy() {
        let dir = scratch("order-a-reject");
        let path = dir.path().join("t.artc");
        let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::None);
        trie.enable_lockfree();
        // `None` cannot guarantee acknowledged⇒durable, so the durable path must
        // refuse it rather than silently weaken the invariant.
        assert!(
            trie.insert_cas_durable("x").is_err(),
            "insert_cas_durable must reject a non-synchronous durability policy"
        );
    }

    // ──────────────────── R-B (proven overlay DELETE) ────────────────────

    /// The R-B durable remove rejects a non-synchronous policy EXACTLY as the
    /// durable insert/increment paths do (the durable entry points agree).
    #[test]
    fn remove_cas_durable_rejects_non_synchronous_policy() {
        let dir = scratch("rb-remove-reject");
        let path = dir.path().join("t.artc");
        let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Periodic);
        trie.enable_lockfree();
        assert!(
            trie.remove_cas_durable("x").is_err(),
            "remove_cas_durable must reject a non-synchronous durability policy"
        );
    }

    /// Single-thread durable remove round-trip. Insert durably, remove durably
    /// (Ok(true) — cleared a present term, cache invalidated so `contains_lockfree`
    /// reports absent), remove again (Ok(false) — already absent, NO new WAL hole),
    /// then reopen WITH NO CHECKPOINT: the removed term must stay absent (the
    /// `Remove` record replays over the recovered tree) while a co-inserted,
    /// never-removed term survives.
    #[test]
    fn remove_cas_durable_clears_and_survives_reopen_without_checkpoint() {
        let dir = scratch("rb-remove-roundtrip");
        let path = dir.path().join("t.artc");

        {
            let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();

            // Insert "apple" and "apricot" (shared "ap" prefix), then remove
            // "apple" — "apricot" must remain reachable (subtree retained).
            assert!(trie.insert_cas_durable("apple").expect("durable insert"));
            assert!(trie.insert_cas_durable("apricot").expect("durable insert"));
            assert!(trie.contains_lockfree("apple"));
            assert!(trie.contains_lockfree("apricot"));

            let wm_before_remove = trie.committed_watermark.watermark();

            // Remove a PRESENT term → Ok(true); the positive cache MUST be
            // invalidated so the term reads absent immediately (the §3.4 guard).
            assert!(
                trie.remove_cas_durable("apple").expect("durable remove"),
                "removing a present term returns Ok(true)"
            );
            assert!(
                !trie.contains_lockfree("apple"),
                "removed term must read ABSENT — stale positive cache would resurrect it"
            );
            assert!(
                trie.contains_lockfree("apricot"),
                "the shared-prefix sibling must survive the remove (subtree retained)"
            );
            // The Remove appended exactly one LSN; the watermark advanced past it.
            assert!(
                trie.committed_watermark.watermark() > wm_before_remove,
                "the durable Remove must advance the committed watermark"
            );

            // Removing an ABSENT term → Ok(false) and NO watermark hole: a no-op
            // remove must not append a WAL record at all.
            let wm_before_noop = trie.committed_watermark.watermark();
            assert!(
                !trie.remove_cas_durable("apple").expect("idempotent remove"),
                "removing an already-absent term returns Ok(false)"
            );
            assert!(
                !trie.remove_cas_durable("never-present").expect("absent remove"),
                "removing a never-present term returns Ok(false)"
            );
            assert_eq!(
                trie.committed_watermark.watermark(),
                wm_before_noop,
                "a no-op remove must NOT append a WAL record / advance the watermark"
            );
            // DROP WITHOUT CHECKPOINT — durability rests entirely on the WAL.
        }

        // Reopen: the durable Remove replays over the recovered tree, so "apple"
        // is gone; "apricot" (never removed) survives.
        let trie = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        assert!(
            !Dictionary::contains(&trie, "apple"),
            "durably-removed term \"apple\" reappeared after reopen (Order-A remove broken)"
        );
        assert!(
            Dictionary::contains(&trie, "apricot"),
            "co-inserted, never-removed term \"apricot\" lost after reopen"
        );
    }

    /// `try_increment_cas_durable` (the counter Order-A path): each durably-
    /// acknowledged delta survives a reopen WITH NO CHECKPOINT, replayed from the
    /// delta-based `BatchIncrement` WAL records. The reopened counts equal the
    /// summed deltas — the #41-closed witness for the counter overlay.
    #[test]
    fn try_increment_cas_durable_survives_reopen_without_checkpoint() {
        let dir = scratch("order-a-incr-durable");
        let path = dir.path().join("t.artc");
        // (key, number of +delta steps, delta) → expected = steps*delta.
        let plan: [(&str, u64, u64); 4] = [
            ("apple", 3, 1),
            ("apricot", 2, 10),
            ("band", 1, 7),
            ("cherry", 4, 25),
        ];

        {
            let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            let mut expected_watermark = 0u64;
            for (key, steps, delta) in plan {
                let mut last = 0;
                for _ in 0..steps {
                    last = trie
                        .try_increment_cas_durable(key, delta)
                        .expect("durable increment");
                    // Each durable increment appends exactly one BatchIncrement
                    // LSN; the contiguous watermark must cover every one of them.
                    expected_watermark += 1;
                    assert!(
                        trie.committed_watermark.watermark() >= expected_watermark,
                        "watermark must cover {expected_watermark} durable increments, got {}",
                        trie.committed_watermark.watermark()
                    );
                }
                assert_eq!(last, steps * delta, "live overlay count for {key:?}");
            }
            // DROP WITHOUT CHECKPOINT — durability rests entirely on the WAL.
        }

        // Reopen: the summed deltas must replay into the recovered tree.
        let trie = PersistentARTrieChar::<u64>::open(&path).expect("reopen");
        for (key, steps, delta) in plan {
            assert_eq!(
                trie.get_value(key),
                Some(steps * delta),
                "durably-incremented {key:?} lost/wrong after reopen-without-checkpoint (Order-A increment broken)"
            );
        }
        assert_eq!(trie.get_value("never-incremented"), None);
    }

    /// S3 hazard-D control (the distinguishing case): a `V=u64` key touched by BOTH a
    /// ranked overwrite (`insert_cas_with_value_durable`) AND a `try_increment_cas_durable`
    /// must recover COMMIT-ORDERED after reopen. Here the increment commits FIRST and
    /// the set OVERWRITES it last ⇒ the recovered value MUST be the set value (5), not
    /// set+delta (12). The 3 seed writes push the increment's data LSN (=7) ABOVE the
    /// later set's published-root version (=5) — the magnitude inversion that makes an
    /// UNRANKED increment (keyed by its lsn) wrongly sort AFTER the set. S3 ranks the
    /// increment in the same `root.version` domain, so it sorts BEFORE the set (gen 4 <
    /// 5) and the set wins. This test FAILS (k=12) without S3's increment-rank.
    #[test]
    fn s3_increment_then_set_same_key_set_wins_after_reopen() {
        let dir = scratch("s3-inc-then-set");
        let path = dir.path().join("t.artc");
        {
            let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            // Advance the LSN past the root.version domain (each durable write burns 2
            // LSNs but bumps root.version by 1), so the increment's data LSN exceeds the
            // later set's published-root version.
            for k in ["aa", "bb", "cc"] {
                trie.insert_cas_with_value_durable(k, 1).expect("seed");
            }
            // increment THEN set on the same key: the SET is the last writer. Use
            // UPSERT (always-write) — `insert_cas_with_value_durable` is insert-only and
            // would skip a key already made present by the increment.
            trie.try_increment_cas_durable("k", 7).expect("increment");
            trie.upsert_cas_durable("k", 5).expect("set");
            // DROP WITHOUT CHECKPOINT — WAL-only durability.
        }
        let trie = PersistentARTrieChar::<u64>::open(&path).expect("reopen");
        assert_eq!(
            trie.get_value("k"),
            Some(5),
            "increment-then-set: the SET must win (k=5). An UNRANKED increment (keyed \
             by its larger lsn) would sort after the set → k=12 (hazard D)"
        );
    }

    /// S3 coverage twin: set THEN increment ⇒ the increment accumulates onto the set.
    #[test]
    fn s3_set_then_increment_same_key_accumulates_after_reopen() {
        let dir = scratch("s3-set-then-inc");
        let path = dir.path().join("t.artc");
        {
            let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            trie.insert_cas_with_value_durable("k", 5).expect("set");
            trie.try_increment_cas_durable("k", 1).expect("increment");
            // DROP WITHOUT CHECKPOINT.
        }
        let trie = PersistentARTrieChar::<u64>::open(&path).expect("reopen");
        assert_eq!(
            trie.get_value("k"),
            Some(6),
            "set(5) then +1 must recover commit-ordered as 6"
        );
    }

    /// The counter Order-A path rejects a non-synchronous policy, exactly as the
    /// membership path does (the two durable entry points agree).
    #[test]
    fn try_increment_cas_durable_rejects_non_synchronous_policy() {
        let dir = scratch("order-a-incr-reject");
        let path = dir.path().join("t.artc");
        let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Periodic);
        trie.enable_lockfree();
        assert!(
            trie.try_increment_cas_durable("x", 1).is_err(),
            "try_increment_cas_durable must reject a non-synchronous durability policy"
        );
    }

    /// Concurrent soak: many threads durably-insert disjoint keys under shared-
    /// prefix CAS contention (WAL-only — no checkpoint, per the safety boundary).
    /// Every acknowledged key MUST survive a reopen via WAL replay — the
    /// #41-closed property under concurrency.
    #[test]
    fn concurrent_durable_writers_all_survive_reopen() {
        let dir = scratch("order-a-soak");
        let path = dir.path().join("t.artc");
        let n_threads = 6;
        let per_thread = 100;

        let acknowledged: Vec<String> = {
            let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
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
                            let key = format!("p{t}_{i:04}");
                            if trie.insert_cas_durable(&key).expect("durable insert") {
                                acked.push(key);
                            }
                        }
                        acked
                    })
                })
                .collect();

            let acked: Vec<String> = handles
                .into_iter()
                .flat_map(|h| h.join().expect("writer thread"))
                .collect();
            // DROP WITHOUT CHECKPOINT — durability rests entirely on the WAL.
            drop(trie);
            acked
        };

        assert_eq!(
            acknowledged.len(),
            n_threads * per_thread,
            "every distinct durable key must be newly acknowledged exactly once"
        );

        // Reopen: every acknowledged key must replay from the WAL.
        let trie = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        for key in &acknowledged {
            assert!(
                Dictionary::contains(&trie, key),
                "acknowledged durable key {key:?} lost after concurrent-write reopen (Order-A broken)"
            );
        }
        assert!(!Dictionary::contains(&trie, "never-acknowledged"));
    }

    /// **RB5 — durable MIXED insert/remove soak (the R-B analogue of
    /// `concurrent_durable_writers_all_survive_reopen`).** N threads concurrently
    /// insert AND remove both DISJOINT (per-thread) and SHARED keys under Immediate
    /// durability (WAL-only — no checkpoint, per the safety boundary). After the
    /// chaotic concurrent phase quiesces, the LIVE overlay membership is the ground
    /// truth (the net of every acknowledged op under the root-CAS linearization);
    /// we snapshot it, drop WITHOUT a checkpoint, reopen, and assert the recovered
    /// live set EQUALS that snapshot EXACTLY — every net insert survived (Order-A
    /// durable + replay) and every net remove stayed removed (the `Remove` record
    /// replays over the recovered tree; REC-A). A torn state (a removed key
    /// resurrected, or a present key lost) on reopen would fail.
    #[test]
    fn concurrent_durable_mixed_insert_remove_reopen_equals_live_set() {
        // Immediate-durability variant (the original RB5 soak). OD5 runs this
        // ≥50× green under the wrapped runner.
        run_mixed_insert_remove_soak("rb-mixed-soak", |trie| {
            trie.set_durability_policy(DurabilityPolicy::Immediate);
        });
    }

    /// **OD5 GroupCommit twin** of the mixed insert/remove soak. Identical body,
    /// but durability is `GroupCommit` (the rank append is coalesced through the
    /// group-commit coordinator, still durable-before-ack). Gated on the
    /// `group-commit` feature. Proves the Order-A replay-order fix holds under the
    /// batched-fsync policy too, not just `Immediate`.
    #[cfg(feature = "group-commit")]
    #[test]
    fn concurrent_durable_mixed_insert_remove_reopen_equals_live_set_group_commit() {
        use crate::persistent_artrie::group_commit::GroupCommitConfig;
        run_mixed_insert_remove_soak("rb-mixed-soak-gc", |trie| {
            trie.set_durability_policy(DurabilityPolicy::GroupCommit);
            trie.enable_group_commit(GroupCommitConfig::default())
                .expect("enable group commit");
        });
    }

    /// Shared body for the mixed insert/remove soak (no-drift between the
    /// `Immediate` and `GroupCommit` variants). `configure` installs the
    /// durability policy (and, for the GroupCommit twin, the coordinator) on the
    /// freshly-created trie BEFORE `enable_lockfree`.
    fn run_mixed_insert_remove_soak(
        prefix: &str,
        configure: impl Fn(&mut PersistentARTrieChar<()>),
    ) {
        let dir = scratch(prefix);
        let path = dir.path().join("t.artc");
        let n_threads = 6;
        let per_thread = 80;
        // The shared key pool every thread contends insert-vs-remove on.
        let shared: Vec<String> = (0..40).map(|i| format!("s{:03}", i)).collect();

        let live_snapshot: std::collections::BTreeSet<String> = {
            let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create");
            configure(&mut trie);
            trie.enable_lockfree();
            let trie = Arc::new(trie);
            let barrier = Arc::new(Barrier::new(n_threads));

            let handles: Vec<_> = (0..n_threads)
                .map(|t| {
                    let trie = Arc::clone(&trie);
                    let barrier = Arc::clone(&barrier);
                    let shared = shared.clone();
                    thread::spawn(move || {
                        barrier.wait();
                        // Disjoint per-thread keys: insert then (for odd i) remove,
                        // so each thread's net is deterministic but still exercises
                        // the durable remove path heavily.
                        for i in 0..per_thread {
                            let key = format!("d{t}_{i:04}");
                            trie.insert_cas_durable(&key).expect("durable insert");
                            if i % 3 == 0 {
                                trie.remove_cas_durable(&key).expect("durable remove");
                            }
                        }
                        // Shared keys: all threads contend insert-vs-remove (the
                        // chaotic, interleaving-dependent part).
                        for (i, k) in shared.iter().enumerate() {
                            if (i + t) % 2 == 0 {
                                trie.insert_cas_durable(k).expect("durable insert");
                            } else {
                                trie.remove_cas_durable(k).expect("durable remove");
                            }
                        }
                    })
                })
                .collect();
            for h in handles {
                h.join().expect("worker thread");
            }

            // ── QUIESCENCE ── the live overlay is now the ground-truth net set.
            // Reclaim the trie (all worker Arcs dropped at join) to read + drop it.
            let trie = Arc::try_unwrap(trie)
                .unwrap_or_else(|_| panic!("outstanding trie references after join"));

            // Snapshot the live membership over every key the workers touched.
            let mut snapshot = std::collections::BTreeSet::new();
            for t in 0..n_threads {
                for i in 0..per_thread {
                    let key = format!("d{t}_{i:04}");
                    if trie.contains_lockfree(&key) {
                        snapshot.insert(key);
                    }
                }
            }
            for k in &shared {
                if trie.contains_lockfree(k) {
                    snapshot.insert(k.clone());
                }
            }

            // Sanity on the deterministic disjoint net: i%3==0 keys were removed,
            // the rest remain present.
            for t in 0..n_threads {
                for i in 0..per_thread {
                    let key = format!("d{t}_{i:04}");
                    let expected_present = i % 3 != 0;
                    assert_eq!(
                        snapshot.contains(&key),
                        expected_present,
                        "disjoint key {key:?} net membership wrong at quiescence"
                    );
                }
            }
            // DROP WITHOUT CHECKPOINT — durability rests entirely on the WAL.
            drop(trie);
            snapshot
        };

        // Reopen: the recovered live set must EQUAL the pre-drop snapshot exactly.
        let trie = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        // (a) Every net-present key survived.
        for key in &live_snapshot {
            assert!(
                Dictionary::contains(&trie, key),
                "net-present key {key:?} lost after mixed-workload reopen (Order-A insert/replay broken)"
            );
        }
        // (b) Every touched-but-net-absent key stayed absent (no resurrection).
        for t in 0..n_threads {
            for i in 0..per_thread {
                let key = format!("d{t}_{i:04}");
                if !live_snapshot.contains(&key) {
                    assert!(
                        !Dictionary::contains(&trie, &key),
                        "net-removed key {key:?} resurrected after reopen (Order-A remove/replay broken)"
                    );
                }
            }
        }
        for k in &shared {
            assert_eq!(
                Dictionary::contains(&trie, k),
                live_snapshot.contains(k),
                "shared key {k:?} reopen membership disagrees with the quiesced live net"
            );
        }
        assert!(!Dictionary::contains(&trie, "never-touched"));
    }

    // ====================================================================
    // OD4 — DETERMINISTIC s019 regression (Order-A replay-order fix, C′).
    //
    // Forces the s019 interleaving with a controlled scheduler (the test-only
    // `commit_rendezvous` hooks): the Insert APPENDS FIRST (lower LSN) but its
    // visibility CAS lands LAST; the Remove APPENDS SECOND (higher LSN) but its
    // CAS lands FIRST. So the WAL physical/LSN order is `Insert@lsnI,
    // Remove@lsnR` with lsnI < lsnR, while the CAS/visibility last-writer is the
    // Insert ⇒ the quiesced overlay is PRESENT. The PUBLISHED-ROOT versions make
    // the Insert's commit GENERATION strictly greater than the Remove's.
    //
    // Drop WITHOUT a checkpoint and reopen: recovery MUST reconstruct PRESENT.
    // With OD2's CommitRank append in place, `reconcile_lww` orders by generation
    // ⇒ the Insert wins ⇒ present (PASS). With the rank append reverted, recovery
    // falls back to LSN order ⇒ the higher-LSN Remove wins ⇒ ABSENT (FAIL).
    //
    // DIFFERENTIAL CONFIRMED (OD4): reverting the four `append_commit_rank(...)`
    // calls in `insert_cas_durable`/`remove_cas_durable` to `rank_lsn = lsn` makes
    // `replay_orders_by_commit_rank_not_lsn` FAIL ("s019 LOST after reopen") and
    // the resurrection twin FAIL ("s019 RESURRECTED"); restoring the rank append
    // makes both PASS. The differential proves the tests have teeth.
    //
    // GENERATION SOURCE (the §3.6 fix): the commit generation is the
    // PUBLISHED-ROOT `version` (bumped by the spine path-copy on EVERY
    // publication, fixed at the root CAS), NOT the leaf version — the insert
    // finalize is an in-place `try_set_final` that does NOT bump the leaf, so an
    // insert re-finalizing a leaf a remove cleared would otherwise TIE the
    // remove's generation and lose this race even WITH CommitRank present.
    // ====================================================================

    /// Shared scheduler state for the OD4 rendezvous. `i_appended` is raised by
    /// the insert thread once its data record is durable; `r_committed` is raised
    /// by the remove thread once its clear CAS has won. The condvar wakes the
    /// waiter on each transition.
    struct S019Sched {
        state: std::sync::Mutex<S019Flags>,
        cv: std::sync::Condvar,
    }
    #[derive(Default)]
    struct S019Flags {
        i_appended: bool,
        r_committed: bool,
    }
    impl S019Sched {
        fn new() -> Arc<Self> {
            Arc::new(S019Sched {
                state: std::sync::Mutex::new(S019Flags::default()),
                cv: std::sync::Condvar::new(),
            })
        }
        fn set_i_appended(&self) {
            self.state.lock().expect("lock").i_appended = true;
            self.cv.notify_all();
        }
        fn set_r_committed(&self) {
            self.state.lock().expect("lock").r_committed = true;
            self.cv.notify_all();
        }
        fn wait_i_appended(&self) {
            let mut g = self.state.lock().expect("lock");
            while !g.i_appended {
                g = self.cv.wait(g).expect("wait");
            }
        }
        fn wait_r_committed(&self) {
            let mut g = self.state.lock().expect("lock");
            while !g.r_committed {
                g = self.cv.wait(g).expect("wait");
            }
        }
    }

    /// Stage the s019 interleaving on a shared trie and return the path. The trie
    /// is dropped WITHOUT a checkpoint inside (durability rests on the WAL).
    fn stage_s019(prefix: &str) -> tempfile::TempDir {
        use super::{set_commit_rendezvous, RendezvousPhase};

        let dir = scratch(prefix);
        let path = dir.path().join("t.artc");
        {
            let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();

            // Pre-seed "s019" PRESENT (committed), then drop ONLY its positive
            // cache entry: the overlay still holds it final (so the remove's
            // presence precheck finds it), but the insert thread's fast-path cache
            // check will MISS and proceed to append (so we get a real Insert
            // record with a lower LSN than the Remove).
            trie.insert_cas_durable("s019").expect("seed insert");
            trie.lockfree_cache
                .as_ref()
                .expect("cache enabled")
                .remove("s019");

            let trie = Arc::new(trie);
            let sched = S019Sched::new();

            // INSERT thread: appends first (lower LSN), parks post-append until the
            // remove has committed, THEN its CAS lands last (higher generation).
            let ti = {
                let trie = Arc::clone(&trie);
                let sched = Arc::clone(&sched);
                thread::spawn(move || {
                    let s = Arc::clone(&sched);
                    set_commit_rendezvous(Some(Box::new(move |phase| {
                        if phase == RendezvousPhase::AfterAppend {
                            // Data durable: announce, then block so the remove's
                            // CAS lands before ours.
                            s.set_i_appended();
                            s.wait_r_committed();
                        }
                    })));
                    let r = trie.insert_cas_durable("s019").expect("durable insert");
                    set_commit_rendezvous(None);
                    r
                })
            };

            // REMOVE thread: waits until the insert has appended (so the remove's
            // append gets the HIGHER LSN), then runs to completion; its CAS lands
            // first and signals the insert to proceed.
            let tr = {
                let trie = Arc::clone(&trie);
                let sched = Arc::clone(&sched);
                thread::spawn(move || {
                    sched.wait_i_appended();
                    let s = Arc::clone(&sched);
                    set_commit_rendezvous(Some(Box::new(move |phase| {
                        if phase == RendezvousPhase::AfterCommit {
                            s.set_r_committed();
                        }
                    })));
                    let r = trie.remove_cas_durable("s019").expect("durable remove");
                    set_commit_rendezvous(None);
                    r
                })
            };

            let _i_added = ti.join().expect("insert thread");
            let _r_removed = tr.join().expect("remove thread");

            // QUIESCED: the overlay's committed-visible state is PRESENT (the
            // insert's CAS was the last writer).
            let trie =
                Arc::try_unwrap(trie).unwrap_or_else(|_| panic!("outstanding trie refs"));
            assert!(
                trie.contains_lockfree("s019"),
                "pre-drop: s019 must be PRESENT (insert is the CAS last-writer); \
                 the staging did not realize the s019 interleaving"
            );
            // DROP WITHOUT CHECKPOINT — durability is WAL-only.
            drop(trie);
        }
        dir
    }

    /// THE OD4 regression: after the s019 interleaving + drop-no-checkpoint +
    /// reopen, the net-present key MUST be recovered present. Fails pre-OD2 (rank
    /// reverted ⇒ LSN-order replay drops it); passes post-OD2.
    #[test]
    fn replay_orders_by_commit_rank_not_lsn() {
        let dir = stage_s019("od4-s019-present");
        let path = dir.path().join("t.artc");
        let trie = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        assert!(
            Dictionary::contains(&trie, "s019"),
            "s019 LOST after reopen: replay used LSN order (Remove@higher-LSN won) \
             instead of commit generation — the Order-A replay-order bug"
        );
    }

    /// FIX-A (S2) regression: a cache-cold IDEMPOTENT insert reaches the
    /// `AlreadyExists` arm and ranks its `CommitRank` with the OBSERVED-root version
    /// (the present-root version), which is `<` a subsequent real remove's published
    /// version — so after drop-no-checkpoint + reopen the term is recovered ABSENT
    /// (the remove sorts last and wins), NOT resurrected. Exercises the idempotent
    /// arm's observed-version path end-to-end through WAL replay. (The fully
    /// concurrent observe-stale-snapshot race that further distinguishes FIX-A from a
    /// second-load/global-claim rank is proven by the version-chain argument in
    /// docs/design/dg-recon-commitseq-stamp-seed-step.md §11; staging it
    /// deterministically needs finer interleaving control than the OD4 harness
    /// exposes and is deferred to the S4 Overlay-drop gate.)
    #[test]
    fn fixa_idempotent_cache_cold_observed_version_then_remove_stays_absent() {
        let dir = scratch("fixa-observed-absent");
        let path = dir.path().join("t.artc");
        {
            let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            // Seed "obs" PRESENT (newly inserted).
            assert!(
                trie.insert_cas_durable("obs").expect("seed insert"),
                "seed must be newly inserted"
            );
            // Drop ONLY its positive-cache entry so the next insert MISSES the
            // fast-path, appends, and reaches the idempotent AlreadyExists arm (the
            // term is still final in the overlay).
            trie.lockfree_cache
                .as_ref()
                .expect("cache enabled")
                .remove("obs");
            // Idempotent insert: cache-cold ⇒ AlreadyExists arm ⇒ Ok(false). FIX-A
            // ranks this with the OBSERVED-root version (where "obs" is present).
            assert!(
                !trie.insert_cas_durable("obs").expect("idempotent insert"),
                "the cache-cold re-insert must be a NO-OP (idempotent AlreadyExists arm)"
            );
            // A real remove publishes a strictly-higher version (v_rem > v_obs).
            assert!(
                trie.remove_cas_durable("obs").expect("remove"),
                "remove must clear a present 'obs'"
            );
            drop(trie); // DROP WITHOUT CHECKPOINT — durability is WAL-only.
        }
        // Reopen: pure WAL replay. The idempotent insert's OBSERVED (lower) version
        // sorts BEFORE the remove's higher version ⇒ obs stays ABSENT.
        let trie = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        assert!(
            !Dictionary::contains(&trie, "obs"),
            "RESURRECTION: the idempotent insert out-ranked the remove — obs was \
             wrongly recovered present"
        );
    }

    /// Resurrection-polarity twin: the same controlled scheduler but the net op is
    /// a REMOVE — the Insert APPENDS SECOND (higher LSN) yet the Remove's CAS lands
    /// LAST (higher generation), so the quiesced overlay is ABSENT. Reopen MUST NOT
    /// resurrect it. This guards the opposite direction (no false-present).
    #[test]
    fn replay_orders_by_commit_rank_not_lsn_resurrection_polarity() {
        use super::{set_commit_rendezvous, RendezvousPhase};

        let dir = scratch("od4-s019-absent");
        let path = dir.path().join("t.artc");
        {
            let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            // Seed present (so the remove can clear it), drop only the cache entry
            // so the insert thread still appends.
            trie.insert_cas_durable("s019").expect("seed insert");
            trie.lockfree_cache
                .as_ref()
                .expect("cache enabled")
                .remove("s019");

            let trie = Arc::new(trie);
            let sched = S019Sched::new();

            // REMOVE thread appends FIRST (lower LSN) but parks until the insert
            // has committed, so the remove's CAS lands LAST (higher generation) ⇒
            // net ABSENT. (i_appended/r_committed are reused as generic "first op
            // appended" / "second op committed" signals.)
            let tr = {
                let trie = Arc::clone(&trie);
                let sched = Arc::clone(&sched);
                thread::spawn(move || {
                    let s = Arc::clone(&sched);
                    set_commit_rendezvous(Some(Box::new(move |phase| {
                        if phase == RendezvousPhase::AfterAppend {
                            s.set_i_appended();
                            s.wait_r_committed();
                        }
                    })));
                    trie.remove_cas_durable("s019").expect("durable remove");
                    set_commit_rendezvous(None);
                })
            };
            // INSERT thread appends SECOND (higher LSN); its CAS lands FIRST and
            // signals the remove to proceed.
            let ti = {
                let trie = Arc::clone(&trie);
                let sched = Arc::clone(&sched);
                thread::spawn(move || {
                    sched.wait_i_appended();
                    let s = Arc::clone(&sched);
                    set_commit_rendezvous(Some(Box::new(move |phase| {
                        if phase == RendezvousPhase::AfterCommit {
                            s.set_r_committed();
                        }
                    })));
                    trie.insert_cas_durable("s019").expect("durable insert");
                    set_commit_rendezvous(None);
                })
            };
            tr.join().expect("remove thread");
            ti.join().expect("insert thread");

            let trie =
                Arc::try_unwrap(trie).unwrap_or_else(|_| panic!("outstanding trie refs"));
            assert!(
                !trie.contains_lockfree("s019"),
                "pre-drop: s019 must be ABSENT (remove is the CAS last-writer)"
            );
            drop(trie);
        }
        let trie = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        assert!(
            !Dictionary::contains(&trie, "s019"),
            "s019 RESURRECTED after reopen: replay used LSN order (Insert@higher-LSN \
             won) instead of commit generation"
        );
    }
}

#[cfg(test)]
mod concurrent_increment_tests {
    //! **G1 path-copy increment — concurrent correctness.**
    //!
    //! The G1 rework replaced the wait-free in-place `fetch_add` (which is
    //! impossible over an *immutable* `Option<u64>` value) with a **path-copy CAS**
    //! loop: each increment loads the published root, reads the current count,
    //! builds a value-carrying leaf and a path-copied spine, and CAS-publishes the
    //! new root (the single-phase model the vocab overlay uses).
    //!
    //! ## The CAS-retry race (why no increment is lost)
    //!
    //! Two threads `T1`, `T2` increment the SAME key from a snapshot where the
    //! count is `c`. Both compute `c + 1` and build a new root off the SAME loaded
    //! root `R`. The root CAS (`ArcSwapOption::compare_and_swap`, pointer-identity
    //! on `R`) serializes them: exactly one — say `T1` — succeeds, publishing a
    //! root with count `c + 1`. `T2`'s CAS sees the published root is no longer
    //! `R`, so it FAILS, `T2` bumps `cas_retries`, loops, RE-LOADS the now-published
    //! root, RE-READS the count as `c + 1`, and publishes `c + 2`. The loser folds
    //! its delta onto the winner's value rather than clobbering it, so the final
    //! count equals the number of increments — **no lost update**. (This is the
    //! standard lock-free-counter argument; the root CAS is the linearization
    //! point.) These tests are the empirical witness: a lost update under
    //! contention would make the summed total fall short.
    //!
    //! Scratch is real disk (`target/test-tmp`), never `/tmp` (tmpfs on this host).

    use crate::persistent_artrie_char::PersistentARTrieChar;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp")
    }

    /// N threads each increment the SAME key `per_thread` times by 1. With no lost
    /// update the final count is exactly `n_threads * per_thread`. This is the
    /// direct stress of the CAS-retry race (all writers contend on one spine).
    #[test]
    fn concurrent_increments_same_key_sum_exactly() {
        let dir = scratch("lf-incr-same");
        let path = dir.path().join("t.artc");
        let n_threads = 8usize;
        let per_thread = 500u64;

        let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        trie.enable_lockfree();
        let trie = Arc::new(trie);
        let barrier = Arc::new(Barrier::new(n_threads));

        let handles: Vec<_> = (0..n_threads)
            .map(|_| {
                let trie = Arc::clone(&trie);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..per_thread {
                        trie.try_increment_cas("hot", 1).expect("increment");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("increment thread");
        }

        let expected = n_threads as u64 * per_thread;
        assert_eq!(
            trie.get_lockfree("hot"),
            Some(expected),
            "lost increment under CAS-retry contention: a path-copy loser must \
             re-read the winner's count and retry, never clobber it"
        );
        // CAS retries are expected under real contention (not asserted > 0 to avoid
        // flakiness on a fast uniprocessor), but the count MUST be exact regardless.
    }

    /// N threads increment DISTINCT keys; each key's final count is its own thread's
    /// contribution. Exercises concurrent path-copies of disjoint spines sharing the
    /// single root CAS (so distinct-key writers still serialize on the root, and the
    /// re-read-on-conflict must preserve every key's independent count).
    #[test]
    fn concurrent_increments_distinct_keys_each_exact() {
        let dir = scratch("lf-incr-distinct");
        let path = dir.path().join("t.artc");
        let n_threads = 8usize;
        let per_thread = 300u64;

        let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        trie.enable_lockfree();
        let trie = Arc::new(trie);
        let barrier = Arc::new(Barrier::new(n_threads));

        let handles: Vec<_> = (0..n_threads)
            .map(|t| {
                let trie = Arc::clone(&trie);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    let key = format!("k{t}");
                    for _ in 0..per_thread {
                        trie.try_increment_cas(&key, 1).expect("increment");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("increment thread");
        }

        for t in 0..n_threads {
            assert_eq!(
                trie.get_lockfree(&format!("k{t}")),
                Some(per_thread),
                "distinct-key count must equal its writer's contribution; a \
                 conflicting path-copy must not drop a sibling key's value"
            );
        }
    }

    /// Mixed deltas (not just +1) on a shared key still sum exactly — guards the
    /// `cur.checked_add(delta)` read-modify-write under contention.
    #[test]
    fn concurrent_increments_mixed_deltas_sum_exactly() {
        let dir = scratch("lf-incr-mixed");
        let path = dir.path().join("t.artc");
        let n_threads = 6usize;
        let per_thread = 200u64;

        let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        trie.enable_lockfree();
        let trie = Arc::new(trie);
        let barrier = Arc::new(Barrier::new(n_threads));

        // Thread t adds delta (t+1) each iteration → total = per_thread * Σ(t+1).
        let handles: Vec<_> = (0..n_threads)
            .map(|t| {
                let trie = Arc::clone(&trie);
                let barrier = Arc::clone(&barrier);
                let delta = (t + 1) as u64;
                thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..per_thread {
                        trie.try_increment_cas("acc", delta).expect("increment");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("increment thread");
        }

        let expected: u64 = per_thread * (1..=n_threads as u64).sum::<u64>();
        assert_eq!(
            trie.get_lockfree("acc"),
            Some(expected),
            "mixed-delta concurrent increments must sum exactly (no lost RMW)"
        );
    }
}
