//! `DurableOverlayWrite<K, V, S>` — the SHARED GENERIC **Order-A** durable-write
//! skeleton (Template Method), extracted from the char variant so byte can reuse
//! it rather than copy-paste the data-loss-critical control flow
//! (`docs/design/overlay-durable-architecture.md` §"The trait family", trait 2).
//!
//! # What this trait is
//!
//! A **subtrait of [`LockFreeOverlay`]** that adds the Order-A durable-write
//! machinery. It is a *seam trait* (Template Method + Strategy): the INVARIANT
//! skeleton — the durability-policy gate, the Order-A step ordering (durable WAL
//! append → publish via the overlay CAS → `mark_committed`), and the commit-rank
//! + watermark tail — lives here ONCE as default methods; the per-variant steps
//!   (the WAL-record builder, the value-domain bound, the char/byte-node-building
//!   CAS publish, the present-hoist) are provided by abstract SEAM hooks the variant
//!   supplies.
//!
//! # The Order-A ORDERING is sacred (data-loss-critical — DO NOT REORDER)
//!
//! Every durable op MUST, in this exact order:
//!   1. **append + sync the WAL record DURABLE** (`append_durable_wal`) — BEFORE
//!      any visibility, so a crash either replays the record (acked) or never had
//!      it (un-acked). Order B — CAS-then-log — is rejected: it can expose a
//!      visible-but-not-durable write. The single append covers every CAS retry
//!      (never re-appended: that would burn LSNs and punch holes in the watermark).
//!   2. **publish via the overlay root CAS** (the variant's publish inner) — the
//!      visibility point.
//!   3. **bind the commit rank durable, then `mark_committed`** ([`Self::commit_rank_and_mark`])
//!      — the data record is now durable AND visible; the committed watermark
//!      advances to cover BOTH the data LSN and the rank LSN so the contiguous
//!      prefix does not stall. The watermark is the ONLY safe `checkpoint_lsn`
//!      under out-of-order lock-free commit (the GAP_LEDGER #41 footgun, TLC-
//!      verified in `formal-verification/tla+/LockFreeDurableCheckpoint.tla`).
//!
//! The present-hoist (step 0, before the append) MUST stay **NON-FAULTING** on the
//! membership insert hot path (`find_leaf_lockfree`, NEVER `find_leaf_faulting`):
//! a faulting read before the append, racing a checkpoint/eviction that holds the
//! buffer-manager lock, is the lock-ordering inversion that deadlocked the soak for
//! 75+ minutes (memory `feedback_production-deadlock-is-costly`). The present-hoist
//! is a per-op SEAM (it is non-faulting for membership, faulting-with-fallback for
//! the valued/upsert variants whose return value must reflect a term under an
//! evicted prefix, and absent for the increment whose inner does the read), so the
//! membership seam carries this NON-FAULTING contract; the skeleton only fixes the
//! hoist-BEFORE-append ORDER.
//!
//! # Seam-boundary rationale (the "sensible > maximal" rule, design §0)
//!
//! GENERIC defaults (the steps that operate only on the already-generic
//! [`WalRecord`] / [`Lsn`] / [`DurabilityPolicy`] / the committed watermark):
//!   - [`Self::durable_policy_gate`] — the Immediate|GroupCommit gate (byte-exact
//!     message reconstruction). ONE copy of the "acknowledged ⇒ durable" discipline.
//!   - [`Self::commit_rank_and_mark`] — the Order-A step 2.5 + 3 tail (append the
//!     `CommitRank` durable, then `mark_committed(data_lsn)` + `mark_committed(rank_lsn)`).
//!     ONE copy of the commit-rank + watermark ordering, shared by all 5 writes.
//!   - [`Self::mark_committed_burned`] — the idempotent-arm liveness `mark_committed`.
//!   - [`Self::try_increment_cas_durable_default`] — the FULL increment template
//!     (the cleanest Order-A skeleton: it touches only the `try_increment_cas_inner`
//!     seam + the generic gate/append/rank/mark), so it is wholly generic.
//!
//! SEAM hooks (the steps that touch char/byte-specific node building or the
//! per-variant value domain — `OverlayNode` building, `str`→bytes encoding, the
//! u64-vs-i64 counter domain):
//!   - [`Self::durability_policy`], [`Self::append_durable_wal`],
//!     [`Self::append_commit_rank`], [`Self::mark_committed`] — the WAL/watermark
//!     accessors.
//!   - [`Self::bound_increment_delta`] / [`Self::build_increment_record`] /
//!     [`Self::increment_publish_inner`] — the increment's value-domain bound (BOTH
//!     variants' counter is now `u64`; `bound_increment_delta` is `i64::try_from(delta)`
//!     for both — rejecting a single WAL `BatchIncrement` delta `> i64::MAX`, the
//!     `i64` being only the WAL DELTA domain, never the leaf type), the delta
//!     `WalRecord` builder, and the proven path-copy publish (`try_increment_cas_inner`).
//!
//! The insert / remove / valued-insert / upsert public methods stay INHERENT on the
//! variant (their CAS-publish loops + per-op present-hoists are char-node-building
//! seams), but their bodies call the generic [`Self::durable_policy_gate`] +
//! [`Self::commit_rank_and_mark`] / [`Self::mark_committed_burned`] defaults — so the
//! data-loss-critical commit ordering lives in ONE place and byte reuses it verbatim.

use crate::persistent_artrie::core::durability::DurabilityPolicy;
use crate::persistent_artrie::core::error::Result;
use crate::persistent_artrie::core::key_encoding::KeyEncoding;
use crate::persistent_artrie::core::overlay::flip::LockFreeOverlay;
use crate::persistent_artrie::core::wal::{Lsn, WalRecord};
use crate::value::DictionaryValue;

/// Type-level brand for byte/char writes that must publish through the exact
/// semantic root-CAS path.
pub(crate) enum RegistryEligibleMutation {}

/// Type-level brand for vocab writes, whose representation cannot be evicted.
pub(crate) enum EvictionIneligibleMutation {}

/// The write-semantics discriminator the generic value publish seam
/// ([`DurableOverlayWrite::value_publish_inner`]) honors per CAS iteration
/// (G5/F0). The three durable value writes share ONE seam (the path-copy +
/// root-CAS loop), differing only in the per-iteration pre-check this enum
/// selects — so the data-loss-critical Order-A skeleton stays in ONE place.
pub(crate) enum ValueWriteMode {
    /// `insert_with_value` semantics: abort (do NOT overwrite) if the leaf is
    /// already final — the `entry().or_insert` / insert-once contract. The CAS
    /// loop re-checks finality on the FRESHLY-loaded root each iteration, so a
    /// concurrent insert that won is observed and yields
    /// [`ValuePublishOutcome::NotApplied`] (the caller then burns the appended
    /// WAL LSN, never ranks it).
    InsertOnce,
    /// `upsert` / `insert_with_value`-overwrite semantics: ALWAYS write the value
    /// (last-writer = the root-CAS winner). Never yields `NotApplied`.
    Upsert,
    /// `compare_and_swap` semantics: write only if the leaf's CURRENT value (read
    /// on the freshly-loaded root each iteration) bincode-serializes to
    /// `expected_bytes`; otherwise yield [`ValuePublishOutcome::NotApplied`]. The
    /// comparison is BINCODE BYTES (not `PartialEq` — `DictionaryValue` does not
    /// bound it). `None` = "expected absent" (matches an absent/non-final leaf).
    CompareAndSwap { expected_bytes: Option<Vec<u8>> },
}

/// The outcome of the generic value publish seam ([`DurableOverlayWrite::value_publish_inner`]).
pub(crate) enum ValuePublishOutcome {
    /// The root CAS landed; the leaf now carries the value. Carries the WINNING
    /// commit generation (the durable global `commit_seq` claimed at the winning
    /// iteration) so the caller binds `commit_rank_and_mark`.
    Published(u64),
    /// The mode's per-iteration pre-check refused the write (insert-once: leaf
    /// already final; CAS: `expected` no longer matches). NO root CAS landed; the
    /// caller MUST NOT rank the already-appended WAL record (it acked no state
    /// change → dropped on Overlay-regime replay) but MUST `mark_committed_burned`
    /// it for watermark liveness.
    NotApplied,
}

/// Zero-sized witness that a write publishes through the semantic root-CAS path.
///
/// The exact root revision is the sole authority: byte and char semantic CASes
/// clear its eviction binding in the same atomic transition that publishes the
/// new root.  This marker enforces that call graph at compile time without a
/// runtime counter, lock, registry write, allocation, branch, or failure mode.
/// Vocab remains explicitly eviction-ineligible through its distinct marker.
pub(crate) struct SemanticMutationPublicationPermit<'a, Eligibility> {
    lifetime: std::marker::PhantomData<&'a ()>,
    eligibility: std::marker::PhantomData<fn() -> Eligibility>,
}

impl<'a> SemanticMutationPublicationPermit<'a, RegistryEligibleMutation> {
    #[inline(always)]
    pub(crate) fn exact_root_cas() -> Self {
        Self {
            lifetime: std::marker::PhantomData,
            eligibility: std::marker::PhantomData,
        }
    }
}

impl SemanticMutationPublicationPermit<'static, EvictionIneligibleMutation> {
    pub(crate) fn eviction_ineligible() -> Self {
        Self {
            lifetime: std::marker::PhantomData,
            eligibility: std::marker::PhantomData,
        }
    }
}

/// Durable data-WAL record whose semantic visibility decision is still pending.
///
/// Ownership of this value keeps the root-CAS witness attached to every retry.
/// A terminal Applied or Idempotent decision must consume it explicitly before
/// CommitRank I/O. Error and unwind paths discard the zero-sized witness through
/// ordinary Drop; no runtime publication resource is held.
#[must_use = "a durable mutation must reach a terminal visibility decision"]
pub(crate) struct PendingDurableMutation<'a, Eligibility> {
    lsn: Lsn,
    permit: SemanticMutationPublicationPermit<'a, Eligibility>,
}

impl<'a> PendingDurableMutation<'a, RegistryEligibleMutation> {
    pub(crate) fn guarded(
        lsn: Lsn,
        permit: SemanticMutationPublicationPermit<'a, RegistryEligibleMutation>,
    ) -> Self {
        Self { lsn, permit }
    }
}

impl PendingDurableMutation<'static, EvictionIneligibleMutation> {
    pub(crate) fn eviction_ineligible(lsn: Lsn) -> Self {
        Self {
            lsn,
            permit: SemanticMutationPublicationPermit::eviction_ineligible(),
        }
    }
}

impl<'a, Eligibility> PendingDurableMutation<'a, Eligibility> {
    pub(crate) fn permit(&self) -> &SemanticMutationPublicationPermit<'a, Eligibility> {
        &self.permit
    }

    pub(crate) fn commit_visible(self) -> Lsn {
        let Self {
            lsn,
            permit: _permit,
        } = self;
        lsn
    }

    pub(crate) fn cancel_unpublished(self) -> Lsn {
        let Self {
            lsn,
            permit: _permit,
        } = self;
        lsn
    }
}

#[cfg(test)]
mod publication_witness_tests {
    use super::{
        EvictionIneligibleMutation, RegistryEligibleMutation, SemanticMutationPublicationPermit,
    };

    #[test]
    fn semantic_publication_witnesses_are_zero_sized() {
        assert_eq!(
            std::mem::size_of::<SemanticMutationPublicationPermit<'static, RegistryEligibleMutation>>(
            ),
            0
        );
        assert_eq!(
            std::mem::size_of::<
                SemanticMutationPublicationPermit<'static, EvictionIneligibleMutation>,
            >(),
            0
        );
    }
}

/// Test-only rendezvous at the exact semantic-data-WAL/visibility boundary.
///
/// The hook is thread-local so independently running tests and unrelated writer
/// threads cannot intercept one another. Production builds contain neither the
/// storage nor a branch at the append chokepoint.
#[cfg(test)]
pub(crate) type SemanticWalRendezvousHook = Box<dyn Fn()>;

#[cfg(test)]
thread_local! {
    static SEMANTIC_WAL_RENDEZVOUS: std::cell::RefCell<Option<SemanticWalRendezvousHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Install or clear this thread's semantic-WAL rendezvous hook.
#[cfg(test)]
pub(crate) fn set_semantic_wal_rendezvous(hook: Option<SemanticWalRendezvousHook>) {
    SEMANTIC_WAL_RENDEZVOUS.with(|slot| *slot.borrow_mut() = hook);
}

/// Fire after a semantic data record is durable and before its pending root-CAS
/// witness is handed to the visibility machine.
#[cfg(test)]
pub(crate) fn semantic_wal_rendezvous() {
    SEMANTIC_WAL_RENDEZVOUS.with(|slot| {
        if let Some(hook) = slot.borrow().as_ref() {
            hook();
        }
    });
}

/// Deterministic test-only failures at three semantic-WAL boundaries. The
/// storage and branches are absent from production.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SemanticWalFaultPoint {
    /// The mutation began, but the semantic data record was not appended.
    DataAppend,
    /// The semantic data record was appended, but its policy sync did not complete.
    DataSync,
    /// Visibility landed, but CommitRank append failed.
    CommitRankAppend,
}

#[cfg(test)]
thread_local! {
    static SEMANTIC_WAL_FAULT: std::cell::Cell<Option<SemanticWalFaultPoint>> =
        const { std::cell::Cell::new(None) };
}

/// Arm one one-shot WAL boundary failure for this thread.
#[cfg(test)]
pub(crate) fn set_semantic_wal_fault(point: Option<SemanticWalFaultPoint>) {
    SEMANTIC_WAL_FAULT.with(|slot| slot.set(point));
}

/// Fail exactly once when the armed boundary is reached.
#[cfg(test)]
pub(crate) fn inject_semantic_wal_fault(point: SemanticWalFaultPoint) -> Result<()> {
    let armed = SEMANTIC_WAL_FAULT.with(|slot| slot.get());
    if armed != Some(point) {
        return Ok(());
    }
    SEMANTIC_WAL_FAULT.with(|slot| slot.set(None));
    Err(
        crate::persistent_artrie::core::error::PersistentARTrieError::Wal(format!(
            "injected semantic WAL boundary failure at {point:?}"
        )),
    )
}

/// Test-only rendezvous immediately before a production membership root CAS.
#[cfg(test)]
pub(crate) type SemanticCasRendezvousHook = Box<dyn Fn()>;

#[cfg(test)]
thread_local! {
    static SEMANTIC_CAS_RENDEZVOUS: std::cell::RefCell<Option<SemanticCasRendezvousHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_semantic_cas_rendezvous(hook: Option<SemanticCasRendezvousHook>) {
    SEMANTIC_CAS_RENDEZVOUS.with(|slot| *slot.borrow_mut() = hook);
}

#[cfg(test)]
pub(crate) fn semantic_cas_rendezvous() {
    SEMANTIC_CAS_RENDEZVOUS.with(|slot| {
        if let Some(hook) = slot.borrow().as_ref() {
            hook();
        }
    });
}

/// Test-only observation point after semantic visibility released its permit and
/// immediately before CommitRank I/O.
#[cfg(test)]
pub(crate) type SemanticCommitRendezvousHook = Box<dyn Fn()>;

#[cfg(test)]
thread_local! {
    static SEMANTIC_COMMIT_RENDEZVOUS: std::cell::RefCell<Option<SemanticCommitRendezvousHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_semantic_commit_rendezvous(hook: Option<SemanticCommitRendezvousHook>) {
    SEMANTIC_COMMIT_RENDEZVOUS.with(|slot| *slot.borrow_mut() = hook);
}

#[cfg(test)]
fn semantic_commit_rendezvous() {
    SEMANTIC_COMMIT_RENDEZVOUS.with(|slot| {
        if let Some(hook) = slot.borrow().as_ref() {
            hook();
        }
    });
}

/// The SHARED GENERIC Order-A durable-write surface (design trait 2).
///
/// `K` is the key encoding (`ByteKey`/`CharKey`), `V` the value, `S` the block
/// storage. `Self::CounterValue` (inherited from [`LockFreeOverlay`]) is the
/// per-variant counter monomorph — `u64` for BOTH byte and char now (the
/// u64-restoration; see `overlay_write_mode.rs`). The increment publish seam stays
/// per-variant only because `try_increment_cas_inner` is `<u64,S>`-specialized
/// (named via a SAFE `Any` downcast), not because the counter types differ. The
/// `i64` here is ONLY the WAL `BatchIncrement` DELTA domain (`bound_increment_delta`
/// — both variants `i64::try_from(delta)`, rejecting a single delta `> i64::MAX`),
/// NOT the leaf counter type.
pub(crate) trait DurableOverlayWrite<K: KeyEncoding, V: DictionaryValue, S>:
    LockFreeOverlay<K, V, S>
{
    /// Compile-time proof of whether this overlay participates in eviction.
    type PublicationEligibility;

    // ========================================================================
    // REQUIRED SEAM (variant provides) — the WAL/watermark accessors + the
    // increment's value-domain bound / record builder / proven publish inner.
    // ========================================================================

    /// This trie's durability policy. The Order-A gate ([`Self::durable_policy_gate`])
    /// rejects everything but `Immediate`/`GroupCommit` so "acknowledged ⇒ durable"
    /// holds.
    fn durability_policy(&self) -> DurabilityPolicy;

    /// Append + sync `record` to the WAL **DURABLE** (per policy) and return its
    /// assigned LSN. This is Order-A step 1: the returned LSN is durable at return,
    /// BEFORE the caller performs the visibility-publishing root CAS. Returns `0`
    /// when no WAL writer is installed (no durability available). The shared
    /// [`WalRecord`] is the K-agnostic boundary; the variant constructs it from the
    /// raw key bytes (`str`→bytes for char).
    fn append_durable_wal(
        &self,
        record: WalRecord,
    ) -> Result<PendingDurableMutation<'_, Self::PublicationEligibility>>;

    /// **Order-A step 2.5** — append + sync a [`WalRecord::CommitRank`] binding the
    /// durable data record at `data_lsn` to the commit `generation` its visibility
    /// CAS landed at, returning the rank record's own LSN. Called by
    /// [`Self::commit_rank_and_mark`] AFTER the visibility CAS wins and BEFORE the op
    /// is acked. `term` is the raw key bytes (NOT lossy-UTF8). Recovery's
    /// `reconcile_lww` consumes these to order same-term replay by commit generation.
    fn append_commit_rank(&self, data_lsn: Lsn, term: &[u8], generation: u64) -> Result<Lsn>;

    /// **Order-A step 3** — mark `lsn` committed in the committed watermark (called
    /// by [`Self::commit_rank_and_mark`] / [`Self::mark_committed_burned`] AFTER the
    /// root CAS lands). Advances the contiguous committed-durable prefix. Idempotent.
    fn mark_committed(&self, lsn: Lsn);

    // ---- increment value-domain seam (the u64-vs-i64 divergence) ----

    /// Bound `delta` to the variant's persistence value domain BEFORE it is logged,
    /// so the WAL never records a delta the merge/recovery path cannot represent.
    /// Char `u64`: `delta > LOCKFREE_COUNTER_MAX` reject + `i64::try_from`. Byte will
    /// put its negative-`i64` reject here. Returns the bounded `i64` the delta
    /// `WalRecord` carries (the WAL increment domain is `i64` for every variant).
    fn bound_increment_delta(&self, key: &str, delta: Self::CounterValue) -> Result<i64>;

    /// Build the delta `WalRecord` (single-entry [`WalRecord::BatchIncrement`],
    /// delta-based + commutative on replay) for `key_bytes` carrying the
    /// already-`bound_increment_delta`-checked `bounded` delta.
    fn build_increment_record(&self, key_bytes: &[u8], bounded: i64) -> WalRecord;

    /// The PROVEN path-copy increment publish inner (char `try_increment_cas_inner`):
    /// the CAS-retry / re-read-on-conflict loop is the formally-checked no-lost-update
    /// arbiter. Returns `(new_accumulated_value, commit_generation)` on success — the
    /// winning published-root generation the durable wrapper ranks. Errors (overflow
    /// at the accumulated value) BEFORE any CAS, so an `?` early-return leaves the
    /// already-appended delta UNRANKED (benign — replays under Owned, dropped un-acked
    /// under Overlay).
    fn increment_publish_inner(
        &self,
        key: &str,
        delta: Self::CounterValue,
        permit: &SemanticMutationPublicationPermit<'_, Self::PublicationEligibility>,
    ) -> Result<(Self::CounterValue, u64)>;

    // ========================================================================
    // DEFAULT-PROVIDED GENERIC METHODS — the data-loss-critical Order-A skeleton.
    // DO NOT OVERRIDE (they encode the gate + the append→publish→mark ORDER).
    // ========================================================================

    /// **Order-A durability gate.** Reject every policy but `Immediate`/`GroupCommit`
    /// so an acknowledged write is guaranteed durable before it becomes visible
    /// ("acknowledged ⇒ durable" only holds under a synchronous policy). `method` is
    /// the public method name and `noun` the op noun (`"write"`/`"remove"`/
    /// `"increment"`) — together they reconstruct the EXACT char message
    /// (byte-identical), so the message-asserting tests are unaffected.
    fn durable_policy_gate(&self, method: &str, noun: &str) -> Result<()> {
        match self.durability_policy() {
            DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit => Ok(()),
            DurabilityPolicy::Periodic | DurabilityPolicy::None => Err(
                crate::persistent_artrie::core::error::PersistentARTrieError::InvalidOperation(
                    format!(
                        "{method} requires Immediate or GroupCommit durability so an \
                         acknowledged {noun} is guaranteed durable before it becomes visible"
                    ),
                ),
            ),
        }
    }

    /// **Order-A step 2.5 + 3 (the committed-arm tail).** AFTER the visibility CAS
    /// wins: bind the durable data record at `data_lsn` to its commit `generation`
    /// (durable BEFORE ack), then advance the committed watermark to include BOTH the
    /// data LSN and the rank LSN so the contiguous prefix does not stall on the rank
    /// record. ONE copy of the data-loss-critical commit-rank + watermark ordering,
    /// shared by all 5 durable writes. `key_bytes` is the raw key bytes the data
    /// record mutated.
    ///
    /// Order is sacred: `append_commit_rank` (durable) THEN `mark_committed(data)` +
    /// `mark_committed(rank)`. DO NOT REORDER. An `?` on the rank append returns Err
    /// with NEITHER LSN marked — correct, the write is durable+visible but not yet
    /// acked (recovery replays the data record; the watermark stalls at `data_lsn`).
    fn commit_rank_and_mark(&self, data_lsn: Lsn, key_bytes: &[u8], generation: u64) -> Result<()> {
        #[cfg(test)]
        semantic_commit_rendezvous();
        let rank_lsn = self.append_commit_rank(data_lsn, key_bytes, generation)?;
        self.mark_committed(data_lsn);
        self.mark_committed(rank_lsn);
        Ok(())
    }

    /// **Order-A idempotent-arm liveness mark.** A redundant data record that acked
    /// NO new state change (insert's `AlreadyExists`, remove's `AlreadyAbsent`) is
    /// NOT ranked (ranking a no-op in the commit_seq domain resurrects — it took no
    /// root CAS, so it has no causal position; under the Overlay regime an UNRANKED
    /// record is DROPPED on replay). But the burned `lsn` MUST still be marked for
    /// LIVENESS, or the contiguous watermark stalls and checkpoint reclaim with it.
    fn mark_committed_burned(&self, lsn: Lsn) {
        self.mark_committed(lsn);
    }

    /// **Order-A durable increment — the canonical Template-Method skeleton.** The
    /// counter-delta durable write expressed wholly generically (it touches only the
    /// generic gate/append/rank/mark + the `bound_increment_delta` /
    /// `build_increment_record` / `increment_publish_inner` seams):
    ///
    ///   gate → enable-check → empty short-circuit → value-domain bound → **append
    ///   the delta DURABLE (step 1)** → **publish via the proven path-copy inner
    ///   (step 2)** → **commit-rank + mark (step 3)** → return the new count.
    ///
    /// The single append happens ONCE, before the inner's CAS loop, and covers every
    /// CAS retry (never re-appended: that would double-count the delta and punch a
    /// hole in the watermark). The inner errors BEFORE any CAS on overflow, so an `?`
    /// early-return leaves the already-appended delta UNRANKED (benign). `key_bytes`
    /// is the raw key bytes (`key.as_bytes()` for char — the K boundary).
    fn try_increment_cas_durable_default(
        &self,
        key: &str,
        key_bytes: &[u8],
        delta: Self::CounterValue,
    ) -> Result<Self::CounterValue> {
        // "Acknowledged ⇒ durable" only holds under a synchronous policy.
        self.durable_policy_gate("try_increment_cas_durable", "increment")?;

        // install_overlay() must have run (the inner would otherwise panic; surface
        // it as a recoverable error on the durable path instead).
        if self.lockfree_root().is_none() {
            return Err(
                crate::persistent_artrie::core::error::PersistentARTrieError::InvalidOperation(
                    "Lock-free mode not enabled. Call install_overlay() first.".to_string(),
                ),
            );
        }

        // Empty-string support (H4): the empty key "" now flows through the template
        // like any other key — `increment_publish_inner` → the variant's
        // `try_increment_cas_inner` handles "" via fresh-root-CAS at depth 0
        // (`build_value_path_iterative` reads the existing root counter and republishes
        // a fresh `as_final().with_value` root), and bound/append/rank below are
        // key-length-agnostic. (The former empty short-circuit + `empty_value` param are
        // removed — "" now carries a real durable, RANKED root counter, not a dropped 0
        // that `reconcile_lww` would discard as unranked on reopen.)

        // Bound the delta to the i64 persistence domain BEFORE logging it, so the
        // WAL never records a delta the merge/recovery path cannot represent.
        let bounded = self.bound_increment_delta(key, delta)?;

        // ORDER A — step 1: append + sync the DELTA record DURABLE, before any
        // visibility. Single-entry `BatchIncrement` (delta-based, commutative on
        // replay). Returned LSN is durable-per-policy here.
        let pending = self.append_durable_wal(self.build_increment_record(key_bytes, bounded))?;

        // ORDER A — step 2: publish via the PROVEN path-copy increment inner (its
        // CAS-retry loop is the formally-checked no-lost-update arbiter; we do not
        // re-append on its internal retries). The inner returns the WINNING
        // published-root generation so step 3 can rank the durable delta in the SAME
        // root.version domain as the overwrite producers. On overflow at the
        // accumulated value the inner errors AFTER the durable append — the delta is
        // durably logged but not made visible (the documented Order-A
        // "durable-but-visible-only-after-reopen" window, not a lost write); `?`
        // early-returns leaving the already-appended delta UNRANKED (benign).
        let (new_val, generation) = self.increment_publish_inner(key, delta, pending.permit())?;

        // ORDER A — step 3: durable AND visible — bind the commit rank, then advance
        // the committed watermark over both LSNs.
        let lsn = pending.commit_visible();
        self.commit_rank_and_mark(lsn, key_bytes, generation)?;
        Ok(new_val)
    }

    // ========================================================================
    // G5/F0 — the GENERIC durable VALUE-write surface (arbitrary `V`).
    //
    // The value path is path-copy-then-root-CAS, structurally identical to the
    // membership/counter path the overlay already proves. These defaults own the
    // data-loss-critical Order-A skeleton (gate → present-hoist → append durable
    // WAL → publish via the value seam → rank-or-burn); the per-variant node
    // building lives in the SEAMS below (they name the concrete `OverlayNode<K,V>`
    // via `build_value_path_iterative`, just like `increment_publish_inner`).
    //
    // EMPTY TERM "": carries NO special case here — the seam's
    // `build_value_path_iterative(&root, &units, 0, value)` at `units == []` IS
    // the RANKED depth-0 root publish (G5-NEW-4). The UNRANKED
    // `overlay_publish_root_value` is reserved for the no-WAL reestablish fold.
    // ========================================================================

    // ---- value seams (variant provides — they touch `OverlayNode<K,V>`) ----

    /// FAULTING-with-fallback presence check for the valued-insert hoist + the
    /// upsert existed-probe: returns whether `key_bytes` is present (final) in the
    /// trie, faulting an evicted prefix in so the answer reflects on-disk state
    /// (the valued path's return value must be exact, unlike the membership hot
    /// path which is non-faulting). Falls back to the in-memory walk on I/O error.
    fn value_present_faulting(&self, key_bytes: &[u8]) -> Result<bool>;

    /// FAULTING read of the leaf's current value at `key_bytes` (for the CAS
    /// compare + `get_or_insert`'s read-your-write). `None` = absent / non-final.
    fn value_read_faulting(&self, key_bytes: &[u8]) -> Result<Option<V>>;

    /// The PROVEN path-copy value publish inner: the CAS-retry loop that is the
    /// no-lost-update arbiter, honoring `mode` per iteration on the freshly-loaded
    /// root. Returns [`ValuePublishOutcome::Published`] (with the winning commit
    /// generation) or [`ValuePublishOutcome::NotApplied`] (mode pre-check refused).
    /// Does NOT append WAL — the default owns Order-A step 1.
    fn value_publish_inner(
        &self,
        key_bytes: &[u8],
        value: V,
        mode: ValueWriteMode,
        permit: &SemanticMutationPublicationPermit<'_, Self::PublicationEligibility>,
    ) -> Result<ValuePublishOutcome>;

    // ---- generic durable value-write defaults (the Order-A skeleton) ----

    /// **Order-A durable INSERT-with-value (insert-once).** gate → enable-check →
    /// faulting present-hoist (no-op + NO WAL if already present) → append the
    /// `Insert` record DURABLE (step 1) → publish via the value seam in
    /// [`ValueWriteMode::InsertOnce`] (step 2) → rank-or-burn (step 3). Returns
    /// `Ok(true)` iff this call newly inserted the term; `Ok(false)` if it was
    /// already present (hoist) or a concurrent insert won the race (NotApplied →
    /// the appended LSN is burned, never ranked). `key_bytes` is the raw key.
    fn insert_cas_with_value_durable_default(&self, key_bytes: &[u8], value: V) -> Result<bool> {
        self.durable_policy_gate("insert_cas_with_value_durable", "write")?;
        if self.lockfree_root().is_none() {
            return Err(
                crate::persistent_artrie::core::error::PersistentARTrieError::InvalidOperation(
                    "Lock-free mode not enabled. Call install_overlay() first.".to_string(),
                ),
            );
        }
        // INSERT (not upsert): already present ⇒ no-op with NO WAL (don't burn an
        // LSN / punch a watermark hole). Faulting (the return value must reflect a
        // term under an evicted prefix).
        if self.value_present_faulting(key_bytes)? {
            return Ok(false);
        }
        // ORDER A — step 1: append + sync the valued Insert record DURABLE.
        let value_bytes = crate::serialization::bincode_compat::serialize(&value).map_err(|e| {
            crate::persistent_artrie::core::error::PersistentARTrieError::internal(format!(
                "Failed to serialize value: {}",
                e
            ))
        })?;
        let pending = self.append_durable_wal(WalRecord::Insert {
            term: key_bytes.to_vec(),
            value: Some(value_bytes),
        })?;
        // ORDER A — step 2: publish via the value seam (insert-once: re-checks
        // finality per iteration on the freshly-loaded root).
        match self.value_publish_inner(
            key_bytes,
            value,
            ValueWriteMode::InsertOnce,
            pending.permit(),
        )? {
            // ORDER A — step 3: durable AND visible — rank.
            ValuePublishOutcome::Published(generation) => {
                let lsn = pending.commit_visible();
                self.commit_rank_and_mark(lsn, key_bytes, generation)?;
                Ok(true)
            }
            // Raced: a concurrent insert won. The appended Insert acked NO new
            // state (Ok(false)); burn the LSN for liveness, NEVER rank it.
            ValuePublishOutcome::NotApplied => {
                let lsn = pending.cancel_unpublished();
                self.mark_committed_burned(lsn);
                Ok(false)
            }
        }
    }

    /// **Order-A durable UPSERT (always-write).** Like the insert default but the
    /// value is ALWAYS written (last-writer-wins = the root-CAS winner). The
    /// existed-probe is advisory (for the return flag only — the write is
    /// unconditional, so a race is harmless: the CAS is the linearization point).
    /// Returns `Ok(true)` iff the term was newly inserted (`false` = updated).
    fn upsert_cas_durable_default(&self, key_bytes: &[u8], value: V) -> Result<bool> {
        self.durable_policy_gate("upsert_cas_durable", "write")?;
        if self.lockfree_root().is_none() {
            return Err(
                crate::persistent_artrie::core::error::PersistentARTrieError::InvalidOperation(
                    "Lock-free mode not enabled. Call install_overlay() first.".to_string(),
                ),
            );
        }
        let existed = self.value_present_faulting(key_bytes)?;
        // ORDER A — step 1: append + sync the Upsert record DURABLE.
        let value_bytes = crate::serialization::bincode_compat::serialize(&value).map_err(|e| {
            crate::persistent_artrie::core::error::PersistentARTrieError::internal(format!(
                "Failed to serialize value: {}",
                e
            ))
        })?;
        let pending = self.append_durable_wal(WalRecord::Upsert {
            term: key_bytes.to_vec(),
            value: value_bytes,
        })?;
        // ORDER A — step 2: publish (always-write).
        match self.value_publish_inner(
            key_bytes,
            value,
            ValueWriteMode::Upsert,
            pending.permit(),
        )? {
            ValuePublishOutcome::Published(generation) => {
                let lsn = pending.commit_visible();
                self.commit_rank_and_mark(lsn, key_bytes, generation)?;
                Ok(!existed)
            }
            // Upsert never refuses; if a publish ever returns NotApplied it would
            // leave a durable-but-invisible Upsert — surface it (do NOT silently
            // ack), burning the LSN so the watermark does not stall.
            ValuePublishOutcome::NotApplied => {
                let lsn = pending.cancel_unpublished();
                self.mark_committed_burned(lsn);
                Err(
                    crate::persistent_artrie::core::error::PersistentARTrieError::internal(
                        "upsert_cas_durable: value publish unexpectedly refused (NotApplied); the \
                     Upsert record is durable and replays on reopen",
                    ),
                )
            }
        }
    }

    /// **Order-A durable compare-and-swap.** Reads the current value (faulting),
    /// compares it to `expected` by BINCODE BYTES (not `PartialEq`); on mismatch
    /// returns `Ok(false)` with NO WAL (a failed CAS is a no-op — burns no LSN). On
    /// match: append the `Upsert{new}` record DURABLE, then publish via the value
    /// seam in [`ValueWriteMode::CompareAndSwap`] which RE-CHECKS `expected` against
    /// the freshly-loaded root each iteration (so a concurrent change between the
    /// initial read and the publish correctly fails the CAS → NotApplied → burn the
    /// LSN, never rank). Returns `Ok(true)` iff the swap landed.
    fn compare_and_swap_cas_durable_default(
        &self,
        key_bytes: &[u8],
        expected: Option<V>,
        new: V,
    ) -> Result<bool> {
        self.durable_policy_gate("compare_and_swap", "write")?;
        if self.lockfree_root().is_none() {
            return Err(
                crate::persistent_artrie::core::error::PersistentARTrieError::InvalidOperation(
                    "Lock-free mode not enabled. Call install_overlay() first.".to_string(),
                ),
            );
        }
        // Read current + bincode both sides (comparison = bytes, NOT PartialEq).
        let current = self.value_read_faulting(key_bytes)?;
        let expected_bytes = match &expected {
            Some(e) => Some(
                crate::serialization::bincode_compat::serialize(e).map_err(|err| {
                    crate::persistent_artrie::core::error::PersistentARTrieError::internal(format!(
                        "Failed to serialize value: {}",
                        err
                    ))
                })?,
            ),
            None => None,
        };
        let current_bytes = match &current {
            Some(c) => Some(
                crate::serialization::bincode_compat::serialize(c).map_err(|err| {
                    crate::persistent_artrie::core::error::PersistentARTrieError::internal(format!(
                        "Failed to serialize value: {}",
                        err
                    ))
                })?,
            ),
            None => None,
        };
        if current_bytes != expected_bytes {
            // Mismatch: a failed CAS is a no-op (burns no LSN, punches no hole).
            return Ok(false);
        }
        // ORDER A — step 1: append + sync the Upsert{new} record DURABLE.
        let new_bytes = crate::serialization::bincode_compat::serialize(&new).map_err(|e| {
            crate::persistent_artrie::core::error::PersistentARTrieError::internal(format!(
                "Failed to serialize value: {}",
                e
            ))
        })?;
        let pending = self.append_durable_wal(WalRecord::Upsert {
            term: key_bytes.to_vec(),
            value: new_bytes,
        })?;
        // ORDER A — step 2: publish with the per-iteration expected-recheck.
        match self.value_publish_inner(
            key_bytes,
            new,
            ValueWriteMode::CompareAndSwap { expected_bytes },
            pending.permit(),
        )? {
            ValuePublishOutcome::Published(generation) => {
                let lsn = pending.commit_visible();
                self.commit_rank_and_mark(lsn, key_bytes, generation)?;
                Ok(true)
            }
            // The recheck found `expected` no longer matches (concurrent change):
            // CAS fails. The appended Upsert acked NO swap → burn (NEVER rank: an
            // unranked record is dropped on Overlay reopen, so recovery cannot
            // apply a swap the caller was told failed).
            ValuePublishOutcome::NotApplied => {
                let lsn = pending.cancel_unpublished();
                self.mark_committed_burned(lsn);
                Ok(false)
            }
        }
    }

    /// **Order-A durable get-or-insert (insert-once, read-your-write).** Fast path:
    /// if present (faulting read), return the existing value. Else insert-once via
    /// [`Self::insert_cas_with_value_durable_default`]: on a win return `default`;
    /// on a concurrent-insert race (`Ok(false)`) read the WINNER's value back and
    /// return it — so all racers converge on ONE value and the caller reads its own
    /// write (the `entry().or_insert` contract).
    fn get_or_insert_durable_default(&self, key_bytes: &[u8], default: V) -> Result<V> {
        if let Some(v) = self.value_read_faulting(key_bytes)? {
            return Ok(v);
        }
        if self.insert_cas_with_value_durable_default(key_bytes, default.clone())? {
            // We inserted it.
            Ok(default)
        } else {
            // Raced: a concurrent insert won. Read its value (faulting, so an
            // immediately-evicted winner still resolves). Fall back to `default`
            // only in the (essentially impossible) absent-after-faulting-read case.
            match self.value_read_faulting(key_bytes)? {
                Some(v) => Ok(v),
                None => Ok(default),
            }
        }
    }
}
