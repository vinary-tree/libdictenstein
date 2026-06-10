//! **Byte overlay read/write ROUTING tests.**
//!
//! Since L3.3 deleted the owned tree, the overlay is the SOLE representation and every
//! constructor installs it (`route_overlay()` universally true). These tests pin the
//! routed public surface against the overlay:
//!
//! 1. **The empty term routes to the overlay ROOT** (empty-string support H4+H5):
//!    "" is a first-class key on the overlay root — the routed writer publishes it
//!    via fresh-root-CAS (H4) and the routed `get_value("")` / `contains("")` read it
//!    back from the root (H5).
//! 2. **Durable writes round-trip** through the routed public writers (`insert` /
//!    `insert_with_value` / `upsert_bytes` / `increment_bytes` / `get_or_insert_bytes`
//!    / `remove` / `insert_batch` / `remove_prefix`) and survive reopen.
//! 3. **CAS / the trie-to-trie merges / doc-tx / `compact` all SUCCEED** under the
//!    overlay (C2/F6). (L3.3b B6 deleted the owned-tree drains entirely.)
//! 4. **Deep key** — a length-500 (un-path-compressed) overlay spine must not overflow
//!    the routed public reads' DFS.
//!
//! Scratch lives on real disk (`target/test-tmp`), never `/tmp` (tmpfs on this host).

#![cfg(test)]

use std::collections::{BTreeMap, BTreeSet};

use crate::persistent_artrie::PersistentARTrie;
use crate::Dictionary;

fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

/// **Empty-term routes to the overlay root (empty-string support H4+H5).** Under
/// the overlay the empty term "" IS the root: the routed public writer publishes ""
/// to the root via fresh-root-CAS (H4), and the routed `get_value("")` /
/// `contains("")` read it back from the root (the former owned-only exception is
/// removed — H5). Supersedes the old `m3_empty_term_get_value_reads_owned_under_overlay`,
/// which pinned the now-removed exception.
#[test]
fn m3_empty_term_routes_to_overlay() {
    let dir = scratch("byte-m3-empty-term");
    let path = dir.path().join("e.part");

    let empty_count: u64 = 77;
    let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
    assert!(trie.route_overlay());

    // The routed public writer publishes "" to the overlay ROOT (durable, H4).
    assert!(
        trie.upsert_bytes(b"", empty_count)
            .expect("overlay empty upsert"),
        "upsert(\"\") newly inserted"
    );
    // The routed read reads "" from the overlay root (H5 — no owned exception).
    assert_eq!(
        trie.get_value_bytes(b""),
        Some(empty_count),
        "empty-term get_value routes to the overlay root (empty-string support H5)"
    );
    assert!(
        trie.contains_bytes(b""),
        "contains(\"\") reads the overlay root final flag"
    );
    // Upsert overwrites the root value (LWW); returns false (updated, not inserted).
    assert!(
        !trie
            .upsert_bytes(b"", 99)
            .expect("overlay empty upsert overwrite"),
        "second upsert(\"\") updates"
    );
    assert_eq!(trie.get_value_bytes(b""), Some(99));
    // A non-empty absent term routes to the (otherwise-empty) overlay → None.
    assert_eq!(
        trie.get_value_bytes(b"nonexistent"),
        None,
        "non-empty absent term routes to the overlay → None"
    );
}

/// **Durable writes round-trip through the ROUTED public writers.** Build via the
/// public writers, then read back via the routed reads — and (the durability
/// witness) reopen WITHOUT a checkpoint and confirm the writes survived via WAL
/// replay. NB: reopen is owned-regime-reconstructed; we re-flip + reestablish is
/// M4, so here we read the durable owned tree the WAL rebuilt (the overlay-write
/// durability is the WAL record, which replays into the owned tree on reopen).
#[test]
fn m3_routed_writes_round_trip_and_survive_reopen() {
    let dir = scratch("byte-m3-write-roundtrip");
    let path = dir.path().join("w.part");

    {
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
        assert!(trie.route_overlay());

        // Each routed public writer. `insert_with_value` returns `bool` (byte
        // signature), the durable failure-soft path reports false on error.
        assert!(
            trie.insert_with_value("alpha", 10),
            "insert_with_value newly inserted"
        );
        assert!(
            !trie.insert_with_value("alpha", 999),
            "insert_with_value is INSERT (no-op on existing)"
        );
        assert_eq!(trie.upsert_bytes(b"alpha", 11).expect("upsert"), false); // updated
        assert!(trie.upsert_bytes(b"beta", 20).expect("upsert new")); // newly inserted
        assert_eq!(trie.increment_bytes(b"beta", 5).expect("increment"), 25);
        assert_eq!(
            trie.get_or_insert_bytes(b"gamma", 7)
                .expect("get_or_insert"),
            7
        );
        assert_eq!(
            trie.get_or_insert_bytes(b"gamma", 100)
                .expect("get_or_insert existing"),
            7
        );
        // insert_batch (routes per-entry to the durable overlay).
        let n = trie.insert_batch(&[
            ("delta".to_string(), Some(4)),
            ("epsilon".to_string(), Some(5)),
        ]);
        assert_eq!(n, 2, "insert_batch newly inserted 2");

        // Routed reads see them.
        assert_eq!(trie.get_value_bytes(b"alpha"), Some(11));
        assert_eq!(trie.get_value_bytes(b"beta"), Some(25));
        assert_eq!(trie.get_value_bytes(b"gamma"), Some(7));
        assert_eq!(trie.get_value_bytes(b"delta"), Some(4));
        assert_eq!(trie.get_value_bytes(b"epsilon"), Some(5));

        // Routed remove.
        assert!(trie.remove("delta"));
        assert_eq!(trie.get_value_bytes(b"delta"), None);
        // remove_prefix (overlay remove-CAS): remove the "e*" family ("epsilon").
        let removed = trie.remove_prefix(b"e");
        assert_eq!(removed, 1, "remove_prefix removed epsilon");
        assert_eq!(trie.get_value_bytes(b"epsilon"), None);

        trie.sync().expect("sync");
    }

    // Reopen WITHOUT a checkpoint — the durable WAL records replay (into the owned
    // tree; the reopened trie is owned-regime since flip_to_overlay on a fresh WAL
    // stamps Overlay, but the reopen rebuilds owned + would re-flip at M4). We read
    // the surviving terms via the public reads.
    let reopened = PersistentARTrie::<u64>::open(&path).expect("reopen");
    // The acked writes survived (alpha=11 after upsert, beta=25 after increment,
    // gamma=7, delta+epsilon removed).
    assert_eq!(
        reopened.get_value_bytes(b"alpha"),
        Some(11),
        "alpha durable through reopen"
    );
    assert_eq!(
        reopened.get_value_bytes(b"beta"),
        Some(25),
        "beta durable through reopen"
    );
    assert_eq!(reopened.get_value_bytes(b"gamma"), Some(7));
    assert_eq!(reopened.get_value_bytes(b"delta"), None, "delta removed");
    assert_eq!(
        reopened.get_value_bytes(b"epsilon"),
        None,
        "epsilon removed"
    );
}

/// **Overlay-routed ops all SUCCEED.** F0/G5 (NH2) supports `compare_and_swap`, C2
/// routes the trie-to-trie merges (`merge_from`/`merge_replace`/`merge_from_batched`/
/// `merge_from_batched_grouped`) and `begin_document` through the overlay, and F6 makes
/// `compact` succeed under the overlay too (it sources the snapshot from the overlay and
/// re-flips to preserve the regime). (L3.3b B6 deleted the owned-tree drains
/// `merge_lockfree_{,values_}to_persistent` entirely, so their former reject assertions
/// are gone with them.)
#[test]
fn m3_overlay_routed_ops_succeed() {
    let dir = scratch("byte-m3-rejects");
    let path = dir.path().join("r.part");
    let other_path = dir.path().join("other.part");

    let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
    assert!(trie.route_overlay());
    trie.increment_bytes(b"seed", 1).expect("seed");

    // compare_and_swap / _bytes — Flip F0/G5 (NH2): now WORKS under the overlay (the
    // generic overlay value-CAS; bincode-byte compare + per-iteration recheck). seed
    // == 1 (from the increment above), so a matching CAS swaps to 2; a stale CAS no-ops.
    assert!(
        trie.compare_and_swap("seed", Some(1), 2)
            .expect("CAS should succeed under overlay (NH2)"),
        "compare_and_swap with matching expected now swaps under overlay"
    );
    assert_eq!(
        trie.get_value_bytes(b"seed"),
        Some(2),
        "CAS swapped seed 1→2"
    );
    assert!(
        !trie
            .compare_and_swap_bytes(b"seed", Some(1), 9)
            .expect("CAS mismatch returns Ok(false)"),
        "compare_and_swap_bytes with a now-stale expected (1, current is 2) ⇒ no swap"
    );
    assert_eq!(
        trie.get_value_bytes(b"seed"),
        Some(2),
        "failed CAS left seed unchanged"
    );

    // F2-migrate: C2 routes the trie-to-trie merges through the overlay — they now
    // SUCCEED (read self via the overlay seam, combine via merge_fn, publish). `other`
    // holds `x=100`; `seed=2` (from the CAS above), no overlap, so each merge inserts x.
    let mut other = PersistentARTrie::<u64>::create(&other_path).expect("create other");
    other.increment_bytes(b"x", 100).expect("other seed");
    assert_eq!(
        trie.merge_from(&other, |a, _| *a)
            .expect("merge_from succeeds under overlay (C2)"),
        1,
        "merge_from now processes the one other-only term under the overlay"
    );
    assert_eq!(
        trie.get_value_bytes(b"x"),
        Some(100),
        "merge_from inserted x"
    );
    assert_eq!(
        trie.merge_replace(&other)
            .expect("merge_replace succeeds under overlay (C2)"),
        1,
        "merge_replace now processes the overlapping term under the overlay"
    );
    assert_eq!(
        trie.merge_from_batched(&other, |a, _| *a, 100)
            .expect("merge_from_batched succeeds under overlay (C2)"),
        1
    );
    assert_eq!(
        trie.merge_from_batched_grouped(&other, |a, _| *a, 100)
            .expect("merge_from_batched_grouped succeeds under overlay (C2)"),
        1
    );

    // doc-tx: C2 made begin_document succeed under the overlay (it skips the orphan
    // BeginTx WAL append; commit_document is per-op durable).
    let tx = trie
        .begin_document("doc")
        .expect("begin_document succeeds under overlay (C2)");
    assert_eq!(
        trie.commit_document(tx).expect("empty commit"),
        0,
        "an empty doc-tx commits 0 ops under the overlay"
    );

    // compact (file-replacer) now SUCCEEDS under the overlay (F6): it sources the
    // snapshot from the overlay (enumeration AND values), rebuilds a dense owned image,
    // and RE-FLIPS to preserve the regime — no longer a reject.
    let cfg = crate::persistent_artrie::CompactionConfig::default();
    trie.compact(cfg, |_| {})
        .expect("F6: compact succeeds under the overlay");
    assert!(
        trie.route_overlay(),
        "F6: compact preserves the overlay regime (re-flip after reopen)"
    );
}

/// A length-500 (un-path-compressed) overlay spine must not overflow the routed
/// public reads' DFS (the deep-key crash guard, byte twin of the M2a deep-key test
/// but via the PUBLIC routed reads).
#[test]
fn m3_deep_key_routed_reads_no_stack_overflow() {
    let dir = scratch("byte-m3-deep");
    let path = dir.path().join("deep.part");

    let deep: Vec<u8> = vec![b'a'; 500];
    let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
    assert!(trie.route_overlay());
    trie.increment_bytes(&deep, 11).expect("deep increment");

    assert_eq!(Dictionary::len(&trie), Some(1));
    assert_eq!(trie.get_value_bytes(&deep), Some(11));
    let all: BTreeSet<Vec<u8>> = trie.iter().collect();
    assert!(all.contains(&deep));
    let with_values: BTreeMap<Vec<u8>, Option<u64>> = trie.iter_with_values().collect();
    assert_eq!(with_values.get(&deep), Some(&Some(11)));
}
