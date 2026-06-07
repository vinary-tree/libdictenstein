//! Correspondence checks for persistent char-trie prefix operations.
//!
//! These tests connect `PersistentPrefixSpec.v` to the Rust implementation:
//! prefix iteration must enumerate the same domain as a `BTreeMap`, valued and
//! arena-aware variants must preserve those values, and batched prefix removal
//! must refine the same reference-map deletion for every batch size.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use std::collections::{BTreeMap, BTreeSet};
use tempfile::tempdir;

fn seed_entries() -> Vec<(&'static str, i32)> {
    vec![
        ("", 0),
        ("a", 1),
        ("app", 2),
        ("apple", 3),
        ("application", 4),
        ("apply", 5),
        ("banana", 6),
        ("band", 7),
        ("bandana", 8),
        ("emoji🙂", 9),
        ("emoji🙂x", 10),
        ("日本", 11),
        ("日本人", 12),
        ("日本語", 13),
        ("日曜日", 14),
        ("月曜日", 15),
    ]
}

fn reference_map() -> BTreeMap<String, i32> {
    let mut map = BTreeMap::new();
    for (term, value) in seed_entries() {
        map.insert(term.to_string(), value);
    }
    map.insert("app".to_string(), 22);
    map.insert("日本".to_string(), 111);
    map
}

fn build_trie(reference: &BTreeMap<String, i32>) -> PersistentARTrieChar<i32> {
    let trie = PersistentARTrieChar::new();
    for (term, value) in reference {
        trie.insert_with_value(term, *value)
            .expect("insert value into char trie");
    }
    trie
}

fn expected_prefix(reference: &BTreeMap<String, i32>, prefix: &str) -> BTreeMap<String, i32> {
    reference
        .iter()
        .filter(|(term, _)| term.starts_with(prefix))
        .map(|(term, value)| (term.clone(), *value))
        .collect()
}

fn term_set(terms: Option<Vec<String>>) -> BTreeSet<String> {
    terms.unwrap_or_default().into_iter().collect()
}

fn value_map(entries: Option<Vec<(String, i32)>>) -> BTreeMap<String, i32> {
    entries.unwrap_or_default().into_iter().collect()
}

fn assert_prefix_views(
    trie: &PersistentARTrieChar<i32>,
    reference: &BTreeMap<String, i32>,
    prefix: &str,
) {
    let expected = expected_prefix(reference, prefix);
    let expected_terms: BTreeSet<String> = expected.keys().cloned().collect();

    let terms = trie.iter_prefix(prefix).expect("iter_prefix");
    assert_eq!(term_set(terms), expected_terms, "term view for {prefix:?}");

    let values = trie
        .iter_prefix_with_values(prefix)
        .expect("iter_prefix_with_values");
    assert_eq!(value_map(values), expected, "value view for {prefix:?}");

    let arena_terms = trie
        .iter_prefix_with_arena(prefix)
        .expect("iter_prefix_with_arena")
        .unwrap_or_default();
    let arena_term_set: BTreeSet<String> =
        arena_terms.iter().map(|entry| entry.term.clone()).collect();
    assert_eq!(
        arena_term_set, expected_terms,
        "arena term view for {prefix:?}"
    );
    assert!(
        arena_terms
            .iter()
            .all(|entry| entry.term.starts_with(prefix)),
        "arena terms must not escape the requested prefix"
    );

    let arena_values = trie
        .iter_prefix_with_values_and_arena(prefix)
        .expect("iter_prefix_with_values_and_arena")
        .unwrap_or_default();
    let arena_value_map: BTreeMap<String, i32> = arena_values
        .iter()
        .map(|entry| (entry.term.clone(), entry.value))
        .collect();
    assert_eq!(arena_value_map, expected, "arena value view for {prefix:?}");
    assert!(
        arena_values
            .iter()
            .all(|entry| entry.term.starts_with(prefix)),
        "arena value terms must not escape the requested prefix"
    );
}

fn assert_full_map(trie: &PersistentARTrieChar<i32>, reference: &BTreeMap<String, i32>) {
    assert_eq!(trie.len(), reference.len());
    assert_prefix_views(trie, reference, "");

    for (term, value) in reference {
        assert!(trie.contains(term), "missing term {term:?}");
        // F2-migrate: Bucket A — `get()` returns None under the overlay; read via `get_value`.
        assert_eq!(trie.get_value(term), Some(*value), "value for {term:?}");
    }
}

#[test]
fn prefix_iteration_refines_btree_map_for_ascii_unicode_and_empty_prefix() {
    let reference = reference_map();
    let trie = build_trie(&reference);

    for prefix in [
        "",
        "a",
        "app",
        "apple",
        "ban",
        "emoji🙂",
        "日本",
        "日",
        "月",
        "missing",
    ] {
        assert_prefix_views(&trie, &reference, prefix);
    }
}

#[test]
fn remove_prefix_batched_matches_reference_deletion_for_all_batch_sizes() {
    let original = reference_map();

    for prefix in ["", "a", "app", "ban", "emoji🙂", "日", "日本", "missing"] {
        for batch_size in [0usize, 1, 2, 3, 8, 1024] {
            let mut reference = original.clone();
            let expected_removed = reference
                .keys()
                .filter(|term| term.starts_with(prefix))
                .count();
            let trie = build_trie(&reference);

            let removed = trie
                .remove_prefix_batched(prefix, batch_size)
                .expect("remove_prefix_batched");
            reference.retain(|term, _| !term.starts_with(prefix));

            assert_eq!(
                removed, expected_removed,
                "removed count for prefix {prefix:?}, batch {batch_size}"
            );
            assert_full_map(&trie, &reference);
            assert_prefix_views(&trie, &reference, prefix);

            let removed_again = trie
                .remove_prefix_batched(prefix, batch_size)
                .expect("idempotent remove_prefix_batched");
            assert_eq!(
                removed_again, 0,
                "second removal must be idempotent for prefix {prefix:?}, batch {batch_size}"
            );
            assert_full_map(&trie, &reference);
        }
    }
}

#[test]
fn remove_prefix_default_matches_reference_deletion() {
    let original = reference_map();

    for prefix in ["", "app", "日本", "missing"] {
        let mut reference = original.clone();
        let expected_removed = reference
            .keys()
            .filter(|term| term.starts_with(prefix))
            .count();
        let trie = build_trie(&reference);

        let removed = trie.remove_prefix(prefix).expect("remove_prefix");
        reference.retain(|term, _| !term.starts_with(prefix));

        assert_eq!(removed, expected_removed, "removed count for {prefix:?}");
        assert_full_map(&trie, &reference);
    }
}

#[test]
fn disk_backed_prefix_semantics_survive_sync_reopen_and_deletion() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("char_prefix.artrie");
    let mut reference = reference_map();

    {
        let trie = PersistentARTrieChar::<i32>::create(&path).expect("create trie");
        for (term, value) in &reference {
            trie.upsert(term, *value).expect("upsert value");
        }
        trie.sync().expect("initial sync");
    }

    {
        let trie = PersistentARTrieChar::<i32>::open(&path).expect("open trie");
        for prefix in ["", "app", "emoji🙂", "日本", "missing"] {
            assert_prefix_views(&trie, &reference, prefix);
        }

        let removed = trie
            .remove_prefix_batched("日本", 1)
            .expect("remove unicode prefix");
        let expected_removed = reference
            .keys()
            .filter(|term| term.starts_with("日本"))
            .count();
        reference.retain(|term, _| !term.starts_with("日本"));
        assert_eq!(removed, expected_removed);
        assert_full_map(&trie, &reference);
        trie.sync().expect("post-delete sync");
    }

    let reopened = PersistentARTrieChar::<i32>::open(&path).expect("reopen trie");
    assert_full_map(&reopened, &reference);
    assert_prefix_views(&reopened, &reference, "日本");
}
