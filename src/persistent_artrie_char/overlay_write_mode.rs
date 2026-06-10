//! Char seam impl of the shared [`LockFreeOverlay`] flip + thin inherent
//! wrappers preserving the char public surface.
//!
//! The overlay-flip genericization (`docs/design/overlay-flip-genericization.md`
//! §2, Step 1) extracted the lock-free-overlay flip (route + read-engine + flip +
//! reestablish) into the SHARED GENERIC
//! [`LockFreeOverlay`](crate::persistent_artrie_core::overlay::flip::LockFreeOverlay)
//! trait in `persistent_artrie_core::overlay::flip`. This module now holds only:
//!
//! 1. the char SEAM impl `impl LockFreeOverlay<CharKey, V, S> for
//!    PersistentARTrieChar<V, S>` (the per-variant owned readers / overlay
//!    publishers / WAL accessors / `CounterValue = u64`);
//! 2. thin inherent wrappers (`route_overlay`/`flip_to_overlay`/
//!    `overlay_eligible_v`) that DELEGATE to the trait, so the existing
//!    inherent-syntax call sites (`self.route_overlay()`, …) keep compiling and
//!    behaving IDENTICALLY.
//!
//! **L3.3 — the overlay is the SOLE representation.** Every char constructor
//! installs the lock-free overlay, so `route_overlay()` is universally true; the
//! owned tree, the `OverlayWriteMode` kill-switch enum, and `kill_switch_to_owned`
//! were deleted.
//!
//! **Byte-identical guarantee.** The trait bodies are a token-for-token port of
//! the char originals; only the two boundary conversions changed (the overlay
//! read engine accumulates `Vec<u32>` and the seam converts via
//! `CharKey::units_to_term`/`units_from_str`, both defined to reproduce the exact
//! char behavior — `units_to_term` IS `char::from_u32(_).unwrap_or('\u{FFFD}')`
//! per unit). The existing E1 correspondence suite + the full nextest run are the
//! oracle.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::Result;
use crate::persistent_artrie_core::durability::DurabilityPolicy;
use crate::persistent_artrie_core::key_encoding::{CharKey, KeyEncoding};
use crate::persistent_artrie_core::overlay::checkpoint::OverlayCheckpoint;
use crate::persistent_artrie_core::overlay::durable_write::{
    DurableOverlayWrite, ValuePublishOutcome, ValueWriteMode,
};
// Phase 4 (DRY K-generic lift): `find_leaf_faulting` (called on the value read-fault
// path below) is now a default method of the shared `OverlayEvictable` trait.
use crate::persistent_artrie_core::overlay::evict::OverlayEvictable;
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::persistent_artrie_core::wal::{Lsn, RankRegime, WalRecord};
use crate::value::DictionaryValue;

use super::persist::CheckpointSnapshot;

// ============================================================================
// Char seam impl of the shared LockFreeOverlay flip
// ============================================================================

impl<V: DictionaryValue, S: BlockStorage> LockFreeOverlay<CharKey, V, S>
    for super::PersistentARTrieChar<V, S>
{
    /// `u64` — the char counter monomorph (byte's is `i64`).
    type CounterValue = u64;

    // ---- small accessors ----

    #[inline]
    fn lockfree_root(
        &self,
    ) -> Option<&crate::persistent_artrie_core::overlay::AtomicNodePtr<CharKey, V>> {
        // `super::nodes::AtomicNodePtr<V>` IS `overlay::AtomicNodePtr<CharKey, V>`
        // (a type alias — see `nodes::atomic_ptr`), so this borrow is identity.
        self.lockfree_root.as_ref()
    }

    #[inline]
    fn install_overlay(&mut self) {
        // Delegate to the existing inherent `install_overlay` (lockfree_cas.rs),
        // which sets up the `AtomicNodePtr` root + cache and stamps the WAL Overlay
        // regime on an EMPTY WAL. Unchanged behavior.
        super::PersistentARTrieChar::install_overlay(self)
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
                log::warn!("flip_to_overlay: could not stamp Overlay regime: {:?}", e);
            }
        }
    }

    /// **S5-12 (V-1) + F2 (G5)** — overlay eligibility: ANY `V` is eligible.
    ///
    /// Arbitrary-V overlay routing is the production default: the generic value
    /// path (F0 durable write / F1 reestablish + read) routes every `V` through
    /// the lock-free overlay. Since L3.3 deleted the owned tree, the overlay is the
    /// SOLE representation.
    fn overlay_eligible_v() -> bool {
        true
    }

    // ---- overlay publishers (the per-variant write seam) ----

    fn overlay_publish_membership(&self, units: &[u32]) {
        // No-WAL CAS insert (recovered terms are already durable; re-logging would
        // double-log). Unchanged from the char membership reestablish.
        let term = CharKey::units_to_term(units);
        self.insert_cas(&term);
    }

    fn overlay_counter_get(&self, units: &[u32]) -> Option<u64> {
        // SAFE `Any` downcast to `<u64, S>` + the lock-free point read.
        use std::any::Any;
        let term = CharKey::units_to_term(units);
        (self as &dyn Any)
            .downcast_ref::<super::PersistentARTrieChar<u64, S>>()
            .and_then(|trie_u64| trie_u64.get_lockfree(&term))
    }

    fn overlay_contains(&self, units: &[u32]) -> bool {
        let term = CharKey::units_to_term(units);
        self.contains_lockfree(&term)
    }

    fn overlay_publish_value(&self, units: &[u32], value: V) {
        // G5/F1: no-WAL path-copy value SET (recovered terms are already durable).
        // The overlay is FRESH at reestablish, so the path-copy never hits an OnDisk
        // child and the CAS contends with nothing — but the retry loop is kept for
        // uniformity with the durable publishers. `units` ARE the chars for char.
        use super::nodes::persistent_node::PersistentCharNode;
        let lockfree_root = match self.lockfree_root.as_ref() {
            Some(r) => r,
            None => return,
        };
        let _epoch = self.epoch_manager.enter_read();
        loop {
            let root = match lockfree_root.load() {
                Some(r) => r,
                None => {
                    let _ = lockfree_root.try_init(Arc::new(PersistentCharNode::<V>::new()));
                    continue;
                }
            };
            match self.build_value_path_recursive(&root, units, 0, value.clone()) {
                Some(new_root) => match lockfree_root.compare_exchange(&root, new_root) {
                    Ok(_) => {
                        if let Some(ref cache) = self.lockfree_cache {
                            cache.insert(CharKey::units_to_term(units), true);
                        }
                        return;
                    }
                    Err(_) => continue,
                },
                // OnDisk-blocked: impossible on a fresh reestablish overlay; bail.
                None => return,
            }
        }
    }

    // G5.2 (RT-1): `overlay_value_get` (the BUG #46 FAULTING fix) is now the shared
    // default on `LockFreeOverlay` (via `OverlayEvictable::find_leaf_faulting`), byte +
    // char IDENTICAL. The char seam impl is REMOVED. The prior char Err-arm fell back
    // to the non-faulting `find_leaf_lockfree`; `find_leaf_faulting` is infallible in
    // practice (it returns `Ok` on every branch, doing its own liveness walk on
    // exhaustion), so that arm was unreachable — behavior is byte-for-byte preserved.
    // Regression still pinned by tests/overlay_eviction_arbitrary_v_bug46.rs.

    fn claim_commit_seq(&self) -> u64 {
        // Empty-string support: the per-iteration commit generation — the SAME
        // `self.commit_seq` char's durable insert/increment paths claim.
        use std::sync::atomic::Ordering;
        self.commit_seq.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn note_cas_retry(&self) {
        use std::sync::atomic::Ordering;
        self.cas_retries.fetch_add(1, Ordering::Relaxed);
    }

    fn install_prebuilt_overlay_root_seam(
        &mut self,
        root: Arc<crate::persistent_artrie_core::overlay::node::OverlayNode<CharKey, V>>,
    ) {
        // `OverlayNode<CharKey, V>` IS `PersistentCharNode<V>` (a type alias — see
        // `nodes::persistent_node`), so this is an identity install. Delegates to the
        // inherent F5 helper (same module as the private `lockfree_root` field).
        self.install_prebuilt_overlay_root_inherent(root)
    }

    fn overlay_try_remove_path(&self, units: &[u32]) {
        // F5 WAL-tail Remove arm: no-WAL overlay remove via the inherent helper
        // (which uses the existing single-arbiter `try_remove_lockfree_path`).
        self.overlay_remove_no_wal(units)
    }

    fn load_root_immutable_seam(&mut self, root_ptr: u64) -> Result<bool> {
        // F7 — the char `load_root_immutable` takes `(buffer_manager, root_ptr)`. Clone
        // the `Arc<RwLock<BufferManager>>` out of `self` first to release the immutable
        // borrow before the `&mut self` call. PRECONDITION (converter): the WAL is already
        // Overlay-regime, so the V-2 install check inside passes. char's `load_root_immutable`
        // gracefully falls back to an EMPTY image on a corrupt load and returns
        // `image_loaded`, which we forward (so the converter's drain skips nothing the absent
        // image fails to cover).
        let buffer_manager = self.buffer_manager.clone().ok_or_else(|| {
            crate::persistent_artrie_core::error::PersistentARTrieError::internal(
                "F7 load_root_immutable_seam: no buffer manager",
            )
        })?;
        let (_term_count, image_loaded) = self.load_root_immutable(&buffer_manager, root_ptr)?;
        Ok(image_loaded)
    }
}

// ============================================================================
// Char seam impl of the shared DurableOverlayWrite (Order-A) skeleton
// (overlay-durable-architecture.md, trait 2). The generic defaults own the
// data-loss-critical control flow (the durability gate, the append→publish→mark
// ORDER, the commit-rank + watermark tail, the full increment template); this
// impl supplies ONLY the per-variant seams (the WAL/watermark accessors + the
// increment's u64 value-domain bound / delta-record builder / proven path-copy
// publish). Byte-identical: each seam delegates to the EXISTING char inherent
// helper the originals already called.
// ============================================================================

impl<V: DictionaryValue, S: BlockStorage> DurableOverlayWrite<CharKey, V, S>
    for super::PersistentARTrieChar<V, S>
{
    #[inline]
    fn durability_policy(&self) -> DurabilityPolicy {
        // The inherent accessor (wal_helpers.rs) — unchanged value.
        super::PersistentARTrieChar::durability_policy(self)
    }

    #[inline]
    fn append_durable_wal(&self, record: WalRecord) -> Result<Lsn> {
        // Order-A step 1: char's existing append+sync-durable helper (wal_helpers.rs).
        self.append_to_wal_returning_lsn(record)
    }

    #[inline]
    fn append_commit_rank(&self, data_lsn: Lsn, term: &[u8], generation: u64) -> Result<Lsn> {
        // Order-A step 2.5: char's existing CommitRank append (wal_helpers.rs).
        super::PersistentARTrieChar::append_commit_rank(self, data_lsn, term, generation)
    }

    #[inline]
    fn mark_committed(&self, lsn: Lsn) {
        // Order-A step 3: advance the committed watermark — char's existing field.
        self.committed_watermark.mark_committed(lsn);
    }

    // ---- increment value-domain seam (counter `u64`; byte + char identical) ----

    fn bound_increment_delta(&self, key: &str, delta: u64) -> Result<i64> {
        // A SINGLE durable `BatchIncrement` delta is carried in ONE `i64` WAL chunk,
        // so a `delta > i64::MAX` cannot be logged by one durable call (a magnitude
        // above `i64::MAX` is reachable via the merge chunker
        // `split_u64_delta_to_i64_chunks` or multiple durable increments, NOT a single
        // delta). The former `delta > LOCKFREE_COUNTER_MAX` check is gone (vacuous now
        // that `LOCKFREE_COUNTER_MAX == u64::MAX`); the `i64::try_from` reject IS the
        // real per-call WAL-delta-domain bound — FAIL LOUD rather than wrap.
        i64::try_from(delta).map_err(|_| {
            crate::persistent_artrie_core::error::PersistentARTrieError::InvalidOperation(format!(
                "try_increment_cas_durable delta for term {:?} exceeds the i64 per-call WAL delta \
                 domain: {}",
                key, delta
            ))
        })
    }

    #[inline]
    fn build_increment_record(&self, key_bytes: &[u8], bounded: i64) -> WalRecord {
        // The exact delta record the char original logged: a single-entry,
        // delta-based BatchIncrement (commutative on replay).
        WalRecord::BatchIncrement {
            entries: vec![(key_bytes.to_vec(), bounded)],
        }
    }

    fn increment_publish_inner(&self, key: &str, delta: u64) -> Result<(u64, u64)> {
        // `try_increment_cas_inner` is u64-specialized (`impl<S> ...<u64, S>`), so
        // downcast `self` to the nameable `<u64, S>` monomorph via a SAFE `Any`
        // (the same zero-`unsafe` pattern as `overlay_counter_get`). The
        // counter durable path runs only for the u64 monomorph (the value route),
        // so this downcast always succeeds there; an ineligible `V` returns the
        // empty result (the durable increment is never reached for non-u64 `V`).
        use std::any::Any;
        match (self as &dyn Any).downcast_ref::<super::PersistentARTrieChar<u64, S>>() {
            Some(trie_u64) => trie_u64.try_increment_cas_inner(key, delta),
            None => Ok((0, 0)),
        }
    }

    // ---- G5/F0 value seams (char): faulting present/read + the mode-aware
    // path-copy CAS publish. They name the concrete `OverlayNode<CharKey,V>` via
    // the (now generic) `build_value_path_recursive` / `find_leaf_*`. ----

    fn value_present_faulting(&self, key_bytes: &[u8]) -> Result<bool> {
        let term = std::str::from_utf8(key_bytes).map_err(|e| {
            crate::persistent_artrie_core::error::PersistentARTrieError::internal(format!(
                "char key not valid UTF-8: {}",
                e
            ))
        })?;
        let chars: Vec<u32> = term.chars().map(|c| c as u32).collect();
        let lockfree_root = self.lockfree_root.as_ref().ok_or_else(|| {
            crate::persistent_artrie_core::error::PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call install_overlay() first.".to_string(),
            )
        })?;
        let _epoch = self.epoch_manager.enter_read();
        // FAULTING (the valued return value must reflect a term under an evicted
        // prefix), with the in-memory fallback on I/O error (mirrors the prior
        // inline valued-insert hoist).
        Ok(
            match self.find_leaf_faulting(
                lockfree_root,
                &chars,
                super::lockfree_cas::DEFAULT_MAX_FAULTIN_RETRIES,
            ) {
                Ok(found) => found.is_some(),
                Err(_) => self.find_leaf_lockfree(lockfree_root, &chars).is_some(),
            },
        )
    }

    fn value_read_faulting(&self, key_bytes: &[u8]) -> Result<Option<V>> {
        let term = std::str::from_utf8(key_bytes).map_err(|e| {
            crate::persistent_artrie_core::error::PersistentARTrieError::internal(format!(
                "char key not valid UTF-8: {}",
                e
            ))
        })?;
        let chars: Vec<u32> = term.chars().map(|c| c as u32).collect();
        let lockfree_root = self.lockfree_root.as_ref().ok_or_else(|| {
            crate::persistent_artrie_core::error::PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call install_overlay() first.".to_string(),
            )
        })?;
        let _epoch = self.epoch_manager.enter_read();
        Ok(
            match self.find_leaf_faulting(
                lockfree_root,
                &chars,
                super::lockfree_cas::DEFAULT_MAX_FAULTIN_RETRIES,
            ) {
                Ok(found) => found.and_then(|leaf| leaf.get_value()),
                Err(_) => self
                    .find_leaf_lockfree(lockfree_root, &chars)
                    .and_then(|leaf| leaf.get_value()),
            },
        )
    }

    fn value_publish_inner(
        &self,
        key_bytes: &[u8],
        value: V,
        mode: ValueWriteMode,
    ) -> Result<ValuePublishOutcome> {
        use super::nodes::persistent_node::PersistentCharNode;
        let term = std::str::from_utf8(key_bytes).map_err(|e| {
            crate::persistent_artrie_core::error::PersistentARTrieError::internal(format!(
                "char key not valid UTF-8: {}",
                e
            ))
        })?;
        let chars: Vec<u32> = term.chars().map(|c| c as u32).collect();
        let lockfree_root = self.lockfree_root.as_ref().ok_or_else(|| {
            crate::persistent_artrie_core::error::PersistentARTrieError::InvalidOperation(
                "Lock-free mode not enabled. Call install_overlay() first.".to_string(),
            )
        })?;
        let _epoch = self.epoch_manager.enter_read();
        loop {
            // S4 commit_seq CLAIM (loop-top, re-claimed per iteration) — the winning
            // claim is strictly monotone in the global root-CAS order + durable.
            let commit_seq = self.commit_seq.fetch_add(1, Ordering::AcqRel) + 1;
            let root = match lockfree_root.load() {
                Some(r) => r,
                None => {
                    let new_root = Arc::new(PersistentCharNode::<V>::new());
                    let _ = lockfree_root.try_init(new_root);
                    continue;
                }
            };
            // Mode pre-check on the FRESHLY-loaded root (so a concurrent change
            // between the caller's initial read and this CAS is observed).
            match &mode {
                ValueWriteMode::InsertOnce => {
                    // Already final ⇒ a concurrent insert won (the caller's hoist
                    // missed it / it raced); do NOT overwrite — insert-once.
                    if self.find_leaf_recursive(&root, &chars, 0).is_some() {
                        return Ok(ValuePublishOutcome::NotApplied);
                    }
                }
                ValueWriteMode::Upsert => {}
                ValueWriteMode::CompareAndSwap { expected_bytes } => {
                    // Re-check `expected` against the current leaf value (bincode
                    // bytes, NOT PartialEq). Mismatch ⇒ the CAS fails this round.
                    let cur = self
                        .find_leaf_recursive(&root, &chars, 0)
                        .and_then(|leaf| leaf.get_value());
                    let cur_bytes = match &cur {
                        Some(v) => {
                            Some(crate::serialization::bincode_compat::serialize(v).map_err(|e| {
                                crate::persistent_artrie_core::error::PersistentARTrieError::internal(
                                    format!("Failed to serialize value: {}", e),
                                )
                            })?)
                        }
                        None => None,
                    };
                    if &cur_bytes != expected_bytes {
                        return Ok(ValuePublishOutcome::NotApplied);
                    }
                }
            }
            // Build the valued spine (clone `value` per iteration — V: Clone — since
            // build_value_path consumes it and we may retry).
            let new_root = match self.build_value_path_recursive(&root, &chars, 0, value.clone()) {
                Some(r) => r,
                // I/O error faulting an evicted prefix: the WAL record is ALREADY
                // durable, but we cannot make the write visible. Surface it (the
                // record replays on reopen) — same as the prior inline valued path.
                None => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    return Err(crate::persistent_artrie_core::error::PersistentARTrieError::internal(
                        "value_publish_inner: could not fault an evicted prefix in to publish the \
                         valued leaf; the record is durable and replays on reopen",
                    ));
                }
            };
            match lockfree_root.compare_exchange(&root, new_root) {
                Ok(_) => {
                    if let Some(ref cache) = self.lockfree_cache {
                        cache.insert(term.to_string(), true);
                    }
                    return Ok(ValuePublishOutcome::Published(commit_seq));
                }
                Err(_actual) => {
                    self.cas_retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }
}

// ============================================================================
// Char seam impl of the shared OverlayCheckpoint route-split skeleton
// (overlay-durable-architecture.md, trait 3). The generic default owns the
// RES-4 route-split DECISION (capture the LIVE representation — overlay vs owned)
// + the total-loss-guard assert; this impl supplies ONLY the per-variant capture
// + publish seams (genuinely per-variant: char arena on-disk format). Each seam
// delegates to the EXISTING char inherent method, so the route-split is
// byte-identical to the prior inherent `checkpoint()` body.
// ============================================================================

impl<V: DictionaryValue, S: BlockStorage> OverlayCheckpoint<CharKey, V, S>
    for super::PersistentARTrieChar<V, S>
{
    type CheckpointSnapshot = CheckpointSnapshot;

    #[inline]
    fn has_eviction_coordinator(&self) -> bool {
        self.eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .is_some()
    }

    #[inline]
    fn capture_overlay_snapshot(&self) -> Result<CheckpointSnapshot> {
        // The overlay arm — char's existing immutable-overlay capture (persist.rs)
        // with its data-loss-critical watermark-before-root capture ordering.
        self.capture_snapshot_immutable()
    }

    #[inline]
    fn publish_overlay_snapshot_retaining(&self, snapshot: &CheckpointSnapshot) -> Result<()> {
        self.publish_immutable_snapshot_retaining_wal(snapshot)
    }

    #[inline]
    fn publish_overlay_snapshot_retaining_with_eviction(
        &self,
        snapshot: CheckpointSnapshot,
    ) -> Result<()> {
        self.publish_immutable_snapshot_retaining_wal_with_eviction(snapshot)
    }
}

// ============================================================================
// Thin inherent wrappers — preserve the char public/`pub(crate)` surface by
// DELEGATING to the trait (so the ~40 existing inherent-syntax call sites and
// the external `route_overlay` tests keep compiling, behavior identical).
// ============================================================================

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// **Flip F0 — production-write/read-path router.** `true` iff reads/writes/
    /// checkpoint take the lock-free overlay path for this trie. Thin delegator to
    /// [`LockFreeOverlay::route_overlay`]. Kept `pub` (external tests call it).
    #[inline]
    pub fn route_overlay(&self) -> bool {
        <Self as LockFreeOverlay<CharKey, V, S>>::route_overlay(self)
    }

    /// **S5-12 (V-1)** — overlay-eligibility gate (`V ∈ {(), u64}`). Thin delegator.
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn overlay_eligible_v() -> bool {
        <Self as LockFreeOverlay<CharKey, V, S>>::overlay_eligible_v()
    }

    /// **S5-10c — flip construction helper.** Thin delegator to
    /// [`LockFreeOverlay::flip_to_overlay`].
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn flip_to_overlay(&mut self) -> bool {
        <Self as LockFreeOverlay<CharKey, V, S>>::flip_to_overlay(self)
    }
}

#[cfg(test)]
mod tests {
    /// S5-12 (V-1) + F2: `overlay_eligible_v()` is true for ALL `V` (arbitrary-V
    /// overlay routing is the default), so a fresh `create::<String>()` create-flips
    /// to the overlay.
    #[test]
    fn v1_arbitrary_v_create_flips_to_overlay() {
        use crate::persistent_artrie_char::PersistentARTrieChar;
        assert!(PersistentARTrieChar::<u64>::overlay_eligible_v());
        assert!(PersistentARTrieChar::<()>::overlay_eligible_v());
        assert!(PersistentARTrieChar::<String>::overlay_eligible_v());

        std::fs::create_dir_all("target/test-tmp").ok();
        let dir = tempfile::Builder::new()
            .prefix("v1-gate")
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp");
        let path = dir.path().join("t.artc");
        let trie = PersistentARTrieChar::<String>::create(&path).expect("create");
        assert!(
            trie.route_overlay(),
            "a String trie create-flips to the overlay (default)"
        );
    }
}
