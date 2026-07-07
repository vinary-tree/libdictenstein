#![cfg(feature = "persistent-artrie")]
//! **Vocab F4 — lock-collapse deadlock-freedom loom (the safety net for the vocab collapse).**
//!
//! Run exhaustively with:
//! `RUSTFLAGS="--cfg loom" cargo test --features persistent-artrie \
//!   --test vocab_lockfree_f4_lock_hierarchy_loom`
//!
//! `SharedVocabARTrie` collapsed from `Arc<RwLock<PersistentVocabARTrie>>` to a bare
//! `Arc<PersistentVocabARTrie>`. The inner trie is fully lock-free (overlay CAS + DashMap
//! forward/reverse caches + atomic counters). The only surviving inner locks are:
//!   - `CK` ← `checkpoint_lock: Arc<Mutex<()>>` — serializes concurrent `checkpoint()`.
//!   - `EC` ← `eviction_coordinator: Mutex<Option<Arc<…>>>` — a LEAF (enable/disable/force/touch).
//!
//! Vocab has NO `merge_lock` and NO owned-root lock (overlay-only), and — unlike byte/char — its
//! `checkpoint` does NOT read `EC` under `CK` (the vocab eviction callback is a no-op `|_| (0,0)`,
//! so there is no eviction-aware checkpoint publish). Hence `CK` and `EC` are INDEPENDENT — there
//! is no cross-lock nesting, so the hierarchy is trivially acyclic. The one load-bearing property
//! is `disable_eviction`'s **drop-before-join** (EC released before the worker `.join()`),
//! preserved verbatim from byte/char.
//!
//! Loom exhaustively enumerates every thread interleaving; any deadlock (a lock cycle, or a
//! join-while-holding a lock the worker needs) leaves all threads blocked and FAILS the test.
//! Each model mirrors the LOCK STRUCTURE of the real vocab methods, not their data.

use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;

/// The collapsed vocab handle's inner-lock set. `CK` and `EC` are independent (no nesting).
struct VocabLockModel {
    /// `CK` ← `checkpoint_lock`.
    ck: Mutex<()>,
    /// `EC` ← `eviction_coordinator` (`Some(())` = a coordinator is installed). A LEAF.
    eviction: Mutex<Option<()>>,
    /// The lock-free overlay's committed term count (relaxed atomic — no lock).
    overlay_committed: AtomicUsize,
}

impl VocabLockModel {
    fn new() -> Self {
        Self {
            ck: Mutex::new(()),
            eviction: Mutex::new(Some(())),
            overlay_committed: AtomicUsize::new(0),
        }
    }
}

/// `SharedVocabARTrie::checkpoint` — takes **CK**, then a lock-free overlay snapshot/publish.
/// Vocab's checkpoint does NOT touch `EC` (no eviction-aware publish — the callback is a no-op).
fn checkpoint_thread(m: &Arc<VocabLockModel>) {
    let _ck = m.ck.lock().expect("CK");
    // overlay snapshot capture + publish: lock-free against the atomic overlay root.
    m.overlay_committed.load(Ordering::Acquire);
}

/// Vocab eviction worker — the coordinator is installed but its char callback is a NO-OP; the
/// worker takes a BRIEF **EC** (accounting), releases it, and touches nothing on the overlay.
fn eviction_worker(m: &Arc<VocabLockModel>) {
    let _installed = {
        let ec = m.eviction.lock().expect("EC (worker)");
        ec.is_some()
    }; // EC dropped here — the no-op callback evicts nothing (no overlay CAS).
}

/// `disable_eviction` — take **EC**, `.take()` the coordinator into a statement-temporary, DROP
/// the EC guard, THEN join the worker (drop-before-join). Joining while holding EC would deadlock
/// (the worker briefly takes EC); joining with EC released is deadlock-free.
fn disable_thread(m: &Arc<VocabLockModel>, worker: thread::JoinHandle<()>) {
    let _taken = {
        let mut ec = m.eviction.lock().expect("EC (disable take)");
        ec.take()
    }; // EC guard dropped HERE — BEFORE the join below.
    worker.join().expect("worker join");
}

/// Lock-free overlay `insert` — takes **NOTHING**; its effect must always land (no-lost-write).
fn writer_thread(m: &Arc<VocabLockModel>) {
    m.overlay_committed.fetch_add(1, Ordering::Release);
}

/// **The headline:** `checkpoint(CK) ‖ disable_eviction(EC, drop-before-join) ‖ lock-free writer`.
/// Loom proves every schedule terminates (no deadlock) and the lock-free writer's effect lands.
#[test]
fn checkpoint_disable_writer_is_deadlock_free() {
    loom::model(|| {
        let m = Arc::new(VocabLockModel::new());

        let worker = {
            let m = Arc::clone(&m);
            thread::spawn(move || eviction_worker(&m))
        };
        let ckpt = {
            let m = Arc::clone(&m);
            thread::spawn(move || checkpoint_thread(&m))
        };
        let writer = {
            let m = Arc::clone(&m);
            thread::spawn(move || writer_thread(&m))
        };

        // `disable_eviction` on THIS thread: drop-before-join of the worker.
        disable_thread(&m, worker);

        ckpt.join().expect("checkpoint join");
        writer.join().expect("writer join");

        assert_eq!(
            m.overlay_committed.load(Ordering::Acquire),
            1,
            "the lock-free overlay write must always commit (never excluded)"
        );
    });
}

/// **Drop-before-join is LOAD-BEARING.** The worker maximally contends `EC` against
/// `disable_eviction`'s take; because disable releases EC before the join, the worker's EC
/// acquisition always makes progress (the buggy held-EC-across-join variant would hang a schedule).
#[test]
fn disable_releases_ec_before_join() {
    loom::model(|| {
        let m = Arc::new(VocabLockModel::new());

        let worker = {
            let m = Arc::clone(&m);
            thread::spawn(move || {
                let ec = m.eviction.lock().expect("worker EC");
                let _ = ec.is_some();
                drop(ec);
                m.overlay_committed.fetch_add(1, Ordering::Release);
            })
        };

        disable_thread(&m, worker);

        // Reached on EVERY schedule ⇒ no deadlock.
        let _ = m.overlay_committed.load(Ordering::Acquire);
    });
}

/// Two concurrent `checkpoint()` calls contend `CK`; loom proves both complete (CK serializes
/// checkpoint‖checkpoint without deadlock) while a lock-free writer's effect still lands.
#[test]
fn concurrent_checkpoints_serialize_via_ck() {
    loom::model(|| {
        let m = Arc::new(VocabLockModel::new());

        let c1 = {
            let m = Arc::clone(&m);
            thread::spawn(move || checkpoint_thread(&m))
        };
        let c2 = {
            let m = Arc::clone(&m);
            thread::spawn(move || checkpoint_thread(&m))
        };

        writer_thread(&m);

        c1.join().expect("checkpoint c1 join");
        c2.join().expect("checkpoint c2 join");

        assert_eq!(
            m.overlay_committed.load(Ordering::Acquire),
            1,
            "the lock-free writer's effect lands regardless of checkpoint serialization"
        );
    });
}
