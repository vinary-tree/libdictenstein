//! Correspondence checks for persistent trie cursor pagination and merge paths.
//!
//! These tests connect `PersistentMergeSpec.v` to the Rust implementation:
//! cursor pagination must enumerate the reference source map without gaps or
//! duplicates, and each public merge variant must refine the same reference
//! `BTreeMap` merge.

#![cfg(feature = "persistent-artrie")]
#![allow(deprecated)]

use libdictenstein::persistent_artrie::PersistentARTrie;
#[cfg(feature = "parallel-merge")]
use libdictenstein::persistent_artrie::{SharedARTrie, SharedARTrieParallelExt};
#[cfg(feature = "parallel-merge")]
use libdictenstein::ARTrie;
use libdictenstein::{Dictionary, MappedDictionary};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(feature = "parallel-merge")]
use std::path::Path;
#[cfg(feature = "parallel-merge")]
use tempfile::TempDir;

fn map_from(entries: &[(&str, i32)]) -> BTreeMap<String, i32> {
    entries
        .iter()
        .map(|(term, value)| ((*term).to_string(), *value))
        .collect()
}

fn build_trie(entries: &BTreeMap<String, i32>) -> PersistentARTrie<i32> {
    let mut trie = PersistentARTrie::new();
    for (term, value) in entries {
        trie.insert_with_value(term, *value);
    }
    trie
}

fn reference_merge<F>(
    target: &BTreeMap<String, i32>,
    source: &BTreeMap<String, i32>,
    merge: F,
) -> BTreeMap<String, i32>
where
    F: Fn(i32, i32) -> i32,
{
    let mut expected = target.clone();
    for (term, source_value) in source {
        expected
            .entry(term.clone())
            .and_modify(|target_value| *target_value = merge(*target_value, *source_value))
            .or_insert(*source_value);
    }
    expected
}

fn assert_trie_matches_reference(
    trie: &PersistentARTrie<i32>,
    expected: &BTreeMap<String, i32>,
    observed_terms: &BTreeSet<String>,
) {
    assert_eq!(trie.len(), Some(expected.len()));

    for term in observed_terms {
        assert_eq!(
            trie.get_value(term),
            expected.get(term).copied(),
            "value mismatch for {term}"
        );
    }
}

fn all_terms(left: &BTreeMap<String, i32>, right: &BTreeMap<String, i32>) -> BTreeSet<String> {
    left.keys().chain(right.keys()).cloned().collect()
}

#[test]
fn cursor_pagination_refines_prefix_reference_for_all_batch_sizes() {
    let source = map_from(&[
        ("alpha", 1),
        ("alphabet", 2),
        ("alpine", 3),
        ("banana", 4),
        ("band", 5),
        ("bandana", 6),
        ("cab", 7),
        ("zebra", 8),
    ]);
    let trie = build_trie(&source);

    for (prefix, limit) in [
        (b"".as_slice(), 1usize),
        (b"".as_slice(), 3),
        (b"ban".as_slice(), 1),
        (b"ba".as_slice(), 2),
        (b"zz".as_slice(), 2),
    ] {
        let mut cursor: Option<Vec<u8>> = None;
        let mut collected = Vec::new();
        let mut seen = BTreeSet::new();
        let mut iterations = 0usize;

        loop {
            iterations += 1;
            assert!(
                iterations <= source.len() + 2,
                "cursor pagination did not converge for prefix {:?}",
                String::from_utf8_lossy(prefix)
            );

            let batch = trie
                .iter_prefix_from_cursor(prefix, cursor.as_deref(), limit)
                .expect("cursor page");

            assert!(batch.len() <= limit);

            let terms: Vec<Vec<u8>> = batch.iter().map(|entry| entry.term.clone()).collect();
            assert!(
                terms.windows(2).all(|pair| pair[0] < pair[1]),
                "batch must be strictly sorted"
            );

            for term in &terms {
                assert!(term.starts_with(prefix), "term must match prefix");
                if let Some(previous) = &cursor {
                    assert!(term.as_slice() > previous.as_slice(), "cursor is exclusive");
                }
                assert!(
                    seen.insert(String::from_utf8(term.clone()).expect("test term is utf8")),
                    "term repeated across cursor pages"
                );
            }

            if let Some(last) = terms.last() {
                cursor = Some(last.clone());
                collected.extend(
                    terms
                        .into_iter()
                        .map(|term| String::from_utf8(term).expect("test term is utf8")),
                );
            }

            if batch.len() < limit {
                break;
            }
        }

        let expected: Vec<String> = source
            .keys()
            .filter(|term| term.as_bytes().starts_with(prefix))
            .cloned()
            .collect();
        assert_eq!(collected, expected);
    }
}

#[test]
fn batched_merge_matches_single_pass_reference_for_all_batch_sizes() {
    let source = map_from(&[
        ("alpha", 1),
        ("banana", 2),
        ("band", 3),
        ("delta", 4),
        ("epsilon", 5),
        ("zeta", 6),
    ]);
    let target = map_from(&[("alpha", 10), ("carrot", 20), ("delta", 30)]);
    let observed_terms = all_terms(&target, &source);
    let expected = reference_merge(&target, &source, |old, new| old * 10 + new);

    for batch_size in [0usize, 1, 2, 3, 16] {
        let source_trie = build_trie(&source);
        let mut target_trie = build_trie(&target);

        let processed = target_trie
            .merge_from_batched(&source_trie, |old, new| old * 10 + new, batch_size)
            .expect("batched merge");

        assert_eq!(processed, source.len());
        assert_trie_matches_reference(&target_trie, &expected, &observed_terms);
    }
}

#[test]
fn grouped_batched_merge_matches_ordinary_batched_merge() {
    let source = map_from(&[
        ("app", 1),
        ("apple", 2),
        ("application", 3),
        ("banana", 4),
        ("band", 5),
        ("cab", 6),
        ("can", 7),
    ]);
    let target = map_from(&[("app", 100), ("banana", 200), ("dog", 300)]);
    let observed_terms = all_terms(&target, &source);
    let expected = reference_merge(&target, &source, |old, new| old - new);

    for batch_size in [1usize, 2, 4, 32] {
        let source_trie = build_trie(&source);
        let mut ordinary = build_trie(&target);
        let mut grouped = build_trie(&target);

        let ordinary_count = ordinary
            .merge_from_batched(&source_trie, |old, new| old - new, batch_size)
            .expect("ordinary batched merge");
        let grouped_count = grouped
            .merge_from_batched_grouped(&source_trie, |old, new| old - new, batch_size)
            .expect("grouped batched merge");

        assert_eq!(ordinary_count, source.len());
        assert_eq!(grouped_count, source.len());
        assert_trie_matches_reference(&ordinary, &expected, &observed_terms);
        assert_trie_matches_reference(&grouped, &expected, &observed_terms);
    }
}

#[cfg(feature = "parallel-merge")]
fn build_shared_trie(path: &Path, entries: &BTreeMap<String, i32>) -> SharedARTrie<i32> {
    let trie = SharedARTrie::create(path).expect("create shared trie");
    {
        let mut guard = trie.write();
        for (term, value) in entries {
            guard.insert_with_value(term, *value);
        }
    }
    trie
}

#[cfg(feature = "parallel-merge")]
#[test]
fn parallel_merge_matches_single_pass_reference() {
    let source = map_from(&[
        ("alpha", 1),
        ("amber", 2),
        ("beta", 3),
        ("delta", 4),
        ("omega", 5),
    ]);
    let target = map_from(&[("alpha", 10), ("beta", 20), ("kappa", 30)]);
    let observed_terms = all_terms(&target, &source);
    let expected = reference_merge(&target, &source, |old, new| old * 100 + new);

    let temp_dir = TempDir::new().expect("temp dir");
    let source_trie = build_shared_trie(&temp_dir.path().join("source.part"), &source);
    let target_trie = build_shared_trie(&temp_dir.path().join("target.part"), &target);

    let processed = target_trie
        .merge_from_parallel(&source_trie, |old, new| old * 100 + new)
        .expect("parallel merge");

    assert_eq!(processed, source.len());
    let guard = target_trie.read();
    assert_trie_matches_reference(&guard, &expected, &observed_terms);
}
