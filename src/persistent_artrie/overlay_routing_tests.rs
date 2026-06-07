//! **M3 byte read/write ROUTING + REJECT correspondence.**
//!
//! The byte twin of char's `persistent_artrie_char_e1_readflip_correspondence`
//! plus the M2a `overlay_correspondence_tests`. M3 WIRES byte's public reads +
//! writes to the overlay under `route_overlay()` (the byte twin of char's E1
//! read-flip + the write-route checklist). Everything is gated on `route_overlay()`
//! which is FALSE in production until M4's create-flip, so this suite drives the
//! overlay EXPLICITLY (`enable_lockfree()` + `set_overlay_write_mode(LockFreeOverlay)`)
//! to exercise the routed arms.
//!
//! It proves:
//! 1. **Routed public reads** (`contains_bytes` / `get_value_bytes` / the
//!    `Dictionary`/`MappedDictionary` trait reads / `len` / `iter` /
//!    `iter_with_values` / `iter_prefix*` / `iter_prefix_from_cursor`) equal the
//!    OWNED oracle of the same data.
//! 2. **The value-carrying iter route** (`iter_with_values` /
//!    `iter_prefix_with_values` /`iter_prefix_with_values_and_arena`) carries the
//!    overlay value — NOT enumerate-overlay-then-value-owned (audit §C.2).
//! 3. **The empty term routes to the overlay ROOT** (empty-string support H4+H5):
//!    "" is a first-class key on the overlay root — the routed writer publishes it
//!    via fresh-root-CAS and the routed `get_value("")` / `contains("")` read it back.
//! 4. **The reject guards fire** under the overlay (`compact` / the trie-to-trie
//!    merges / the lockfree drains / doc-tx / `compare_and_swap*`) → `InvalidOperation`.
//! 5. **Durable writes round-trip** through the ROUTED public writers (`insert` /
//!    `insert_with_value` / `upsert_bytes` / `increment_bytes` / `get_or_insert_bytes`
//!    / `remove` / `insert_batch` / `remove_prefix`).
//! 6. **INERT pre-flip:** with `route_overlay()` false (the production default) the
//!    routed methods take the owned arm verbatim — the M2a/M2b/baseline suites are
//!    the oracle; this file's `inert_*` test pins the default explicitly.
//!
//! Scratch lives on real disk (`target/test-tmp`), never `/tmp` (tmpfs on this host).

#![cfg(test)]

use std::collections::{BTreeMap, BTreeSet};

use crate::persistent_artrie::PersistentARTrie;
use crate::persistent_artrie_core::durability::DurabilityPolicy;
use crate::{Dictionary, MappedDictionary};

fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

const MEMBERSHIP_TERMS: &[&[u8]] = &[
    b"apple",
    b"application",
    b"apply",
    b"apt",
    b"banana",
    b"band",
    b"bandana",
    b"\x00\x01\x02",
    b"\xff\xfe",
    b"party",
];

const PROBE_PREFIXES: &[&[u8]] = &[b"app", b"ban", b"b", b"", b"xyz", b"appz", b"\x00", b"\xff"];

/// Build a membership (`V=()`) overlay-routed trie holding `MEMBERSHIP_TERMS` via
/// the ROUTED public `insert`/`upsert_bytes` writers, plus an owned oracle holding
/// the same terms via the owned path. Returns `(overlay, owned, dir)` so `dir`
/// outlives both (it owns the scratch backing).
fn build_membership() -> (
    PersistentARTrie<()>,
    PersistentARTrie<()>,
    tempfile::TempDir,
) {
    let dir = scratch("byte-m3-membership");
    let owned_path = dir.path().join("owned.part");
    let overlay_path = dir.path().join("overlay.part");

    // Owned oracle (default byte trie is owned in M3 — no create-flip yet).
    let owned = PersistentARTrie::<()>::create(&owned_path).expect("create owned");
    owned.kill_switch_to_owned();
    assert!(!owned.route_overlay(), "default byte trie is owned in M3");
    for t in MEMBERSHIP_TERMS {
        owned.upsert_bytes(t, ()).expect("owned upsert_bytes");
    }

    // Overlay trie: EXPLICIT opt-in flip, then write through the ROUTED public
    // writers (so this exercises the M3 write routes, not the raw overlay CAS).
    let mut overlay = PersistentARTrie::<()>::create(&overlay_path).expect("create overlay");
    overlay.flip_to_overlay();
    assert!(overlay.route_overlay(), "explicit flip routes to the overlay");
    for t in MEMBERSHIP_TERMS {
        // Route the public `insert` (str) for the UTF-8 ones; `upsert_bytes`/`insert`
        // both route to the durable overlay membership write. Use `insert` via bytes
        // through `upsert_bytes` for the non-UTF-8 terms (membership = unit value).
        match std::str::from_utf8(t) {
            Ok(s) => {
                overlay.insert(s);
            }
            Err(_) => {
                // Non-UTF-8 membership: the public `insert` takes &str, so use the
                // routed `insert_cas_durable` directly (the byte audit routes
                // `insert`→`insert_cas_durable`; this is that same durable write).
                overlay.insert_cas_durable(t).expect("durable membership insert");
            }
        }
    }
    (overlay, owned, dir)
}

/// Routed membership reads == owned oracle (the headline read correspondence).
#[test]
fn m3_membership_routed_reads_correspond() {
    let (overlay, owned, _dir) = build_membership();

    // `len` via the Dictionary trait (D2-style routed body) and the routed
    // `contains_bytes`.
    assert_eq!(
        Dictionary::len(&overlay),
        Dictionary::len(&owned),
        "Dictionary::len overlay vs owned"
    );
    assert_eq!(Dictionary::len(&overlay), Some(MEMBERSHIP_TERMS.len()));

    for t in MEMBERSHIP_TERMS
        .iter()
        .copied()
        .chain([b"absent".as_slice(), b"ap", b"z"])
    {
        assert_eq!(
            overlay.contains_bytes(t),
            owned.contains_bytes(t),
            "routed contains_bytes mismatch for {t:?}"
        );
    }

    // `Dictionary::contains` (trait body, routes via contains_bytes) for the UTF-8
    // terms.
    for t in MEMBERSHIP_TERMS {
        if let Ok(s) = std::str::from_utf8(t) {
            assert_eq!(
                Dictionary::contains(&overlay, s),
                Dictionary::contains(&owned, s),
                "Dictionary::contains mismatch for {s:?}"
            );
        }
    }

    // `iter` (terms) via the routed arena chokepoint, as a SET.
    let overlay_iter: BTreeSet<Vec<u8>> = overlay.iter().collect();
    let owned_iter: BTreeSet<Vec<u8>> = owned.iter().collect();
    assert_eq!(overlay_iter, owned_iter, "iter() set overlay vs owned");
    let expected: BTreeSet<Vec<u8>> = MEMBERSHIP_TERMS.iter().map(|s| s.to_vec()).collect();
    assert_eq!(overlay_iter, expected);

    // `iter_prefix` set + None-vs-Some(empty) shape for each probe prefix.
    for p in PROBE_PREFIXES {
        let o: Option<BTreeSet<Vec<u8>>> =
            owned.iter_prefix(p).map(|it| it.collect());
        let v: Option<BTreeSet<Vec<u8>>> =
            overlay.iter_prefix(p).map(|it| it.collect());
        assert_eq!(
            o.is_none(),
            v.is_none(),
            "iter_prefix None-vs-Some shape mismatch for {p:?}"
        );
        assert_eq!(o, v, "iter_prefix({p:?}) set mismatch");
    }
}

/// Build an `i64`-counter overlay-routed trie + owned oracle, written through the
/// ROUTED public writers (`increment_bytes` / `upsert_bytes`).
fn build_counters() -> (
    PersistentARTrie<i64>,
    PersistentARTrie<i64>,
    Vec<(Vec<u8>, i64)>,
    tempfile::TempDir,
) {
    let dir = scratch("byte-m3-counter");
    let owned_path = dir.path().join("owned.part");
    let overlay_path = dir.path().join("overlay.part");

    let entries: Vec<(Vec<u8>, i64)> = vec![
        (b"apple".to_vec(), 3),
        (b"application".to_vec(), 17),
        (b"apply".to_vec(), 1),
        (b"banana".to_vec(), 5000),
        (b"band".to_vec(), 42),
        (b"party".to_vec(), 99),
    ];

    let owned = PersistentARTrie::<i64>::create(&owned_path).expect("create owned");
    owned.kill_switch_to_owned();
    for (t, v) in &entries {
        owned.upsert_bytes(t, *v).expect("owned upsert_bytes");
    }
    assert!(!owned.route_overlay());

    let mut overlay = PersistentARTrie::<i64>::create(&overlay_path).expect("create overlay");
    overlay.flip_to_overlay();
    assert!(overlay.route_overlay(), "i64 flip routes");
    // Route the public `increment_bytes` (the durable add-only path); single
    // increment from 0 == the count.
    for (t, v) in &entries {
        let got = overlay.increment_bytes(t, *v).expect("routed increment_bytes");
        assert_eq!(got, *v, "increment_bytes returns the new accumulated count");
    }
    (overlay, owned, entries, dir)
}

/// Routed counter reads == owned oracle, incl. the VALUE-CARRYING iter route.
#[test]
fn m3_counter_routed_reads_correspond_incl_value_carrying_iter() {
    let (overlay, owned, entries, _dir) = build_counters();

    assert_eq!(
        Dictionary::len(&overlay),
        Dictionary::len(&owned),
        "len overlay vs owned"
    );

    // Per-term value via the routed `get_value_bytes` + the `MappedDictionary`
    // trait body (routes via get_value_bytes).
    for (t, v) in &entries {
        assert_eq!(
            overlay.get_value_bytes(t),
            owned.get_value_bytes(t),
            "routed get_value_bytes mismatch for {t:?}"
        );
        assert_eq!(overlay.get_value_bytes(t), Some(*v));
        if let Ok(s) = std::str::from_utf8(t) {
            assert_eq!(
                MappedDictionary::get_value(&overlay, s),
                MappedDictionary::get_value(&owned, s),
                "MappedDictionary::get_value mismatch for {s:?}"
            );
        }
    }
    assert_eq!(overlay.get_value_bytes(b"absent"), None);

    // VALUE-CARRYING iter route (audit §C.2): `iter_with_values` must carry the
    // OVERLAY value, not re-read None from the empty owned tree.
    let overlay_map: BTreeMap<Vec<u8>, Option<i64>> = overlay.iter_with_values().collect();
    let owned_map: BTreeMap<Vec<u8>, Option<i64>> = owned.iter_with_values().collect();
    assert_eq!(
        overlay_map, owned_map,
        "iter_with_values overlay vs owned (value-carrying)"
    );
    // Spot-check a value is actually present (NOT None — the §C.2 bug would make it None).
    assert_eq!(
        overlay_map.get(b"banana".as_slice()),
        Some(&Some(5000)),
        "iter_with_values carries the overlay count (not None)"
    );

    // `iter_prefix_with_values` (value-carrying chokepoint) per prefix.
    for p in PROBE_PREFIXES {
        let o: Option<BTreeMap<Vec<u8>, i64>> = owned
            .iter_prefix_with_values(p)
            .map(|it| it.collect());
        let v: Option<BTreeMap<Vec<u8>, i64>> = overlay
            .iter_prefix_with_values(p)
            .map(|it| it.collect());
        assert_eq!(
            o.is_none(),
            v.is_none(),
            "iter_prefix_with_values None-vs-Some shape mismatch for {p:?}"
        );
        assert_eq!(o, v, "iter_prefix_with_values({p:?}) mismatch");
    }

    // `iter_prefix_from_cursor` (merge-read chokepoint): the full prefix from a
    // None cursor == the value-carrying overlay enumeration.
    let from_cursor: BTreeMap<Vec<u8>, i64> = overlay
        .iter_prefix_from_cursor(b"", None, usize::MAX)
        .expect("iter_prefix_from_cursor")
        .into_iter()
        .map(|e| (e.term, e.value))
        .collect();
    let expected: BTreeMap<Vec<u8>, i64> = entries.iter().cloned().collect();
    assert_eq!(from_cursor, expected, "iter_prefix_from_cursor value-carrying");
    // A cursor strictly after "banana" excludes "banana"/"apple"/... and keeps "party".
    let after_banana: BTreeSet<Vec<u8>> = overlay
        .iter_prefix_from_cursor(b"", Some(b"banana"), usize::MAX)
        .expect("cursor")
        .into_iter()
        .map(|e| e.term)
        .collect();
    assert!(after_banana.contains(b"party".as_slice()));
    assert!(!after_banana.contains(b"banana".as_slice()));
    assert!(!after_banana.contains(b"apple".as_slice()));
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

    let empty_count: i64 = 77;
    let mut trie = PersistentARTrie::<i64>::create(&path).expect("create");
    trie.flip_to_overlay();
    assert!(trie.route_overlay());

    // The routed public writer publishes "" to the overlay ROOT (durable, H4).
    assert!(
        trie.upsert_bytes(b"", empty_count).expect("overlay empty upsert"),
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
        !trie.upsert_bytes(b"", 99).expect("overlay empty upsert overwrite"),
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
        let mut trie = PersistentARTrie::<i64>::create(&path).expect("create");
        trie.flip_to_overlay();
        assert!(trie.route_overlay());

        // Each routed public writer. `insert_with_value` returns `bool` (byte
        // signature), the durable failure-soft path reports false on error.
        assert!(trie.insert_with_value("alpha", 10), "insert_with_value newly inserted");
        assert!(
            !trie.insert_with_value("alpha", 999),
            "insert_with_value is INSERT (no-op on existing)"
        );
        assert_eq!(trie.upsert_bytes(b"alpha", 11).expect("upsert"), false); // updated
        assert!(trie.upsert_bytes(b"beta", 20).expect("upsert new")); // newly inserted
        assert_eq!(trie.increment_bytes(b"beta", 5).expect("increment"), 25);
        assert_eq!(
            trie.get_or_insert_bytes(b"gamma", 7).expect("get_or_insert"),
            7
        );
        assert_eq!(
            trie.get_or_insert_bytes(b"gamma", 100).expect("get_or_insert existing"),
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
    let reopened = PersistentARTrie::<i64>::open(&path).expect("reopen");
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
    assert_eq!(reopened.get_value_bytes(b"epsilon"), None, "epsilon removed");
}

/// **The reject guards under the overlay.** F0/G5 (NH2) supports `compare_and_swap`,
/// C2 now routes the trie-to-trie merges (`merge_from`/`merge_replace`/
/// `merge_from_batched`/`merge_from_batched_grouped`) and `begin_document` through the
/// overlay (they SUCCEED), and F6 makes `compact` succeed under the overlay too (it
/// sources the snapshot from the overlay and re-flips to preserve the regime). The only
/// guards that STILL fire are the owned-tree DRAINS
/// (`merge_lockfree_to_persistent`/`merge_lockfree_values_to_persistent`) — draining the
/// durable overlay back into the owned tree would destroy durable state.
///
/// C2 made these succeed for the byte counter `i64` (overlay-eligible like all `V`),
/// so the old reject assertions were stale.
#[test]
fn m3_reject_guards_fire_under_overlay() {
    let dir = scratch("byte-m3-rejects");
    let path = dir.path().join("r.part");
    let other_path = dir.path().join("other.part");

    let mut trie = PersistentARTrie::<i64>::create(&path).expect("create");
    trie.flip_to_overlay();
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
    assert_eq!(trie.get_value_bytes(b"seed"), Some(2), "CAS swapped seed 1→2");
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
    let mut other = PersistentARTrie::<i64>::create(&other_path).expect("create other");
    other.flip_to_overlay();
    other.increment_bytes(b"x", 100).expect("other seed");
    assert_eq!(
        trie.merge_from(&other, |a, _| *a).expect("merge_from succeeds under overlay (C2)"),
        1,
        "merge_from now processes the one other-only term under the overlay"
    );
    assert_eq!(trie.get_value_bytes(b"x"), Some(100), "merge_from inserted x");
    assert_eq!(
        trie.merge_replace(&other).expect("merge_replace succeeds under overlay (C2)"),
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

    // The owned-tree DRAINS still reject under the overlay (draining the durable
    // overlay into the owned tree would destroy durable state).
    assert!(
        is_invalid_op(trie.merge_lockfree_to_persistent()),
        "merge_lockfree_to_persistent must reject under overlay"
    );
    assert!(
        is_invalid_op(trie.merge_lockfree_values_to_persistent()),
        "merge_lockfree_values_to_persistent must reject under overlay"
    );

    // doc-tx: C2 made begin_document succeed under the overlay (it skips the orphan
    // BeginTx WAL append; commit_document is per-op durable).
    let tx = trie.begin_document("doc").expect("begin_document succeeds under overlay (C2)");
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

/// **INERT pre-flip:** with `route_overlay()` false (the production default), the
/// routed methods take the OWNED arm verbatim — the broken-by-overlay operations
/// (merge / compact / doc-tx / CAS) all SUCCEED on the owned path, proving the
/// false-arm is the verbatim owned body (no regression injected by the routes).
#[test]
fn m3_inert_pre_flip_owned_arm_unchanged() {
    let dir = scratch("byte-m3-inert");
    let path = dir.path().join("i.part");
    let other_path = dir.path().join("io.part");

    let mut trie = PersistentARTrie::<i64>::create(&path).expect("create");
    // **M4b REFRAME.** A fresh `create::<i64>()` now create-flips to the overlay, but
    // THIS test asserts the OWNED arm (the false-arm of the M3 routes) is unchanged —
    // so force the owned path with the kill-switch (the M4b precedent for owned-path
    // feature tests). The kill-switch restamps the WAL Owned (the trie is fresh,
    // current_lsn()==1), so the trie is genuinely owned-regime hereafter.
    trie.kill_switch_to_owned();
    assert!(
        !trie.route_overlay(),
        "kill_switch_to_owned forces the owned arm (M4b: fresh create flips by default)"
    );

    // Owned writes via the routed public writers (must take the owned arm).
    assert!(trie.insert_with_value("a", 1), "owned insert_with_value");
    assert!(trie.upsert_bytes(b"b", 2).expect("upsert"));
    assert_eq!(trie.increment_bytes(b"a", 4).expect("increment"), 5);
    assert_eq!(trie.get_or_insert_bytes(b"c", 9).expect("goi"), 9);
    let _ = trie.insert_batch(&[("d".to_string(), Some(4))]);

    // Owned reads via the routed public reads.
    assert_eq!(trie.get_value_bytes(b"a"), Some(5));
    assert_eq!(trie.get_value_bytes(b"b"), Some(2));
    assert!(trie.contains_bytes(b"c"));
    assert_eq!(Dictionary::len(&trie), Some(4));

    // The "broken-under-overlay" operations SUCCEED on the owned path (false-arm =
    // verbatim owned body — no reject leaks into the owned regime).
    assert!(
        trie.compare_and_swap_bytes(b"a", Some(5), 6).expect("owned CAS"),
        "compare_and_swap works on the owned path (inert)"
    );
    assert_eq!(trie.get_value_bytes(b"a"), Some(6));

    let other = PersistentARTrie::<i64>::create(&other_path).expect("create other");
    other.kill_switch_to_owned();
    other.upsert_bytes(b"x", 100).expect("other upsert");
    let merged = trie.merge_replace(&other).expect("owned merge_replace works");
    assert_eq!(merged, 1, "owned merge processed 1 term (inert)");
    assert_eq!(trie.get_value_bytes(b"x"), Some(100));

    // doc-tx works on the owned path.
    let tx = trie.begin_document("doc").expect("owned begin_document works");
    let committed = trie.commit_document(tx).expect("owned commit_document works");
    assert_eq!(committed, 0, "empty doc-tx commits 0 (inert)");

    // A durability policy that the durable overlay would reject is irrelevant on
    // the owned path: the owned writers don't gate on it. (Sanity: the trie used
    // Immediate by default.)
    assert!(matches!(
        trie.durability_policy(),
        DurabilityPolicy::Immediate | DurabilityPolicy::GroupCommit
    ));
}

/// A length-500 (un-path-compressed) overlay spine must not overflow the routed
/// public reads' DFS (the deep-key crash guard, byte twin of the M2a deep-key test
/// but via the PUBLIC routed reads).
#[test]
fn m3_deep_key_routed_reads_no_stack_overflow() {
    let dir = scratch("byte-m3-deep");
    let path = dir.path().join("deep.part");

    let deep: Vec<u8> = vec![b'a'; 500];
    let mut trie = PersistentARTrie::<i64>::create(&path).expect("create");
    trie.flip_to_overlay();
    assert!(trie.route_overlay());
    trie.increment_bytes(&deep, 11).expect("deep increment");

    assert_eq!(Dictionary::len(&trie), Some(1));
    assert_eq!(trie.get_value_bytes(&deep), Some(11));
    let all: BTreeSet<Vec<u8>> = trie.iter().collect();
    assert!(all.contains(&deep));
    let with_values: BTreeMap<Vec<u8>, Option<i64>> = trie.iter_with_values().collect();
    assert_eq!(with_values.get(&deep), Some(&Some(11)));
}

// ---------------------------------------------------------------------------
// Helper: did the call reject with InvalidOperation?
// ---------------------------------------------------------------------------
fn is_invalid_op<T>(
    r: crate::persistent_artrie::error::Result<T>,
) -> bool {
    matches!(
        r,
        Err(crate::persistent_artrie::error::PersistentARTrieError::InvalidOperation(_))
    )
}
