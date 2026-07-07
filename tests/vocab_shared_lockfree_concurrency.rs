//! Runtime concurrency for the collapsed `SharedVocabARTrie` (`Arc<PersistentVocabARTrie>`).
//!
//! After the F4 lock-collapse there is no outer `RwLock`, so concurrent readers, writers, and a
//! checkpoint must all make progress on the shared handle without blocking each other. Every
//! thread's `.join()` returning is the deadlock-freedom signal; the final assertions pin
//! correctness (no lost/duplicated writes, reverse-map round-trip, durability across reopen). The
//! term count spans multiple arenas, so the concurrent checkpoint loop also exercises the
//! multi-checkpoint arena→block mapping fix under load.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::vocab::PersistentVocabARTrie;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

const WRITERS: usize = 8;
const PER_WRITER: usize = 300;
const READERS: usize = 8;
const SEEDS: usize = 100;

#[test]
fn shared_arc_concurrent_readers_writers_checkpoint_no_deadlock() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("v.vocab");
    let trie = Arc::new(PersistentVocabARTrie::create(&path).unwrap());

    for i in 0..SEEDS {
        trie.insert(&format!("seed{i}")).expect("seed insert");
    }

    let mut handles = Vec::with_capacity(WRITERS + READERS + 1);

    // Writers — each inserts a disjoint block of terms through the shared handle (`&self`, no lock).
    for w in 0..WRITERS {
        let t = Arc::clone(&trie);
        handles.push(thread::spawn(move || {
            for i in 0..PER_WRITER {
                t.insert(&format!("w{w}_{i}")).expect("concurrent insert");
            }
        }));
    }

    // Readers — hammer the lock-free read path concurrently with the writers. Pre-collapse these
    // would have blocked on the outer write lock; now they never block.
    for _ in 0..READERS {
        let t = Arc::clone(&trie);
        handles.push(thread::spawn(move || {
            for _ in 0..1000 {
                let _ = t.get_index("seed0");
                let _ = t.contains("seed25");
                let _ = t.get_term(0);
                let _ = t.len();
            }
        }));
    }

    // A checkpoint thread running concurrently with reads + writes (insert‖checkpoint is safe: the
    // committed watermark is captured before the immutable-root snapshot). Repeated checkpoints
    // while writers grow the trie past a single arena also exercise the multi-checkpoint
    // arena→block mapping fix under concurrency.
    {
        let t = Arc::clone(&trie);
        handles.push(thread::spawn(move || {
            for _ in 0..12 {
                t.checkpoint().expect("concurrent checkpoint");
                thread::sleep(Duration::from_millis(2));
            }
        }));
    }

    // All joins returning == no deadlock.
    for h in handles {
        h.join().expect("thread join (no deadlock)");
    }

    // Correctness: every seed and every writer term is present exactly once.
    for i in 0..SEEDS {
        assert!(trie.contains(&format!("seed{i}")), "missing seed{i}");
    }
    for w in 0..WRITERS {
        for i in 0..PER_WRITER {
            assert!(trie.contains(&format!("w{w}_{i}")), "missing w{w}_{i}");
        }
    }
    assert_eq!(
        trie.len(),
        SEEDS + WRITERS * PER_WRITER,
        "no lost or duplicated writes under concurrency"
    );

    // Reverse map (id → term) round-trips: get_index(term) → id, get_term(id) → term.
    for w in 0..WRITERS {
        for i in (0..PER_WRITER).step_by(37) {
            let term = format!("w{w}_{i}");
            let id = trie.get_index(&term).expect("term has an id");
            assert_eq!(
                trie.get_term(id).as_deref(),
                Some(term.as_str()),
                "reverse map disagrees for {term:?} (id {id})"
            );
        }
    }

    // Survives drop → reopen (durability of the concurrently-built, multi-checkpoint image).
    drop(trie);
    let (reopened, _r) =
        PersistentVocabARTrie::open_with_recovery(&path).expect("reopen after concurrent build");
    assert_eq!(
        reopened.len(),
        SEEDS + WRITERS * PER_WRITER,
        "reopen len after concurrent build"
    );
    for w in 0..WRITERS {
        assert!(
            reopened.contains(&format!("w{w}_{}", PER_WRITER - 1)),
            "reopen missing w{w}_{}",
            PER_WRITER - 1
        );
    }
}
