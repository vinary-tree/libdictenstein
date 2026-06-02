//! Property-based + concurrency tests for the lock-free char overlay (Phase A).
//!
//! These exercise the genuinely-atomic `arc-swap` root and the owned-`Child`
//! path-copy write path (the leak-fix) through the public opt-in overlay API
//! (`enable_lockfree` / `insert_cas` / `contains_lockfree`):
//!
//! - `overlay_insert_contains_match_btreeset_oracle` — a random interleaving of
//!   inserts and membership queries must agree with a `BTreeSet<String>` oracle,
//!   including the "is this insert new?" boolean. A small alphabet `[a-d]` with
//!   short terms forces heavy prefix sharing, inline↔heap tier transitions, and
//!   repeated path-copies of the hot `a`-subtree.
//! - `concurrent_contended_inserts_finalize_each_term_exactly_once` — many
//!   threads racing to insert the SAME term set must finalize each term exactly
//!   once (sum of `true` returns == |distinct terms|) and leave every term
//!   visible. This stresses the CAS-retry loop + `try_set_final` race over the
//!   atomic root with owned children.
//!
//! All scratch dirs live under `target/test-tmp` (real disk) — never `/tmp`,
//! which is tmpfs (RAM) on this host.
//!
//! Run with: cargo test --features persistent-artrie --test persistent_lockfree_overlay_proptest

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::Dictionary;
use proptest::prelude::*;
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::TempDir;

/// A scratch directory on real disk (`target/test-tmp`), never tmpfs `/tmp`.
fn scratch_dir(prefix: &str) -> TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

/// A fresh lock-free overlay trie on real-disk scratch.
fn lockfree_trie(prefix: &str) -> (TempDir, PersistentARTrieChar<()>) {
    let dir = scratch_dir(prefix);
    let path = dir.path().join("overlay.artc");
    let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create trie");
    trie.enable_lockfree();
    (dir, trie)
}

#[derive(Debug, Clone)]
enum Op {
    /// Insert via CAS; must return `true` iff the term was not already present.
    Insert(String),
    /// Membership query; must match the oracle exactly.
    Contains(String),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    // Small alphabet + short terms → heavy prefix sharing and tier transitions.
    prop_oneof![
        "[a-d]{1,4}".prop_map(Op::Insert),
        "[a-d]{1,4}".prop_map(Op::Contains),
    ]
}

proptest! {
    // Disk-backed → keep the case count modest but meaningful.
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn overlay_insert_contains_match_btreeset_oracle(
        ops in prop::collection::vec(op_strategy(), 1..200)
    ) {
        let (_dir, trie) = lockfree_trie("overlay-proptest");
        let mut oracle: BTreeSet<String> = BTreeSet::new();

        for op in ops {
            match op {
                Op::Insert(t) => {
                    let expected_new = !oracle.contains(&t);
                    let got = trie.insert_cas(&t);
                    prop_assert_eq!(
                        got, expected_new,
                        "insert_cas({:?}) returned {} but oracle newness was {}",
                        t, got, expected_new
                    );
                    oracle.insert(t);
                }
                Op::Contains(t) => {
                    prop_assert_eq!(
                        trie.contains_lockfree(&t), oracle.contains(&t),
                        "contains_lockfree({:?}) disagrees with oracle", t
                    );
                }
            }
        }

        // Final reconciliation against the full oracle.
        for t in &oracle {
            prop_assert!(trie.contains_lockfree(t), "oracle term {:?} missing from overlay", t);
        }
        // Terms outside the [a-d] alphabet were never inserted.
        for absent in ["zzzz", "eeee", "qqqq"] {
            prop_assert!(!trie.contains_lockfree(absent), "absent term {:?} reported present", absent);
        }
    }
}

#[test]
fn concurrent_contended_inserts_finalize_each_term_exactly_once() {
    let (_dir, trie) = lockfree_trie("overlay-proptest-mt");
    let trie = Arc::new(trie);

    // 120 distinct terms with shared prefixes (to provoke CAS contention on the
    // shared spine), each inserted by EVERY thread → maximal contention.
    let terms: Vec<String> = (0..120).map(|i| format!("t{:03}", i)).collect();
    let n_threads = 6;
    let barrier = Arc::new(Barrier::new(n_threads));

    let handles: Vec<_> = (0..n_threads)
        .map(|_| {
            let trie = Arc::clone(&trie);
            let terms = terms.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                let mut wins = 0usize;
                for t in &terms {
                    if trie.insert_cas(t) {
                        wins += 1;
                    }
                }
                wins
            })
        })
        .collect();

    let total_wins: usize = handles
        .into_iter()
        .map(|h| h.join().expect("thread join"))
        .sum();

    // Exactly one thread finalizes each distinct term, regardless of interleaving.
    assert_eq!(
        total_wins,
        terms.len(),
        "each of {} distinct terms must be finalized exactly once across {} contending threads",
        terms.len(),
        n_threads
    );

    // Every term is visible after the race.
    for t in &terms {
        assert!(
            trie.contains_lockfree(t),
            "term {t:?} not present after concurrent insert"
        );
    }
    assert!(!trie.contains_lockfree("absent-term"));
}

/// The decisive **data-loss** witness for the prefix-insert fix.
///
/// Inserting a term that is a proper prefix of an existing term ("d" after "da")
/// must (a) report newness `true` from `insert_cas`, and (b) survive the
/// **cache-only** `merge_lockfree_to_persistent` into the persistent trie.
/// Pre-fix, `insert_cas("d")` returned `false` and skipped the lock-free cache,
/// so the merge dropped "d" — silent data loss. `contains_lockfree` masked it
/// (the overlay trie-walk still found the final node); only reading the merged
/// persistent trie via `Dictionary::contains` reveals the loss.
#[test]
fn prefix_insert_survives_merge_into_persistent_trie() {
    let (_dir, mut trie) = lockfree_trie("overlay-prefix-merge");

    assert!(trie.insert_cas("da"), "\"da\" is a new term");
    assert!(
        trie.insert_cas("d"),
        "\"d\" (a proper prefix of \"da\") is a new term — insert_cas must report true"
    );
    assert!(trie.contains_lockfree("d"));
    assert!(trie.contains_lockfree("da"));

    let merged = trie
        .merge_lockfree_to_persistent()
        .expect("merge lock-free overlay into persistent trie");
    assert_eq!(
        merged, 2,
        "both \"da\" and \"d\" must be merged (none dropped)"
    );

    // Read the PERSISTENT trie — the layer the cache-only merge writes to, and
    // where the pre-fix data loss manifested.
    assert!(
        Dictionary::contains(&trie, "d"),
        "prefix term \"d\" was lost during merge into the persistent trie (data loss)"
    );
    assert!(Dictionary::contains(&trie, "da"));
}

/// Contended version: N threads race to insert the prefix "d" (after "da").
/// Exactly one finalizes it, and it survives the merge into the persistent trie.
#[test]
fn contended_prefix_inserts_finalize_once_and_survive_merge() {
    let dir = scratch_dir("overlay-contended-prefix");
    let path = dir.path().join("overlay.artc");
    let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create trie");
    trie.enable_lockfree();
    assert!(trie.insert_cas("da"));

    let trie = Arc::new(trie);
    let n_threads = 6;
    let barrier = Arc::new(Barrier::new(n_threads));
    let handles: Vec<_> = (0..n_threads)
        .map(|_| {
            let trie = Arc::clone(&trie);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                trie.insert_cas("d") as usize
            })
        })
        .collect();
    let wins: usize = handles
        .into_iter()
        .map(|h| h.join().expect("thread join"))
        .sum();
    assert_eq!(
        wins, 1,
        "exactly one of {n_threads} threads must finalize the contended prefix term \"d\""
    );

    // All thread Arcs are dropped after join, so we can reclaim the trie to merge.
    let mut trie = Arc::try_unwrap(trie)
        .unwrap_or_else(|_| panic!("outstanding trie references after thread join"));
    trie.merge_lockfree_to_persistent().expect("merge");
    assert!(
        Dictionary::contains(&trie, "d"),
        "contended prefix term \"d\" was lost during merge (data loss)"
    );
    assert!(Dictionary::contains(&trie, "da"));
}
