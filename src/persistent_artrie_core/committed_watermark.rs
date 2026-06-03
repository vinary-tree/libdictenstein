//! Committed-LSN watermark for the lock-free **Order-A** durable write path.
//!
//! Under lock-free CAS, writes can **commit** (become visible via the root CAS)
//! out of LSN order — a writer holding LSN 6 may CAS-publish before the writer
//! holding LSN 5. The committed **watermark** is the largest `L` such that every
//! LSN in `1..=L` has committed. It is the **only safe `checkpoint_lsn`**: the
//! appended/synced WAL frontier is NOT safe, because a write appended-before but
//! committed-after a checkpoint capture has `lsn ≤ frontier` yet is absent from
//! the (pre-commit) snapshot — so frontier-bounded WAL reclaim would archive it
//! out of recovery's reach (the GAP_LEDGER #41 data-loss footgun; the TLA spec
//! `formal-verification/tla+/LockFreeDurableCheckpoint.tla` `_Unsafe.cfg`
//! exhibits exactly this loss, while the watermark cfg is loss-free).
//!
//! This type is the **executable refinement** of the spec's `Watermark`:
//! `contiguous` ≙ `Watermark`, [`mark_committed`](CommittedWatermark::mark_committed)
//! ≙ the spec's `Commit(w)` action.
//!
//! # Shared across variants (DRY)
//!
//! This watermark is key-encoding-agnostic (pure LSN bookkeeping), so it lives in
//! `persistent_artrie_core` and is reused by every lock-free durable ARTrie variant
//! (char, byte, …) rather than duplicated per variant. It is the
//! `OverlayCheckpoint` route-split's WAL-retention floor.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Tracks the contiguous committed-LSN prefix under out-of-order lock-free commit.
///
/// `watermark()` is a lock-free `Acquire` read (never blocks writers or the
/// checkpoint capture); [`mark_committed`](Self::mark_committed) briefly serializes
/// committers under a `Mutex` to close the prefix, but runs **after** the writer's
/// root CAS has already made the write visible — so it is off the contended
/// CAS-retry loop and preserves the lock-free *progress* of visible publication.
#[derive(Debug)]
pub struct CommittedWatermark {
    /// Largest `L` with every LSN in `(base, L]` committed. Written `Release`
    /// (only under `pending`'s lock); read `Acquire` lock-free.
    contiguous: AtomicU64,
    /// Out-of-order committed LSNs above `contiguous`, awaiting prefix closure.
    /// Bounded by the number of concurrently in-flight writers.
    pending: Mutex<BTreeSet<u64>>,
}

impl CommittedWatermark {
    /// Create a watermark whose contiguous prefix already covers `1..=base`
    /// (e.g. the durable WAL frontier recovered from disk on open, so replayed
    /// LSNs are treated as already committed).
    pub fn new(base: u64) -> Self {
        Self {
            contiguous: AtomicU64::new(base),
            pending: Mutex::new(BTreeSet::new()),
        }
    }

    /// The committed watermark: the largest `L` such that every LSN in `1..=L`
    /// has committed. Lock-free read — safe to call from the checkpoint capture
    /// **before** loading the atomic root (the mandated ordering that makes
    /// `visible ⊆ durable-prefix` hold).
    #[inline]
    pub fn watermark(&self) -> u64 {
        self.contiguous.load(Ordering::Acquire)
    }

    /// Mark `lsn` committed — called by a writer **after** its root CAS lands
    /// (the write is already durable in the WAL and now visible). Advances the
    /// contiguous prefix by as far as the newly-closable run allows.
    ///
    /// `lsn <= watermark()` (already covered) is a no-op, so this is idempotent.
    /// Every committed LSN MUST be marked, or the watermark stalls (a liveness
    /// issue — checkpoints stop advancing `checkpoint_lsn` — not a safety one).
    pub fn mark_committed(&self, lsn: u64) {
        let mut pending = self
            .pending
            .lock()
            .expect("CommittedWatermark pending lock poisoned");
        // `contiguous` is only written here under this lock, so this load is the
        // authoritative current value.
        let mut cur = self.contiguous.load(Ordering::Acquire);
        if lsn <= cur {
            return; // already covered — idempotent no-op
        }
        pending.insert(lsn);
        // Drain every LSN that closes the prefix immediately above `cur`.
        while pending.remove(&(cur + 1)) {
            cur += 1;
        }
        self.contiguous.store(cur, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::CommittedWatermark;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn empty_base_starts_at_zero() {
        let w = CommittedWatermark::new(0);
        assert_eq!(w.watermark(), 0);
    }

    #[test]
    fn out_of_order_does_not_advance_until_prefix_closes() {
        let w = CommittedWatermark::new(0);
        // LSN 2 commits before 1 — watermark stays 0 (1 is still uncommitted).
        w.mark_committed(2);
        assert_eq!(
            w.watermark(),
            0,
            "gap at LSN 1 must hold the watermark at 0"
        );
        // 1 closes the prefix → 1,2 both committed → watermark jumps to 2.
        w.mark_committed(1);
        assert_eq!(w.watermark(), 2, "closing LSN 1 drains the 1,2 run");
        // 3 extends the prefix.
        w.mark_committed(3);
        assert_eq!(w.watermark(), 3);
    }

    #[test]
    fn scrambled_commit_order_closes_in_one_drain() {
        let w = CommittedWatermark::new(0);
        for lsn in [3u64, 5, 4, 2] {
            w.mark_committed(lsn);
            // None of these closes the prefix (1 is missing).
            assert_eq!(w.watermark(), 0, "still 0 until LSN 1 commits (have {lsn})");
        }
        // 1 arrives last → drains 1,2,3,4,5 in a single sweep.
        w.mark_committed(1);
        assert_eq!(w.watermark(), 5);
    }

    #[test]
    fn recovered_base_treated_as_committed_prefix() {
        let w = CommittedWatermark::new(5);
        assert_eq!(w.watermark(), 5);
        // 7 commits out of order above the recovered base.
        w.mark_committed(7);
        assert_eq!(w.watermark(), 5, "gap at 6 holds the watermark at the base");
        w.mark_committed(6);
        assert_eq!(w.watermark(), 7);
        // Marking an already-covered LSN is a no-op.
        w.mark_committed(3);
        assert_eq!(w.watermark(), 7);
    }

    #[test]
    fn marking_is_idempotent() {
        let w = CommittedWatermark::new(0);
        w.mark_committed(1);
        w.mark_committed(1);
        w.mark_committed(1);
        assert_eq!(w.watermark(), 1);
    }

    #[test]
    fn concurrent_committers_converge_to_full_prefix() {
        // Every LSN in 1..=200 is marked exactly once across many threads (each
        // in a scrambled order); the final watermark must be 200 (full prefix),
        // and it must never have skipped ahead of a real gap.
        let w = Arc::new(CommittedWatermark::new(0));
        let n_threads = 8;
        let max = 200u64;
        let handles: Vec<_> = (0..n_threads)
            .map(|t| {
                let w = Arc::clone(&w);
                thread::spawn(move || {
                    // Each thread commits the LSNs congruent to its id mod n,
                    // descending — so the global order is heavily scrambled.
                    let mut lsn = max - (t as u64);
                    while lsn >= 1 {
                        w.mark_committed(lsn);
                        // Invariant: the watermark is always a true contiguous
                        // prefix bound — it never exceeds an LSN that is not yet
                        // marked by SOME thread. We can't cheaply assert the full
                        // set here, but monotonicity must hold.
                        if lsn < n_threads as u64 {
                            break;
                        }
                        lsn -= n_threads as u64;
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("committer thread");
        }
        assert_eq!(
            w.watermark(),
            max,
            "once every LSN 1..=200 is committed the watermark must reach 200"
        );
    }
}
