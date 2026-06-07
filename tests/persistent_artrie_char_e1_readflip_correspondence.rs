//! **S5-12 E1 read-flip correspondence.** The read-flip routes the public read API
//! (`contains`/`len`/`is_empty`/`get_value`/`iter`/`iter_prefix*`) to the immutable
//! lock-free overlay when `route_overlay()` is true (the production default for ALL
//! `V` after the create-flip). This suite proves the overlay read answers
//! IDENTICALLY to the proven owned-tree read for the same data: build two tries from
//! the same terms — one forced to the owned path (`kill_switch_to_owned`), one left on
//! the default overlay path — and assert their public reads agree. It also pins the
//! two read-fidelity traps the red-team flagged: the `None`-vs-`Some(empty)` prefix
//! distinction, and unbounded recursion on a deep (un-path-compressed) overlay key.
//!
//! Scratch lives on real disk (`target/test-tmp`), never `/tmp` (tmpfs on this host).

#![cfg(feature = "persistent-artrie")]

use std::collections::{BTreeMap, BTreeSet};

use libdictenstein::persistent_artrie_char::PersistentARTrieChar;

fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

const MEMBERSHIP_TERMS: &[&str] = &[
    "apple",
    "application",
    "apply",
    "apt",
    "banana",
    "band",
    "bandana",
    "日本",
    "日本語",
    "🎉party",
];

// Prefixes covering: present-with-finals, present-internal, the empty prefix (= all),
// and absent (no in-memory edge — must be `None`, not `Some(empty)`).
const PROBE_PREFIXES: &[&str] = &["app", "ban", "b", "日", "", "xyz", "appz", "🎉"];

/// `V = ()` membership: overlay reads must equal owned reads for `len`, `is_empty`,
/// `contains`, and `iter_prefix` (as a set AND in the `None`-vs-`Some(empty)` shape).
#[test]
fn e1_membership_reads_correspond_overlay_vs_owned() {
    let dir = scratch("e1-membership");
    let owned_path = dir.path().join("owned.artc");
    let overlay_path = dir.path().join("overlay.artc");

    // Owned trie: force the proven owned path so its reads are the pre-flip oracle.
    let owned = PersistentARTrieChar::<()>::create(&owned_path).expect("create owned");
    owned.kill_switch_to_owned();
    for t in MEMBERSHIP_TERMS {
        owned.insert(t).expect("owned insert");
    }

    // Overlay trie: the default create-flip routes writes (and now reads) to the overlay.
    let overlay = PersistentARTrieChar::<()>::create(&overlay_path).expect("create overlay");
    assert!(
        overlay.route_overlay(),
        "an eligible-V (`()`) create must route to the overlay"
    );
    for t in MEMBERSHIP_TERMS {
        overlay.insert(t).expect("overlay insert");
    }

    // Counts.
    assert_eq!(owned.len(), overlay.len(), "len mismatch");
    assert_eq!(owned.len(), MEMBERSHIP_TERMS.len(), "len should be term count");
    assert_eq!(owned.is_empty(), overlay.is_empty(), "is_empty mismatch");
    assert!(!overlay.is_empty());

    // Membership (present + absent).
    for t in MEMBERSHIP_TERMS.iter().chain(["absent", "ap", "z"].iter()) {
        assert_eq!(
            owned.contains(t),
            overlay.contains(t),
            "contains mismatch for {t:?}"
        );
    }

    // Prefix iteration: identical as a SET, and identical `None`-vs-`Some` shape.
    for p in PROBE_PREFIXES {
        let o = owned.iter_prefix(p).expect("owned iter_prefix");
        let v = overlay.iter_prefix(p).expect("overlay iter_prefix");
        assert_eq!(
            o.is_none(),
            v.is_none(),
            "None-vs-Some(empty) prefix shape mismatch for {p:?} (owned={o:?})"
        );
        let o_set: Option<BTreeSet<String>> = o.map(|v| v.into_iter().collect());
        let v_set: Option<BTreeSet<String>> = v.map(|v| v.into_iter().collect());
        assert_eq!(o_set, v_set, "iter_prefix({p:?}) set mismatch");
    }

    // The empty prefix enumerates the whole dictionary.
    let all: BTreeSet<String> = overlay
        .iter_prefix("")
        .expect("iter_prefix(\"\")")
        .expect("non-empty trie ⇒ Some")
        .into_iter()
        .collect();
    let expected: BTreeSet<String> = MEMBERSHIP_TERMS.iter().map(|s| s.to_string()).collect();
    assert_eq!(all, expected, "iter_prefix(\"\") must equal the full term set");
}

/// `V = u64` counters: overlay reads must equal owned reads for `get_value` and
/// `iter_prefix_with_values` (the `u64` value per final), plus `len`/`contains`.
#[test]
fn e1_counter_reads_correspond_overlay_vs_owned() {
    let dir = scratch("e1-counter");
    let owned_path = dir.path().join("owned.artc");
    let overlay_path = dir.path().join("overlay.artc");

    let entries: Vec<(&str, u64)> = vec![
        ("apple", 3),
        ("application", 17),
        ("apply", 1),
        ("banana", 5000),
        ("band", 42),
        ("日本", 7),
        ("🎉party", 99),
    ];

    let owned = PersistentARTrieChar::<u64>::create(&owned_path).expect("create owned");
    owned.kill_switch_to_owned();
    for (t, v) in &entries {
        owned.upsert(t, *v).expect("owned upsert");
    }

    let overlay = PersistentARTrieChar::<u64>::create(&overlay_path).expect("create overlay");
    assert!(overlay.route_overlay(), "u64 create must route to the overlay");
    for (t, v) in &entries {
        overlay.upsert(t, *v).expect("overlay upsert");
    }

    assert_eq!(owned.len(), overlay.len(), "len mismatch");

    // Per-term value: owned `get_value` (owned tree) == overlay `get_value` (value-route).
    for (t, v) in &entries {
        assert_eq!(owned.get_value(t), Some(*v), "owned get_value for {t:?}");
        assert_eq!(
            overlay.get_value(t),
            owned.get_value(t),
            "overlay get_value mismatch for {t:?}"
        );
    }
    // Absent term: both `None`.
    assert_eq!(owned.get_value("absent"), None);
    assert_eq!(overlay.get_value("absent"), None);

    // `iter_prefix_with_values`: identical (term → value) maps, identical None/Some shape.
    for p in PROBE_PREFIXES {
        let o = owned.iter_prefix_with_values(p).expect("owned");
        let v = overlay.iter_prefix_with_values(p).expect("overlay");
        assert_eq!(
            o.is_none(),
            v.is_none(),
            "None-vs-Some shape mismatch for {p:?}"
        );
        let o_map: Option<BTreeMap<String, u64>> = o.map(|v| v.into_iter().collect());
        let v_map: Option<BTreeMap<String, u64>> = v.map(|v| v.into_iter().collect());
        assert_eq!(o_map, v_map, "iter_prefix_with_values({p:?}) mismatch");
    }
}

/// The overlay is NOT path-compressed (one node per code point), so a length-N key is
/// an N-deep overlay spine; the enumerators recurse by depth. A length-500 key must
/// not overflow the stack in `len`/`contains`/`iter_prefix` (the same depth a
/// production lock-free point read already tolerates).
#[test]
fn e1_deep_key_overlay_reads_no_stack_overflow() {
    let dir = scratch("e1-deep-key");
    let path = dir.path().join("deep.artc");

    let deep: String = "a".repeat(500);
    let deep_unicode: String = "日".repeat(500);

    let overlay = PersistentARTrieChar::<u64>::create(&path).expect("create");
    assert!(overlay.route_overlay());
    overlay.upsert(&deep, 11).expect("deep insert");
    overlay.upsert(&deep_unicode, 22).expect("deep unicode insert");

    // Count / membership / value walks at depth 500 must not overflow.
    assert_eq!(overlay.len(), 2);
    assert!(overlay.contains(&deep));
    assert!(overlay.contains(&deep_unicode));
    assert_eq!(overlay.get_value(&deep), Some(11));
    assert_eq!(overlay.get_value(&deep_unicode), Some(22));

    // Whole-tree enumeration (the deepest DFS) at depth 500 must not overflow.
    let all: BTreeSet<String> = overlay
        .iter_prefix("")
        .expect("iter_prefix")
        .expect("Some")
        .into_iter()
        .collect();
    assert_eq!(all.len(), 2);
    assert!(all.contains(&deep));

    // A deep prefix walk (navigate 499 levels, then collect).
    let prefix: String = "a".repeat(499);
    let under = overlay
        .iter_prefix(&prefix)
        .expect("iter_prefix deep")
        .expect("Some");
    assert_eq!(under, vec![deep.clone()], "deep prefix walk");
}

/// E1 read fidelity on the OWNED read path: a `String`-valued trie that has been
/// `kill_switch_to_owned()`'d serves its reads from the owned tree unchanged. Arbitrary-V
/// overlay routing is the default (so `String` create-flips), so the kill-switch is the
/// supported way to force the owned read path; this is the owned-path control for the
/// overlay read-flip.
#[test]
fn e1_owned_read_path_after_kill_switch() {
    let dir = scratch("e1-inert");
    let path = dir.path().join("ineligible.artc");

    let trie = PersistentARTrieChar::<String>::create(&path).expect("create");
    // Arbitrary-V overlay routing is the default, so `String` create-flips; kill-switch
    // it to the owned path so this test exercises the inert owned read path.
    trie.kill_switch_to_owned();
    assert!(!trie.route_overlay(), "reads stay on the owned path");
    trie.upsert("hello", "world".to_string()).expect("upsert");
    trie.upsert("help", "me".to_string()).expect("upsert");

    assert_eq!(trie.len(), 2);
    assert!(trie.contains("hello"));
    assert_eq!(trie.get_value("hello"), Some("world".to_string()));
    let under: BTreeSet<String> = trie
        .iter_prefix("hel")
        .expect("iter_prefix")
        .expect("Some")
        .into_iter()
        .collect();
    assert_eq!(
        under,
        ["hello", "help"].iter().map(|s| s.to_string()).collect()
    );
}
