//! Char seam impl of the shared [`LockFreeOverlay`] flip + thin inherent
//! wrappers preserving the char public surface.
//!
//! The overlay-flip genericization (`docs/design/overlay-flip-genericization.md`
//! §2, Step 1) extracted the lock-free-overlay flip (route + read-engine +
//! flip/kill-switch + reestablish) into the SHARED GENERIC
//! [`LockFreeOverlay`](crate::persistent_artrie_core::overlay::flip::LockFreeOverlay)
//! trait in `persistent_artrie_core::overlay::flip`. This module now holds only:
//!
//! 1. a re-export of [`OverlayWriteMode`] (hoisted to
//!    `persistent_artrie_core::overlay::write_mode`) so the many internal `use
//!    super::overlay_write_mode::OverlayWriteMode` sites still resolve;
//! 2. the char SEAM impl `impl LockFreeOverlay<CharKey, V, S> for
//!    PersistentARTrieChar<V, S>` (the per-variant owned readers / overlay
//!    publishers / WAL accessors / `CounterValue = u64`);
//! 3. thin inherent wrappers (`route_overlay`/`flip_to_overlay`/
//!    `kill_switch_to_owned`/`set_overlay_write_mode`/`overlay_eligible_v`) that
//!    DELEGATE to the trait, so the ~40 existing inherent-syntax call sites
//!    (`self.route_overlay()`, …) keep compiling and behaving IDENTICALLY.
//!
//! **Byte-identical guarantee.** The trait bodies are a token-for-token port of
//! the char originals; only the two boundary conversions changed (the overlay
//! read engine accumulates `Vec<u32>` and the seam converts via
//! `CharKey::units_to_term`/`units_from_str`, both defined to reproduce the exact
//! char behavior — `units_to_term` IS `char::from_u32(_).unwrap_or('\u{FFFD}')`
//! per unit). The existing E1 correspondence suite + the full nextest run are the
//! oracle.

// Re-export so `super::overlay_write_mode::OverlayWriteMode` resolves everywhere
// it was used before the hoist (ctors, persist, atomic_ops, document_tx, …).
pub(crate) use crate::persistent_artrie_core::overlay::write_mode::OverlayWriteMode;

// Only the feature-OFF eligibility branch uses TypeId (the feature-ON branch is
// `true`), so gate the import to avoid an unused-import warning when the feature is on.
#[cfg(not(feature = "overlay-arbitrary-v"))]
use std::any::TypeId;
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
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::persistent_artrie_core::wal::{Lsn, RankRegime, WalRecord};
use crate::value::DictionaryValue;

use super::persist::CheckpointSnapshot;
use super::types::CharTrieRoot;

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
    fn overlay_write_mode(&self) -> OverlayWriteMode {
        self.overlay_write_mode
    }

    #[inline]
    fn set_overlay_write_mode(&mut self, mode: OverlayWriteMode) {
        self.overlay_write_mode = mode;
    }

    #[inline]
    fn enable_lockfree(&mut self) {
        // Delegate to the existing inherent `enable_lockfree` (lockfree_cas.rs),
        // which sets up the `AtomicNodePtr` root + cache and stamps the WAL Overlay
        // regime on an EMPTY WAL. Unchanged behavior.
        super::PersistentARTrieChar::enable_lockfree(self)
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

    fn wal_stamp_owned_regime(&self) {
        if let Some(ref writer) = self.wal_writer {
            if let Err(e) = writer.set_owned_regime() {
                log::warn!("kill_switch_to_owned: could not stamp Owned regime: {:?}", e);
            }
        }
    }

    /// **S5-12 (V-1) + F2 (G5)** — overlay eligibility.
    ///
    /// With the `overlay-arbitrary-v` Cargo feature, ANY `V: DictionaryValue` is
    /// eligible — the generic value path (F0 durable write / F1 reestablish + read)
    /// routes arbitrary `V` through the overlay. WITHOUT it (the production default —
    /// the F2 flip "lands dark"), only the original `{u64, ()}` monomorphs are
    /// eligible, so production behavior is BYTE-IDENTICAL until the feature is
    /// deliberately enabled (the owner-gated production activation — design F2).
    /// `DictionaryValue: 'static` ⇒ `TypeId` is callable.
    fn overlay_eligible_v() -> bool {
        #[cfg(feature = "overlay-arbitrary-v")]
        {
            true
        }
        #[cfg(not(feature = "overlay-arbitrary-v"))]
        {
            TypeId::of::<V>() == TypeId::of::<u64>() || TypeId::of::<V>() == TypeId::of::<()>()
        }
    }

    // ---- UN-ROUTED owned readers (D1 — read the OWNED tree directly) ----

    fn owned_first_units(&self) -> Result<(Vec<u32>, bool)> {
        // Disjoint first-code-point cover. D1: `owned_iter_prefix("")` is the
        // UN-routed owned reader (it walks `self.root`, never the overlay), so it is
        // safe even when the trie is already in overlay-write mode (the reestablish
        // caller flips before dispatching).
        use std::collections::BTreeSet;
        let mut first_units: BTreeSet<u32> = BTreeSet::new();
        let mut has_empty_term = false;
        if let Some(all_terms) = self.owned_iter_prefix("")? {
            for term in &all_terms {
                match term.chars().next() {
                    Some(c) => {
                        first_units.insert(c as u32);
                    }
                    None => has_empty_term = true,
                }
            }
        }
        Ok((first_units.into_iter().collect(), has_empty_term))
    }

    fn owned_units_under(&self, prefix: &[u32]) -> Result<Option<Vec<Vec<u32>>>> {
        // D1: UN-routed owned reader. Convert the single-unit prefix and each
        // recovered term to/from `Vec<u32>` via the `CharKey` boundary so the
        // generic fold publishes the SAME terms the char originals did.
        let prefix_str = CharKey::units_to_term(prefix);
        Ok(self.owned_iter_prefix(&prefix_str)?.map(|terms| {
            terms
                .iter()
                .map(|t| CharKey::units_from_str(t).into_vec())
                .collect()
        }))
    }

    fn owned_units_with_values_under(
        &self,
        prefix: &[u32],
    ) -> Result<Option<Vec<(Vec<u32>, V)>>> {
        // D1: UN-routed owned reader.
        let prefix_str = CharKey::units_to_term(prefix);
        Ok(self
            .owned_iter_prefix_with_values(&prefix_str)?
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|(t, v)| (CharKey::units_from_str(&t).into_vec(), v))
                    .collect()
            }))
    }

    fn owned_has_empty_term_value(&self) -> Option<V> {
        // D1: UN-routed owned reader (`owned_get` reads `self.root`).
        self.owned_get("").cloned()
    }

    fn clear_owned(&mut self) {
        self.root = CharTrieRoot::Empty;
        self.len.store(0, Ordering::Release);
    }

    // ---- overlay publishers (the per-variant write seam) ----

    fn overlay_publish_membership(&self, units: &[u32]) {
        // No-WAL CAS insert (recovered terms are already durable; re-logging would
        // double-log). Unchanged from the char membership reestablish.
        let term = CharKey::units_to_term(units);
        self.insert_cas(&term);
    }

    fn overlay_publish_counter(&self, units: &[u32], value: u64) {
        // `V == u64` in this routed branch (the counter reestablish runs only for
        // the u64 monomorph via the dispatch). SAFE `Any` downcast to the nameable
        // `<u64, S>` monomorph, then the no-WAL `increment_cas` — the same pattern
        // as the char `overlay_get_value`/`reestablish_overlay_dispatch`.
        use std::any::Any;
        let term = CharKey::units_to_term(units);
        if let Some(trie_u64) =
            (self as &dyn Any).downcast_ref::<super::PersistentARTrieChar<u64, S>>()
        {
            trie_u64.increment_cas(&term, value);
        }
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

    fn overlay_value_get(&self, units: &[u32]) -> Option<V> {
        // Non-faulting leaf value read (exact: overlay finals never evicted in prod).
        let lockfree_root = self.lockfree_root.as_ref()?;
        let _epoch = self.epoch_manager.enter_read();
        self.find_leaf_lockfree(lockfree_root, units)
            .and_then(|leaf| leaf.get_value())
    }

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

    // ---- increment value-domain seam (char `u64`; byte will reject negative i64) ----

    fn bound_increment_delta(&self, key: &str, delta: u64) -> Result<i64> {
        // Byte-identical to the char `try_increment_cas_durable` up-front bound:
        // reject > LOCKFREE_COUNTER_MAX, then `i64::try_from` into the WAL delta
        // domain (the WAL increment domain is `i64` for every variant).
        if delta > super::lockfree_cas::LOCKFREE_COUNTER_MAX {
            return Err(super::PersistentARTrieChar::<u64, S>::lockfree_increment_overflow_error(
                key, None, delta,
            ));
        }
        i64::try_from(delta).map_err(|_| {
            crate::persistent_artrie_core::error::PersistentARTrieError::InvalidOperation(format!(
                "try_increment_cas_durable delta for term {:?} exceeds i64 persistence domain: {}",
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
        // (the same zero-`unsafe` pattern as `overlay_publish_counter`). The
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
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
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
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
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
                "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
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
        self.eviction_coordinator.is_some()
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

    #[inline]
    fn capture_owned_snapshot(&self) -> Result<CheckpointSnapshot> {
        self.capture_snapshot()
    }

    #[inline]
    fn publish_owned_and_reclaim(&self, snapshot: CheckpointSnapshot) -> Result<()> {
        self.publish_durable_and_reclaim(snapshot)
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

    /// **Restart-time kill-switch setter.** Thin delegator to the seam.
    // S5-12 flip API: exercised by tests; the production caller is the owner-gated
    // flip (not yet wired), so allow dead_code in non-test builds only.
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn set_overlay_write_mode(&mut self, mode: OverlayWriteMode) {
        <Self as LockFreeOverlay<CharKey, V, S>>::set_overlay_write_mode(self, mode)
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

    /// **Kill-switch — one-release fallback.** Thin delegator to
    /// [`LockFreeOverlay::kill_switch_to_owned`]. Kept `pub` (external callers).
    #[inline]
    pub fn kill_switch_to_owned(&mut self) {
        <Self as LockFreeOverlay<CharKey, V, S>>::kill_switch_to_owned(self)
    }
}

#[cfg(test)]
mod tests {
    use super::OverlayWriteMode;

    #[test]
    fn default_is_owned_tree_and_inert() {
        // The scaffold MUST default to the proven owned path and report that the
        // overlay is not in use — proving it changes no current behavior.
        assert_eq!(OverlayWriteMode::default(), OverlayWriteMode::OwnedTree);
        assert!(!OverlayWriteMode::default().uses_overlay());
    }

    #[test]
    fn overlay_variant_reports_overlay() {
        assert!(OverlayWriteMode::LockFreeOverlay.uses_overlay());
    }

    /// S5-10c: `flip_to_overlay` makes `route_overlay()` true (overlay is the live
    /// write target); `kill_switch_to_owned` reverts it to the owned path.
    #[test]
    fn flip_to_overlay_then_kill_switch_round_trips_route_overlay() {
        use crate::persistent_artrie_char::PersistentARTrieChar;
        std::fs::create_dir_all("target/test-tmp").ok();
        let dir = tempfile::Builder::new()
            .prefix("flip-helper")
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp");
        let path = dir.path().join("t.artc");
        let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create");

        // Post-flip: `create()` create-flips an eligible-V (u64) trie, so a FRESH trie
        // already routes to the overlay. Round-trip the kill-switch from there.
        assert!(
            trie.route_overlay(),
            "create-flip routes a fresh eligible-V (u64) trie to the overlay"
        );
        trie.kill_switch_to_owned();
        assert!(
            !trie.route_overlay(),
            "kill_switch_to_owned must revert to the owned path"
        );
        assert!(
            trie.flip_to_overlay(),
            "flip_to_overlay must re-engage the overlay"
        );
        assert!(trie.route_overlay());
    }

    /// S5-12 (V-1): the TypeId gate — `overlay_eligible_v()` is true only for
    /// `{u64, ()}`, and `flip_to_overlay` is a NO-OP for arbitrary `V` (which would
    /// otherwise get a write-broken overlay). Arbitrary V stays on the owned path.
    #[test]
    fn v1_typeid_gate_flip_is_noop_for_arbitrary_v() {
        use crate::persistent_artrie_char::PersistentARTrieChar;
        assert!(PersistentARTrieChar::<u64>::overlay_eligible_v());
        assert!(PersistentARTrieChar::<()>::overlay_eligible_v());
        assert!(!PersistentARTrieChar::<String>::overlay_eligible_v());

        std::fs::create_dir_all("target/test-tmp").ok();
        let dir = tempfile::Builder::new()
            .prefix("v1-gate")
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp");
        let path = dir.path().join("t.artc");
        let mut trie = PersistentARTrieChar::<String>::create(&path).expect("create");
        assert!(
            !trie.flip_to_overlay(),
            "flip_to_overlay must be a no-op for arbitrary V"
        );
        assert!(
            !trie.route_overlay(),
            "an arbitrary-V trie must stay on the owned path (no broken overlay)"
        );
    }
}
