//! Integration tests for the libgrammstein support surface (task #45), exercised
//! through the PUBLIC API an external embedder actually uses:
//!
//! 1. `commit_document(&self)` — an `Arc`'d trie commits chunked document transactions
//!    without `&mut`, including from another thread. This is the shape a lock-free
//!    embedder needs when it also arms `enable_eviction` (which requires a bare
//!    `Arc<PersistentARTrie>`, not an `Arc<RwLock<…>>` that can hand out `&mut`).
//! 2. `eviction_stats().resident_bytes` — a public resident-overlay-heap gauge (the
//!    estimate was previously only reachable via `pub(crate)` accessors). It is folded
//!    into the snapshot the existing `eviction_stats()` trait method returns, so byte,
//!    char, and vocab all carry it with no per-variant code.
//! 3. `eviction_stats().nodes_evicted` fed by the synchronous checkpoint-tail
//!    resident-budget eviction. Previously only the async memory-pressure loop recorded
//!    it; the `checkpoint()` resident-budget path under-reported (always 0), so an
//!    embedder could not confirm the budget was reclaiming.
//!
//! Scratch is REAL DISK (`target/test-tmp`), never `/tmp` (tmpfs on this host) — the
//! eviction faults from the disk-backed image, which tmpfs would not exercise.

#![cfg(feature = "persistent-artrie")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use libdictenstein::artrie_trait::EvictableARTrie;
use libdictenstein::persistent_artrie::char::PersistentARTrieChar;
use libdictenstein::persistent_artrie::core::durability::DurabilityPolicy;
use libdictenstein::persistent_artrie::eviction::EvictionConfig;
use libdictenstein::persistent_artrie::{PersistentARTrie, WalConfig};
use libdictenstein::{DictionaryValue, MappedDictionary};
use serde::{Deserialize, Serialize};

/// A scratch directory on real disk (`target/test-tmp`), never tmpfs `/tmp`. Each call
/// returns a fresh unique dir, so parallel tests never collide on the WAL sidecar.
fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

/// (1) byte: an `Arc<PersistentARTrie>` commits a chunked document transaction with no
/// `&mut` — including from a spawned thread (compiles only because `commit_document`
/// takes `&self`; runs only because the trie is `Send + Sync`).
#[test]
fn arc_commit_document_byte_needs_no_mut() {
    let dir = scratch("libg-arc-commit-byte");
    let path = dir.path().join("c.artb");
    let trie = PersistentARTrie::<u64>::create(&path).expect("create");
    trie.set_durability_policy(DurabilityPolicy::Immediate);
    let trie = Arc::new(trie);

    // Commit through the Arc on ANOTHER thread — the exact libgrammstein shape.
    let worker = Arc::clone(&trie);
    let committed = std::thread::spawn(move || {
        let mut tx = worker.begin_document("doc-A").expect("begin");
        worker.tx_insert(&mut tx, "alpha", Some(1));
        worker.tx_insert(&mut tx, "beta", Some(2));
        worker.commit_document(tx).expect("commit on Arc")
    })
    .join()
    .expect("worker thread");
    assert_eq!(committed, 2, "two new terms committed via the Arc");

    // A second commit through the original Arc handle (still no `&mut` anywhere).
    let mut tx2 = trie.begin_document("doc-B").expect("begin2");
    trie.tx_insert(&mut tx2, "gamma", Some(3));
    assert_eq!(trie.commit_document(tx2).expect("commit2"), 1);

    assert_eq!(MappedDictionary::get_value(&*trie, "alpha"), Some(1));
    assert_eq!(MappedDictionary::get_value(&*trie, "beta"), Some(2));
    assert_eq!(MappedDictionary::get_value(&*trie, "gamma"), Some(3));
}

/// (1) char twin — same `&self`-on-`Arc` capability for the UTF-8 trie.
#[test]
fn arc_commit_document_char_needs_no_mut() {
    let dir = scratch("libg-arc-commit-char");
    let path = dir.path().join("c.artc");
    let trie = PersistentARTrieChar::<u64>::create_with_config(&path, WalConfig::no_archive())
        .expect("create");
    trie.set_durability_policy(DurabilityPolicy::Immediate);
    let trie = Arc::new(trie);

    let worker = Arc::clone(&trie);
    let committed = std::thread::spawn(move || {
        let mut tx = worker.begin_document("doc-A").expect("begin");
        worker.tx_insert(&mut tx, "αlpha", Some(1));
        worker.tx_insert(&mut tx, "βeta", Some(2));
        worker.commit_document(tx).expect("commit on Arc")
    })
    .join()
    .expect("worker thread");
    assert_eq!(committed, 2);

    assert_eq!(MappedDictionary::get_value(&*trie, "αlpha"), Some(1));
    assert_eq!(MappedDictionary::get_value(&*trie, "βeta"), Some(2));
}

/// (2) + (3): via the PUBLIC API only (`enable_eviction` + `checkpoint()` +
/// `eviction_stats()`), the resident-budget checkpoint tail feeds `nodes_evicted`, and
/// `resident_bytes` reports the live overlay heap (bounded by the budget).
#[test]
fn public_eviction_stats_resident_bytes_and_checkpoint_tail_nodes_evicted() {
    /// Build an eviction-enabled char counter trie, insert N keys, checkpoint once, and
    /// return `(resident_bytes, nodes_evicted)` from the public stats snapshot.
    fn run(budget: Option<usize>) -> (u64, u64) {
        let dir = scratch("libg-evict-stats");
        let path = dir.path().join("e.artc");
        let trie = PersistentARTrieChar::<u64>::create_with_config(&path, WalConfig::no_archive())
            .expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        let trie = Arc::new(trie);
        let config = EvictionConfig {
            resident_budget_bytes: budget,
            ..EvictionConfig::without_memory_monitor()
        };
        trie.enable_eviction(config).expect("enable_eviction");

        for i in 0..2_000u32 {
            let term = format!("ngram-{i:06}");
            trie.try_increment_cas_durable(&term, 1).expect("increment");
        }
        // The PRODUCTION checkpoint route-splits to the resident-budget tail.
        trie.checkpoint().expect("checkpoint");
        let stats = trie.eviction_stats();
        trie.disable_eviction().ok();
        (stats.resident_bytes, stats.nodes_evicted)
    }

    // A small budget: the checkpoint tail evicts the cold overlay nodes down to it.
    let (budg_resident, budg_evicted) = run(Some(4_000));
    // No budget (the default): the tail evicts nothing; the heap is unbounded.
    let (ctrl_resident, ctrl_evicted) = run(None);

    assert!(
        budg_evicted > 0,
        "the checkpoint-tail resident-budget eviction must feed nodes_evicted (the #45 fix); got {budg_evicted}"
    );
    assert_eq!(
        ctrl_evicted, 0,
        "no resident budget ⇒ no checkpoint-tail eviction; got {ctrl_evicted}"
    );
    assert!(
        ctrl_resident > 0,
        "resident_bytes must report the live overlay heap; got 0 with 2000 resident nodes"
    );
    assert!(
        budg_resident <= ctrl_resident,
        "the resident budget must not grow the resident heap ({budg_resident} <= {ctrl_resident})"
    );
}

// =============================================================================
// C1 — lock-free byte-keyed read-modify-write (`update_or_insert_bytes`)
// C2 — raw-`Vec<u8>` valued iteration (`iter_bytes_with_values`)
//
// The byte-native training surface libgrammstein's MKN LEB128 term-id store binds
// to: a `&self`, arbitrary-`V`, lock-free RMW keyed on raw bytes, and a lossless
// `(Vec<u8>, V)` iterator. All scratch is real disk (`target/test-tmp`) via the
// `scratch` helper above; the default `DurabilityPolicy::Immediate` fsyncs each
// acknowledged write, so the reopen tests exercise genuine durability.
// =============================================================================

/// A two-field struct value with the shape of libgrammstein's `NgramEntry`: proves
/// `update_or_insert_bytes` is correct for an arbitrary, non-counter `V` and that
/// both fields advance together (no torn writes) under concurrent contention.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Pair {
    a: u64,
    b: u64,
}
impl DictionaryValue for Pair {}

/// C1 concurrency: N threads hammer the SAME byte key with an increment closure; the
/// final value equals the total number of calls — no lost updates. Covers the empty
/// key too (the overlay root).
#[test]
fn update_or_insert_bytes_concurrent_no_lost_updates() {
    fn assert_counter(key: Vec<u8>) {
        const WRITERS: usize = 8;
        const INCREMENTS: usize = 64;
        let dir = scratch("libg-uoib-nolost");
        let path = dir.path().join("c.artb");
        let trie = Arc::new(PersistentARTrie::<u64>::create(&path).expect("create"));
        // Pre-insert the key so every racing call takes the UPDATE branch: this
        // isolates the no-lost-update invariant (all N calls +1 ⇒ N) from the
        // insert-winner race, which is covered by the one_insert_winner test.
        trie.upsert_bytes(&key, 0u64).expect("pre-insert seed");

        let key = Arc::new(key);
        let barrier = Arc::new(Barrier::new(WRITERS));
        let mut handles = Vec::with_capacity(WRITERS);
        for _ in 0..WRITERS {
            let trie = Arc::clone(&trie);
            let key = Arc::clone(&key);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..INCREMENTS {
                    trie.update_or_insert_bytes(&key, 0u64, |v| *v += 1)
                        .expect("update_or_insert_bytes");
                }
            }));
        }
        for h in handles {
            h.join().expect("writer thread");
        }
        assert_eq!(
            trie.get_value_bytes(&key),
            Some((WRITERS * INCREMENTS) as u64),
            "every increment landed (no lost updates) for key {key:?}"
        );
    }
    assert_counter(b"hot-key".to_vec());
    assert_counter(Vec::new()); // the empty key IS the overlay root
}

/// C1: all writers race the same FRESH key; exactly one call reports `Ok(true)` (the
/// insert), and the final value equals total-calls − 1 (the inserter stores the
/// default `0` WITHOUT applying the closure; every other call adds 1).
#[test]
fn update_or_insert_bytes_concurrent_one_insert_winner() {
    const WRITERS: usize = 8;
    const INCREMENTS: usize = 64;
    let dir = scratch("libg-uoib-onewinner");
    let path = dir.path().join("c.artb");
    let trie = Arc::new(PersistentARTrie::<u64>::create(&path).expect("create"));

    let barrier = Arc::new(Barrier::new(WRITERS));
    let insert_winners = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(WRITERS);
    for _ in 0..WRITERS {
        let trie = Arc::clone(&trie);
        let barrier = Arc::clone(&barrier);
        let insert_winners = Arc::clone(&insert_winners);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..INCREMENTS {
                if trie
                    .update_or_insert_bytes(b"race", 0u64, |v| *v += 1)
                    .expect("update_or_insert_bytes")
                {
                    insert_winners.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    for h in handles {
        h.join().expect("writer thread");
    }
    assert_eq!(
        insert_winners.load(Ordering::Relaxed),
        1,
        "exactly one call inserts; the rest update"
    );
    assert_eq!(
        trie.get_value_bytes(b"race"),
        Some((WRITERS * INCREMENTS - 1) as u64),
        "insert stores the default (0) without the closure; the other calls each +1"
    );
}

/// C1 arbitrary-`V`: concurrent two-field updates keep the fields consistent
/// (`b == 2*a` — both advance together under the root-CAS) with no lost updates
/// (`a == total updates`). Proves the closure RMW works for a struct `V` (no
/// `Counter` bound).
#[test]
fn update_or_insert_bytes_concurrent_two_field_struct_consistent() {
    const WRITERS: usize = 8;
    const UPDATES: usize = 64;
    let dir = scratch("libg-uoib-pair");
    let path = dir.path().join("c.artb");
    let trie = Arc::new(PersistentARTrie::<Pair>::create(&path).expect("create"));

    let barrier = Arc::new(Barrier::new(WRITERS));
    let mut handles = Vec::with_capacity(WRITERS);
    for _ in 0..WRITERS {
        let trie = Arc::clone(&trie);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..UPDATES {
                trie.update_or_insert_bytes(b"pair", Pair::default(), |p| {
                    p.a += 1;
                    p.b += 2;
                })
                .expect("update_or_insert_bytes");
            }
        }));
    }
    for h in handles {
        h.join().expect("writer thread");
    }
    let got = trie.get_value_bytes(b"pair").expect("present");
    // The insert winner stores `Pair::default()` = {0,0} WITHOUT the closure, so the
    // remaining WRITERS*UPDATES-1 calls each apply (+1, +2).
    let updates = (WRITERS * UPDATES - 1) as u64;
    assert_eq!(got.a, updates, "no lost updates on field a");
    assert_eq!(
        got.b,
        2 * updates,
        "field b stays consistent with a (both fields published together — no torn write)"
    );
}

/// C1 key coverage: arbitrary byte keys — `0x00`, `0x7F`, `0x80`, `0xFF`, a mixed
/// non-UTF-8 key, the empty key, and a long (>64 B) key — each round-trips through
/// insert-then-update. First call inserts the default → `Ok(true)`; second applies
/// the closure → `Ok(false)`.
#[test]
fn update_or_insert_bytes_key_coverage_insert_then_update() {
    let dir = scratch("libg-uoib-keycov");
    let path = dir.path().join("c.artb");
    let trie = PersistentARTrie::<u64>::create(&path).expect("create");

    let keys: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"\x00".to_vec(),
        b"\x7f".to_vec(),
        b"\x80".to_vec(),
        b"\xff".to_vec(),
        b"\x00\xff\x80\x7f".to_vec(),
        vec![0xABu8; 100], // long (>64 B) key
    ];
    for key in &keys {
        let inserted = trie
            .update_or_insert_bytes(key, 10u64, |v| *v += 5)
            .expect("insert");
        assert!(inserted, "first call inserts key {key:?} → Ok(true)");
        assert_eq!(
            trie.get_value_bytes(key),
            Some(10),
            "default stored for key {key:?}"
        );

        let updated = trie
            .update_or_insert_bytes(key, 999u64, |v| *v += 5)
            .expect("update");
        assert!(!updated, "second call updates key {key:?} → Ok(false)");
        assert_eq!(
            trie.get_value_bytes(key),
            Some(15),
            "closure applied (10+5) for key {key:?}; default 999 ignored on update"
        );
    }
}

/// C1 persistence: `update_or_insert_bytes` writes (a mix of insert + update, incl.
/// a non-UTF-8 key and the empty key) survive `checkpoint()` + reopen, plus a
/// post-checkpoint WAL tail; a post-reopen update on a survived key returns
/// `Ok(false)`.
#[test]
fn update_or_insert_bytes_survives_checkpoint_reopen() {
    let dir = scratch("libg-uoib-ckpt");
    let path = dir.path().join("c.artb");
    let non_utf8: Vec<u8> = b"\x80\x00\xff".to_vec();
    {
        let trie = PersistentARTrie::<u64>::create(&path).expect("create");
        assert!(trie
            .update_or_insert_bytes(b"alpha", 1u64, |v| *v += 100)
            .expect("ins alpha"));
        assert!(!trie
            .update_or_insert_bytes(b"alpha", 999u64, |v| *v += 4)
            .expect("upd alpha")); // 1 → 5
        assert!(trie
            .update_or_insert_bytes(&non_utf8, 7u64, |v| *v += 1)
            .expect("ins non-utf8"));
        assert!(trie
            .update_or_insert_bytes(b"", 42u64, |v| *v += 1)
            .expect("ins empty"));
        trie.sync().expect("sync");
        trie.checkpoint().expect("checkpoint");
        assert!(trie
            .update_or_insert_bytes(b"post", 9u64, |v| *v += 1)
            .expect("post-ckpt tail"));
        trie.sync().expect("sync tail");
    }
    let trie = PersistentARTrie::<u64>::open(&path).expect("reopen");
    assert_eq!(
        trie.get_value_bytes(b"alpha"),
        Some(5),
        "insert+update survived checkpoint"
    );
    assert_eq!(
        trie.get_value_bytes(&non_utf8),
        Some(7),
        "non-UTF-8 key survived"
    );
    assert_eq!(trie.get_value_bytes(b""), Some(42), "empty key survived");
    assert_eq!(
        trie.get_value_bytes(b"post"),
        Some(9),
        "post-checkpoint WAL tail survived"
    );
    assert!(
        !trie
            .update_or_insert_bytes(b"alpha", 0u64, |v| *v += 10)
            .expect("post-reopen upd"),
        "a survived key updates (not inserts) after reopen → Ok(false)"
    );
    assert_eq!(trie.get_value_bytes(b"alpha"), Some(15));
}

/// C2: a set of non-UTF-8 varint-shaped byte keys inserted with values iterates back
/// byte-for-byte via `iter_bytes_with_values()` — no stringification loss for high
/// bytes (`0x80..=0xFF`) or `0x00`.
#[test]
fn iter_bytes_with_values_roundtrips_non_utf8_keys() {
    let dir = scratch("libg-iterbwv");
    let path = dir.path().join("c.artb");
    let trie = PersistentARTrie::<u64>::create(&path).expect("create");

    let expected: std::collections::BTreeMap<Vec<u8>, u64> = [
        (b"\x80".to_vec(), 1u64),
        (b"\x00\x01".to_vec(), 2),
        (b"\xff\xfe\xfd".to_vec(), 3),
        (b"\x82\x00\x91".to_vec(), 4),
        (vec![0xC0, 0x80], 5), // overlong-form bytes — never valid UTF-8
    ]
    .into_iter()
    .collect();
    for (k, v) in &expected {
        // Fresh keys: the insert branch stores the default `*v`; the closure is a no-op.
        assert!(trie.update_or_insert_bytes(k, *v, |_| {}).expect("insert"));
    }

    let got: std::collections::BTreeMap<Vec<u8>, u64> = trie.iter_bytes_with_values().collect();
    assert_eq!(
        got, expected,
        "every non-UTF-8 key round-trips byte-for-byte with its value"
    );
}
