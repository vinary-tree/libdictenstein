//! Vocab seam impls of the shared overlay traits (V1 — the overlay flip).
//!
//! Mirrors `persistent_artrie_char::overlay_write_mode`, instantiated at the CONCRETE
//! `V = u64` (the vocabulary index — vocab is not generic over its value), reusing the
//! shared Order-A durable skeleton (`DurableOverlayWrite`). Vocab's value is a
//! WRITE-ONCE id: a term gets an id once and it never changes, so only
//! `ValueWriteMode::InsertOnce` is meaningful; Upsert / CompareAndSwap are rejected.
//!
//! The vocab overlay node IS `PersistentCharNode<u64>` = `OverlayNode<CharKey, u64>`
//! (a type alias). Vocab reuses `CharKey` (the `VOCB` 96-byte file header — read via
//! `read_vocab_header` / `open_without_validation` — is independent of `K::FILE_MAGIC`,
//! so there is no char/vocab file confusion). The lock-free CAS-walk helpers
//! (`try_insert_lockfree_path` / `find_in_lockfree_trie`) live in `lockfree_cas.rs`.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::eviction::EvictionCoordinator;
use crate::persistent_artrie_core::concurrency::EpochManager;
use crate::persistent_artrie_core::durability::DurabilityPolicy;
use crate::persistent_artrie_core::key_encoding::{CharKey, KeyEncoding};
use crate::persistent_artrie_core::overlay::durable_write::{
    DurableOverlayWrite, ValuePublishOutcome, ValueWriteMode,
};
use crate::persistent_artrie_core::overlay::evict::OverlayEvictable;
use crate::persistent_artrie_core::overlay::f5_build::build_overlay_root_from_terms;
use crate::persistent_artrie_core::overlay::faulter::OverlayFaulter;
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::persistent_artrie_core::overlay::node::OverlayNode;
use crate::persistent_artrie_core::overlay::AtomicNodePtr;
use crate::persistent_artrie_core::swizzled_ptr::SwizzledPtr;
use crate::persistent_artrie_core::wal::{Lsn, RankRegime, WalRecord};
use dashmap::DashMap;

use super::dict_impl::PersistentVocabARTrie;

// The vocab overlay node alias (V = u64).
type VocabOverlayNode = OverlayNode<CharKey, u64>;

// ============================================================================
// OverlayEvictable (required supertrait of LockFreeOverlay)
// ============================================================================

impl<S: BlockStorage> OverlayEvictable<CharKey, u64, S> for PersistentVocabARTrie<S> {
    #[inline]
    fn overlay_root_slot(&self) -> Option<&AtomicNodePtr<CharKey, u64>> {
        self.lockfree_root.as_ref()
    }

    #[inline]
    fn overlay_epoch_manager(&self) -> &EpochManager {
        &self.epoch_manager
    }

    #[inline]
    fn overlay_eviction_coordinator(&self) -> Option<Arc<EvictionCoordinator>> {
        // Vocab's coordinator is a bare `Option<Arc<..>>` (no Mutex, unlike char).
        self.eviction_coordinator.as_ref().map(Arc::clone)
    }

    #[inline]
    fn note_faultin_cas(&self) {
        self.cas_retries.fetch_add(1, Ordering::Relaxed);
    }
}

// ============================================================================
// OverlayFaulter — vocab's overlay is IN-MEMORY (no overlay-node disk loader).
// ============================================================================

impl<S: BlockStorage> OverlayFaulter<CharKey, u64> for PersistentVocabARTrie<S> {
    #[inline]
    fn fault_overlay_slot(&self, _slot: &SwizzledPtr) -> Option<Arc<VocabOverlayNode>> {
        // Vocab's lock-free overlay never evicts its finals to disk in production
        // (RT5 — overlay finals are not eviction targets), so there is no overlay-node
        // disk loader. Degrade to "no child" (matches byte, which also has neither
        // eviction nor production fault-in). A real loader would only be needed if
        // vocab enabled overlay eviction — out of scope.
        None
    }
}

// ============================================================================
// LockFreeOverlay — the central flip seam (V1.5)
// ============================================================================

impl<S: BlockStorage> LockFreeOverlay<CharKey, u64, S> for PersistentVocabARTrie<S> {
    /// The vocabulary index is a `u64`.
    type CounterValue = u64;

    // route_overlay() uses the trait default (lockfree_root().is_some()) — single lock-free impl:
    // ALL production ctors (mmap + io_uring create/open) flip at construction, so the overlay is
    // the sole representation; there is no toggle path that sets lockfree_root without flipping.

    #[inline]
    fn lockfree_root(&self) -> Option<&AtomicNodePtr<CharKey, u64>> {
        self.lockfree_root.as_ref()
    }

    #[inline]
    fn enable_lockfree(&mut self) {
        // The inherent `enable_lockfree` (lockfree_cas.rs) installs the AtomicNodePtr root +
        // cache. It does NOT stamp the Overlay regime (the flip does — see `route_overlay`).
        PersistentVocabARTrie::enable_lockfree(self);
    }

    #[inline]
    fn wal_current_lsn(&self) -> Option<u64> {
        self.wal_writer.as_ref().map(|w| w.current_lsn())
    }

    #[inline]
    fn wal_is_overlay_regime(&self) -> bool {
        self.wal_writer
            .as_ref()
            .map(|w| w.rank_regime() == RankRegime::Overlay)
            .unwrap_or(false)
    }

    fn wal_stamp_overlay_regime(&self) {
        if let Some(ref writer) = self.wal_writer {
            if let Err(e) = writer.set_overlay_regime() {
                log::warn!(
                    "vocab flip_to_overlay: could not stamp Overlay regime: {:?}",
                    e
                );
            }
        }
    }

    /// Vocab's value (`u64` id) is always overlay-eligible.
    fn overlay_eligible_v() -> bool {
        true
    }

    fn overlay_publish_membership(&self, units: &[u32]) {
        // Vocab inserts ALWAYS carry a value (the id); there are no membership-only
        // inserts, so the F5 WAL-tail applier never routes a vocab term here (vocab
        // only logs `Insert{value: Some(id)}` → `overlay_publish_value`). A membership
        // publish would have no id to assign, so this is unreachable in production.
        debug_assert!(
            false,
            "vocab overlay_publish_membership: vocab inserts always carry a value=id"
        );
        let _ = units;
    }

    fn overlay_counter_get(&self, units: &[u32]) -> Option<u64> {
        // The lock-free point read returning the stored id (vocab is concrete u64,
        // so no `Any` downcast is needed — unlike the generic char/byte seam).
        let term = CharKey::units_to_term(units);
        self.get_index_lockfree(&term)
    }

    fn overlay_contains(&self, units: &[u32]) -> bool {
        let term = CharKey::units_to_term(units);
        self.get_index_lockfree(&term).is_some()
    }

    fn overlay_publish_value(&self, units: &[u32], value: u64) {
        // F5/no-WAL path-copy value SET (recovered terms are already durable). The
        // overlay is FRESH at reestablish, so the CAS contends with nothing. `units`
        // ARE the chars (CharKey). value = the id.
        let lockfree_root = match self.lockfree_root.as_ref() {
            Some(r) => r,
            None => return,
        };
        let _epoch = self.epoch_manager.enter_read();
        loop {
            let root = match lockfree_root.load() {
                Some(r) => r,
                None => {
                    let _ = lockfree_root.try_init(Arc::new(
                        crate::persistent_artrie_char::nodes::PersistentCharNode::<u64>::new(),
                    ));
                    continue;
                }
            };
            match self.try_insert_lockfree_path(&root, units, value) {
                Ok(new_root) => match lockfree_root.compare_exchange(&root, new_root) {
                    Ok(_) => {
                        if let Some(ref cache) = self.lockfree_cache {
                            cache.insert(CharKey::units_to_term(units), value);
                        }
                        return;
                    }
                    Err(_) => continue,
                },
                // Already final (recovery re-applying the same term) or OnDisk-blocked
                // (impossible on a fresh reestablish overlay): the value is already set.
                Err(_) => return,
            }
        }
    }

    fn claim_commit_seq(&self) -> u64 {
        self.commit_seq.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn note_cas_retry(&self) {
        self.cas_retries.fetch_add(1, Ordering::Relaxed);
    }

    fn install_prebuilt_overlay_root_seam(&mut self, root: Arc<VocabOverlayNode>) {
        self.install_prebuilt_overlay_root_inherent(root)
    }

    fn overlay_try_remove_path(&self, units: &[u32]) {
        // Vocab is insert-only in production (no public remove API), so the F5 Remove
        // arm is not reached for vocab. The inherent helper is a correct lock-free
        // remove kept for completeness (so a future remove stays non-blocking).
        self.overlay_remove_no_wal(units)
    }

    fn load_root_immutable_seam(&mut self, root_ptr: u64) -> Result<bool> {
        // V1: build the overlay from the loaded owned tree (vocab's reopen populates
        // self.root). V5 will make this codec-direct from the dense VOCB image.
        let (_term_count, image_loaded) = self.load_root_immutable(root_ptr)?;
        Ok(image_loaded)
    }
}

// ============================================================================
// DurableOverlayWrite — the Order-A durable skeleton seam (V1.7)
// ============================================================================

impl<S: BlockStorage> DurableOverlayWrite<CharKey, u64, S> for PersistentVocabARTrie<S> {
    #[inline]
    fn durability_policy(&self) -> DurabilityPolicy {
        self.durability_policy
    }

    #[inline]
    fn append_durable_wal(&self, record: WalRecord) -> Result<Lsn> {
        // Order-A step 1: the &self durable append + sync (V1.2).
        self.append_to_wal_returning_lsn(record)
    }

    #[inline]
    fn append_commit_rank(&self, data_lsn: Lsn, term: &[u8], generation: u64) -> Result<Lsn> {
        // Order-A step 2.5 (V1.2).
        self.append_vocab_commit_rank(data_lsn, term, generation)
    }

    #[inline]
    fn mark_committed(&self, lsn: Lsn) {
        self.committed_watermark.mark_committed(lsn);
    }

    // ---- increment seams: vocab has NO counter-increment path (ids are write-once,
    // not incremented). The durable-increment template is never invoked for vocab. ----

    fn bound_increment_delta(&self, key: &str, _delta: u64) -> Result<i64> {
        Err(PersistentARTrieError::InvalidOperation(format!(
            "vocab does not support counter increment (term {key:?}); ids are write-once"
        )))
    }

    fn build_increment_record(&self, key_bytes: &[u8], bounded: i64) -> WalRecord {
        // Unreachable for vocab (bound_increment_delta errs first), but a valid record
        // shape is required by the signature.
        WalRecord::BatchIncrement {
            entries: vec![(key_bytes.to_vec(), bounded)],
        }
    }

    fn increment_publish_inner(&self, key: &str, _delta: u64) -> Result<(u64, u64)> {
        Err(PersistentARTrieError::InvalidOperation(format!(
            "vocab does not support counter increment (term {key:?}); ids are write-once"
        )))
    }

    // ---- value seams (the core of the lock-free Order-A insert) ----

    fn value_present_faulting(&self, key_bytes: &[u8]) -> Result<bool> {
        let chars = vocab_chars(key_bytes)?;
        let lockfree_root = self.require_lockfree_root()?;
        let _epoch = self.epoch_manager.enter_read();
        // Vocab's overlay is in-memory (no eviction) → the non-faulting find suffices.
        match lockfree_root.load() {
            Some(root) => Ok(self.find_in_lockfree_trie(&root, &chars).is_some()),
            None => Ok(false),
        }
    }

    fn value_read_faulting(&self, key_bytes: &[u8]) -> Result<Option<u64>> {
        let chars = vocab_chars(key_bytes)?;
        let lockfree_root = self.require_lockfree_root()?;
        let _epoch = self.epoch_manager.enter_read();
        match lockfree_root.load() {
            Some(root) => Ok(self.find_in_lockfree_trie(&root, &chars)),
            None => Ok(None),
        }
    }

    fn value_publish_inner(
        &self,
        key_bytes: &[u8],
        value: u64,
        mode: ValueWriteMode,
    ) -> Result<ValuePublishOutcome> {
        // Vocab ids are WRITE-ONCE → only InsertOnce is meaningful.
        match mode {
            ValueWriteMode::InsertOnce => {}
            ValueWriteMode::Upsert | ValueWriteMode::CompareAndSwap { .. } => {
                return Err(PersistentARTrieError::InvalidOperation(
                    "vocab values are write-once ids; Upsert / CompareAndSwap are not supported"
                        .to_string(),
                ));
            }
        }
        let chars = vocab_chars(key_bytes)?;
        let term = std::str::from_utf8(key_bytes).map_err(|e| {
            PersistentARTrieError::internal(format!("vocab key not valid UTF-8: {e}"))
        })?;
        let lockfree_root = self.require_lockfree_root()?;
        let _epoch = self.epoch_manager.enter_read();
        loop {
            // Order-A generation CLAIM (loop-top, re-claimed per iteration): the
            // winning claim is strictly monotone in the global root-CAS order + durable.
            let commit_seq = self.commit_seq.fetch_add(1, Ordering::AcqRel) + 1;
            let root = match lockfree_root.load() {
                Some(r) => r,
                None => {
                    let _ = lockfree_root.try_init(Arc::new(
                        crate::persistent_artrie_char::nodes::PersistentCharNode::<u64>::new(),
                    ));
                    continue;
                }
            };
            // InsertOnce pre-check on the freshly-loaded root: a concurrent insert may
            // have won between the caller's present-hoist and this CAS.
            if self.find_in_lockfree_trie(&root, &chars).is_some() {
                return Ok(ValuePublishOutcome::NotApplied);
            }
            // Build the valued spine (value = id). `try_insert_lockfree_path` is
            // ALSO insert-once (Err if a racer made it final) — the second guard.
            match self.try_insert_lockfree_path(&root, &chars, value) {
                Ok(new_root) => match lockfree_root.compare_exchange(&root, new_root) {
                    Ok(_) => {
                        if let Some(ref cache) = self.lockfree_cache {
                            cache.insert(term.to_string(), value);
                        }
                        return Ok(ValuePublishOutcome::Published(commit_seq));
                    }
                    Err(_) => {
                        self.cas_retries.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                },
                // A concurrent insert finalized the term between the pre-check and the
                // build: insert-once → NotApplied.
                Err(_existing) => return Ok(ValuePublishOutcome::NotApplied),
            }
        }
    }
}

// ============================================================================
// Thin inherent wrappers — preserve a `route_overlay()` surface (delegates to
// the trait), mirroring char.
// ============================================================================

impl<S: BlockStorage> PersistentVocabARTrie<S> {
    /// `true` iff reads/writes/checkpoint take the lock-free overlay path. Thin
    /// delegator to [`LockFreeOverlay::route_overlay`].
    #[inline]
    pub fn route_overlay(&self) -> bool {
        <Self as LockFreeOverlay<CharKey, u64, S>>::route_overlay(self)
    }

    /// Flip-construction helper. Thin delegator to [`LockFreeOverlay::flip_to_overlay`].
    #[inline]
    pub fn flip_to_overlay(&mut self) -> bool {
        <Self as LockFreeOverlay<CharKey, u64, S>>::flip_to_overlay(self)
    }

    /// Require the lock-free root, else a uniform "enable_lockfree() first" error.
    #[inline]
    pub(super) fn require_lockfree_root(&self) -> Result<&AtomicNodePtr<CharKey, u64>> {
        self.lockfree_root.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
            )
        })
    }

    /// Lock-free point read returning the stored id: the cache, else the in-memory
    /// overlay walk. `None` if absent (or lock-free not enabled).
    pub(super) fn get_index_lockfree(&self, term: &str) -> Option<u64> {
        if let Some(ref cache) = self.lockfree_cache {
            if let Some(e) = cache.get(term) {
                return Some(*e);
            }
        }
        let lockfree_root = self.lockfree_root.as_ref()?;
        let _epoch = self.epoch_manager.enter_read();
        let root = lockfree_root.load()?;
        let chars: Vec<u32> = term.chars().map(|c| c as u32).collect();
        self.find_in_lockfree_trie(&root, &chars)
    }

    /// Install a prebuilt overlay root (F5 reestablish / reopen): replace the lock-free
    /// root slot with one holding `root`, ensuring the cache exists.
    pub(super) fn install_prebuilt_overlay_root_inherent(&mut self, root: Arc<VocabOverlayNode>) {
        self.lockfree_root = Some(AtomicNodePtr::new(root));
        if self.lockfree_cache.is_none() {
            self.lockfree_cache = Some(DashMap::new());
        }
    }

    /// Overlay remove (the F5 Remove arm). Vocab is INSERT-ONLY — it collects a corpus
    /// vocabulary for language modeling and never deletes terms — so no Remove WAL
    /// records are ever written and this is unreachable. No-op.
    pub(super) fn overlay_remove_no_wal(&self, _units: &[u32]) {
        debug_assert!(
            false,
            "vocab overlay_remove_no_wal: vocab is insert-only (terms are never deleted)"
        );
    }

    /// Order-A step 1: append `record` to the WAL (`&self`) + sync per the durability
    /// policy, returning the appended LSN.
    pub(super) fn append_to_wal_returning_lsn(&self, record: WalRecord) -> Result<Lsn> {
        let wal = self.wal_writer.as_ref().ok_or_else(|| {
            PersistentARTrieError::Wal(
                "vocab WAL writer unavailable for durable append".to_string(),
            )
        })?;
        let lsn = wal
            .append(record)
            .map_err(|e| PersistentARTrieError::Wal(format!("vocab WAL append failed: {e}")))?;
        self.next_lsn.fetch_max(lsn + 1, Ordering::AcqRel);
        match self.durability_policy {
            DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit => {
                let synced = wal.sync().map_err(|e| {
                    PersistentARTrieError::Wal(format!("vocab WAL sync failed: {e}"))
                })?;
                self.synced_lsn.fetch_max(synced, Ordering::AcqRel);
            }
            DurabilityPolicy::Periodic | DurabilityPolicy::None => {}
        }
        Ok(lsn)
    }

    /// Order-A step 2.5: append a `CommitRank` binding the durable data record at
    /// `data_lsn` to its commit `generation` (`reconcile_lww` orders replay by it).
    pub(super) fn append_vocab_commit_rank(
        &self,
        data_lsn: Lsn,
        term: &[u8],
        generation: u64,
    ) -> Result<Lsn> {
        self.append_to_wal_returning_lsn(WalRecord::CommitRank {
            data_lsn,
            term: term.to_vec(),
            generation,
        })
    }

    /// Build the overlay root from the on-disk image (the F7 reopen seam).
    ///
    /// **V1:** enumerates `(term, id)` from the loaded owned tree (vocab's reopen
    /// populates `self.root` before the overlay is built) and builds the overlay via
    /// the shared `build_overlay_root_from_terms`. **V5** makes this codec-direct
    /// (a single dense-image walk from `root_ptr`, no owned tree) once the owned tree
    /// is deleted. `root_ptr == 0` ⇒ an empty overlay.
    pub(super) fn load_root_immutable(&mut self, root_ptr: u64) -> Result<(usize, bool)> {
        if root_ptr == 0 {
            let empty = build_overlay_root_from_terms::<CharKey, u64, _>(
                Vec::<(Vec<u32>, Option<u64>)>::new(),
                None,
            );
            self.install_prebuilt_overlay_root_inherent(empty);
            return Ok((0, false));
        }
        // Collect the term set first (releases the `iter_terms` borrow before the
        // `&mut self` install). The per-term `get_index` is a one-time reopen cost; V5
        // makes it a single codec walk.
        let all_terms: Vec<String> = self.iter_terms().collect();
        let mut terms: Vec<(Vec<u32>, Option<u64>)> = Vec::with_capacity(all_terms.len());
        let mut empty_term: Option<Option<u64>> = None;
        for term in &all_terms {
            if let Some(id) = self.get_index(term) {
                if term.is_empty() {
                    empty_term = Some(Some(id));
                } else {
                    terms.push((term.chars().map(|c| c as u32).collect(), Some(id)));
                }
            }
        }
        let count = terms.len() + usize::from(empty_term.is_some());
        let overlay_root = build_overlay_root_from_terms::<CharKey, u64, _>(terms, empty_term);
        self.install_prebuilt_overlay_root_inherent(overlay_root);
        Ok((count, true))
    }
}

/// Decode raw UTF-8 key bytes into vocab's `u32` char units.
#[inline]
fn vocab_chars(key_bytes: &[u8]) -> Result<Vec<u32>> {
    let term = std::str::from_utf8(key_bytes)
        .map_err(|e| PersistentARTrieError::internal(format!("vocab key not valid UTF-8: {e}")))?;
    Ok(term.chars().map(|c| c as u32).collect())
}
