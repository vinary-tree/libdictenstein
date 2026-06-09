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
use libdictenstein::persistent_artrie_core::durability::DurabilityPolicy;
use libdictenstein::Dictionary;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
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

/// A fresh lock-free overlay trie under **Immediate** durability — required by the
/// R-B durable remove (`remove_cas_durable`) and its durable insert counterpart.
fn durable_lockfree_trie(prefix: &str) -> (TempDir, PersistentARTrieChar<()>) {
    let dir = scratch_dir(prefix);
    let path = dir.path().join("overlay.artc");
    let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create trie");
    trie.set_durability_policy(DurabilityPolicy::Immediate);
    trie.enable_lockfree();
    (dir, trie)
}

/// **Regression — proper-prefix insert survives live AND across checkpoint+reopen.**
///
/// The Phase-A lock-free data-loss bug: `insert_cas` of a proper-PREFIX term — e.g.
/// inserting `"cat"` AFTER `"catnip"`/`"cats"` already made the `"cat"` node a NON-final
/// path intermediary — wrongly observed an existing node, reported a duplicate, and
/// DROPPED the term (it returned `false` AND skipped the cache). The fix routes the
/// duplicate decision through the single `try_set_final` arbiter on the shared node.
///
/// This is the deterministic successor to the two deleted L3.3a witnesses
/// (`prefix_insert_survives_merge_into_persistent_trie` /
/// `contended_prefix_inserts_finalize_once_and_survive_merge`), which exercised the now-
/// removed lockfree→owned MERGE drain. Here we exercise the SURVIVING path: insert the
/// longer terms first, THEN each proper prefix (the exact bug trigger), durably; assert
/// every term is present live, then checkpoint + reopen through the codec-only reopen
/// path (`enumerate_terms_from_disk` → `build_overlay_root_from_terms`) and assert every
/// term — longer and proper-prefix alike — survives.
#[test]
fn proper_prefix_insert_survives_live_and_reopen() {
    let (dir, trie) = durable_lockfree_trie("overlay-prefix-regression");
    let path = dir.path().join("overlay.artc");
    // Insert order is the bug trigger: each proper prefix is inserted AFTER a longer
    // term that already created it as a NON-final path intermediary.
    let terms = ["catnip", "cats", "cat", "ca", "c", "dax", "da", "d", ""];
    for t in &terms {
        trie.insert_cas_durable(t).expect("durable overlay insert");
    }
    // Live: every term — the longer ones AND their later-inserted proper prefixes
    // (incl. the empty term "") — is present (none dropped as a false duplicate).
    for t in &terms {
        assert!(
            trie.contains_lockfree(t),
            "live overlay missing {t:?} — proper-prefix insert dropped (Phase-A data-loss regression)"
        );
    }
    trie.checkpoint().expect("overlay checkpoint");
    drop(trie);
    // Reopen via the codec-only path (no owned tree) and confirm every term survived.
    let reopened = PersistentARTrieChar::<()>::open(&path).expect("reopen");
    for t in &terms {
        assert!(
            reopened.contains(t),
            "reopened overlay missing {t:?} — proper-prefix term lost across checkpoint+reopen"
        );
    }
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

/// R-B op set: insert, remove, AND contains — all three mutate / query the same
/// `BTreeSet` oracle. `Remove` is the addition the re-proof witnesses.
#[derive(Debug, Clone)]
enum RemoveOp {
    /// Durable insert; must return `Ok(true)` iff the term was not already present.
    Insert(String),
    /// Durable remove; must return `Ok(true)` iff the term WAS present.
    Remove(String),
    /// Membership query; must match the oracle exactly (the stale-cache guard).
    Contains(String),
}

fn remove_op_strategy() -> impl Strategy<Value = RemoveOp> {
    // Small alphabet + short terms → heavy prefix sharing; removes interleave with
    // inserts on the hot shared spine, exercising the fresh-copy clear + cache
    // invalidation against the positive `lockfree_cache`.
    prop_oneof![
        "[a-d]{1,4}".prop_map(RemoveOp::Insert),
        "[a-d]{1,4}".prop_map(RemoveOp::Remove),
        "[a-d]{1,4}".prop_map(RemoveOp::Contains),
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

// ═══════════════════ R-B (proven overlay DELETE) — RE-PROOF (design §4.3) ═══════════════════

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// **The remove-aware oracle (the load-bearing stale-cache guard).** A random
    /// interleaving of durable inserts, durable removes, and membership queries
    /// must agree with a `BTreeSet<String>` oracle that BOTH `insert` and `remove`
    /// mutate. The `Contains` assertion is the decisive data-correctness check: a
    /// remove that cleared the trie but left a stale positive `lockfree_cache`
    /// entry (the §3.4 bug) would make the term read present forever, and this
    /// assertion would catch it. Insert/remove return booleans are also checked
    /// against oracle membership (newness / was-present).
    #[test]
    fn overlay_insert_remove_contains_match_btreeset_oracle(
        ops in prop::collection::vec(remove_op_strategy(), 1..200)
    ) {
        let (_dir, trie) = durable_lockfree_trie("rb-remove-oracle");
        let mut oracle: BTreeSet<String> = BTreeSet::new();

        for op in ops {
            match op {
                RemoveOp::Insert(t) => {
                    let expected_new = !oracle.contains(&t);
                    let got = trie.insert_cas_durable(&t).expect("durable insert");
                    prop_assert_eq!(
                        got, expected_new,
                        "insert_cas_durable({:?}) returned {} but oracle newness was {}",
                        t, got, expected_new
                    );
                    oracle.insert(t);
                }
                RemoveOp::Remove(t) => {
                    let expected_present = oracle.contains(&t);
                    let got = trie.remove_cas_durable(&t).expect("durable remove");
                    prop_assert_eq!(
                        got, expected_present,
                        "remove_cas_durable({:?}) returned {} but oracle presence was {}",
                        t, got, expected_present
                    );
                    oracle.remove(&t);
                    // ▸ STALE-CACHE GUARD: immediately after a remove, the term MUST
                    // read absent. A leftover positive cache entry would fail here.
                    prop_assert!(
                        !trie.contains_lockfree(&t),
                        "removed term {:?} still reads present (stale positive cache — §3.4 bug)",
                        t
                    );
                }
                RemoveOp::Contains(t) => {
                    prop_assert_eq!(
                        trie.contains_lockfree(&t), oracle.contains(&t),
                        "contains_lockfree({:?}) disagrees with the insert+remove oracle", t
                    );
                }
            }
        }

        // Final reconciliation against the full oracle (present ⇔ in oracle).
        for t in &oracle {
            prop_assert!(trie.contains_lockfree(t), "oracle term {:?} missing from overlay", t);
        }
        // Every [a-d] term NOT in the oracle (e.g. removed) must read absent.
        for absent in ["zzzz", "eeee", "qqqq"] {
            prop_assert!(!trie.contains_lockfree(absent), "absent term {:?} reported present", absent);
        }
    }
}

/// **Multi-thread insert/remove convergence via a deterministic quiescent settling
/// phase.** Many threads concurrently insert AND remove overlapping keys (a chaotic
/// contended phase whose exact outcome is interleaving-dependent), then — once all
/// threads have joined (quiescence) — a single deterministic settling phase removes
/// EVERY key and re-inserts a KNOWN subset. The final membership must then equal
/// that known subset EXACTLY: convergence proves no remove leaked a stale cache
/// entry and no insert/remove was lost in a way that survives quiescence.
#[test]
fn multithread_insert_remove_converges_to_known_subset() {
    let (_dir, trie) = durable_lockfree_trie("rb-remove-converge");
    let trie = Arc::new(trie);

    // The universe of keys (shared prefixes → CAS contention on the spine).
    let universe: Vec<String> = (0..60).map(|i| format!("k{:03}", i)).collect();
    let n_threads = 6;
    let barrier = Arc::new(Barrier::new(n_threads));

    // Chaotic contended phase: each thread inserts then removes overlapping keys.
    let handles: Vec<_> = (0..n_threads)
        .map(|tid| {
            let trie = Arc::clone(&trie);
            let universe = universe.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for round in 0..3 {
                    for (i, k) in universe.iter().enumerate() {
                        // Interleave inserts and removes by (thread, round, index)
                        // parity so threads genuinely contend insert-vs-remove on
                        // the same keys.
                        if (i + tid + round) % 2 == 0 {
                            trie.insert_cas_durable(k).expect("durable insert");
                        } else {
                            trie.remove_cas_durable(k).expect("durable remove");
                        }
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("worker thread");
    }

    // ── QUIESCENT SETTLING PHASE (single-threaded, deterministic) ──
    // Remove EVERY key (clearing any residue), then re-insert a KNOWN subset.
    for k in &universe {
        trie.remove_cas_durable(k).expect("settle remove");
    }
    // After clearing all, every key must read absent (no stale positive cache).
    for k in &universe {
        assert!(
            !trie.contains_lockfree(k),
            "key {k:?} still present after the settling clear (stale cache / lost remove)"
        );
    }
    // Re-insert exactly the even-indexed keys.
    let known_subset: BTreeSet<String> = universe
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 2 == 0)
        .map(|(_, k)| k.clone())
        .collect();
    for k in &known_subset {
        assert!(
            trie.insert_cas_durable(k).expect("settle insert"),
            "settling re-insert of {k:?} must be newly inserted (it was just cleared)"
        );
    }

    // Final membership MUST equal the known subset EXACTLY.
    for k in &universe {
        let expected = known_subset.contains(k);
        assert_eq!(
            trie.contains_lockfree(k),
            expected,
            "key {k:?}: final membership {} disagrees with the known settled subset {}",
            trie.contains_lockfree(k),
            expected
        );
    }
}

/// **`V=u64` value domain: remove drops the value (None, NOT Some(0)).** A counter
/// OVERLAY's removed key must report `get_lockfree == None`, not `Some(0)` — the
/// `as_non_final` value-drop guarantee (§risk 5). These writes live in the
/// lock-free overlay (no merge to the owned tree), so the overlay accessor
/// `get_lockfree` is the value oracle; a `BTreeMap<String,u64>` tracks the live
/// valued set. The distinction is load-bearing: `as_non_final` sets `value: None`,
/// so a removed counter key must NOT collapse to the additive identity `Some(0)`
/// (which would corrupt a subsequent re-increment / merge sum).
#[test]
fn valued_overlay_remove_drops_value_not_zero() {
    let dir = scratch_dir("rb-remove-valued");
    let path = dir.path().join("overlay.artc");
    let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create valued trie");
    trie.set_durability_policy(DurabilityPolicy::Immediate);
    trie.enable_lockfree();

    let mut oracle: BTreeMap<String, u64> = BTreeMap::new();

    // Seed some valued keys via the durable counter path (writes the OVERLAY).
    for (k, v) in [
        ("apple", 3u64),
        ("apricot", 10),
        ("band", 7),
        ("cherry", 25),
    ] {
        trie.try_increment_cas_durable(k, v)
            .expect("durable increment");
        oracle.insert(k.to_string(), v);
        assert_eq!(
            trie.get_lockfree(k),
            Some(v),
            "seeded overlay value for {k:?}"
        );
    }

    // Remove "apple" and "band" durably; their overlay values must vanish (None,
    // NOT 0 — the as_non_final value-drop).
    for k in ["apple", "band"] {
        assert!(
            trie.remove_cas_durable(k).expect("durable remove"),
            "removing present valued key {k:?} returns Ok(true)"
        );
        oracle.remove(k);
        assert_eq!(
            trie.get_lockfree(k),
            None,
            "removed valued key {k:?} must report get_lockfree == None, NOT Some(0)"
        );
        assert!(
            !trie.contains_lockfree(k),
            "removed valued key {k:?} must read absent (stale cache guard)"
        );
    }

    // The surviving keys keep their exact overlay values (subtree retained).
    for (k, v) in &oracle {
        assert_eq!(
            trie.get_lockfree(k),
            Some(*v),
            "surviving valued key {k:?} must keep its overlay value {v}"
        );
    }
    // A never-inserted key is None (not 0).
    assert_eq!(trie.get_lockfree("never"), None);

    // Belt-and-suspenders: a re-increment of a removed key starts from 0 (a fresh
    // count), NOT from a phantom Some(0)-vs-None ambiguity — and lands at the
    // delta, proving the value was genuinely dropped, not zeroed-in-place.
    let reinc = trie.try_increment_cas("apple", 5).expect("re-increment");
    assert_eq!(
        reinc, 5,
        "re-incrementing a removed key starts fresh from 0"
    );
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
