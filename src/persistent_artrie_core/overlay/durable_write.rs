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
//! (the WAL-record builder, the value-domain bound, the char/byte-node-building
//! CAS publish, the present-hoist) are deferred to abstract SEAM hooks the variant
//! supplies.
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
//!     [`Self::increment_publish_inner`] — the increment's value-domain bound (char
//!     `u64`: `delta > LOCKFREE_COUNTER_MAX` reject + `i64::try_from`; byte will reject
//!     a negative `i64` here), the delta `WalRecord` builder, and the proven
//!     path-copy publish (char `try_increment_cas_inner`).
//!
//! The insert / remove / valued-insert / upsert public methods stay INHERENT on the
//! variant (their CAS-publish loops + per-op present-hoists are char-node-building
//! seams), but their bodies call the generic [`Self::durable_policy_gate`] +
//! [`Self::commit_rank_and_mark`] / [`Self::mark_committed_burned`] defaults — so the
//! data-loss-critical commit ordering lives in ONE place and byte reuses it verbatim.

use crate::persistent_artrie_core::durability::DurabilityPolicy;
use crate::persistent_artrie_core::error::Result;
use crate::persistent_artrie_core::key_encoding::KeyEncoding;
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::persistent_artrie_core::wal::{Lsn, WalRecord};
use crate::value::DictionaryValue;

/// The SHARED GENERIC Order-A durable-write surface (design trait 2).
///
/// `K` is the key encoding (`ByteKey`/`CharKey`), `V` the value, `S` the block
/// storage. `Self::CounterValue` (inherited from [`LockFreeOverlay`]) is the
/// per-variant counter monomorph (`u64` for char, `i64` for byte) — the one
/// divergence that makes the value-domain bound + increment publish seams, not
/// blanket defaults.
pub(crate) trait DurableOverlayWrite<K: KeyEncoding, V: DictionaryValue, S>:
    LockFreeOverlay<K, V, S>
{
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
    fn append_durable_wal(&self, record: WalRecord) -> Result<Lsn>;

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
            DurabilityPolicy::Periodic | DurabilityPolicy::None => {
                Err(crate::persistent_artrie_core::error::PersistentARTrieError::InvalidOperation(
                    format!(
                        "{method} requires Immediate or GroupCommit durability so an \
                         acknowledged {noun} is guaranteed durable before it becomes visible"
                    ),
                ))
            }
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
        empty_value: Self::CounterValue,
    ) -> Result<Self::CounterValue> {
        // "Acknowledged ⇒ durable" only holds under a synchronous policy.
        self.durable_policy_gate(
            "try_increment_cas_durable",
            "increment",
        )?;

        // enable_lockfree() must have run (the inner would otherwise panic; surface
        // it as a recoverable error on the durable path instead).
        if self.lockfree_root().is_none() {
            return Err(
                crate::persistent_artrie_core::error::PersistentARTrieError::InvalidOperation(
                    "Lock-free mode not enabled. Call enable_lockfree() first.".to_string(),
                ),
            );
        }

        if key.is_empty() {
            return Ok(empty_value);
        }

        // Bound the delta to the i64 persistence domain BEFORE logging it, so the
        // WAL never records a delta the merge/recovery path cannot represent.
        let bounded = self.bound_increment_delta(key, delta)?;

        // ORDER A — step 1: append + sync the DELTA record DURABLE, before any
        // visibility. Single-entry `BatchIncrement` (delta-based, commutative on
        // replay). Returned LSN is durable-per-policy here.
        let lsn = self.append_durable_wal(self.build_increment_record(key_bytes, bounded))?;

        // ORDER A — step 2: publish via the PROVEN path-copy increment inner (its
        // CAS-retry loop is the formally-checked no-lost-update arbiter; we do not
        // re-append on its internal retries). The inner returns the WINNING
        // published-root generation so step 3 can rank the durable delta in the SAME
        // root.version domain as the overwrite producers. On overflow at the
        // accumulated value the inner errors AFTER the durable append — the delta is
        // durably logged but not made visible (the documented Order-A
        // "durable-but-visible-only-after-reopen" window, not a lost write); `?`
        // early-returns leaving the already-appended delta UNRANKED (benign).
        let (new_val, generation) = self.increment_publish_inner(key, delta)?;

        // ORDER A — step 3: durable AND visible — bind the commit rank, then advance
        // the committed watermark over both LSNs.
        self.commit_rank_and_mark(lsn, key_bytes, generation)?;
        Ok(new_val)
    }
}
