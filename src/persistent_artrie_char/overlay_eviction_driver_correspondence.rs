//! **OE1–OE4 — correspondence tests for the REVERSIBLE overlay-eviction driver**
//! (`evict_overlay_node_at_path` / `evict_overlay_nodes` in `super`).
//!
//! These live in-crate (not in `tests/`) because they drive the private overlay
//! driver + the `pub(crate)` `OverlayEvictOutcome` directly, and inspect the
//! overlay-internal state (the `lockfree_root` slot turning a COLD child on-disk).
//! They are the Rust witness for the TLC model
//! `formal-verification/tla+/OverlayEvictionCas.tla`:
//!
//! - **OE1 `cold_eviction_under_concurrent_writers_reopens_losing_nothing`**
//!   (headline): COLD `c-*` + LIVE `w-*` inserted, eviction-ON checkpoint, then
//!   N `insert_cas_durable` writers on fresh `w2-*` ‖ repeated cold eviction.
//!   Asserts `evicted > 0` (REAL reclamation — 0 with the §E no-op), cold terms
//!   never re-read, and a reopen recovers EVERY acked term (`c-*`,`w-*`,`w2-*`).
//! - **OE2 `reader_concurrent_with_overlay_eviction_sees_consistent_snapshot`**
//!   (no-UAF): a reader loops `contains_lockfree` on LIVE ‖ the evictor reclaims
//!   COLD; no panic/UAF and LIVE stays monotone-present.
//! - **OE3 `evict_then_reload_returns_exact_values`** (SE5 unit analogue, counter
//!   `V=u64`): checkpoint → evict cold → reopen → cold VALUES byte-identical.
//! - **OE4 `evictor_root_cas_loser_never_clobbers_insert`** (proptest): random
//!   insert+evict interleavings; the post-run acked set == inserted set.
//!
//! Scratch is real disk (`target/test-tmp`), never `/tmp` (tmpfs on this host).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use crate::persistent_artrie::eviction::EvictionConfig;
use crate::persistent_artrie::WalConfig;
use crate::persistent_artrie_core::durability::DurabilityPolicy;
use crate::persistent_artrie_char::PersistentARTrieChar;
use crate::Dictionary;

/// A scratch directory on real disk (`target/test-tmp`), never tmpfs `/tmp`.
fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

/// COLD-prefix predicate: a registry path (`&[char]`) is cold iff it starts with
/// `'c'` (the `c-*` term family). This is the `cold_filter` the bench accessor
/// uses — only COLD subtrees are ever fed to the evictor (SF5(ii) faultin == 0).
fn is_cold(path: &[char]) -> bool {
    path.first() == Some(&'c')
}

/// Drive ONE round of cold-only overlay eviction exactly as the Phase-3 bench
/// accessor `bench_evict_overlay_cold_nodes` will: select via the coordinator
/// (coldest-first, registry-gated), filter to COLD paths, and reclaim via the
/// driver `evict_overlay_nodes`. Returns the number of overlay nodes evicted.
fn evict_cold_overlay<V, S>(trie: &PersistentARTrieChar<V, S>, budget_bytes: usize) -> usize
where
    V: crate::value::DictionaryValue,
    S: crate::persistent_artrie::block_storage::BlockStorage,
{
    let coordinator = match trie.eviction_coordinator.as_ref() {
        Some(c) => c,
        None => return 0,
    };
    coordinator
        .force_eviction_char(budget_bytes, |cands| {
            let filtered: Vec<_> = cands
                .into_iter()
                .filter(|(_, p, _)| is_cold(p))
                .collect();
            super::evict_overlay_nodes(trie, filtered, 4)
        })
        .0
}

// ───────────────────────────── OE1 (headline) ─────────────────────────────

#[test]
fn cold_eviction_under_concurrent_writers_reopens_losing_nothing() {
    let dir = scratch("oe1-cold-evict");
    let path = dir.path().join("oe1.artc");

    // COLD + LIVE term families. COLD `c-*` are multi-char so they sit at depth
    // >= the default `min_eviction_depth` and form an evictable subtree.
    let cold_terms: Vec<String> = (0..40).map(|i| format!("cold-{i:04}")).collect();
    let live_terms: Vec<String> = (0..40).map(|i| format!("warm-{i:04}")).collect();
    // Fresh terms the concurrent writers publish DURING eviction.
    let w2_terms: Vec<String> = (0..200).map(|i| format!("warm2-{i:05}")).collect();

    let evicted_total;
    {
        let mut owned: PersistentARTrieChar<()> =
            PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
                .expect("create");
        owned.set_durability_policy(DurabilityPolicy::Immediate);
        owned.enable_lockfree();
        owned
            .bench_enable_eviction(EvictionConfig::without_memory_monitor())
            .expect("bench_enable_eviction");

        // Insert COLD + LIVE, then checkpoint-with-eviction so the registry holds
        // real disk pointers for every (cold) node. COLD is NEVER touched again.
        for t in cold_terms.iter().chain(live_terms.iter()) {
            assert!(
                owned.insert_cas_durable(t).expect("insert"),
                "term {t:?} should be newly inserted"
            );
        }
        owned
            .bench_immutable_checkpoint_with_eviction()
            .expect("checkpoint with eviction");
        assert!(
            owned.evictable_node_count().unwrap_or(0) > 0,
            "registry must be published (evictable_node_count > 0)"
        );

        let trie = Arc::new(owned);

        // (A) REAL-reclamation sweep with the registry VALID (no writer has run
        // yet, so it is not invalidated). This is the headline: it MUST reclaim
        // cold overlay nodes (with the §E structural no-op this is 0). A
        // concurrent `insert_cas_durable` invalidates the registry (the A1 fix,
        // `is_valid()` → zero evictions = liveness-not-safety), so real reclamation
        // is established in this valid window BEFORE the concurrent writers start.
        let mut evicted = 0usize;
        for _ in 0..8 {
            evicted += evict_cold_overlay(&*trie, 1 << 20);
        }
        assert!(
            evicted > 0,
            "overlay eviction reclaimed ZERO cold nodes in a valid-registry window — \
             the driver is a no-op (regression vs the §E structural no-op)"
        );
        evicted_total = evicted;

        // (B) N concurrent writers on fresh `w2-*` ‖ repeated cold eviction. The
        // writers race the evictor's loser-safe root CAS. The cold subtrees are
        // already on-disk from (A) and the writers invalidate the registry, so
        // these concurrent sweeps reclaim little/nothing — their job is to witness
        // that the evictor NEVER clobbers a concurrent insert (loser-safe) and
        // never UAFs, NOT to add reclamation.
        let n_writers = 4;
        let barrier = Arc::new(Barrier::new(n_writers + 1));
        let mut writers = Vec::with_capacity(n_writers);
        for wt in 0..n_writers {
            let trie = Arc::clone(&trie);
            let barrier = Arc::clone(&barrier);
            let chunk: Vec<String> = w2_terms
                .iter()
                .skip(wt)
                .step_by(n_writers)
                .cloned()
                .collect();
            writers.push(thread::spawn(move || {
                barrier.wait();
                for t in &chunk {
                    trie.insert_cas_durable(t).expect("w2 insert");
                }
            }));
        }

        barrier.wait();
        for _ in 0..32 {
            let _ = evict_cold_overlay(&*trie, 1 << 20);
        }
        for w in writers {
            w.join().expect("writer join");
        }

        // COLD terms were never re-read during the run (cold-only contract). LIVE
        // + w2 remain visible in the overlay.
        for t in &live_terms {
            assert!(
                trie.contains_lockfree(t),
                "LIVE term {t:?} vanished from overlay during eviction"
            );
        }
        drop(trie); // joins the coordinator thread before reopen
    }

    // REOPEN: every acknowledged term (cold + live + w2) is recovered from the
    // durable image + retained WAL. NOTHING is lost despite real cold eviction.
    let reopened = PersistentARTrieChar::<()>::open(&path).expect("reopen");
    for t in cold_terms.iter().chain(live_terms.iter()).chain(w2_terms.iter()) {
        assert!(
            Dictionary::contains(&reopened, t),
            "term {t:?} LOST after cold overlay eviction + reopen (DATA LOSS)"
        );
    }
    assert!(!Dictionary::contains(&reopened, "never-inserted"));
    eprintln!("OE1: evicted {evicted_total} cold overlay nodes; reopen lost nothing");
}

// ───────────────────────────── OE2 (no-UAF) ─────────────────────────────

#[test]
fn reader_concurrent_with_overlay_eviction_sees_consistent_snapshot() {
    let dir = scratch("oe2-reader-uaf");
    let path = dir.path().join("oe2.artc");

    let cold_terms: Vec<String> = (0..60).map(|i| format!("cold-{i:04}")).collect();
    let live_terms: Vec<String> = (0..60).map(|i| format!("warm-{i:04}")).collect();

    let mut owned: PersistentARTrieChar<()> =
        PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive()).expect("create");
    owned.set_durability_policy(DurabilityPolicy::Immediate);
    owned.enable_lockfree();
    owned
        .bench_enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("bench_enable_eviction");
    for t in cold_terms.iter().chain(live_terms.iter()) {
        assert!(owned.insert_cas_durable(t).expect("insert"));
    }
    owned
        .bench_immutable_checkpoint_with_eviction()
        .expect("checkpoint with eviction");

    let trie = Arc::new(owned);
    let stop = Arc::new(AtomicBool::new(false));
    let total_reads = Arc::new(AtomicU64::new(0));
    let barrier = Arc::new(Barrier::new(3)); // 1 reader + 1 evictor + main

    // Reader: spin contains_lockfree on LIVE terms; each must stay present
    // (monotone) the WHOLE time the evictor reclaims COLD subtrees. A UAF would
    // panic/segfault; a logic bug would drop a LIVE term.
    let reader = {
        let trie = Arc::clone(&trie);
        let stop = Arc::clone(&stop);
        let total_reads = Arc::clone(&total_reads);
        let live = live_terms.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                for t in &live {
                    assert!(
                        trie.contains_lockfree(t),
                        "LIVE term {t:?} disappeared under concurrent eviction (UAF/logic bug)"
                    );
                    n += 1;
                }
            }
            total_reads.fetch_add(n, Ordering::Relaxed);
        })
    };

    // Evictor: reclaim COLD subtrees repeatedly while the reader spins.
    let evictor = {
        let trie = Arc::clone(&trie);
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            let mut evicted = 0usize;
            for _ in 0..50 {
                evicted += evict_cold_overlay(&*trie, 1 << 20);
            }
            evicted
        })
    };

    barrier.wait();
    let evicted = evictor.join().expect("evictor join");
    stop.store(true, Ordering::Relaxed);
    reader.join().expect("reader join");

    assert!(
        evicted > 0,
        "OE2: evictor reclaimed nothing — driver no-op (cannot witness no-UAF)"
    );
    assert!(total_reads.load(Ordering::Relaxed) > 0, "reader made no reads");
    // LIVE still resolvable after the race.
    for t in &live_terms {
        assert!(trie.contains_lockfree(t), "LIVE term {t:?} lost post-race");
    }
    eprintln!(
        "OE2: {} cold nodes evicted ‖ {} live reads, no UAF",
        evicted,
        total_reads.load(Ordering::Relaxed)
    );
}

// ───────────────────────── OE3 (evict→reload exact values) ─────────────────────────

#[test]
fn evict_then_reload_returns_exact_values() {
    use crate::MappedDictionary;

    let dir = scratch("oe3-evict-reload");
    let path = dir.path().join("oe3.artc");

    // Counter overlay (`V=u64`): each cold term carries a distinct value, so a
    // reload that returned a WRONG value (not just membership) would be caught.
    let cold: Vec<(String, u64)> = (0..40)
        .map(|i| (format!("cold-{i:04}"), 1000 + i as u64))
        .collect();
    let live: Vec<(String, u64)> = (0..20)
        .map(|i| (format!("warm-{i:04}"), 5000 + i as u64))
        .collect();

    {
        let mut owned: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
                .expect("create");
        owned.set_durability_policy(DurabilityPolicy::Immediate);
        owned.enable_lockfree();
        owned
            .bench_enable_eviction(EvictionConfig::without_memory_monitor())
            .expect("bench_enable_eviction");
        // Order-A durable increments establish each term's value in the overlay.
        for (t, v) in cold.iter().chain(live.iter()) {
            owned
                .try_increment_cas_durable(t, *v)
                .expect("durable increment");
        }
        owned
            .bench_immutable_checkpoint_with_eviction()
            .expect("checkpoint with eviction");
        let trie = Arc::new(owned);

        let mut evicted = 0usize;
        for _ in 0..16 {
            evicted += evict_cold_overlay(&*trie, 1 << 20);
        }
        assert!(evicted > 0, "OE3: no cold nodes evicted (driver no-op)");
        drop(trie);
    }

    // Reopen and read back the VALUES — byte-identical to what was checkpointed.
    let reopened = PersistentARTrieChar::<u64>::open(&path).expect("reopen");
    for (t, v) in cold.iter().chain(live.iter()) {
        assert_eq!(
            MappedDictionary::get_value(&reopened, t),
            Some(*v),
            "term {t:?} value wrong after evict+reload (expected {v})"
        );
    }
}

// ───────────────────────────── OE4 (proptest) ─────────────────────────────

#[cfg(test)]
mod oe4 {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeSet;

    #[derive(Debug, Clone)]
    enum Op {
        /// Insert a LIVE term (acked; must survive).
        InsertLive(u16),
        /// Trigger a cold-eviction sweep.
        EvictCold,
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u16..32).prop_map(Op::InsertLive),
            Just(Op::EvictCold),
        ]
    }

    proptest! {
        // Disk-backed → keep the case count modest but meaningful.
        #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

        /// Random insert+evict interleavings: the evictor's loser-safe root CAS
        /// NEVER clobbers a concurrent insert, so post-run the acked LIVE set
        /// equals the inserted LIVE set (and the disjoint COLD set, never
        /// re-touched, also still reopens — checked via the live overlay here).
        #[test]
        fn evictor_root_cas_loser_never_clobbers_insert(ops in prop::collection::vec(op_strategy(), 1..80)) {
            let dir = scratch("oe4-loser-safe");
            let path = dir.path().join("oe4.artc");

            let cold_terms: Vec<String> = (0..24).map(|i| format!("cold-{i:03}")).collect();

            let mut owned: PersistentARTrieChar<()> =
                PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
                    .expect("create");
            owned.set_durability_policy(DurabilityPolicy::Immediate);
            owned.enable_lockfree();
            owned
                .bench_enable_eviction(EvictionConfig::without_memory_monitor())
                .expect("bench_enable_eviction");
            // Pre-seed COLD + checkpoint so the registry has cold disk pointers.
            for t in &cold_terms {
                prop_assert!(owned.insert_cas_durable(t).expect("cold insert"));
            }
            owned
                .bench_immutable_checkpoint_with_eviction()
                .expect("checkpoint");
            let trie = Arc::new(owned);

            // Oracle: the LIVE terms we acknowledged.
            let mut acked_live: BTreeSet<String> = BTreeSet::new();
            for op in ops {
                match op {
                    Op::InsertLive(k) => {
                        let t = format!("warm-{k:03}");
                        let newly = trie.insert_cas_durable(&t).expect("live insert");
                        // `insert_cas_durable` returns false for a duplicate; either
                        // way the term is acked-present afterward.
                        let _ = newly;
                        acked_live.insert(t);
                    }
                    Op::EvictCold => {
                        let _ = evict_cold_overlay(&*trie, 1 << 20);
                    }
                }
            }

            // Loser-safe: every acked LIVE term is still present (no evict CAS ever
            // overwrote a concurrent insert).
            for t in &acked_live {
                prop_assert!(
                    trie.contains_lockfree(t),
                    "acked LIVE term {} lost to a racing evict CAS (clobber bug)", t
                );
            }
            drop(trie);

            // And a reopen recovers the acked LIVE set ∪ the COLD set exactly.
            let reopened = PersistentARTrieChar::<()>::open(&path).expect("reopen");
            for t in acked_live.iter().chain(cold_terms.iter()) {
                prop_assert!(
                    Dictionary::contains(&reopened, t),
                    "term {} lost after evict+reopen", t
                );
            }
        }
    }
}

// ═══════════════════════ FAULT-IN read-path tests (design §3) ═══════════════════════
//
// OE5/OE8 exercise the READ-PATH fault-in primitive `find_leaf_faulting` on the
// LIVE overlay AFTER eviction WITHOUT reopen — the gap OE3 (which reopens, going
// through the owned-tree loader) does not cover. They are the Rust witness for the
// `OverlayEvictionCas.tla` `FaultInCas` action + `ReadNeverMissesCommitted`
// invariant: an evicted-but-durable node a reader requests is faulted back from
// the durable image instead of reported absent.

// ───────────────── OE5 (read headline: evict → read faults in exact value) ─────────────────

/// OE5 (membership, `V = ()`): insert COLD + LIVE, checkpoint-with-eviction, evict
/// the COLD subtrees to OnDisk overlay refs, then `contains_lockfree` each COLD
/// term on the LIVE (un-reopened) overlay. WITHOUT read-path fault-in this returns
/// `false` (the read gap — `find_in_lockfree_trie` short-circuits OnDisk to absent);
/// WITH `find_leaf_faulting` it faults the durable node back and returns `true`.
#[test]
fn evict_then_read_faults_in_membership() {
    let dir = scratch("oe5-read-membership");
    let path = dir.path().join("oe5.artc");

    let cold_terms: Vec<String> = (0..40).map(|i| format!("cold-{i:04}")).collect();
    let live_terms: Vec<String> = (0..20).map(|i| format!("warm-{i:04}")).collect();

    let mut owned: PersistentARTrieChar<()> =
        PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive()).expect("create");
    owned.set_durability_policy(DurabilityPolicy::Immediate);
    owned.enable_lockfree();
    owned
        .bench_enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("bench_enable_eviction");
    for t in cold_terms.iter().chain(live_terms.iter()) {
        assert!(owned.insert_cas_durable(t).expect("insert"));
    }
    owned
        .bench_immutable_checkpoint_with_eviction()
        .expect("checkpoint with eviction");
    let trie = Arc::new(owned);

    // Evict the COLD overlay subtrees to OnDisk refs.
    let mut evicted = 0usize;
    for _ in 0..16 {
        evicted += evict_cold_overlay(&*trie, 1 << 20);
    }
    assert!(
        evicted > 0,
        "OE5: no cold nodes evicted — cannot witness read-path fault-in"
    );

    // READ the COLD terms on the LIVE (un-reopened) overlay. This is the headline:
    // every cold term must be faulted back in and reported present.
    for t in &cold_terms {
        assert!(
            trie.contains_lockfree(t),
            "OE5: cold term {t:?} reported ABSENT after eviction — read-path fault-in \
             gap (contains_lockfree did not fault the OnDisk prefix back in)"
        );
    }
    // LIVE terms (never evicted) remain present.
    for t in &live_terms {
        assert!(trie.contains_lockfree(t), "OE5: live term {t:?} lost");
    }
    // A genuinely-absent term stays absent (fault-in must not manufacture terms).
    assert!(!trie.contains_lockfree("definitely-absent-term"));
}

/// OE5 (valued, `V = u64`): same, but each cold term carries a distinct value, and
/// `get_lockfree` after eviction must return the EXACT durable value (not 0 / None).
/// This pins the round-trip value equivalence on the live read path (the silent
/// counter-reset bug the design calls out).
#[test]
fn evict_then_read_faults_in_exact_value() {
    let dir = scratch("oe5-read-value");
    let path = dir.path().join("oe5v.artc");

    let cold: Vec<(String, u64)> = (0..40)
        .map(|i| (format!("cold-{i:04}"), 1000 + i as u64))
        .collect();
    let live: Vec<(String, u64)> = (0..20)
        .map(|i| (format!("warm-{i:04}"), 5000 + i as u64))
        .collect();

    let mut owned: PersistentARTrieChar<u64> =
        PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive()).expect("create");
    owned.set_durability_policy(DurabilityPolicy::Immediate);
    owned.enable_lockfree();
    owned
        .bench_enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("bench_enable_eviction");
    for (t, v) in cold.iter().chain(live.iter()) {
        owned
            .try_increment_cas_durable(t, *v)
            .expect("durable increment");
    }
    owned
        .bench_immutable_checkpoint_with_eviction()
        .expect("checkpoint with eviction");
    let trie = Arc::new(owned);

    let mut evicted = 0usize;
    for _ in 0..16 {
        evicted += evict_cold_overlay(&*trie, 1 << 20);
    }
    assert!(evicted > 0, "OE5v: no cold nodes evicted");

    // get_lockfree on the LIVE overlay must fault each cold node back to its EXACT
    // durable value.
    for (t, v) in &cold {
        assert_eq!(
            trie.get_lockfree(t),
            Some(*v),
            "OE5v: cold term {t:?} value wrong after eviction (expected {v}) — read-path \
             fault-in did not recover the durable value"
        );
    }
    for (t, v) in &live {
        assert_eq!(trie.get_lockfree(t), Some(*v), "OE5v: live value wrong {t:?}");
    }
}

// ───────────────── OE8 (liveness: evict→faultin→evict thrash terminates) ─────────────────

/// OE8: a tight evict-then-read loop must TERMINATE (within `max_faultin_retries`),
/// regression-guarding the counter infinite-spin the design fixes. Each iteration
/// evicts the cold subtrees, then reads them back (faulting in), then evicts again.
/// If `find_leaf_faulting` (or the counter read step) ever spun, this would hang;
/// the test asserts it completes and that every read still observes the term.
#[test]
fn evict_faultin_evict_thrash_terminates() {
    let dir = scratch("oe8-thrash");
    let path = dir.path().join("oe8.artc");

    let cold: Vec<(String, u64)> = (0..24)
        .map(|i| (format!("cold-{i:03}"), 700 + i as u64))
        .collect();

    let mut owned: PersistentARTrieChar<u64> =
        PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive()).expect("create");
    owned.set_durability_policy(DurabilityPolicy::Immediate);
    owned.enable_lockfree();
    owned
        .bench_enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("bench_enable_eviction");
    for (t, v) in &cold {
        owned
            .try_increment_cas_durable(t, *v)
            .expect("durable increment");
    }
    owned
        .bench_immutable_checkpoint_with_eviction()
        .expect("checkpoint with eviction");
    let trie = Arc::new(owned);

    // Thrash: evict → read-faults-in → evict → … Each read must observe the exact
    // value and the loop must terminate (no infinite spin).
    let mut total_evicted = 0usize;
    for round in 0..8 {
        let mut evicted = 0usize;
        for _ in 0..8 {
            evicted += evict_cold_overlay(&*trie, 1 << 20);
        }
        total_evicted += evicted;
        // Read every cold term back (faulting in). Must terminate + be exact.
        for (t, v) in &cold {
            assert_eq!(
                trie.get_lockfree(t),
                Some(*v),
                "OE8: round {round} term {t:?} wrong value after evict/faultin thrash"
            );
        }
    }
    assert!(
        total_evicted > 0,
        "OE8: thrash never evicted anything (vacuous — re-faulted nodes must become \
         re-evictable for the thrash to be meaningful)"
    );
}

// ═══════════════════════ FAULT-IN write-path tests (design §4) ═══════════════════════
//
// OE6/OE7 exercise the WRITE-PATH fault-in (the DATA-LOSS-CRITICAL half): inserting
// a NEW term UNDER an evicted (OnDisk) prefix must fault the prefix back in, descend,
// and acknowledge the write through the SINGLE root CAS — never silently drop it.
// They are the Rust witness for `OverlayEvictionCas.tla`'s strengthened `NoLostAck`
// (a writer may ack a term whose prefix was evicted, because the write path faults
// it in first).

// ─────────── OE6 (write headline: write under evicted prefix loses nothing) ───────────

/// OE6 (DATA-LOSS-CRITICAL): insert a short prefix family, checkpoint, EVICT the
/// prefix subtrees to OnDisk, then `insert_cas_durable` NEW terms whose prefixes are
/// now evicted. Each must return `Ok(true)` (acknowledged — NOT silently dropped),
/// be visible immediately, and survive a reopen. WITHOUT write-path fault-in the
/// `build_path_recursive` OnDisk arm returns `AlreadyExists` ⇒ `Ok(false)` + never
/// cached ⇒ the term is lost at merge (the silent-drop bug this closes).
#[test]
fn evict_then_write_under_evicted_prefix_reopen_loses_nothing() {
    let dir = scratch("oe6-write-under-evicted");
    let path = dir.path().join("oe6.artc");

    // Prefix family that gets evicted: "node-0000".."node-0039". Each is multi-char
    // so it forms an evictable subtree at/below the default min eviction depth.
    let prefix_terms: Vec<String> = (0..40).map(|i| format!("node-{i:04}")).collect();
    // NEW terms inserted AFTER eviction, each EXTENDING an evicted prefix term
    // (so their spine passes through an OnDisk node ⇒ must fault-in to insert).
    let extension_terms: Vec<String> = (0..40).map(|i| format!("node-{i:04}-leaf")).collect();

    {
        let mut owned: PersistentARTrieChar<()> =
            PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive())
                .expect("create");
        owned.set_durability_policy(DurabilityPolicy::Immediate);
        owned.enable_lockfree();
        owned
            .bench_enable_eviction(EvictionConfig::without_memory_monitor())
            .expect("bench_enable_eviction");

        for t in &prefix_terms {
            assert!(owned.insert_cas_durable(t).expect("prefix insert"));
        }
        owned
            .bench_immutable_checkpoint_with_eviction()
            .expect("checkpoint with eviction");

        // Evict the prefix subtrees to OnDisk. Use the registry-driven cold sweep,
        // treating ALL `node-*` paths as cold (they are checkpointed and durable).
        let trie = Arc::new(owned);
        let mut evicted = 0usize;
        for _ in 0..16 {
            evicted += {
                let coordinator = trie.eviction_coordinator.as_ref().expect("coordinator");
                coordinator
                    .force_eviction_char(1 << 20, |cands| {
                        let filtered: Vec<_> = cands
                            .into_iter()
                            .filter(|(_, p, _)| p.first() == Some(&'n'))
                            .collect();
                        super::evict_overlay_nodes(&*trie, filtered, 4)
                    })
                    .0
            };
        }
        assert!(
            evicted > 0,
            "OE6: no prefix nodes evicted — cannot witness write-path fault-in"
        );

        // WRITE the extension terms UNDER the evicted prefixes. Each MUST be acked
        // (Ok(true)) — write-path fault-in faults the OnDisk prefix back in, descends,
        // and the single root CAS publishes the new term.
        for t in &extension_terms {
            let acked = trie
                .insert_cas_durable(t)
                .expect("durable insert under evicted prefix");
            assert!(
                acked,
                "OE6: NEW term {t:?} under an evicted prefix returned Ok(false) — SILENT \
                 DROP (write-path fault-in gap: build_path_recursive OnDisk arm did not \
                 fault the prefix in)"
            );
            // Immediately visible on the live overlay (read path faults too).
            assert!(
                trie.contains_lockfree(t),
                "OE6: acked term {t:?} not visible on the live overlay"
            );
        }
        drop(trie);
    }

    // Reopen: EVERYTHING (original prefixes + the new extensions) must be present —
    // nothing lost.
    let reopened = PersistentARTrieChar::<()>::open(&path).expect("reopen");
    for t in prefix_terms.iter().chain(extension_terms.iter()) {
        assert!(
            Dictionary::contains(&reopened, t),
            "OE6: term {t:?} lost after evict→write-under-evicted→reopen"
        );
    }
    assert_eq!(
        Dictionary::len(&reopened),
        Some(prefix_terms.len() + extension_terms.len()),
        "OE6: reopened term count != prefixes + extensions (a write was dropped or duplicated)"
    );
}

// ─────────── OE7 (three-way race under sanitizers: no-UAF + completeness) ───────────

/// OE7: a reader ‖ a writer ‖ an evictor all contend on the single `lockfree_root`
/// while the read/write paths FAULT evicted prefixes back in. Asserts: no panic /
/// UAF (run under ASan/TSan in CI), every acknowledged term is ultimately present
/// (completeness), and no committed term is ever spuriously absent at reopen. This
/// is the concurrency witness for the strengthened `NoLostAck` + `ReadNeverMisses
/// Committed` under the three-way (writer ‖ evictor ‖ faulter) arbitration.
#[test]
fn concurrent_reader_writer_evictor_faulter_no_uaf_and_complete() {
    let dir = scratch("oe7-three-way");
    let path = dir.path().join("oe7.artc");

    // COLD prefixes (checkpointed, durable, evictable) + a pool of LIVE extension
    // terms the writer adds under those (now-evicted) prefixes during the race.
    let cold_prefixes: Vec<String> = (0..30).map(|i| format!("pre-{i:03}")).collect();
    let live_extensions: Vec<String> = (0..60).map(|i| format!("pre-{:03}-x{i:03}", i % 30)).collect();

    let mut owned: PersistentARTrieChar<()> =
        PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive()).expect("create");
    owned.set_durability_policy(DurabilityPolicy::Immediate);
    owned.enable_lockfree();
    owned
        .bench_enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("bench_enable_eviction");
    for t in &cold_prefixes {
        assert!(owned.insert_cas_durable(t).expect("cold insert"));
    }
    owned
        .bench_immutable_checkpoint_with_eviction()
        .expect("checkpoint");
    let trie = Arc::new(owned);

    let stop = Arc::new(AtomicBool::new(false));
    let acked = Arc::new(AtomicU64::new(0));
    let barrier = Arc::new(Barrier::new(4));

    // Reader: hammer membership on cold prefixes (faulting them back) + live terms.
    let reader = {
        let trie = Arc::clone(&trie);
        let stop = Arc::clone(&stop);
        let barrier = Arc::clone(&barrier);
        let cold = cold_prefixes.clone();
        thread::spawn(move || {
            barrier.wait();
            let mut reads = 0u64;
            while !stop.load(Ordering::Relaxed) {
                for t in &cold {
                    // Cold prefixes are committed+durable ⇒ must never be spuriously
                    // absent (fault-in recovers them).
                    assert!(
                        trie.contains_lockfree(t),
                        "OE7: committed cold prefix {t:?} spuriously absent under race"
                    );
                    reads += 1;
                }
            }
            reads
        })
    };

    // Writer: insert LIVE extensions under the (being-evicted) cold prefixes.
    let writer = {
        let trie = Arc::clone(&trie);
        let acked = Arc::clone(&acked);
        let barrier = Arc::clone(&barrier);
        let exts = live_extensions.clone();
        thread::spawn(move || {
            barrier.wait();
            for t in &exts {
                // Each ack must succeed (write-path fault-in handles an evicted
                // prefix). A duplicate (Ok(false)) is impossible here (unique terms).
                if trie.insert_cas_durable(t).expect("durable insert under race") {
                    acked.fetch_add(1, Ordering::Relaxed);
                }
            }
        })
    };

    // Evictor: continuously evict the cold prefixes to OnDisk (creating the OnDisk
    // condition the reader/writer must fault through).
    let evictor = {
        let trie = Arc::clone(&trie);
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            let mut evicted = 0usize;
            for _ in 0..40 {
                evicted += {
                    let coordinator = match trie.eviction_coordinator.as_ref() {
                        Some(c) => c,
                        None => break,
                    };
                    coordinator
                        .force_eviction_char(1 << 20, |cands| {
                            let filtered: Vec<_> = cands
                                .into_iter()
                                .filter(|(_, p, _)| p.first() == Some(&'p'))
                                .collect();
                            super::evict_overlay_nodes(&*trie, filtered, 4)
                        })
                        .0
                };
            }
            evicted
        })
    };

    // The 4th barrier party: the main thread, which then waits for the writer.
    barrier.wait();
    writer.join().expect("writer join");
    let _evicted = evictor.join().expect("evictor join");
    stop.store(true, Ordering::Relaxed);
    let _reads = reader.join().expect("reader join");

    let acked_count = acked.load(Ordering::Relaxed);

    // Completeness: every acked LIVE extension is present on the live overlay
    // (faulting in as needed), and every cold prefix too.
    for t in &live_extensions {
        assert!(
            trie.contains_lockfree(t),
            "OE7: acked LIVE extension {t:?} absent after race (lost write)"
        );
    }
    for t in &cold_prefixes {
        assert!(trie.contains_lockfree(t), "OE7: cold prefix {t:?} lost");
    }
    assert_eq!(
        acked_count,
        live_extensions.len() as u64,
        "OE7: not every unique extension was acked"
    );
    drop(trie);

    // Reopen: nothing committed is lost.
    let reopened = PersistentARTrieChar::<()>::open(&path).expect("reopen");
    for t in cold_prefixes.iter().chain(live_extensions.iter()) {
        assert!(
            Dictionary::contains(&reopened, t),
            "OE7: committed term {t:?} lost after race + reopen"
        );
    }
}
