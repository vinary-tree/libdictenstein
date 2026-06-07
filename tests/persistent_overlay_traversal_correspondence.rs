//! Overlay-backed `DictionaryNode` traversal correspondence (F7 BLOCKER-1).
//!
//! Under `route_overlay()` the owned tree is empty and the lock-free **overlay**
//! serves reads. `Dictionary::root()` now returns an OVERLAY-backed `DictionaryNode`
//! that navigates the overlay lazily, so the graph walk that the zipper / Levenshtein
//! transducer / fuzzy search drive (`root()` → `transition`/`edges`, checking
//! `is_final`/`value`) works on a flipped trie.
//!
//! This is the gold-standard proof that **overlay traversal ≡ owned traversal**: for
//! several tries (Unicode for char, varied fan-out, multi-level, a final empty
//! string, term-only + valued entries), build the trie feature-on (overlay-routed)
//! and a `kill_switch_to_owned()`'d twin with the SAME data, do a full recursive DFS
//! via the `DictionaryNode` trait surface, and assert the overlay walk yields EXACTLY
//! the same `(term, is_final, value)` set as the owned twin AND as the public
//! `iter`/`iter_with_values` (the `iter_prefix("")`) oracle.
//!
//! Membership uses `V = ()` and valued uses the per-variant counter monomorph
//! (`i64` for byte, `u64` for char); these (like all `V`) are overlay-eligible, so the
//! trie is overlay-routed.
//!
//! ## A pre-existing asymmetry this test surfaced (byte owned-walk gap)
//!
//! The CHAR owned `DictionaryNode` walk is complete (the char trie is bucketless;
//! commit `549b068` added the swizzled-child faulter), so `overlay walk == owned
//! twin walk` holds for char. The BYTE owned `DictionaryNode` walk is **pre-existingly
//! incomplete**: the byte trie stores deeper terms in buckets, and the byte node's
//! `bucket_edges`/children traversal does not fully expand bucket suffixes through the
//! trait surface, so the owned byte walk DROPS multi-unit terms (e.g. an owned walk of
//! {a, ab, abc, b, cat, cats} yields only {a, b, cat}). This is orthogonal to the
//! overlay work — the OVERLAY byte walk is complete and correct. The authoritative
//! oracle for full equivalence is therefore `iter()` / `iter_with_values()` (the
//! complete `iter_prefix("")` path that both variants implement correctly); the owned
//! twin is compared `==` for char and `⊇` for byte (the overlay must find everything
//! the deficient owned byte walk finds, and more — exactly the terms it drops).

#![cfg(feature = "persistent-artrie")]

use std::collections::BTreeMap;

use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::{Dictionary, DictionaryNode, MappedDictionaryNode};

/// Real-disk scratch dir (NOT `/tmp`, which is tmpfs here — disk-backed tries must
/// live on a real disk so the mmap/arena paths behave as in production).
fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

// ===========================================================================
// Generic DFS over the `DictionaryNode` trait surface (the transducer's view).
// ===========================================================================

/// One observed final term: its `is_final` flag (always `true` for a collected
/// entry — collected only at finals) and its `MappedDictionaryNode::value()`.
type WalkEntry<V> = (bool, Option<V>);

/// Depth-first walk of the `DictionaryNode` graph from `node`, collecting
/// `(term -> (is_final, value))` for every final node reached. Uses ONLY the trait
/// surface a transducer drives (`is_final`/`value`/`edges`), so it fails exactly
/// when the overlay walk fails to descend or mis-reports finality/value.
///
/// `push_unit` appends one edge label to the running term (byte → one `u8`; char →
/// one `char`).
fn collect_walk<N, V, F>(node: &N, prefix: &mut Vec<N::Unit>, push: &F, out: &mut BTreeMap<Vec<u8>, WalkEntry<V>>)
where
    N: DictionaryNode + MappedDictionaryNode<Value = V>,
    N::Unit: Copy,
    V: Clone,
    F: Fn(&[N::Unit]) -> Vec<u8>,
{
    if node.is_final() {
        out.insert(push(prefix), (true, node.value()));
    }
    for (unit, child) in node.edges() {
        prefix.push(unit);
        collect_walk(&child, prefix, push, out);
        prefix.pop();
    }
}

/// `transition`-driven descent of an entire term, returning the terminal node (or
/// `None` if any edge is missing). Exercises `transition` independently of `edges`.
fn descend<N: DictionaryNode>(root: &N, units: &[N::Unit]) -> Option<N>
where
    N::Unit: Copy,
{
    let mut node = root.clone();
    for &u in units {
        node = node.transition(u)?;
    }
    Some(node)
}

// ===========================================================================
// Byte variant
// ===========================================================================

/// Walk a byte trie via the `DictionaryNode` surface into `term(Vec<u8>) -> (is_final, value)`.
fn byte_walk<V>(trie: &PersistentARTrie<V, impl libdictenstein::persistent_artrie::block_storage::BlockStorage>) -> BTreeMap<Vec<u8>, WalkEntry<V>>
where
    V: libdictenstein::value::DictionaryValue + Clone,
{
    let mut out = BTreeMap::new();
    let mut prefix: Vec<u8> = Vec::new();
    // byte edge labels ARE the term bytes — identity.
    collect_walk(&trie.root(), &mut prefix, &(|p: &[u8]| p.to_vec()), &mut out);
    out
}

/// Build an overlay-routed byte trie + a kill-switched owned twin with the SAME
/// membership terms, and assert the overlay `DictionaryNode` walk == the owned twin
/// walk == the public `iter()` oracle. Also checks per-term `transition` descent and
/// a negative (absent) edge.
fn byte_membership_case(name: &str, terms: &[&str]) {
    let dir = scratch(name);

    // Overlay-routed (create-flips for V = () by default).
    let overlay_path = dir.path().join("overlay.art");
    let mut overlay = PersistentARTrie::<()>::create(&overlay_path).expect("create overlay");
    assert!(
        overlay.route_overlay(),
        "{name}: byte () trie must be overlay-routed by default"
    );
    for t in terms {
        overlay.insert(t);
    }

    // Owned twin (kill-switched to the proven owned path).
    let owned_path = dir.path().join("owned.art");
    let mut owned = PersistentARTrie::<()>::create(&owned_path).expect("create owned");
    owned.kill_switch_to_owned();
    assert!(!owned.route_overlay(), "{name}: owned twin must NOT route overlay");
    for t in terms {
        owned.insert(t);
    }

    let overlay_walk = byte_walk(&overlay);
    let owned_walk = byte_walk(&owned);

    // (1) AUTHORITATIVE: overlay walk term-set == iter() oracle (= iter_prefix("")).
    // byte `iter()` yields the term as `Vec<u8>`. `value()` for a `()` membership
    // final is `None`, so compare on the KEY SET.
    let oracle_keys: std::collections::BTreeSet<Vec<u8>> = overlay.iter().collect();
    let overlay_keys: std::collections::BTreeSet<Vec<u8>> = overlay_walk.keys().cloned().collect();
    assert_eq!(
        overlay_keys, oracle_keys,
        "{name}: byte overlay walk term-set != iter() oracle"
    );

    // (2) overlay walk ⊇ owned-twin walk: the byte owned `DictionaryNode` walk is
    // pre-existingly INCOMPLETE (drops bucket-stored multi-byte terms), so the
    // overlay walk must contain everything the owned walk finds — and strictly more
    // (the dropped terms). NOT `==` (the owned byte walk gap is orthogonal; see the
    // module doc). Finality matches on the shared keys.
    let owned_keys: std::collections::BTreeSet<Vec<u8>> = owned_walk.keys().cloned().collect();
    assert!(
        owned_keys.is_subset(&overlay_keys),
        "{name}: byte overlay walk must be a superset of the (deficient) owned walk"
    );

    // (3) `transition` descent reaches a final node for every term; an absent first
    // byte yields no transition.
    let root = overlay.root();
    for t in terms {
        let node = descend(&root, t.as_bytes())
            .unwrap_or_else(|| panic!("{name}: byte transition descent lost term {t:?}"));
        assert!(node.is_final(), "{name}: byte term {t:?} terminal not final");
    }
    assert!(
        root.transition(0xFF).is_none() || terms.iter().any(|t| t.as_bytes().first() == Some(&0xFF)),
        "{name}: byte spurious transition for an absent first byte"
    );
}

/// Build an overlay-routed byte `<i64>` trie + a kill-switched owned twin with the
/// SAME (term, value) entries, and assert the overlay walk == owned walk ==
/// `iter_with_values()` oracle (VALUE-aware).
fn byte_valued_case(name: &str, entries: &[(&str, i64)]) {
    let dir = scratch(name);

    let overlay_path = dir.path().join("overlay.art");
    let mut overlay = PersistentARTrie::<i64>::create(&overlay_path).expect("create overlay");
    assert!(
        overlay.route_overlay(),
        "{name}: byte i64 trie must be overlay-routed by default"
    );
    for (t, v) in entries {
        overlay.insert_with_value(t, *v);
    }

    let owned_path = dir.path().join("owned.art");
    let mut owned = PersistentARTrie::<i64>::create(&owned_path).expect("create owned");
    owned.kill_switch_to_owned();
    for (t, v) in entries {
        owned.insert_with_value(t, *v);
    }

    let overlay_walk = byte_walk(&overlay);
    let owned_walk = byte_walk(&owned);

    // AUTHORITATIVE value-aware oracle: iter_with_values() under the overlay. byte
    // yields `(Vec<u8>, Option<V>)`; a present entry always has `Some(v)`.
    let oracle: BTreeMap<Vec<u8>, WalkEntry<i64>> = overlay
        .iter_with_values()
        .map(|(t, v)| (t, (true, v)))
        .collect();
    assert_eq!(
        overlay_walk, oracle,
        "{name}: byte valued overlay walk != iter_with_values() oracle"
    );

    // overlay walk's term-set ⊇ owned-twin walk's term-set. The byte owned
    // `DictionaryNode` walk is pre-existingly deficient in BOTH structure (drops
    // bucket-stored multi-byte terms) AND value (its `value()` returns `None` for
    // owned nodes — the value codec is unavailable at that layer), so only the term
    // KEYS are comparable here; the authoritative value check is against
    // `iter_with_values()` above. See the module doc.
    let overlay_keys: std::collections::BTreeSet<&Vec<u8>> = overlay_walk.keys().collect();
    let owned_keys: std::collections::BTreeSet<&Vec<u8>> = owned_walk.keys().collect();
    assert!(
        owned_keys.is_subset(&overlay_keys),
        "{name}: byte valued overlay walk must be a superset of the (deficient) owned walk"
    );

    // Spot-check `transition` + `value()` for each entry.
    let root = overlay.root();
    for (t, v) in entries {
        let node = descend(&root, t.as_bytes())
            .unwrap_or_else(|| panic!("{name}: byte valued descent lost {t:?}"));
        assert!(node.is_final(), "{name}: byte valued {t:?} not final");
        // Call the `MappedDictionaryNode` trait method explicitly (the inherent
        // byte `value()` returns `Option<&V>` and would shadow it).
        assert_eq!(
            MappedDictionaryNode::value(&node),
            Some(*v),
            "{name}: byte valued {t:?} value mismatch via transition"
        );
    }
}

// ===========================================================================
// Char variant
// ===========================================================================

fn char_walk<V>(trie: &PersistentARTrieChar<V>) -> BTreeMap<Vec<u8>, WalkEntry<V>>
where
    V: libdictenstein::value::DictionaryValue + Clone,
{
    let mut out = BTreeMap::new();
    let mut prefix: Vec<char> = Vec::new();
    // char edge labels are `char`s; the canonical term key is the UTF-8 bytes of the
    // collected chars (so byte/char keys are comparable shapes).
    collect_walk(
        &trie.root(),
        &mut prefix,
        &(|p: &[char]| p.iter().collect::<String>().into_bytes()),
        &mut out,
    );
    out
}

fn char_membership_case(name: &str, terms: &[&str]) {
    let dir = scratch(name);

    let overlay_path = dir.path().join("overlay.artc");
    let mut overlay = PersistentARTrieChar::<()>::create(&overlay_path).expect("create overlay");
    assert!(
        overlay.route_overlay(),
        "{name}: char () trie must be overlay-routed by default"
    );
    for t in terms {
        overlay.insert(t).expect("insert overlay");
    }

    let owned_path = dir.path().join("owned.artc");
    let mut owned = PersistentARTrieChar::<()>::create(&owned_path).expect("create owned");
    owned.kill_switch_to_owned();
    assert!(!owned.route_overlay(), "{name}: char owned twin must NOT route overlay");
    for t in terms {
        owned.insert(t).expect("insert owned");
    }

    let overlay_walk = char_walk(&overlay);
    let owned_walk = char_walk(&owned);
    assert_eq!(
        overlay_walk, owned_walk,
        "{name}: char overlay walk != owned walk"
    );

    // iter() oracle (term-set; membership value() is None).
    let oracle_keys: std::collections::BTreeSet<Vec<u8>> =
        overlay.iter().map(|t| t.into_bytes()).collect();
    let overlay_keys: std::collections::BTreeSet<Vec<u8>> = overlay_walk.keys().cloned().collect();
    assert_eq!(
        overlay_keys, oracle_keys,
        "{name}: char overlay walk term-set != iter() oracle"
    );

    // `transition` descent (char-by-char) reaches a final node for every term.
    let root = overlay.root();
    for t in terms {
        let units: Vec<char> = t.chars().collect();
        let node = descend(&root, &units)
            .unwrap_or_else(|| panic!("{name}: char transition descent lost term {t:?}"));
        assert!(node.is_final(), "{name}: char term {t:?} terminal not final");
    }
    // An absent edge yields no transition.
    assert!(
        root.transition('\u{10FFFF}').is_none()
            || terms.iter().any(|t| t.starts_with('\u{10FFFF}')),
        "{name}: char spurious transition for an absent edge"
    );
}

fn char_valued_case(name: &str, entries: &[(&str, u64)]) {
    let dir = scratch(name);

    let overlay_path = dir.path().join("overlay.artc");
    let mut overlay = PersistentARTrieChar::<u64>::create(&overlay_path).expect("create overlay");
    assert!(
        overlay.route_overlay(),
        "{name}: char u64 trie must be overlay-routed by default"
    );
    for (t, v) in entries {
        overlay.insert_with_value(t, *v).expect("insert overlay");
    }

    let owned_path = dir.path().join("owned.artc");
    let mut owned = PersistentARTrieChar::<u64>::create(&owned_path).expect("create owned");
    owned.kill_switch_to_owned();
    for (t, v) in entries {
        owned.insert_with_value(t, *v).expect("insert owned");
    }

    let overlay_walk = char_walk(&overlay);
    let owned_walk = char_walk(&owned);
    assert_eq!(
        overlay_walk, owned_walk,
        "{name}: char valued overlay walk != owned walk"
    );

    let oracle: BTreeMap<Vec<u8>, WalkEntry<u64>> = overlay
        .iter_with_values()
        .map(|(t, v)| (t.into_bytes(), (true, Some(v))))
        .collect();
    assert_eq!(
        overlay_walk, oracle,
        "{name}: char valued overlay walk != iter_with_values() oracle"
    );

    let root = overlay.root();
    for (t, v) in entries {
        let units: Vec<char> = t.chars().collect();
        let node = descend(&root, &units)
            .unwrap_or_else(|| panic!("{name}: char valued descent lost {t:?}"));
        assert!(node.is_final(), "{name}: char valued {t:?} not final");
        assert_eq!(
            node.value(),
            Some(*v),
            "{name}: char valued {t:?} value mismatch via transition"
        );
    }
}

// ===========================================================================
// Fixtures — varied fan-out, multi-level, shared spines, empty string.
// ===========================================================================

/// Terms covering: a single char, a wide root fan-out (>4 forces the overlay Heap
/// tier), deep shared spines, and proper-prefix terms (cat ⊂ cats ⊂ ...).
const MEMBERSHIP_TERMS: &[&str] = &[
    "a", "ab", "abc", "abd", "abe", "b", "ban", "banana", "bandana", "cat", "cats", "cathedral",
    "d", "do", "dog", "dot", "z", "zoo", "zebra", "m", "n", "o", "p", "q", "r", "s", "t", "u",
];

#[test]
fn byte_overlay_walk_equals_owned_membership() {
    byte_membership_case("byte_membership", MEMBERSHIP_TERMS);
}

#[test]
fn byte_overlay_walk_equals_owned_valued() {
    let entries: Vec<(&str, i64)> = MEMBERSHIP_TERMS
        .iter()
        .enumerate()
        .map(|(i, t)| (*t, (i as i64) * 7 - 11)) // mix of negative + positive
        .collect();
    byte_valued_case("byte_valued", &entries);
}

#[test]
fn byte_overlay_walk_with_empty_string_final() {
    // Empty string is a first-class final (carries its value on the root).
    let dir = scratch("byte_empty");
    let path = dir.path().join("overlay.art");
    let mut overlay = PersistentARTrie::<i64>::create(&path).expect("create");
    overlay.insert_with_value("", 42);
    overlay.insert_with_value("x", 1);
    overlay.insert_with_value("xy", 2);

    let owned_path = dir.path().join("owned.art");
    let mut owned = PersistentARTrie::<i64>::create(&owned_path).expect("create owned");
    owned.kill_switch_to_owned();
    owned.insert_with_value("", 42);
    owned.insert_with_value("x", 1);
    owned.insert_with_value("xy", 2);

    let overlay_walk = byte_walk(&overlay);
    // Authoritative: overlay walk == iter_with_values() (the owned byte walk is
    // pre-existingly incomplete; see module doc).
    let oracle: BTreeMap<Vec<u8>, WalkEntry<i64>> = overlay
        .iter_with_values()
        .map(|(t, v)| (t, (true, v)))
        .collect();
    assert_eq!(
        overlay_walk, oracle,
        "byte empty-string overlay walk != iter_with_values() oracle"
    );
    // Owned twin still has the same data via its (complete) iter_with_values().
    let owned_oracle: BTreeMap<Vec<u8>, WalkEntry<i64>> = owned
        .iter_with_values()
        .map(|(t, v)| (t, (true, v)))
        .collect();
    assert_eq!(oracle, owned_oracle, "byte empty-string overlay vs owned data");

    // The root itself is final and carries the empty-term value.
    let root = overlay.root();
    assert!(root.is_final(), "byte root must be final ('' present)");
    assert_eq!(
        MappedDictionaryNode::value(&root),
        Some(42),
        "byte empty-term value via root()"
    );
    assert!(
        overlay_walk.contains_key(&Vec::<u8>::new()),
        "byte empty term missing from overlay walk"
    );
}

#[test]
fn char_overlay_walk_equals_owned_membership() {
    char_membership_case("char_membership", MEMBERSHIP_TERMS);
}

#[test]
fn char_overlay_walk_equals_owned_membership_unicode() {
    // Multi-byte UTF-8 + beyond-BMP scalars + shared Unicode spines.
    let terms = &[
        "café", "caffeine", "日本", "日本語", "日記", "emoji😀", "emoji😁", "naïve", "résumé",
        "Ωmega", "Ωmicron", "a", "ab",
    ];
    char_membership_case("char_unicode", terms);
}

#[test]
fn char_overlay_walk_equals_owned_valued() {
    let entries: Vec<(&str, u64)> = [
        "café", "caffeine", "日本", "日本語", "emoji😀", "receive", "recipe", "recital",
    ]
    .iter()
    .enumerate()
    .map(|(i, t)| (*t, (i as u64 + 1) * 1000))
    .collect();
    char_valued_case("char_valued_unicode", &entries);
}

#[test]
fn char_overlay_walk_with_empty_string_final() {
    let dir = scratch("char_empty");
    let path = dir.path().join("overlay.artc");
    let mut overlay = PersistentARTrieChar::<u64>::create(&path).expect("create");
    overlay.insert_with_value("", 7).expect("insert");
    overlay.insert_with_value("日", 1).expect("insert");
    overlay.insert_with_value("日本", 2).expect("insert");

    let owned_path = dir.path().join("owned.artc");
    let mut owned = PersistentARTrieChar::<u64>::create(&owned_path).expect("create owned");
    owned.kill_switch_to_owned();
    owned.insert_with_value("", 7).expect("insert");
    owned.insert_with_value("日", 1).expect("insert");
    owned.insert_with_value("日本", 2).expect("insert");

    let overlay_walk = char_walk(&overlay);
    let owned_walk = char_walk(&owned);
    assert_eq!(overlay_walk, owned_walk, "char empty-string overlay != owned");

    let root = overlay.root();
    assert!(root.is_final(), "char root must be final ('' present)");
    assert_eq!(root.value(), Some(7), "char empty-term value via root()");
    assert!(
        overlay_walk.contains_key(&Vec::<u8>::new()),
        "char empty term missing from overlay walk"
    );
}

#[test]
fn byte_overlay_walk_empty_dictionary() {
    // An overlay-routed trie with NO terms: root() must be a valid, non-final,
    // childless node (an empty dictionary), not a panic / spurious final.
    let dir = scratch("byte_empty_dict");
    let path = dir.path().join("overlay.art");
    let overlay = PersistentARTrie::<()>::create(&path).expect("create");
    assert!(overlay.route_overlay());
    let root = overlay.root();
    assert!(!root.is_final(), "empty byte dict root must not be final");
    assert_eq!(root.edges().count(), 0, "empty byte dict root must have no edges");
    assert!(byte_walk(&overlay).is_empty(), "empty byte dict walk must be empty");
}

#[test]
fn char_overlay_walk_empty_dictionary() {
    let dir = scratch("char_empty_dict");
    let path = dir.path().join("overlay.artc");
    let overlay = PersistentARTrieChar::<()>::create(&path).expect("create");
    assert!(overlay.route_overlay());
    let root = overlay.root();
    assert!(!root.is_final(), "empty char dict root must not be final");
    assert_eq!(root.edges().count(), 0, "empty char dict root must have no edges");
    assert!(char_walk(&overlay).is_empty(), "empty char dict walk must be empty");
}

#[test]
fn edge_count_matches_edges_len_byte() {
    // `edge_count()` must agree with the number of `edges()` for the overlay arm.
    let dir = scratch("byte_edge_count");
    let path = dir.path().join("overlay.art");
    let mut overlay = PersistentARTrie::<()>::create(&path).expect("create");
    for t in MEMBERSHIP_TERMS {
        overlay.insert(t);
    }
    fn check(node: &impl DictionaryNode) {
        let edges: Vec<_> = node.edges().collect();
        assert_eq!(
            node.edge_count(),
            Some(edges.len()),
            "edge_count != edges().len() (byte overlay)"
        );
        for (_, child) in edges {
            check(&child);
        }
    }
    check(&overlay.root());
}

#[test]
fn edge_count_matches_edges_len_char() {
    let dir = scratch("char_edge_count");
    let path = dir.path().join("overlay.artc");
    let mut overlay = PersistentARTrieChar::<()>::create(&path).expect("create");
    for t in &["café", "caffeine", "日本", "日本語", "a", "ab", "abc"] {
        overlay.insert(t).expect("insert");
    }
    fn check(node: &impl DictionaryNode) {
        let edges: Vec<_> = node.edges().collect();
        assert_eq!(
            node.edge_count(),
            Some(edges.len()),
            "edge_count != edges().len() (char overlay)"
        );
        for (_, child) in edges {
            check(&child);
        }
    }
    check(&overlay.root());
}
