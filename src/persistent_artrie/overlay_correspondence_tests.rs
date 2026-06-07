//! **M2a byte `LockFreeOverlay` correspondence + reestablish round-trip.**
//!
//! The byte twin of `tests/persistent_artrie_char_e1_readflip_correspondence.rs`.
//! M2a gives byte the shared `LockFreeOverlay<ByteKey, V, S>` trait DEFAULTS (the
//! route predicate, the non-faulting overlay read engine, flip/kill-switch, and
//! the no-WAL reestablish folds) as an OPT-IN, REVERSIBLE capability — NO
//! production create-flip yet (that is M4). So this suite, unlike the char E1
//! integration test, lives IN-CRATE (the read-engine skins + the
//! `LockFreeOverlay` trait are `pub(crate)`) and drives the overlay EXPLICITLY:
//! `enable_lockfree()` + `set_overlay_write_mode(LockFreeOverlay)`, never a
//! create-flip.
//!
//! It proves:
//! 1. **Read correspondence** — with `route_overlay()` true, the trait read engine
//!    (`overlay_len`/`overlay_iter_prefix`/`overlay_iter_prefix_with_values`/
//!    `overlay_get_value`) answers IDENTICALLY to the proven OWNED read of the
//!    same data (an owned-path oracle trie). Membership (`V=()`) and the `i64`
//!    counter monomorph.
//! 2. **`None`-vs-`Some(empty)` prefix shape** — the red-team read-fidelity trap.
//! 3. **Deep key** — a length-500 (un-path-compressed) overlay spine must not
//!    overflow the DFS in `overlay_len`/`overlay_iter_prefix`.
//! 4. **Reestablish round-trip** — build OWNED, `enable_lockfree`, then
//!    `reestablish_overlay_membership`/`_counter` (the no-WAL folds, D1: read the
//!    owned tree via the un-routed `owned_*` seams while `route_overlay()` is
//!    already true) reproduce every owned term in the overlay AND clear the owned
//!    tree LAST.
//!
//! Scratch lives on real disk (`target/test-tmp`), never `/tmp` (tmpfs on this
//! host).

#![cfg(test)]

use std::collections::{BTreeMap, BTreeSet};

use crate::persistent_artrie::overlay_write_mode::OverlayWriteMode;
use crate::persistent_artrie::PersistentARTrie;
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::Dictionary; // `len()` / `is_empty()` are Dictionary-trait methods.

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
    b"\x00\x01\x02", // non-UTF-8 bytes: byte keys are arbitrary bytes
    b"\xff\xfe",
    b"party",
];

// Prefixes covering: present-with-finals, present-internal, the empty prefix
// (= all), and absent (no in-memory edge — must be `None`, not `Some(empty)`).
const PROBE_PREFIXES: &[&[u8]] = &[
    b"app", b"ban", b"b", b"", // all
    b"xyz", b"appz", b"\x00", b"\xff",
];

/// `V = ()` membership: the trait overlay reads must equal the owned reads for
/// `overlay_len`/`overlay_is_empty`/`overlay_get_value`/`overlay_iter_prefix`
/// (as a set AND in the `None`-vs-`Some(empty)` shape).
#[test]
fn m2a_membership_reads_correspond_overlay_vs_owned() {
    let dir = scratch("byte-m2a-membership");
    let owned_path = dir.path().join("owned.part");
    let overlay_path = dir.path().join("overlay.part");

    // Owned oracle: M2a has no create-flip, so a default byte trie IS owned —
    // its public reads are the pre-flip oracle (read `self.root`). Insert the
    // terms BYTE-EXACT via `upsert_bytes` (byte keys are arbitrary bytes, incl.
    // the non-UTF-8 MEMBERSHIP_TERMS), with the unit value `()`. `kill_switch_to_owned`
    // is defensive (M2a never flips a fresh ctor anyway).
    let owned = PersistentARTrie::<()>::create(&owned_path).expect("create owned");
    owned.kill_switch_to_owned();
    assert!(!owned.route_overlay(), "default byte trie is owned in M2a");
    for t in MEMBERSHIP_TERMS {
        owned.upsert_bytes(t, ()).expect("owned upsert_bytes");
    }

    // Overlay trie: EXPLICIT opt-in (enable_lockfree + set LockFreeOverlay), NOT a
    // create-flip. Publish via the no-WAL `insert_cas`.
    let mut overlay = PersistentARTrie::<()>::create(&overlay_path).expect("create overlay");
    overlay.enable_lockfree();
    overlay.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
    assert!(
        overlay.route_overlay(),
        "explicit enable_lockfree + LockFreeOverlay must route to the overlay"
    );
    for t in MEMBERSHIP_TERMS {
        overlay.insert_cas(t);
    }

    // Counts: overlay_len (resident-finals) == owned term count.
    assert_eq!(
        overlay.overlay_len(),
        owned.len().unwrap(),
        "overlay_len vs owned len"
    );
    assert_eq!(overlay.overlay_len(), MEMBERSHIP_TERMS.len());
    assert_eq!(
        overlay.overlay_is_empty(),
        owned.is_empty(),
        "overlay_is_empty vs owned"
    );
    assert!(!overlay.overlay_is_empty());

    // Membership (present + absent): overlay_get_value == owned contains.
    for t in MEMBERSHIP_TERMS
        .iter()
        .copied()
        .chain([b"absent".as_slice(), b"ap", b"z"])
    {
        // `overlay_get_value` returns `Some(Some(()))` present, `Some(None)`
        // absent for the `()` monomorph.
        let overlay_present = matches!(overlay.overlay_get_value(t), Some(Some(())));
        assert_eq!(
            overlay_present,
            owned.contains_bytes(t),
            "membership mismatch for {t:?}"
        );
    }

    // Prefix iteration: identical as a SET, and identical None-vs-Some shape.
    for p in PROBE_PREFIXES {
        let o = owned_iter_prefix_terms(&owned, p);
        let v = overlay.overlay_iter_prefix(p);
        assert_eq!(
            o.is_none(),
            v.is_none(),
            "None-vs-Some(empty) prefix shape mismatch for {p:?} (owned={o:?})"
        );
        let o_set: Option<BTreeSet<Vec<u8>>> = o.map(|v| v.into_iter().collect());
        let v_set: Option<BTreeSet<Vec<u8>>> = v.map(|v| v.into_iter().collect());
        assert_eq!(o_set, v_set, "overlay_iter_prefix({p:?}) set mismatch");
    }

    // The empty prefix enumerates the whole dictionary.
    let all: BTreeSet<Vec<u8>> = overlay
        .overlay_iter_prefix(b"")
        .expect("overlay_iter_prefix(\"\")")
        .into_iter()
        .collect();
    let expected: BTreeSet<Vec<u8>> = MEMBERSHIP_TERMS.iter().map(|s| s.to_vec()).collect();
    assert_eq!(all, expected, "overlay_iter_prefix(\"\") == full term set");
}

/// `V = u64` counters: the trait overlay reads must equal the owned reads for
/// `overlay_get_value` and `overlay_iter_prefix_with_values`, plus `overlay_len`.
/// (Byte's counter monomorph is now `u64`, post-u64-restoration.)
#[test]
fn m2a_counter_reads_correspond_overlay_vs_owned() {
    let dir = scratch("byte-m2a-counter");
    let owned_path = dir.path().join("owned.part");
    let overlay_path = dir.path().join("overlay.part");

    let entries: Vec<(&[u8], u64)> = vec![
        (b"apple", 3),
        (b"application", 17),
        (b"apply", 1),
        (b"banana", 5000),
        (b"band", 42),
        (b"\x00\x01", 7),
        (b"party", 99),
    ];

    // Owned oracle (u64 counters via upsert_bytes — sets the owned value directly).
    let owned = PersistentARTrie::<u64>::create(&owned_path).expect("create owned");
    owned.kill_switch_to_owned();
    for (t, v) in &entries {
        owned.upsert_bytes(t, *v).expect("owned upsert_bytes");
    }
    assert!(!owned.route_overlay());

    // Overlay trie: explicit opt-in; publish the same counts via `increment_cas`
    // (single increment from 0 == the count). `increment_cas` lives on
    // `<u64, S>`, which is this monomorph.
    let mut overlay = PersistentARTrie::<u64>::create(&overlay_path).expect("create overlay");
    overlay.enable_lockfree();
    overlay.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
    assert!(overlay.route_overlay(), "u64 explicit flip must route");
    for (t, v) in &entries {
        overlay.increment_cas(t, *v);
    }

    assert_eq!(overlay.overlay_len(), owned.len().unwrap(), "len mismatch");

    // Per-term value: owned get_value_bytes (owned tree) == overlay value-route.
    for (t, v) in &entries {
        assert_eq!(owned.get_value_bytes(t), Some(*v), "owned value for {t:?}");
        // `overlay_get_value` returns `Some(Some(count))` for a present i64 final.
        let overlay_value = match overlay.overlay_get_value(t) {
            Some(inner) => inner,
            None => panic!("overlay handled the i64 value-route ⇒ Some(_), got None for {t:?}"),
        };
        assert_eq!(
            overlay_value,
            owned.get_value_bytes(t),
            "overlay value-route mismatch for {t:?}"
        );
    }
    // Absent term: owned None; overlay `Some(None)` (handled, absent).
    assert_eq!(owned.get_value_bytes(b"absent"), None);
    assert_eq!(overlay.overlay_get_value(b"absent"), Some(None));

    // `iter_prefix_with_values`: identical (term → value) maps, identical shape.
    for p in PROBE_PREFIXES {
        let o = owned_iter_prefix_values(&owned, p);
        let v = overlay.overlay_iter_prefix_with_values(p);
        assert_eq!(
            o.is_none(),
            v.is_none(),
            "None-vs-Some shape mismatch for {p:?}"
        );
        let o_map: Option<BTreeMap<Vec<u8>, u64>> = o.map(|v| v.into_iter().collect());
        let v_map: Option<BTreeMap<Vec<u8>, u64>> = v.map(|v| v.into_iter().collect());
        assert_eq!(
            o_map, v_map,
            "overlay_iter_prefix_with_values({p:?}) mismatch"
        );
    }
}

/// The overlay is NOT path-compressed (one node per byte), so a length-N key is
/// an N-deep overlay spine; the enumerators recurse by depth. A length-500 key
/// must not overflow the stack in `overlay_len`/`overlay_get_value`/
/// `overlay_iter_prefix`.
#[test]
fn m2a_deep_key_overlay_reads_no_stack_overflow() {
    let dir = scratch("byte-m2a-deep-key");
    let path = dir.path().join("deep.part");

    let deep: Vec<u8> = vec![b'a'; 500];

    let mut overlay = PersistentARTrie::<u64>::create(&path).expect("create");
    overlay.enable_lockfree();
    overlay.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
    assert!(overlay.route_overlay());
    overlay.increment_cas(&deep, 11);

    // Count / value walks at depth 500 must not overflow.
    assert_eq!(overlay.overlay_len(), 1);
    assert_eq!(overlay.overlay_get_value(&deep), Some(Some(11)));

    // Whole-tree enumeration (the deepest DFS) at depth 500 must not overflow.
    let all: BTreeSet<Vec<u8>> = overlay
        .overlay_iter_prefix(b"")
        .expect("overlay_iter_prefix")
        .into_iter()
        .collect();
    assert_eq!(all.len(), 1);
    assert!(all.contains(&deep));

    // A deep prefix walk (navigate 499 levels, then collect).
    let prefix: Vec<u8> = vec![b'a'; 499];
    let under = overlay
        .overlay_iter_prefix(&prefix)
        .expect("overlay_iter_prefix deep");
    assert_eq!(under, vec![deep.clone()], "deep prefix walk");
}

/// **Reestablish round-trip (membership).** Build OWNED, `enable_lockfree` (the
/// overlay is empty), set LockFreeOverlay (route true), then
/// `reestablish_overlay_membership` (D1: reads the recovered owned tree via the
/// un-routed `owned_*` seams while `route_overlay()` is already true) must
/// reproduce every owned term in the overlay and clear the owned tree LAST.
#[test]
fn m2a_reestablish_membership_round_trip() {
    let dir = scratch("byte-m2a-reestablish-mem");
    let path = dir.path().join("r.part");

    let mut trie = PersistentARTrie::<()>::create(&path).expect("create");
    trie.kill_switch_to_owned();
    for t in MEMBERSHIP_TERMS {
        trie.upsert_bytes(t, ()).expect("owned upsert_bytes");
    }
    let owned_before: BTreeSet<Vec<u8>> = MEMBERSHIP_TERMS.iter().map(|s| s.to_vec()).collect();

    // Install the (empty) overlay and route to it — exactly the post-recovery
    // pre-reestablish state the M4 flip will reach.
    trie.enable_lockfree();
    trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
    assert!(trie.route_overlay());
    // Overlay is empty until reestablish; the owned tree still holds the data.
    assert_eq!(trie.overlay_len(), 0, "overlay empty before reestablish");

    // The D1-critical fold: read owned via un-routed seams, publish to overlay,
    // clear owned LAST.
    LockFreeOverlay::reestablish_overlay_membership(&mut trie).expect("reestablish membership");

    // Overlay now holds every owned term.
    let overlay_after: BTreeSet<Vec<u8>> = trie
        .overlay_iter_prefix(b"")
        .expect("overlay_iter_prefix")
        .into_iter()
        .collect();
    assert_eq!(
        overlay_after, owned_before,
        "reestablish must reproduce every owned term in the overlay"
    );
    assert_eq!(trie.overlay_len(), MEMBERSHIP_TERMS.len());

    // Owned tree cleared LAST (term_count zeroed; owned reads now empty). NB: M3
    // routes the public `len()`/`contains_bytes()` to the OVERLAY under
    // `route_overlay()` (which is true here), so the owned-cleared assertion must use
    // the UNROUTED owned readers (the routed reads now see the reestablished overlay).
    assert_eq!(
        trie.term_count.load(std::sync::atomic::Ordering::Acquire),
        0,
        "owned tree's term_count zeroed after reestablish"
    );
    for t in MEMBERSHIP_TERMS {
        assert!(
            !trie.unrouted_contains_bytes(t),
            "owned tree must be empty after reestablish for {t:?}"
        );
    }
}

/// **Reestablish round-trip (counter).** The i64 twin: build OWNED counters,
/// install+route the empty overlay, `reestablish_overlay_counter` must reproduce
/// every NON-EMPTY (term, count) in the overlay and clear owned LAST. Includes a
/// deep key.
///
/// # Empty-term limitation (shared with char)
///
/// The lock-free overlay node represents only NON-empty keys (the byte
/// `insert_cas`/`increment_cas` and the char `insert_cas`/`try_increment_cas`
/// both guard `key.is_empty()` and no-op): the empty term has no edge path to a
/// final node. So the generic `reestablish_overlay_counter` ATTEMPTS to publish
/// the empty-term's count (the RES-6 empty-term partition) via
/// `overlay_publish_counter(&[], v)`, but the overlay primitive drops it. This is
/// a documented overlay property identical across variants (char's counter
/// reestablish behaves the same; its test suite simply never exercised an
/// empty-term counter). The empty-term count therefore survives in DURABLE state
/// (the owned tree / the WAL), NOT in the resident overlay — and a routed public
/// `get_value("")` under the M3 flip will read the owned arm / durable record,
/// not the overlay. We assert the empty term is DROPPED from the overlay (the
/// honest behavior) and that every non-empty term round-trips.
#[test]
fn m2a_reestablish_counter_round_trip() {
    let dir = scratch("byte-m2a-reestablish-ctr");
    let path = dir.path().join("r.part");

    let deep: Vec<u8> = vec![b'z'; 300];
    let empty_term_count: u64 = 13;
    let nonempty_entries: Vec<(Vec<u8>, u64)> = vec![
        (b"apple".to_vec(), 3),
        (b"application".to_vec(), 17),
        (b"banana".to_vec(), 5000),
        (b"\x00\x01".to_vec(), 7),
        (deep.clone(), 22),
    ];

    let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
    trie.kill_switch_to_owned();
    // The empty-term count lives in the owned tree (root value) before reestablish.
    trie.upsert_bytes(b"", empty_term_count)
        .expect("owned empty-term upsert");
    for (t, v) in &nonempty_entries {
        trie.upsert_bytes(t, *v).expect("owned upsert_bytes");
    }
    // Sanity: the owned oracle DOES hold the empty-term count.
    assert_eq!(
        trie.get_value_bytes(b""),
        Some(empty_term_count),
        "owned tree holds the empty-term count before reestablish"
    );
    let nonempty_before: BTreeMap<Vec<u8>, u64> = nonempty_entries.iter().cloned().collect();

    trie.enable_lockfree();
    trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
    assert!(trie.route_overlay());
    assert_eq!(trie.overlay_len(), 0, "overlay empty before reestablish");

    LockFreeOverlay::reestablish_overlay_counter(&mut trie).expect("reestablish counter");

    // Every (term, count) reproduced in the overlay — INCLUDING the empty term,
    // which empty-string support (H3) republishes to the overlay ROOT via the
    // fresh-root-CAS value publisher (it was previously dropped).
    let overlay_after: BTreeMap<Vec<u8>, u64> = trie
        .overlay_iter_prefix_with_values(b"")
        .expect("overlay_iter_prefix_with_values")
        .into_iter()
        .collect();
    let mut expected_with_empty = nonempty_before.clone();
    expected_with_empty.insert(Vec::new(), empty_term_count);
    assert_eq!(
        overlay_after, expected_with_empty,
        "reestablish_counter must reproduce every (term, count) in the overlay INCLUDING \
         the empty term (empty-string support H3)"
    );
    assert_eq!(
        trie.overlay_get_value(b""),
        Some(Some(empty_term_count)),
        "the empty-term count is republished to the overlay ROOT via fresh-root-CAS \
         (empty-string support H3 — previously dropped as Some(None))"
    );
    // Spot-check the value-route + the deep partition.
    assert_eq!(trie.overlay_get_value(b"banana"), Some(Some(5000)));
    assert_eq!(trie.overlay_get_value(&deep), Some(Some(22)), "deep count");

    // Owned cleared LAST. NB: M3 routes the public reads to the OVERLAY under
    // `route_overlay()`, so the owned-cleared assertion uses the UNROUTED owned
    // readers (a routed `get_value_bytes(b"banana")` would now return the
    // reestablished overlay count, not the cleared-owned None).
    assert_eq!(
        trie.term_count.load(std::sync::atomic::Ordering::Acquire),
        0,
        "owned tree's term_count zeroed after reestablish"
    );
    assert_eq!(
        trie.unrouted_get_value_bytes(b"banana"),
        None,
        "owned tree empty after reestablish"
    );
}

// ---------------------------------------------------------------------------
// Owned-oracle prefix helpers — read the OWNED-PATH oracle trie via its PUBLIC
// API (these tries are NOT flipped, so the public reads take the owned arm). We
// normalize the public iterators' shape to match the overlay enumerators'
// `None`-vs-`Some(empty)` contract:
//   - the public byte `iter_prefix(prefix)` returns `None` for an absent prefix
//     and `Some(iter)` otherwise (the arena iterator's shape) — we collect to a
//     `Vec`. The overlay's `overlay_iter_prefix` returns `Some(empty)` for a
//     present-but-childless prefix; the public owned iterator returns `None` for
//     a prefix with no matching terms. For the membership term set used here no
//     probe prefix is "present-internal-with-zero-finals", so the shapes align;
//     this is the same alignment the char E1 suite relies on.
// ---------------------------------------------------------------------------

fn owned_iter_prefix_terms(trie: &PersistentARTrie<()>, prefix: &[u8]) -> Option<Vec<Vec<u8>>> {
    trie.iter_prefix(prefix).map(|it| it.collect())
}

fn owned_iter_prefix_values(
    trie: &PersistentARTrie<u64>,
    prefix: &[u8],
) -> Option<Vec<(Vec<u8>, u64)>> {
    trie.iter_prefix_with_values(prefix).map(|it| it.collect())
}
