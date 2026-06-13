//! Correspondence checks for native persistent suffix-tree-compatible indexes.
//!
//! These tests pin the suffix-tree surface to suffix-automaton substring
//! semantics while also checking that storage is a native compact suffix-tree
//! graph with its own persistence and shared-reference concurrency contract.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{
    PersistentSuffixAutomaton, PersistentSuffixTree, PersistentSuffixTreeChar,
    PersistentSuffixTreeCharNode, PersistentSuffixTreeNode, RecoveryMode,
};
use libdictenstein::{Dictionary, MappedDictionary, SubstringDictionary, SyncStrategy};
use proptest::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::tempdir;

fn segment_dir(path: &Path, extension: &str) -> PathBuf {
    let mut dir = path.to_path_buf();
    dir.set_extension(extension);
    dir
}

fn count_segment_wal_files(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    fs::read_dir(dir)
        .expect("read segment dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "wal"))
        .count()
}

fn sorted_locations(mut locations: Vec<(String, usize)>) -> Vec<(String, usize)> {
    locations.sort();
    locations
}

fn expected_byte_locations(
    texts: &[String],
    pattern: &str,
    positions: &[(usize, usize)],
) -> Vec<(String, usize)> {
    if pattern.is_empty() {
        return texts.iter().cloned().map(|text| (text, 0)).collect();
    }

    let mut locations = Vec::new();
    for &(source_id, finish_byte) in positions {
        let Some(text) = texts.get(source_id) else {
            continue;
        };
        let Some(start) = finish_byte.checked_sub(pattern.len()) else {
            continue;
        };
        locations.push((text.clone(), start));
    }
    locations
}

fn assert_suffix_tree_matches_automaton(texts: Vec<String>, probes: Vec<String>) {
    let automaton = PersistentSuffixAutomaton::<()>::from_texts(texts.iter().map(String::as_str));
    let tree = PersistentSuffixTree::<()>::from_texts(texts.iter().map(String::as_str));

    assert_eq!(tree.len(), automaton.len());
    assert_eq!(tree.string_count(), automaton.string_count());
    assert_eq!(tree.source_texts(), automaton.source_texts());
    assert_eq!(tree.sync_strategy(), SyncStrategy::InternalSync);
    assert!(tree.is_suffix_based());

    for probe in probes {
        let positions = automaton.match_positions(&probe);
        let expected_locations = expected_byte_locations(&texts, &probe, &positions);

        assert_eq!(
            tree.contains(&probe),
            automaton.contains(&probe),
            "contains mismatch for {probe:?}"
        );
        assert_eq!(
            tree.contains_substring(&probe),
            automaton.contains(&probe),
            "substring contains mismatch for {probe:?}"
        );
        assert_eq!(
            tree.match_positions(&probe),
            positions,
            "match position mismatch for {probe:?}"
        );
        assert_eq!(
            sorted_locations(tree.locations(&probe)),
            sorted_locations(expected_locations.clone()),
            "location mismatch for {probe:?}"
        );
        assert_eq!(
            sorted_locations(
                tree.find_exact_substring(&probe)
                    .into_iter()
                    .map(|m| (m.term, m.position))
                    .collect()
            ),
            sorted_locations(expected_locations),
            "substring match mismatch for {probe:?}"
        );

        if let Some(handle) = tree.find(&probe) {
            assert_eq!(tree.freq_at(&handle), tree.freq(&probe));
            assert_eq!(
                sorted_locations(tree.locations_at(&handle, probe.len())),
                sorted_locations(tree.locations(&probe)),
                "node-location mismatch for {probe:?}"
            );
        } else {
            assert!(!tree.contains_substring(&probe));
        }
    }
}

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn suffix_tree_types_are_send_sync() {
    assert_send_sync::<PersistentSuffixTree<i32>>();
    assert_send_sync::<PersistentSuffixTreeChar<i32>>();
    assert_send_sync::<PersistentSuffixTreeNode<i32>>();
    assert_send_sync::<PersistentSuffixTreeCharNode<i32>>();
}

#[test]
fn byte_suffix_tree_uses_native_path_compressed_graph_shape() {
    let texts = ["banana", "bandana", "ananas"];
    let tree = PersistentSuffixTree::<()>::from_texts(texts);
    let explicit_suffix_trie_nodes = 1 + texts
        .iter()
        .map(|text| text.len() * (text.len() + 1) / 2)
        .sum::<usize>();

    assert!(tree.graph_node_count() > 1);
    assert!(tree.graph_edge_count() > 0);
    assert!(
        tree.graph_node_count() < explicit_suffix_trie_nodes,
        "graph_node_count={} explicit_suffix_trie_nodes={explicit_suffix_trie_nodes}",
        tree.graph_node_count()
    );
    assert!(tree.contains("band"));
    assert_eq!(
        sorted_locations(tree.locations("ana")),
        vec![
            ("ananas".to_string(), 0),
            ("ananas".to_string(), 2),
            ("banana".to_string(), 1),
            ("banana".to_string(), 3),
            ("bandana".to_string(), 4),
        ]
    );
}

#[test]
fn byte_suffix_tree_matches_native_suffix_automaton_positions() {
    assert_suffix_tree_matches_automaton(
        vec![
            "banana".to_string(),
            "bandana".to_string(),
            "abracadabra".to_string(),
        ],
        vec![
            "".to_string(),
            "banana".to_string(),
            "ana".to_string(),
            "dana".to_string(),
            "abra".to_string(),
            "cad".to_string(),
            "xyz".to_string(),
        ],
    );
}

#[test]
fn byte_suffix_tree_replays_native_wal_without_automaton_snapshot() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_suffix_tree_wal.pstree");

    {
        let tree = PersistentSuffixTree::<i32>::create(&path).expect("create suffix tree");
        assert!(tree.insert_with_value("banana", 7));
        assert!(tree.insert("bandana"));
        assert!(tree.update_or_insert("ana", 3, |value| *value += 1));
        assert!(tree.remove("banana"));
    }

    let (reopened, report) =
        PersistentSuffixTree::<i32>::open_with_recovery(&path).expect("recover suffix tree");
    assert!(matches!(report.mode, RecoveryMode::RebuildFromWal));
    assert_eq!(report.records_replayed, 8);
    assert_eq!(reopened.string_count(), 1);
    assert_eq!(reopened.active_texts(), vec!["bandana".to_string()]);
    assert!(reopened.contains("dana"));
    assert!(!reopened.contains("banana"));
    assert_eq!(reopened.get_value("ana"), Some(3));
    assert_eq!(reopened.locations("ana"), vec![("bandana".to_string(), 4)]);
}

#[test]
fn byte_suffix_tree_segment_wal_prunes_checkpointed_records_and_replays_tail() {
    let dir = tempdir().expect("temp dir");
    let path = dir
        .path()
        .join("persistent_suffix_tree_segment_tail.pstree");
    let segments = segment_dir(&path, "streewal.d");

    {
        let tree = PersistentSuffixTree::<i32>::create(&path).expect("create suffix tree");
        assert!(tree.insert_with_value("alpha-tree", 1));
        assert!(tree.insert_with_value("beta-tree", 2));
        assert!(
            count_segment_wal_files(&segments) >= 4,
            "prepare/commit segment files should exist before checkpoint"
        );

        tree.checkpoint()
            .expect("checkpoint prunes covered segments");
        assert_eq!(
            count_segment_wal_files(&segments),
            0,
            "checkpointed suffix-tree WAL segments should be pruned"
        );

        assert!(tree.insert_with_value("gamma-tree", 3));
        assert!(
            count_segment_wal_files(&segments) >= 2,
            "post-checkpoint tail should remain for replay"
        );
    }

    let reopened = PersistentSuffixTree::<i32>::open(&path).expect("reopen segment tail");
    assert_eq!(reopened.get_value("alpha-tree"), Some(1));
    assert_eq!(reopened.get_value("beta-tree"), Some(2));
    assert_eq!(reopened.get_value("gamma-tree"), Some(3));
    assert!(reopened.contains("tree"));
    assert!(reopened.contains_substring("gamma"));
}

#[test]
fn char_suffix_tree_preserves_unicode_character_locations() {
    let tree = PersistentSuffixTreeChar::<()>::from_texts(["café 日本 café", "naïve café"]);

    assert!(tree.contains("fé 日"));
    assert!(tree.contains_substring("日本"));
    assert!(!tree.contains("missing"));
    assert_eq!(tree.sync_strategy(), SyncStrategy::InternalSync);
    assert!(tree.is_suffix_based());

    assert_eq!(
        sorted_locations(tree.locations("fé")),
        vec![
            ("café 日本 café".to_string(), 2),
            ("café 日本 café".to_string(), 10),
            ("naïve café".to_string(), 8),
        ]
    );
    assert_eq!(
        tree.locations("日本"),
        vec![("café 日本 café".to_string(), 5)]
    );

    let handle = tree.find("fé").expect("find fé");
    assert_eq!(tree.freq_at(&handle), 3);
    assert_eq!(
        sorted_locations(tree.locations_at(&handle, 2)),
        sorted_locations(tree.locations("fé"))
    );

    let matches = tree.find_exact_substring("日本");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].term, "café 日本 café");
    assert_eq!(matches[0].position, 5);
    assert_eq!(matches[0].length, 2);
}

#[test]
fn byte_values_removal_compaction_and_reopen() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_suffix_tree_byte.art");

    {
        let tree = PersistentSuffixTree::<i32>::create(&path).expect("create suffix tree");
        assert!(tree.insert("banana"));
        assert!(tree.insert("bandana"));
        assert!(tree.contains("ana"));
        assert_eq!(
            sorted_locations(tree.locations("ana")),
            vec![
                ("banana".to_string(), 1),
                ("banana".to_string(), 3),
                ("bandana".to_string(), 4),
            ]
        );

        assert!(tree.update_or_insert("ana", 3, |value| *value += 1));
        assert_eq!(tree.get_value("ana"), Some(3));
        assert!(!tree.update_or_insert("ana", 3, |value| *value += 10));
        assert_eq!(tree.get_value("ana"), Some(13));

        let handle = tree.find("ana").expect("find ana");
        assert_eq!(tree.freq_at(&handle), 3);
        assert_eq!(
            sorted_locations(tree.locations_at(&handle, 3)),
            sorted_locations(tree.locations("ana"))
        );

        assert!(tree.remove("banana"));
        assert!(tree.needs_compaction());
        assert_eq!(tree.string_count(), 1);
        assert_eq!(tree.get_value("banana"), None);
        assert_eq!(tree.locations("ana"), vec![("bandana".to_string(), 4)]);
        tree.compact();
        assert!(!tree.needs_compaction());
        tree.checkpoint().expect("checkpoint suffix tree");
        tree.close();
    }

    let reopened = PersistentSuffixTree::<i32>::open(&path).expect("reopen suffix tree");
    assert_eq!(reopened.string_count(), 1);
    assert!(reopened.contains("dana"));
    assert!(!reopened.contains("banana"));
    assert_eq!(reopened.get_value("ana"), Some(13));
    assert_eq!(reopened.active_texts(), vec!["bandana".to_string()]);
}

#[test]
fn byte_parallel_readers_writers_and_checkpoint_reopen() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_suffix_tree_concurrent.art");
    let tree = Arc::new(PersistentSuffixTree::<i32>::create(&path).expect("create suffix tree"));
    let done = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(5));

    let mut handles = Vec::new();
    for writer in 0..2 {
        let tree = Arc::clone(&tree);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for idx in 0..12 {
                let term = format!("writer-{writer}-term-{idx}-suffix-tree");
                assert!(tree.insert_with_value(&term, writer * 100 + idx));
                assert!(tree.contains("suffix"));
            }
        }));
    }

    for _ in 0..2 {
        let tree = Arc::clone(&tree);
        let done = Arc::clone(&done);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            while !done.load(Ordering::Acquire) {
                assert!(tree.contains(""));
                let _ = tree.contains("term");
                let _ = tree.match_positions("suffix");
                let _ = tree.find_exact_substring("tree");
                thread::yield_now();
            }
        }));
    }

    {
        let tree = Arc::clone(&tree);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..6 {
                tree.checkpoint().expect("concurrent checkpoint");
                thread::yield_now();
            }
        }));
    }

    for handle in handles.drain(..2) {
        handle.join().expect("writer thread");
    }
    done.store(true, Ordering::Release);
    for handle in handles {
        handle.join().expect("reader/checkpoint thread");
    }

    tree.checkpoint().expect("final checkpoint");
    tree.close();
    drop(tree);

    let reopened = PersistentSuffixTree::<i32>::open(&path).expect("reopen suffix tree");
    assert_eq!(reopened.string_count(), 24);
    assert!(reopened.contains("suffix-tree"));
    assert!(reopened.contains("writer-1-term-11"));
    assert_eq!(
        reopened.get_value("writer-1-term-11-suffix-tree"),
        Some(111)
    );
}

#[test]
fn byte_suffix_tree_concurrent_update_or_insert_retries_without_lost_increments() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_suffix_tree_update.art");
    let tree = Arc::new(PersistentSuffixTree::<i32>::create(&path).expect("create suffix tree"));
    assert!(tree.insert_with_value("counter", 0));

    const WRITERS: usize = 6;
    const INCREMENTS: usize = 32;
    let barrier = Arc::new(Barrier::new(WRITERS));
    let mut handles = Vec::new();
    for _ in 0..WRITERS {
        let tree = Arc::clone(&tree);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..INCREMENTS {
                assert!(!tree.update_or_insert("counter", 0, |value| *value += 1));
            }
        }));
    }
    for handle in handles {
        handle.join().expect("update thread");
    }

    let expected = (WRITERS * INCREMENTS) as i32;
    assert_eq!(tree.get_value("counter"), Some(expected));
    tree.checkpoint().expect("checkpoint counter");
    tree.close();
    drop(tree);

    let reopened = PersistentSuffixTree::<i32>::open(&path).expect("reopen suffix tree");
    assert_eq!(reopened.get_value("counter"), Some(expected));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn byte_suffix_tree_property_parity_with_native_suffix_automaton(
        texts in prop::collection::vec("[abc]{0,5}", 0..8),
        probes in prop::collection::vec("[abc]{0,4}", 0..8),
    ) {
        assert_suffix_tree_matches_automaton(texts, probes);
    }
}
