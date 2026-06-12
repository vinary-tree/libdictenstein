//! G5 / Phase F2 — arbitrary-`V` lock-free overlay PRODUCTION-PATH correspondence.
//!
//! Arbitrary-`V` overlay routing is the production default: ANY `V: DictionaryValue`
//! is overlay-eligible, so a `String`-valued trie's `create()` auto-flips to the
//! lock-free overlay and every valued mutation routes through the generic G5 value
//! path (F0 durable write / F1 reestablish + read route). These tests exercise the
//! FULL production path for a NON-counter `V` (`String`):
//!   - create-flip → insert_with_value/upsert/get_or_insert/compare_and_swap →
//!     checkpoint → reopen → read;
//!   - the pure-WAL-replay reopen with NO checkpoint (the #41 witness — every
//!     acknowledged arbitrary-`V` write survives a crash with no checkpoint);
//!   - the empty term `""` carrying an arbitrary-`V` value (G5-NEW-4: the RANKED
//!     depth-0 publish, durable across reopen);
//!   - concurrent writers (the overlay root-CAS arbitrates).
//!
//! Run with: `cargo test --features persistent-artrie`.
//!
//! Real-disk scratch under `ln/` (NOT tmpfs `tempdir()`), per the project's
//! disk-backed-test discipline.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::char::{PersistentARTrieChar, SharedCharARTrie};
use libdictenstein::persistent_artrie::core::shared_access::SharedTrieAccess;
use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::{ARTrie, MappedDictionary, MutableMappedDictionary};
use std::sync::Arc;

fn scratch(tag: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("ln").ok();
    tempfile::Builder::new()
        .prefix(tag)
        .tempdir_in("ln")
        .expect("real-disk scratch under ln")
}

/// create-flip → valued writes (incl. `""`) → checkpoint → post-checkpoint tail →
/// reopen → every value (and the empty-term value) survives.
#[test]
fn char_arbitrary_v_value_roundtrip_checkpoint_reopen() {
    let dir = scratch("f2-char-ckpt");
    let path = dir.path().join("t.artc");
    {
        let trie = PersistentARTrieChar::<String>::create(&path).expect("create");
        assert!(
            trie.route_overlay(),
            "arbitrary-V overlay routing is the default ⇒ a String trie auto-flips to the overlay at create"
        );
        assert!(trie
            .insert_with_value("alpha", "A".to_string())
            .expect("ins"));
        assert!(trie
            .insert_with_value("application", "B".to_string())
            .expect("ins"));
        assert!(trie
            .insert_with_value("ünïcode", "C".to_string())
            .expect("ins"));
        assert!(
            trie.insert_with_value("", "EMPTY".to_string())
                .expect("ins ''"),
            "the empty term carries an arbitrary-V value"
        );
        trie.sync().expect("sync");
        trie.checkpoint().expect("checkpoint");
        assert!(trie
            .insert_with_value("post-ckpt", "D".to_string())
            .expect("ins"));
        trie.sync().expect("sync tail");
    }
    let trie = PersistentARTrieChar::<String>::open(&path).expect("reopen");
    assert_eq!(trie.get_value("alpha"), Some("A".to_string()));
    assert_eq!(trie.get_value("application"), Some("B".to_string()));
    assert_eq!(trie.get_value("ünïcode"), Some("C".to_string()));
    assert_eq!(
        trie.get_value(""),
        Some("EMPTY".to_string()),
        "empty-term arbitrary-V value survives checkpoint+reopen (G5-NEW-4 ranked publish)"
    );
    assert_eq!(trie.get_value("post-ckpt"), Some("D".to_string()));
    assert_eq!(trie.get_value("absent"), None);
}

/// The #41 witness for arbitrary `V`: acknowledged writes survive reopen with NO
/// checkpoint (pure WAL replay), INCLUDING the empty-term value.
#[test]
fn char_arbitrary_v_value_survives_wal_replay_reopen_no_checkpoint() {
    let dir = scratch("f2-char-walreplay");
    let path = dir.path().join("t.artc");
    {
        let trie = PersistentARTrieChar::<String>::create(&path).expect("create");
        for (k, v) in [("apple", "red"), ("banana", "yellow"), ("cherry", "dark")] {
            assert!(trie.insert_with_value(k, v.to_string()).expect("ins"));
        }
        assert!(trie
            .insert_with_value("", "ROOT".to_string())
            .expect("ins ''"));
        trie.sync().expect("sync");
        // DROP WITHOUT CHECKPOINT — durability rests entirely on the WAL.
    }
    let trie = PersistentARTrieChar::<String>::open(&path).expect("reopen");
    assert_eq!(trie.get_value("apple"), Some("red".to_string()));
    assert_eq!(trie.get_value("banana"), Some("yellow".to_string()));
    assert_eq!(trie.get_value("cherry"), Some("dark".to_string()));
    assert_eq!(
        trie.get_value(""),
        Some("ROOT".to_string()),
        "empty-term arbitrary-V value survives WAL-replay reopen (ranked, not dropped)"
    );
}

/// insert-once / upsert-overwrite / get_or_insert / compare_and_swap on a flipped
/// arbitrary-`V` trie, then a reopen confirms the final values are durable.
#[test]
fn char_arbitrary_v_value_ops_then_reopen() {
    let dir = scratch("f2-char-ops");
    let path = dir.path().join("t.artc");
    {
        let mut trie = PersistentARTrieChar::<String>::create(&path).expect("create");
        assert!(trie.insert_with_value("k", "v1".to_string()).expect("ins"));
        assert!(
            !trie.insert_with_value("k", "v2".to_string()).expect("ins2"),
            "insert_with_value of an existing term ⇒ Ok(false) (updated, not newly inserted)"
        );
        assert_eq!(
            trie.get_value("k"),
            Some("v2".to_string()),
            "C0: insert_with_value OVERWRITES on duplicate (upsert semantics, matches owned + map laws)"
        );
        assert!(
            !trie.upsert("k", "v3".to_string()).expect("upsert"),
            "upsert of an existing term ⇒ Ok(false) (updated)"
        );
        assert_eq!(
            trie.get_value("k"),
            Some("v3".to_string()),
            "upsert overwrote"
        );
        assert_eq!(
            trie.get_or_insert("k", "DEF".to_string())
                .expect("goi present"),
            "v3".to_string(),
            "get_or_insert returns the existing value"
        );
        assert_eq!(
            trie.get_or_insert("fresh", "DEF".to_string())
                .expect("goi absent"),
            "DEF".to_string(),
            "get_or_insert inserts + returns the default"
        );
        assert!(
            trie.compare_and_swap("k", Some("v3".to_string()), "v4".to_string())
                .expect("cas match"),
            "CAS with matching expected swaps"
        );
        assert!(
            !trie
                .compare_and_swap("k", Some("WRONG".to_string()), "v5".to_string())
                .expect("cas mismatch"),
            "CAS with non-matching expected ⇒ no swap"
        );
        trie.sync().expect("sync");
    }
    let trie = PersistentARTrieChar::<String>::open(&path).expect("reopen");
    assert_eq!(
        trie.get_value("k"),
        Some("v4".to_string()),
        "final CAS value durable"
    );
    assert_eq!(trie.get_value("fresh"), Some("DEF".to_string()));
}

/// C2: `merge_from` on a flipped arbitrary-`V` (String + concat) trie combines via
/// `merge_fn` reading the OVERLAY self (NOT the empty owned tree — the original
/// merge-into-overlay bug), inserts other-only terms, and the merged values survive a
/// reopen. Reads use `get_value` (the borrow `get` is `None` under the overlay).
#[test]
fn char_arbitrary_v_merge_from_overlay_then_reopen() {
    let dir = scratch("f2-char-merge");
    let path = dir.path().join("self.artc");
    let opath = dir.path().join("other.artc");
    {
        let mut self_t = PersistentARTrieChar::<String>::create(&path).expect("create self");
        let other = PersistentARTrieChar::<String>::create(&opath).expect("create other");
        assert!(
            self_t.route_overlay() && other.route_overlay(),
            "both flipped to overlay"
        );
        self_t
            .insert_with_value("apple", "A".to_string())
            .expect("ins");
        self_t
            .insert_with_value("banana", "B".to_string())
            .expect("ins");
        other
            .insert_with_value("apple", "X".to_string())
            .expect("ins"); // overlap
        other
            .insert_with_value("cherry", "C".to_string())
            .expect("ins"); // other-only
                            // Concat on overlap (proves merge_fn sees the OVERLAY self value); insert on absent.
        let processed = self_t
            .merge_from(&other, |a, b| format!("{a}{b}"))
            .expect("overlay merge");
        assert_eq!(processed, 2, "both other terms processed");
        assert_eq!(
            self_t.get_value("apple"),
            Some("AX".to_string()),
            "overlap combined via merge_fn over the overlay self-read"
        );
        assert_eq!(
            self_t.get_value("banana"),
            Some("B".to_string()),
            "self-only unchanged"
        );
        assert_eq!(
            self_t.get_value("cherry"),
            Some("C".to_string()),
            "other-only inserted"
        );
        self_t.sync().expect("sync");
    }
    let self_t = PersistentARTrieChar::<String>::open(&path).expect("reopen");
    assert_eq!(
        self_t.get_value("apple"),
        Some("AX".to_string()),
        "merged value durable"
    );
    assert_eq!(self_t.get_value("banana"), Some("B".to_string()));
    assert_eq!(self_t.get_value("cherry"), Some("C".to_string()));
}

/// Byte twin: a `String`-valued BYTE trie under the feature round-trips through a
/// checkpoint+reopen (incl. the empty term).
#[test]
fn byte_arbitrary_v_value_roundtrip_checkpoint_reopen() {
    let dir = scratch("f2-byte-ckpt");
    let path = dir.path().join("t.part");
    {
        let trie = PersistentARTrie::<String>::create(&path).expect("create");
        assert!(
            trie.route_overlay(),
            "arbitrary-V overlay routing is the default ⇒ a String byte trie auto-flips to the overlay at create"
        );
        assert!(trie.insert_with_value("alpha", "A".to_string()));
        assert!(trie.insert_with_value("", "EMPTY".to_string()));
        trie.sync().expect("sync");
        trie.checkpoint().expect("checkpoint");
    }
    let trie = PersistentARTrie::<String>::open(&path).expect("reopen");
    assert_eq!(trie.get_value("alpha"), Some("A".to_string()));
    assert_eq!(trie.get_value(""), Some("EMPTY".to_string()));
}

/// Concurrent writers: N threads each insert a disjoint key-set of arbitrary-`V`
/// values through the shared handle; every write survives (the overlay root-CAS is
/// the arbiter). Reopen confirms durability.
#[test]
fn char_arbitrary_v_concurrent_writers_all_survive() {
    let dir = scratch("f2-char-concurrent");
    let path = dir.path().join("t.artc");
    let threads = 8usize;
    let per = 40usize;
    {
        let trie = Arc::new(std::sync::RwLock::new(
            PersistentARTrieChar::<String>::create(&path).expect("create"),
        ));
        assert!(trie.read().unwrap().route_overlay());
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let trie = Arc::clone(&trie);
                std::thread::spawn(move || {
                    for i in 0..per {
                        let k = format!("t{t}-k{i}");
                        let v = format!("v{t}-{i}");
                        trie.write()
                            .unwrap()
                            .insert_with_value(&k, v)
                            .expect("concurrent insert");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("join");
        }
        trie.write().unwrap().sync().expect("sync");
    }
    let trie = PersistentARTrieChar::<String>::open(&path).expect("reopen");
    for t in 0..threads {
        for i in 0..per {
            assert_eq!(
                trie.get_value(&format!("t{t}-k{i}")),
                Some(format!("v{t}-{i}")),
                "concurrent arbitrary-V write t{t}-k{i} survived + durable"
            );
        }
    }
}

/// C2: `A.union_with(&B)` ‖ `B.union_with(&A)` must NOT deadlock — the AB/BA fix
/// snapshots `other` and releases its read lock BEFORE taking `self`'s write lock, so
/// no two `RwLock`s are ever held at once. A regression hangs both joins (the harness
/// times out). The returned counts prove each merge ran to completion.
#[test]
fn char_union_with_no_ab_ba_deadlock() {
    use std::thread;
    let dir = scratch("f2-union-deadlock");
    let a: SharedCharARTrie<u64> =
        SharedCharARTrie::create(&dir.path().join("a.artc")).expect("create a");
    let b: SharedCharARTrie<u64> =
        SharedCharARTrie::create(&dir.path().join("b.artc")).expect("create b");
    a.write().insert_with_value("shared", 1).expect("a ins");
    a.write().insert_with_value("a_only", 10).expect("a ins2");
    b.write().insert_with_value("shared", 2).expect("b ins");
    b.write().insert_with_value("b_only", 20).expect("b ins2");

    let (a1, b1) = (a.clone(), b.clone());
    let (a2, b2) = (a.clone(), b.clone());
    let h1 = thread::spawn(move || a1.union_with(&b1, |x, y| x + y));
    let h2 = thread::spawn(move || b2.union_with(&a2, |x, y| x + y));
    // Deadlock-freedom is the assertion here: a regression hangs these joins forever. The
    // exact processed-COUNT is non-deterministic under concurrency (each side snapshots
    // `other` point-in-time, so it may also observe a term the other side just merged in),
    // so assert the DETERMINISTIC post-condition instead: each trie absorbed the OTHER's
    // exclusive starting term (always present in the other's original snapshot), keeping
    // its value.
    h1.join().expect("A.union_with(&B) completed (no deadlock)");
    h2.join().expect("B.union_with(&A) completed (no deadlock)");
    assert_eq!(
        a.read().get_value("b_only"),
        Some(20),
        "A absorbed B's exclusive term"
    );
    assert_eq!(
        b.read().get_value("a_only"),
        Some(10),
        "B absorbed A's exclusive term"
    );
}

/// C2 byte twin: byte `merge_from` on a flipped String trie combines via `merge_fn`
/// over the overlay self-read (NOT `get_value_impl` over the empty owned tree), inserts
/// other-only terms, and the merged values survive a reopen.
#[test]
fn byte_arbitrary_v_merge_from_overlay_then_reopen() {
    let dir = scratch("f2-byte-merge");
    let path = dir.path().join("self.part");
    let opath = dir.path().join("other.part");
    {
        let mut self_t = PersistentARTrie::<String>::create(&path).expect("create self");
        let other = PersistentARTrie::<String>::create(&opath).expect("create other");
        assert!(
            self_t.route_overlay() && other.route_overlay(),
            "both flipped to overlay"
        );
        self_t.insert_with_value("apple", "A".to_string()); // byte returns bool
        self_t.insert_with_value("banana", "B".to_string());
        other.insert_with_value("apple", "X".to_string()); // overlap
        other.insert_with_value("cherry", "C".to_string()); // other-only
        let processed = self_t
            .merge_from(&other, |a, b| format!("{a}{b}"))
            .expect("overlay merge");
        assert_eq!(processed, 2, "both other terms processed");
        assert_eq!(
            self_t.get_value("apple"),
            Some("AX".to_string()),
            "overlap combined"
        );
        assert_eq!(
            self_t.get_value("banana"),
            Some("B".to_string()),
            "self-only unchanged"
        );
        assert_eq!(
            self_t.get_value("cherry"),
            Some("C".to_string()),
            "other-only inserted"
        );
        self_t.sync().expect("sync");
    }
    let self_t = PersistentARTrie::<String>::open(&path).expect("reopen");
    assert_eq!(
        self_t.get_value("apple"),
        Some("AX".to_string()),
        "merged value durable"
    );
    assert_eq!(self_t.get_value("cherry"), Some("C".to_string()));
}

/// C2 tx-ii: a char document transaction under the overlay applies SETs per-op,
/// durable across reopen (per-op, NOT all-or-nothing — matches owned recovery).
#[test]
fn char_doc_tx_overlay_set_durable_reopen() {
    let dir = scratch("f2-char-doctx-set");
    let path = dir.path().join("t.artc");
    {
        let trie = PersistentARTrieChar::<String>::create(&path).expect("create");
        let mut tx = trie.begin_document("doc1").expect("begin");
        trie.tx_insert(&mut tx, "x", Some("X".to_string()));
        trie.tx_insert(&mut tx, "y", Some("Y".to_string()));
        let n = trie.commit_document(tx).expect("commit");
        assert_eq!(n, 2, "2 ops");
        assert_eq!(trie.get_value("x"), Some("X".to_string()));
        assert_eq!(trie.get_value("y"), Some("Y".to_string()));
        trie.sync().expect("sync");
    }
    let trie = PersistentARTrieChar::<String>::open(&path).expect("reopen");
    assert_eq!(
        trie.get_value("x"),
        Some("X".to_string()),
        "doc-tx SET durable"
    );
    assert_eq!(trie.get_value("y"), Some("Y".to_string()));
}

/// C2 tx-ii: a char document-tx increment (counter trie) aggregates + applies under the
/// overlay; a NEGATIVE aggregate rejects the WHOLE commit (the overlay counter is
/// add-only) BEFORE applying anything.
#[test]
fn char_doc_tx_overlay_increment_and_negative_reject() {
    let dir = scratch("f2-char-doctx-incr");
    let path = dir.path().join("t.artc");
    let trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
    // Positive aggregate: +5 then +3 = 8.
    let mut tx = trie.begin_document("d1").expect("begin");
    trie.tx_increment(&mut tx, "c", 5);
    trie.tx_increment(&mut tx, "c", 3);
    trie.commit_document(tx).expect("commit positive");
    assert_eq!(trie.get_value("c"), Some(8), "aggregate +5+3 = 8");
    // Negative aggregate: +5 then -10 = -5 ⇒ reject, apply nothing.
    let mut tx2 = trie.begin_document("d2").expect("begin2");
    trie.tx_increment(&mut tx2, "d", 5);
    trie.tx_increment(&mut tx2, "d", -10);
    let r = trie.commit_document(tx2);
    assert!(r.is_err(), "negative aggregate rejects the commit");
    assert_eq!(trie.get_value("d"), None, "rejected commit applied nothing");
    assert_eq!(trie.get_value("c"), Some(8), "prior committed value intact");
}

/// C2 byte twin: a byte document transaction under the overlay applies SETs per-op,
/// durable across reopen.
#[test]
fn byte_doc_tx_overlay_set_durable_reopen() {
    let dir = scratch("f2-byte-doctx");
    let path = dir.path().join("t.part");
    {
        let trie = PersistentARTrie::<String>::create(&path).expect("create");
        let mut tx = trie.begin_document("doc1").expect("begin");
        trie.tx_insert(&mut tx, "x", Some("X".to_string()));
        trie.tx_insert(&mut tx, "y", Some("Y".to_string()));
        let n = trie.commit_document(tx).expect("commit");
        assert_eq!(n, 2, "2 ops");
        assert_eq!(trie.get_value("x"), Some("X".to_string()));
        trie.sync().expect("sync");
    }
    let trie = PersistentARTrie::<String>::open(&path).expect("reopen");
    assert_eq!(
        trie.get_value("x"),
        Some("X".to_string()),
        "byte doc-tx SET durable"
    );
    assert_eq!(trie.get_value("y"), Some("Y".to_string()));
}

/// **Owner decision (2026-06-09): byte doc-tx counter increments ACCUMULATE.** Two
/// SEPARATE documents incrementing one counter ADD to the live overlay count (the prior
/// `try_tx_increment_bytes` read an EMPTY owned base via `get_value_impl` and the folded
/// absolute SET silently OVERWROTE the live count). Within-tx deltas aggregate; a
/// net-negative aggregate rejects the whole commit (the overlay counter is add-only).
/// RED→GREEN: pre-fix, doc B would have SET `c` to 4 (overwriting 8); the fix accumulates
/// to 12.
#[test]
fn byte_doc_tx_overlay_increment_accumulates_across_documents() {
    let dir = scratch("byte-doctx-incr-accum");
    let path = dir.path().join("t.part");
    let trie = PersistentARTrie::<u64>::create(&path).expect("create");

    // Document A: within-tx aggregation (+5 +3) over an empty counter → 8.
    let mut tx_a = trie.begin_document("doc-A").expect("begin A");
    trie.tx_increment(&mut tx_a, "c", 5);
    trie.tx_increment(&mut tx_a, "c", 3);
    assert_eq!(
        trie.commit_document(tx_a).expect("commit A"),
        2,
        "2 increment ops"
    );
    assert_eq!(trie.get_value("c"), Some(8), "within-tx aggregate +5+3 = 8");

    // Document B: a SEPARATE document increments the SAME counter → ACCUMULATE over the
    // live value 8 (the prior SET-from-empty-owned-base overwrote it to 4). 8 + 4 = 12.
    let mut tx_b = trie.begin_document("doc-B").expect("begin B");
    trie.tx_increment(&mut tx_b, "c", 4);
    assert_eq!(
        trie.commit_document(tx_b).expect("commit B"),
        1,
        "1 increment op"
    );
    assert_eq!(
        trie.get_value("c"),
        Some(12),
        "cross-document ACCUMULATE: 8 + 4 = 12 (NOT the prior overwrite to 4)"
    );

    // A net-negative aggregate rejects the whole commit; nothing is applied.
    let mut tx_neg = trie.begin_document("doc-neg").expect("begin neg");
    trie.tx_increment(&mut tx_neg, "d", 5);
    trie.tx_increment(&mut tx_neg, "d", -10);
    assert!(
        trie.commit_document(tx_neg).is_err(),
        "net-negative aggregate rejects the commit"
    );
    assert_eq!(trie.get_value("d"), None, "rejected commit applied nothing");
    assert_eq!(
        trie.get_value("c"),
        Some(12),
        "prior committed counter intact"
    );
}
