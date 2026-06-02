//! Bounded loom schedule checks for the lock-free **Order-A durable checkpoint**
//! capture ordering (Migration Phase E, plan §4).
//!
//! These are the executable, schedule-exhaustive companion to the TLA spec
//! `formal-verification/tla+/LockFreeDurableCheckpoint.tla` (and its `_Unsafe.cfg`
//! polarity). They model — with loom's own atomics, exactly as the sibling
//! `persistent_artrie_loom_correspondence.rs` / `persistent_lockfree_overlay_loom.rs`
//! do — the **§4 durability protocol**:
//!
//!   * a **writer** runs **Order A**: append+sync its WAL record DURABLE (reserve
//!     an LSN, mark it durable) → CAS-publish the root (make it visible) →
//!     `mark_committed(lsn)` advance the contiguous committed watermark;
//!   * a **checkpointer** **captures** by reading `checkpoint_lsn` (the watermark
//!     OR — for the negative control — the appended frontier) BEFORE loading the
//!     atomic root, snapshots `{committed writes ≤ checkpoint_lsn}`, publishes that
//!     as the durable checkpoint, and reclaims the WAL `≤ checkpoint_lsn` (retaining
//!     `> checkpoint_lsn`);
//!   * a **crash+reopen** reconstructs `recovered = durableCkpt ∪ {durable WAL
//!     lsn > checkpoint_lsn still retained}`.
//!
//! THE headline invariant (mirrors TLA `NoLostWriteUnderLockFreeCommit`): every
//! write that became **visible** (committed via root CAS) before the crash is in
//! `recovered`. Under lock-free CAS, writes commit OUT OF LSN ORDER, so a write
//! appended *before* a capture but committed *after* it has `lsn ≤ appendedFrontier`
//! yet is absent from the (pre-commit) snapshot — the GAP_LEDGER #41 footgun.
//!
//!   * `committed-watermark` checkpoint_lsn  → NO lost write (positive tests).
//!   * `appended-frontier`  checkpoint_lsn   → loom DRIVES a lost write; the
//!     negative-control test asserts that loss FIRES (via `#[should_panic]`),
//!     encoding WHY the watermark is required (mirroring `_Unsafe.cfg`).
//!
//! Why loom can model this faithfully (no SeqCst store-load Dekker dependency,
//! unlike the EBR reclaim): the protocol is Acquire/Release publication on a CAS
//! root + monotone counters, which loom explores soundly. `RwLock<Option<Arc<…>>>`
//! is the faithful stand-in for the production `ArcSwapOption` root
//! (`load == load_full`, `compare_exchange == compare_and_swap + ptr_eq`), exactly
//! as the existing loom suites note.
//!
//! Bounds: ≤2 writers / 2 LSNs (`MaxL = 2`), tiny — the same scale as the TLA
//! CONSTANTS. Run with:
//!   cargo test --features persistent-artrie --test persistent_lockfree_durable_loom

#![cfg(feature = "persistent-artrie")]

use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex, RwLock};
use loom::thread;

/// Highest LSN the bounded model uses (LSNs are 1..=MAX_LSN). Two writers each
/// take one LSN — enough to exhibit the out-of-order-commit loss.
const MAX_LSN: usize = 2;

/// The shared durable/visibility state — a faithful, minimal model of the char
/// trie's Order-A overlay + WAL + checkpoint. All fields are loom atomics / loom
/// locks so loom explores every interleaving of writers vs the checkpointer.
struct Model {
    /// Next LSN to hand out (1-based; `current_lsn` in the impl). Writers in this
    /// bounded model take FIXED LSNs (1 and 2) so the appended frontier is read
    /// directly from `appended`; this field documents the impl correspondence
    /// (`AsyncWalWriter::current_lsn`) but is not the LSN source here.
    #[allow(dead_code)]
    next_lsn: AtomicUsize,
    /// `appended[l]` — LSN `l`'s WAL record is durable (appended+synced). Order A
    /// sets this BEFORE the root CAS. Index 0 is unused (LSNs are 1-based).
    appended: [AtomicBool; MAX_LSN + 1],
    /// `committed[l]` — LSN `l`'s write is VISIBLE (its root CAS landed). Set
    /// AFTER `appended[l]` (Order A ⇒ committed ⊆ appended).
    committed: [AtomicBool; MAX_LSN + 1],
    /// The atomic published root, modeled as the set of visible LSNs it carries.
    /// `RwLock<…>` stands in for `ArcSwapOption` (see module doc). A write CASes a
    /// new root = old ∪ {its lsn}; the checkpointer `load`s it.
    root: RwLock<Arc<RootSnapshot>>,
    /// Serializes the read-modify-CAS of the root (the model's CAS primitive).
    root_cas_lock: Mutex<()>,
    /// The durable on-disk checkpoint image: the set of LSNs in `durableCkpt`.
    durable_ckpt: RwLock<[bool; MAX_LSN + 1]>,
    /// `checkpoint_lsn` persisted on disk (block-0). Recovery archives `≤` it.
    checkpoint_lsn: AtomicUsize,
    /// The smallest retained WAL lsn (`walRetainedFrom`). Reclaim sets it to
    /// `checkpoint_lsn + 1`; a record `< walRetainedFrom` is archived.
    wal_retained_from: AtomicUsize,
}

/// An immutable published root version: the set of visible LSNs reachable in it
/// (the model's analogue of the immutable overlay spine). Cloned-and-extended on
/// each CAS, shared by `Arc` (so a checkpointer that `load`ed an old root keeps
/// seeing exactly that frozen set — the MVCC snapshot property).
#[derive(Clone)]
struct RootSnapshot {
    visible: [bool; MAX_LSN + 1],
}

impl Model {
    fn new() -> Self {
        Model {
            next_lsn: AtomicUsize::new(1),
            appended: Default::default(),
            committed: Default::default(),
            root: RwLock::new(Arc::new(RootSnapshot {
                visible: [false; MAX_LSN + 1],
            })),
            root_cas_lock: Mutex::new(()),
            durable_ckpt: RwLock::new([false; MAX_LSN + 1]),
            checkpoint_lsn: AtomicUsize::new(0),
            wal_retained_from: AtomicUsize::new(1),
        }
    }

    /// The committed watermark: the largest `L` with every lsn in `1..=L`
    /// committed (the contiguous committed prefix), read lock-free. Mirrors
    /// `CommittedWatermark::watermark` / the TLA `Watermark`.
    fn watermark(&self) -> usize {
        let mut w = 0;
        for l in 1..=MAX_LSN {
            if self.committed[l].load(Ordering::Acquire) {
                w = l;
            } else {
                break; // gap ⇒ prefix closes here
            }
        }
        w
    }

    /// The appended frontier: the max appended lsn (the UNSAFE checkpoint_lsn).
    fn appended_frontier(&self) -> usize {
        let mut f = 0;
        for l in 1..=MAX_LSN {
            if self.appended[l].load(Ordering::Acquire) {
                f = l;
            }
        }
        f
    }

    /// ORDER A — a writer publishes LSN `lsn` for the (model) term it carries.
    ///   1. append+sync DURABLE (set `appended[lsn]`) — BEFORE any visibility;
    ///   2. CAS the root to include `lsn` (the visibility point) and set
    ///      `committed[lsn]`;
    ///   3. `mark_committed` is implicit: `watermark()` recomputes the contiguous
    ///      prefix from `committed`, so setting `committed[lsn]` IS the mark.
    fn order_a_write(&self, lsn: usize) {
        // Step 1: durable WAL append+sync before visibility (Order A).
        self.appended[lsn].store(true, Ordering::Release);

        // Step 2: CAS-publish a new root that includes `lsn`. The lock models the
        // single-winner root CAS; the published `Arc` is an immutable snapshot.
        {
            let _cas = self.root_cas_lock.lock().expect("root cas lock");
            let current = self.root.read().expect("root read").clone();
            let mut next = (*current).clone();
            next.visible[lsn] = true;
            *self.root.write().expect("root write") = Arc::new(next);
            // committed is set under the CAS lock, AFTER the new root is visible,
            // so committed ⊆ (visible in some published root) ⊆ appended.
            self.committed[lsn].store(true, Ordering::Release);
        }
    }

    /// CHECKPOINT capture+publish+reclaim. `use_watermark = true` is the SAFE
    /// committed-watermark checkpoint_lsn; `false` is the UNSAFE appended frontier
    /// (the negative control). Mirrors `CaptureCheckpoint`/`PublishCheckpoint`/
    /// `ReclaimWal` in the TLA spec.
    fn checkpoint(&self, use_watermark: bool) {
        // ── CAPTURE ORDERING: read checkpoint_lsn BEFORE the root load ──────────
        // (The crux: watermark/frontier first, then snapshot the root.)
        let target = if use_watermark {
            self.watermark()
        } else {
            self.appended_frontier()
        };
        // Snapshot the immutable root, restricted to committed writes ≤ target.
        let snap = self.root.read().expect("root read").clone();
        let mut image = [false; MAX_LSN + 1];
        for l in 1..=MAX_LSN {
            // A write is in the durable checkpoint image iff it is visible in the
            // captured root AND ≤ the chosen target.
            if snap.visible[l] && l <= target {
                image[l] = true;
            }
        }

        // PUBLISH: image becomes the durable checkpoint; advance checkpoint_lsn.
        *self.durable_ckpt.write().expect("ckpt write") = image;
        // checkpoint_lsn is monotone non-decreasing.
        loop {
            let cur = self.checkpoint_lsn.load(Ordering::Acquire);
            let next = cur.max(target);
            if cur == next
                || self
                    .checkpoint_lsn
                    .compare_exchange(cur, next, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                break;
            }
        }

        // RECLAIM: archive WAL ≤ checkpoint_lsn, retain > checkpoint_lsn.
        let ckpt_lsn = self.checkpoint_lsn.load(Ordering::Acquire);
        loop {
            let cur = self.wal_retained_from.load(Ordering::Acquire);
            let next = cur.max(ckpt_lsn + 1);
            if cur == next
                || self
                    .wal_retained_from
                    .compare_exchange(cur, next, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                break;
            }
        }
    }

    /// CRASH + REOPEN: recovered = durable checkpoint ∪ {durable WAL records with
    /// lsn > checkpoint_lsn that are still retained (≥ walRetainedFrom)}.
    /// Mirrors `RecoveredSet`.
    fn recovered_set(&self) -> [bool; MAX_LSN + 1] {
        let durable_ckpt = *self.durable_ckpt.read().expect("ckpt read");
        let ckpt_lsn = self.checkpoint_lsn.load(Ordering::Acquire);
        let retained_from = self.wal_retained_from.load(Ordering::Acquire);
        let mut recovered = durable_ckpt;
        for l in 1..=MAX_LSN {
            if self.appended[l].load(Ordering::Acquire) && l > ckpt_lsn && l >= retained_from {
                recovered[l] = true;
            }
        }
        recovered
    }

    /// The set of VISIBLE (committed) LSNs — the writes that were acknowledged and
    /// therefore must survive (`committed` in the TLA spec).
    fn committed_set(&self) -> [bool; MAX_LSN + 1] {
        let mut c = [false; MAX_LSN + 1];
        for l in 1..=MAX_LSN {
            c[l] = self.committed[l].load(Ordering::Acquire);
        }
        c
    }

    /// `NoLostWriteUnderLockFreeCommit`: every committed write is recovered.
    /// Returns the lost LSN (if any) for a precise witness message.
    fn first_lost_committed_write(&self) -> Option<usize> {
        let committed = self.committed_set();
        let recovered = self.recovered_set();
        (1..=MAX_LSN).find(|&l| committed[l] && !recovered[l])
    }
}

/// Drive one writer ‖ checkpointer interleaving, then crash+recover and return
/// whether a committed write was lost. `use_watermark` selects the safe vs unsafe
/// checkpoint_lsn. Two writers (LSN 1 and 2) so out-of-order commit is possible.
fn run_writers_vs_checkpointer(use_watermark: bool) -> Option<usize> {
    let model = Arc::new(Model::new());

    // Writer A takes LSN 1, writer B takes LSN 2 — but loom may run the LSN-2
    // commit BEFORE the LSN-1 commit (out-of-order), which is the whole point.
    let w1 = {
        let model = Arc::clone(&model);
        thread::spawn(move || model.order_a_write(1))
    };
    let w2 = {
        let model = Arc::clone(&model);
        thread::spawn(move || model.order_a_write(2))
    };
    // The checkpointer runs concurrently with both writers (no write-exclusion).
    let ckpt = {
        let model = Arc::clone(&model);
        thread::spawn(move || model.checkpoint(use_watermark))
    };

    w1.join().expect("writer 1");
    w2.join().expect("writer 2");
    ckpt.join().expect("checkpointer");

    // After everything quiesces, crash+reopen and check no committed write is lost.
    model.first_lost_committed_write()
}

/// **Positive — single writer ‖ checkpointer, committed watermark: no lost write.**
#[test]
fn watermark_single_writer_concurrent_checkpoint_loses_nothing() {
    loom::model(|| {
        let model = Arc::new(Model::new());

        let writer = {
            let model = Arc::clone(&model);
            thread::spawn(move || model.order_a_write(1))
        };
        let ckpt = {
            let model = Arc::clone(&model);
            thread::spawn(move || model.checkpoint(true))
        };

        writer.join().expect("writer");
        ckpt.join().expect("checkpointer");

        assert!(
            model.first_lost_committed_write().is_none(),
            "committed watermark must lose no visible write (lost LSN: {:?})",
            model.first_lost_committed_write()
        );
        // Order A holds: every committed write was durably appended first.
        for l in 1..=MAX_LSN {
            if model.committed[l].load(Ordering::Acquire) {
                assert!(
                    model.appended[l].load(Ordering::Acquire),
                    "Order A violated: LSN {l} is visible but not durable"
                );
            }
        }
    });
}

/// **Positive — TWO writers (out-of-order commit possible) ‖ checkpointer,
/// committed watermark: no lost write across ALL interleavings.** This is the
/// headline `NoLostWriteUnderLockFreeCommit` under the genuine out-of-order-commit
/// schedules the watermark exists to handle.
#[test]
fn watermark_two_writers_out_of_order_commit_concurrent_checkpoint_loses_nothing() {
    loom::model(|| {
        let lost = run_writers_vs_checkpointer(/* use_watermark = */ true);
        assert!(
            lost.is_none(),
            "committed-watermark checkpoint_lsn must never lose a visible write under \
             out-of-order lock-free commit; lost LSN {lost:?} (NoLostWriteUnderLockFreeCommit \
             violated — this MUST NOT happen with the watermark)"
        );
    });
}

/// **NEGATIVE CONTROL — appended frontier checkpoint_lsn loses a write.**
///
/// This encodes WHY the committed watermark is required (mirrors the TLA
/// `_Unsafe.cfg`, which violates `NoLostWriteUnderLockFreeCommit`). With the
/// UNSAFE appended-frontier checkpoint_lsn, loom is able to schedule:
///   1. writer B appends LSN 2 (durable) and begins committing;
///   2. writer A appends LSN 1 (durable) and begins committing;
///   3. the checkpointer captures with `ckptTarget = appendedFrontier = 2`, but at
///      capture only (say) LSN 2 is visible in the root — LSN 1 committed just
///      AFTER the snapshot;
///   4. publish: checkpoint_lsn := 2; reclaim archives WAL ≤ 2, so LSN 1's record
///      is discarded;
///   5. crash+reopen: LSN 1 is visible (committed) but neither in the checkpoint
///      image NOR in the retained WAL → LOST.
///
/// `#[should_panic]`: the assertion below FIRES on at least one explored schedule
/// (loom drives the losing interleaving). If a future change made the appended
/// frontier suddenly safe, this test would FAIL to panic and alert us that the
/// negative control no longer demonstrates the hazard.
#[test]
#[should_panic(expected = "NEGATIVE CONTROL fired")]
fn appended_frontier_negative_control_loses_a_write() {
    loom::model(|| {
        let lost = run_writers_vs_checkpointer(/* use_watermark = */ false);
        assert!(
            lost.is_none(),
            "NEGATIVE CONTROL fired: appended-frontier checkpoint_lsn lost visible LSN \
             {lost:?} (a write appended-before-but-committed-after capture was archived \
             out of recovery's reach — exactly the GAP_LEDGER #41 footgun the committed \
             watermark closes; mirrors LockFreeDurableCheckpoint _Unsafe.cfg)"
        );
    });
}
