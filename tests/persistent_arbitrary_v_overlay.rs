//! G5 / Phase F2 — arbitrary-`V` lock-free overlay PRODUCTION-PATH correspondence.
//!
//! Gated on `overlay-arbitrary-v`: with the feature, ANY `V: DictionaryValue` is
//! overlay-eligible, so a `String`-valued trie's `create()` auto-flips to the
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
//! Run with: `cargo test --features "persistent-artrie overlay-arbitrary-v"`.
//!
//! Real-disk scratch under `ln/` (NOT tmpfs `tempdir()`), per the project's
//! disk-backed-test discipline.

#![cfg(all(feature = "persistent-artrie", feature = "overlay-arbitrary-v"))]

use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::persistent_artrie_char::{PersistentARTrieChar, SharedCharTrie};
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
        let mut trie = PersistentARTrieChar::<String>::create(&path).expect("create");
        assert!(
            trie.route_overlay(),
            "feature on ⇒ a String trie auto-flips to the overlay at create"
        );
        assert!(trie.insert_with_value("alpha", "A".to_string()).expect("ins"));
        assert!(trie
            .insert_with_value("application", "B".to_string())
            .expect("ins"));
        assert!(trie.insert_with_value("ünïcode", "C".to_string()).expect("ins"));
        assert!(
            trie.insert_with_value("", "EMPTY".to_string()).expect("ins ''"),
            "the empty term carries an arbitrary-V value"
        );
        trie.sync().expect("sync");
        trie.checkpoint().expect("checkpoint");
        assert!(trie.insert_with_value("post-ckpt", "D".to_string()).expect("ins"));
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
        let mut trie = PersistentARTrieChar::<String>::create(&path).expect("create");
        for (k, v) in [("apple", "red"), ("banana", "yellow"), ("cherry", "dark")] {
            assert!(trie.insert_with_value(k, v.to_string()).expect("ins"));
        }
        assert!(trie.insert_with_value("", "ROOT".to_string()).expect("ins ''"));
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
        assert_eq!(trie.get_value("k"), Some("v3".to_string()), "upsert overwrote");
        assert_eq!(
            trie.get_or_insert("k", "DEF".to_string()).expect("goi present"),
            "v3".to_string(),
            "get_or_insert returns the existing value"
        );
        assert_eq!(
            trie.get_or_insert("fresh", "DEF".to_string()).expect("goi absent"),
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
    assert_eq!(trie.get_value("k"), Some("v4".to_string()), "final CAS value durable");
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
        let mut other = PersistentARTrieChar::<String>::create(&opath).expect("create other");
        assert!(self_t.route_overlay() && other.route_overlay(), "both flipped to overlay");
        self_t.insert_with_value("apple", "A".to_string()).expect("ins");
        self_t.insert_with_value("banana", "B".to_string()).expect("ins");
        other.insert_with_value("apple", "X".to_string()).expect("ins"); // overlap
        other.insert_with_value("cherry", "C".to_string()).expect("ins"); // other-only
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
        assert_eq!(self_t.get_value("banana"), Some("B".to_string()), "self-only unchanged");
        assert_eq!(self_t.get_value("cherry"), Some("C".to_string()), "other-only inserted");
        self_t.sync().expect("sync");
    }
    let self_t = PersistentARTrieChar::<String>::open(&path).expect("reopen");
    assert_eq!(self_t.get_value("apple"), Some("AX".to_string()), "merged value durable");
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
        let mut trie = PersistentARTrie::<String>::create(&path).expect("create");
        assert!(
            trie.route_overlay(),
            "feature on ⇒ a String byte trie auto-flips to the overlay at create"
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
    let a: SharedCharTrie<u64> =
        SharedCharTrie::create(&dir.path().join("a.artc")).expect("create a");
    let b: SharedCharTrie<u64> =
        SharedCharTrie::create(&dir.path().join("b.artc")).expect("create b");
    a.write().insert_with_value("shared", 1).expect("a ins");
    a.write().insert_with_value("a_only", 10).expect("a ins2");
    b.write().insert_with_value("shared", 2).expect("b ins");
    b.write().insert_with_value("b_only", 20).expect("b ins2");

    let (a1, b1) = (a.clone(), b.clone());
    let (a2, b2) = (a.clone(), b.clone());
    let h1 = thread::spawn(move || a1.union_with(&b1, |x, y| x + y));
    let h2 = thread::spawn(move || b2.union_with(&a2, |x, y| x + y));
    // If the AB/BA cross-instance deadlock regressed, these joins hang forever.
    let n1 = h1.join().expect("A.union_with(&B) completed (no deadlock)");
    let n2 = h2.join().expect("B.union_with(&A) completed (no deadlock)");
    assert_eq!(n1, 2, "A processed both of B's terms");
    assert_eq!(n2, 2, "B processed both of A's terms");
}
