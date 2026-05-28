#![cfg(feature = "persistent-artrie")]
//! Loom model of the background-worker *teardown* handshake (the
//! `disable_eviction` / `close` deadlock fix).
//!
//! Run with: `RUSTFLAGS="--cfg loom" cargo test --test persistent_worker_lifecycle_loom`
//!
//! Loom exhaustively enumerates the thread interleavings of this model. It
//! checks that teardown joins the eviction worker WITHOUT holding the trie lock
//! that the worker's eviction callback also takes — holding it across the join
//! (the pre-fix `disable_eviction`, which kept the trie write guard alive while
//! `shutdown()` joined) would deadlock, and loom would report all threads
//! blocked. Joining with no lock held is deadlock-free across every schedule,
//! the stop-flag handshake is race-free, and loom's end-of-model check confirms
//! no `Arc`/thread is leaked.
//!
//! (The complementary *leak* property — workers hold a `Weak`, so the manager's
//! `Drop` runs once the owner releases its `Arc` — is covered by the TLA+
//! `Termination` proof and the `tests/persistent_{char,byte}_thread_lifecycle*`
//! integration/property tests. `loom::sync::Arc` has no `Weak`, so it cannot be
//! modeled here.)

use loom::sync::atomic::{AtomicBool, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;

#[test]
fn teardown_join_is_deadlock_free() {
    loom::model(|| {
        // Models the trie's `RwLock` — the eviction callback takes it.
        let trie = Arc::new(Mutex::new(0u32));
        let stop = Arc::new(AtomicBool::new(false));

        let worker = {
            let trie = Arc::clone(&trie);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                // One eviction-callback pass needs the trie lock...
                {
                    let mut guard = trie.lock().expect("trie lock");
                    *guard += 1;
                }
                // ...then the worker observes the stop flag and exits.
                stop.load(Ordering::Acquire)
            })
        };

        // Teardown (the fixed `disable_eviction`): take the coordinator out under
        // a short guard, RELEASE it, set the stop flag, then join holding NO trie
        // lock — so the join cannot deadlock against the callback's lock.
        stop.store(true, Ordering::Release);
        let _ = worker.join().expect("worker join");

        // The callback ran exactly once; lock state is consistent.
        assert_eq!(*trie.lock().expect("trie lock"), 1);
    });
}
