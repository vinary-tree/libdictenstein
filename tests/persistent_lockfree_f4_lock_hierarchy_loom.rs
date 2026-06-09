#![cfg(feature = "persistent-artrie")]
//! **L3.3c — the post-owned-deletion lock-collapse deadlock-freedom loom (the MANDATORY safety net).**
//!
//! Run with: `RUSTFLAGS="--cfg loom" cargo test --features persistent-artrie \
//!   --test persistent_lockfree_f4_lock_hierarchy_loom`
//!
//! ## What this proves
//! Phase F collapsed `SharedARTrie`/`SharedCharARTrie` from `Arc<RwLock<…>>` to a
//! bare `Arc<…>`, and **L3.3c DELETED the owned trie entirely** — the inner
//! `root: RwLock<TrieRoot<V>>` (the "OR" lock the prior model carried) is GONE.
//! Overlay reads AND writes are lock-free; the only remaining mutual-exclusion
//! users take dedicated INNER locks, which MUST obey the hard, acyclic hierarchy
//!
//! ```text
//!     CK  >  merge_lock  >  EC
//! ```
//!
//! (acquire only in that order; `EC` — the eviction-coordinator `Mutex` — is a
//! strict LEAF: never held across acquiring `CK`/`merge_lock`, and never held
//! across a worker `.join()` — the drop-before-join discipline). The owned-root
//! "OR" rung no longer exists: with no owned tree, no owned reader/writer and no
//! eviction-unswizzle ever takes a root lock — they are all lock-free overlay CAS
//! now. So the only surviving cross-lock nesting is `CK > EC` (the checkpoint's
//! eviction-registry publish reads EC under CK); `merge_lock` is taken alone before
//! lock-free overlay CAS.
//!
//! This is the owner's #1 stated risk ("a trie lock-up in production costs money").
//! Loom **exhaustively enumerates every thread interleaving** of the model below;
//! if any schedule could deadlock (a lock-acquisition cycle, or a join-while-holding
//! a lock the worker needs), loom reports all threads blocked and the test FAILS.
//! It also checks a no-lost-write invariant: every acknowledged writer's effect is
//! observed (the overlay write is lock-free, so it is never excluded by checkpoint,
//! eviction, or disable).
//!
//! ## Faithfulness to the real code
//! These are *small loom models* — they mirror the LOCK STRUCTURE of the real
//! methods, not their data. Each lock and each acquisition order is taken straight
//! from the post-L3.3c implementation:
//! - `checkpoint()` (the `Shared*` trait wrapper) takes **CK**, then the eviction-on
//!   publisher reads **EC** under CK (`CK > EC`); the overlay snapshot capture +
//!   publish is lock-free against the atomic overlay root — modeled in
//!   [`checkpoint_thread`].
//! - the eviction reclaim callback clones the coordinator out under a BRIEF **EC**
//!   lock, releases EC, then unswizzles the overlay slot via a lock-free CAS (no
//!   root lock) — modeled in [`eviction_worker`].
//! - `disable_eviction()` takes **EC**, `.take()`s the coordinator, DROPS the EC
//!   guard, THEN joins the worker (drop-before-join) — modeled in [`disable_thread`].
//! - a lock-free overlay `insert` takes **NOTHING** (a relaxed atomic publish) —
//!   modeled in [`writer_thread`].
//! - a merge driver takes **merge_lock**, then runs lock-free overlay CAS (the owned
//!   `merge_lock > OR` nesting is gone — merge writes via the overlay now) — modeled
//!   in [`merge_thread`].

use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;

/// The collapsed trie's inner-lock set, in hierarchy order `CK > merge_lock > EC`.
/// Mirrors the wrapped post-L3.3c fields exactly:
/// - `ck`         ← `checkpoint_lock: Arc<Mutex<()>>`
/// - `merge_lock` ← `merge_lock: Arc<Mutex<()>>`
/// - `eviction`   ← `eviction_coordinator: Mutex<Option<Arc<…>>>` (EC, a LEAF)
/// - `overlay_committed` ← the lock-free `lockfree_root: AtomicNodePtr` (no lock)
///
/// The owned-root `RwLock<TrieRoot<V>>` ("OR") the prior model carried is DELETED
/// (L3.3c) — there is no owned tree, so it cannot appear in the hierarchy.
struct LockModel {
    ck: Mutex<()>,
    merge_lock: Mutex<()>,
    /// EC — `Some(())` = a coordinator is installed. A LEAF lock.
    eviction: Mutex<Option<()>>,
    /// The lock-free overlay's committed term count (relaxed atomic — no lock).
    overlay_committed: AtomicUsize,
}

impl LockModel {
    fn new() -> Self {
        Self {
            ck: Mutex::new(()),
            merge_lock: Mutex::new(()),
            eviction: Mutex::new(Some(())),
            overlay_committed: AtomicUsize::new(0),
        }
    }
}

/// **Checkpoint thread** — the `Shared*::checkpoint` wrapper.
///
/// Takes **CK** (serialize concurrent checkpoints). The eviction-on overlay
/// publisher reads **EC** under CK (`CK > EC` — EC stays a leaf), then releases EC.
/// The overlay snapshot capture/publish is lock-free against the atomic overlay
/// root (no root lock — OR is gone).
fn checkpoint_thread(m: &Arc<LockModel>) {
    let _ck = m.ck.lock().expect("CK");
    // eviction-on publisher: read EC under CK, then RELEASE EC (leaf — not held
    // across the lock-free overlay publish below).
    {
        let _ec = m.eviction.lock().expect("EC under CK");
        // (in the real code: `eviction_coordinator.lock().is_some()` →
        // `publish_*_with_eviction`; the snapshot/publish is lock-free against the
        // overlay root.)
    } // EC dropped here
      // overlay snapshot capture + publish: lock-free against the atomic root.
    m.overlay_committed.load(Ordering::Acquire);
}

/// **Eviction reclaim callback** — `evict_overlay_nodes` / the char twin.
///
/// Clones the coordinator out under a BRIEF **EC** lock, RELEASES EC, then
/// unswizzles the overlay slot via a lock-free CAS (no root lock — OR is gone).
/// Order: EC taken first then released, THEN the lock-free CAS — never held
/// together with any other lock.
fn eviction_worker(m: &Arc<LockModel>) {
    // Brief EC: clone the coordinator Arc out, then RELEASE before the lock-free CAS.
    let installed = {
        let ec = m.eviction.lock().expect("EC (callback clone-out)");
        ec.is_some()
    }; // EC dropped here
    if installed {
        // Overlay unswizzle is a lock-free CAS publish (no OR write).
        m.overlay_committed.load(Ordering::Acquire);
    }
}

/// **`disable_eviction`** — the drop-before-join discipline (GAP 1 / V11.3).
///
/// Takes **EC**, `.take()`s the coordinator into a statement-temporary, DROPS the
/// EC guard, THEN joins the worker. Joining while holding EC would deadlock (the
/// worker briefly takes EC); joining with EC released is deadlock-free.
fn disable_thread(m: &Arc<LockModel>, worker: thread::JoinHandle<()>) {
    let _taken = {
        let mut ec = m.eviction.lock().expect("EC (disable take)");
        ec.take()
    }; // EC guard dropped HERE — BEFORE the join below.
       // Drop-before-join: the worker may still take EC briefly; we hold nothing.
    worker.join().expect("worker join");
}

/// **Lock-free overlay writer** — `insert` / `insert_cas_durable` under the overlay.
///
/// Takes **NOTHING** (the production hot path): the durable WAL append + the root
/// CAS are lock-free, so a writer is NEVER excluded by checkpoint / eviction /
/// disable. Modeled as a relaxed atomic publish; its effect must always be observed
/// (no-lost-write).
fn writer_thread(m: &Arc<LockModel>) {
    m.overlay_committed.fetch_add(1, Ordering::Release);
}

/// **Merge driver** — `union_with` / `merge_from_parallel` → `merge_entries_overlay`.
///
/// Takes **merge_lock** (the merge‖merge serializer), then runs lock-free overlay
/// CAS per key. Never CK, never EC, never a root lock (the owned `merge_lock > OR`
/// nesting is gone — merge writes via the overlay now).
fn merge_thread(m: &Arc<LockModel>) {
    let _ml = m.merge_lock.lock().expect("merge_lock");
    // overlay merge funnel: lock-free CAS per key (no OR write).
    m.overlay_committed.load(Ordering::Acquire);
}

/// **THE headline:** `checkpoint(+eviction) ‖ disable_eviction ‖ writer`
/// (the exact scenario the task + design §7 mandate). Loom proves every schedule
/// terminates (no deadlock) and the lock-free writer's effect always lands.
#[test]
fn checkpoint_evict_disable_writer_is_deadlock_free() {
    loom::model(|| {
        let m = Arc::new(LockModel::new());

        // The eviction worker (one reclaim pass): EC-brief → lock-free CAS.
        let worker = {
            let m = Arc::clone(&m);
            thread::spawn(move || eviction_worker(&m))
        };

        // Concurrent checkpoint (eviction-on): CK → EC(brief) → lock-free publish.
        let ckpt = {
            let m = Arc::clone(&m);
            thread::spawn(move || checkpoint_thread(&m))
        };

        // Concurrent lock-free writer (takes nothing).
        let writer = {
            let m = Arc::clone(&m);
            thread::spawn(move || writer_thread(&m))
        };

        // `disable_eviction` on THIS thread: drop-before-join of the worker.
        disable_thread(&m, worker);

        ckpt.join().expect("checkpoint join");
        writer.join().expect("writer join");

        // No-lost-write: the lock-free writer's single publish is observed (it is
        // never excluded by checkpoint/eviction/disable).
        assert_eq!(
            m.overlay_committed.load(Ordering::Acquire),
            1,
            "the lock-free overlay write must always commit (never excluded)"
        );
    });
}

/// **Full hierarchy stress:** a merge driver (`merge_lock`) ‖ a checkpoint
/// (`CK > EC`) ‖ an eviction worker (`EC`) ‖ a lock-free writer. Proves the
/// `CK > merge_lock > EC` graph is acyclic across every interleaving (no schedule
/// blocks all threads) now that the owned "OR" rung is gone. Three live threads +
/// main keep the loom state space tractable while covering every surviving
/// lock-order edge (`CK > EC`, plus the independent `merge_lock` and the lock-free
/// writer that must never be excluded).
#[test]
fn full_hierarchy_no_cycle() {
    loom::model(|| {
        let m = Arc::new(LockModel::new());

        let merger = {
            let m = Arc::clone(&m);
            thread::spawn(move || merge_thread(&m))
        };
        let ckpt = {
            let m = Arc::clone(&m);
            thread::spawn(move || checkpoint_thread(&m))
        };
        let evictor = {
            let m = Arc::clone(&m);
            thread::spawn(move || eviction_worker(&m))
        };

        // Lock-free overlay writer on the main thread (takes nothing).
        writer_thread(&m);

        merger.join().expect("merge join");
        ckpt.join().expect("checkpoint join");
        evictor.join().expect("evict join");

        // Loom's all-schedules termination is the deadlock-freedom proof. The merge
        // + checkpoint + eviction paths only READ the lock-free overlay; only the
        // writer publishes, and its single effect must always land (no-lost-write).
        assert_eq!(
            m.overlay_committed.load(Ordering::Acquire),
            1,
            "the lock-free overlay write must always commit (never excluded by \
             merge/checkpoint/eviction)"
        );
    });
}

/// **The negative control (drop-before-join is LOAD-BEARING).** This is the
/// PRE-F4-fix / hazard pattern: `disable_eviction` holds the EC guard ACROSS the
/// worker join while the worker also needs EC. Under loom this deadlocks on the
/// schedule where the worker reaches its EC acquisition only after disable has
/// taken EC and is blocked in `join()` — all threads blocked. We assert that the
/// CORRECT (drop-before-join) discipline does NOT exhibit this, by re-running the
/// headline scenario with the worker FORCED to contend EC against disable.
///
/// (We do not actually run the buggy variant — loom would hang it by design — but
/// this test pins the property that EC is released before the join so the worker's
/// EC acquisition can always make progress.)
#[test]
fn disable_releases_ec_before_join() {
    loom::model(|| {
        let m = Arc::new(LockModel::new());

        // Worker that CONTENDS EC (takes EC, observes the installed flag, releases)
        // — maximizing the EC contention against `disable_eviction`'s take, then
        // runs a lock-free overlay CAS (order: EC released BEFORE the lock-free work).
        let worker = {
            let m = Arc::clone(&m);
            thread::spawn(move || {
                let ec = m.eviction.lock().expect("worker EC");
                // The worker observes whatever disable left (Some before, None after).
                let _ = ec.is_some();
                drop(ec);
                // Lock-free overlay CAS after releasing EC (no root lock — OR is gone).
                m.overlay_committed.fetch_add(1, Ordering::Release);
            })
        };

        // Correct disable: take EC, drop guard, THEN join — never blocks the worker's
        // EC acquisition.
        disable_thread(&m, worker);

        // Reached here on EVERY schedule ⇒ no deadlock (the buggy held-EC-across-join
        // variant would have hung at least one schedule).
        let _ = m.overlay_committed.load(Ordering::Acquire);
    });
}
