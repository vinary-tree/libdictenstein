//! Correspondence test for the non-blocking (write→read **downgrade**) char
//! checkpoint (`SharedCharARTrie::checkpoint`, `mod.rs`).
//!
//! Drives concurrent readers + a writer + a checkpointer on one shared trie,
//! then reopens from disk and asserts every committed key survived (no lost
//! write — the GAP_LEDGER #41 footgun the downgrade avoids) and that readers
//! never observed a torn / vanishing key during a concurrent checkpoint. The
//! reader making progress while checkpoints run is the executable witness of the
//! non-blocking property (the statistical magnitude is proven separately by
//! pgmcp experiment #11 / `examples/exp_checkpoint_throughput.rs`).
//!
//! Resource-safe: the disk-backed trie is created on a REAL-disk scratch dir
//! under `target/` (never tmpfs/`/tmp`), with a hard cap on checkpoint rounds so
//! the copy-on-serialize arena cannot balloon.

#![cfg(feature = "persistent-artrie")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use libdictenstein::persistent_artrie::char::{PersistentARTrieChar, SharedCharARTrie};
use libdictenstein::{ARTrie, Dictionary, MappedDictionary};

/// Real-disk scratch tempdir (NOT the default temp dir, which is tmpfs here).
fn scratch() -> tempfile::TempDir {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-scratch");
    std::fs::create_dir_all(&dir).expect("create real-disk scratch dir");
    tempfile::Builder::new()
        .prefix("nbckpt")
        .tempdir_in(dir)
        .expect("scratch tempdir")
}

const SEED: usize = 200;
const WRITES: usize = 200;
const MAX_CHECKPOINTS: usize = 50; // hard cap → bounded arena growth on disk

#[test]
fn nonblocking_checkpoint_preserves_data_under_concurrent_reads_writes() {
    for round in 0..4 {
        let dir = scratch();
        let path = dir.path().join(format!("nb_{round}.part"));
        let trie: SharedCharARTrie<i64> = ARTrie::create(&path).expect("create shared char trie");

        for i in 0..SEED {
            assert!(ARTrie::insert_with_value(
                &trie,
                &format!("seed-{i}"),
                i as i64
            ));
        }
        ARTrie::checkpoint(&trie).expect("seed checkpoint");

        let barrier = Arc::new(Barrier::new(4)); // reader + writer + checkpointer + main
        let stop = Arc::new(AtomicBool::new(false));

        // Reader: a seeded key must never vanish, even mid-checkpoint.
        let reader = {
            let trie = Arc::clone(&trie);
            let barrier = Arc::clone(&barrier);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                barrier.wait();
                let mut ops = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    let i = ops % SEED;
                    assert!(
                        Dictionary::contains(&trie, &format!("seed-{i}")),
                        "seed key vanished during a concurrent checkpoint"
                    );
                    ops += 1;
                }
                ops
            })
        };

        // Writer: inserts new keys concurrently with checkpoints.
        let writer = {
            let trie = Arc::clone(&trie);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for i in 0..WRITES {
                    assert!(ARTrie::insert_with_value(
                        &trie,
                        &format!("w-{round}-{i}"),
                        1000 + i as i64
                    ));
                }
            })
        };

        // Checkpointer: non-blocking checkpoints, hard-capped + throttled.
        let checkpointer = {
            let trie = Arc::clone(&trie);
            let barrier = Arc::clone(&barrier);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                barrier.wait();
                let mut n = 0usize;
                while !stop.load(Ordering::Relaxed) && n < MAX_CHECKPOINTS {
                    ARTrie::checkpoint(&trie).expect("concurrent non-blocking checkpoint");
                    n += 1;
                    thread::sleep(std::time::Duration::from_millis(1));
                }
                n
            })
        };

        barrier.wait();
        writer.join().expect("writer thread");
        stop.store(true, Ordering::Relaxed);
        let reader_ops = reader.join().expect("reader thread");
        let checkpoint_rounds = checkpointer.join().expect("checkpointer thread");

        // Flush the tail durably.
        ARTrie::checkpoint(&trie).expect("final checkpoint");
        ARTrie::sync(&trie).expect("final sync");

        // The reader made progress concurrently with checkpoints (non-blocking).
        assert!(reader_ops > 0, "reader made no progress");
        assert!(checkpoint_rounds > 0, "no concurrent checkpoint ran");

        // All writes visible in-memory.
        for i in 0..WRITES {
            assert_eq!(
                MappedDictionary::get_value(&trie, &format!("w-{round}-{i}")),
                Some(1000 + i as i64),
                "in-memory write missing"
            );
        }

        // Reopen from disk: every committed key must survive (durability).
        drop(trie);
        let reopened = PersistentARTrieChar::<i64>::open(&path).expect("reopen");
        for i in 0..SEED {
            assert!(
                reopened.contains(&format!("seed-{i}")),
                "seed key lost after reopen (round {round}, i {i})"
            );
        }
        for i in 0..WRITES {
            // F2-migrate: Bucket A — `get()` returns None under the overlay; read via `get_value`.
            assert_eq!(
                reopened.get_value(&format!("w-{round}-{i}")),
                Some(1000 + i as i64),
                "write lost after reopen (round {round}, i {i})"
            );
        }
    }
}
