#![cfg(feature = "persistent-artrie")]

use libdictenstein::dynamic_dawg::DynamicDawgU64;
use libdictenstein::persistent_artrie::PersistentARTrieU64;
use libdictenstein::{CharUnit, Dictionary, DictionaryNode, MappedDictionaryNode, SyncStrategy};
use proptest::prelude::*;
use tempfile::tempdir;

fn sorted_sequences(mut sequences: Vec<Vec<u64>>) -> Vec<Vec<u64>> {
    sequences.sort();
    sequences
}

fn assert_sequence_parity(sequences: Vec<Vec<u64>>, probes: Vec<Vec<u64>>) {
    let volatile = DynamicDawgU64::<()>::new();
    let persistent = PersistentARTrieU64::<()>::new();

    for sequence in &sequences {
        assert_eq!(
            persistent.insert_sequence(sequence),
            volatile.insert_sequence(sequence),
            "insert result mismatch for {sequence:?}"
        );
    }

    assert_eq!(persistent.len(), volatile.len());
    assert_eq!(persistent.sync_strategy(), SyncStrategy::InternalSync);

    for probe in probes {
        assert_eq!(
            persistent.contains_sequence(&probe),
            volatile.contains_sequence(&probe),
            "contains mismatch for {probe:?}"
        );
    }

    assert_eq!(
        sorted_sequences(persistent.iter_sequences().collect()),
        sorted_sequences(volatile.iter().collect())
    );
}

#[test]
fn sequence_operations_match_dynamic_u64_contract() {
    assert_sequence_parity(
        vec![
            vec![],
            vec![1],
            vec![1, 2],
            vec![2, 1],
            vec![u64::MAX, 0, 42],
        ],
        vec![
            vec![],
            vec![1],
            vec![1, 2],
            vec![2],
            vec![u64::MAX, 0, 42],
            vec![42],
        ],
    );
}

#[test]
fn values_prefix_iteration_node_traversal_and_reopen() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_artrie_u64.artrie");

    {
        let trie = PersistentARTrieU64::<i32>::create(&path).expect("create u64 trie");
        assert!(trie.insert_sequence_with_value(&[], 5));
        assert!(trie.insert_sequence_with_value(&[1, 2, 3], 123));
        assert!(trie.insert_sequence(&[1, 2, 4]));
        assert!(trie.insert_sequence_with_value(&[2, 1], 21));
        assert!(!trie.insert_sequence_with_value(&[1, 2, 3], 321));
        assert_eq!(trie.get_sequence_value(&[1, 2, 3]), Some(321));

        let prefix: Vec<_> = trie.iter_sequence_prefix(&[1, 2]).collect();
        assert_eq!(sorted_sequences(prefix), vec![vec![1, 2, 3], vec![1, 2, 4]]);

        let root = trie.root();
        assert!(root.is_final());
        let node = root
            .transition(1)
            .and_then(|node| node.transition(2))
            .and_then(|node| node.transition(3))
            .expect("u64 node traversal");
        assert!(node.is_final());
        assert_eq!(node.value(), Some(321));

        assert!(trie.remove_sequence(&[2, 1]));
        assert!(!trie.contains_sequence(&[2, 1]));
        trie.checkpoint().expect("checkpoint u64 trie");
        trie.close();
    }

    let reopened = PersistentARTrieU64::<i32>::open(&path).expect("open u64 trie");
    assert!(reopened.contains_sequence(&[]));
    assert!(reopened.contains_sequence(&[1, 2, 3]));
    assert!(reopened.contains_sequence(&[1, 2, 4]));
    assert!(!reopened.contains_sequence(&[2, 1]));
    assert_eq!(reopened.get_sequence_value(&[]), Some(5));
    assert_eq!(reopened.get_sequence_value(&[1, 2, 3]), Some(321));
}

#[test]
fn f64_and_string_helpers_use_u64_units() {
    let trie = PersistentARTrieU64::<i32>::new();

    assert!(trie.insert_f64_with_value(&[1.0, -0.0, f64::NAN], 9));
    assert!(trie.contains_f64(&[1.0, -0.0, f64::NAN]));
    assert_eq!(trie.get_f64_value(&[1.0, -0.0, f64::NAN]), Some(9));
    assert!(trie.remove_f64(&[1.0, -0.0, f64::NAN]));
    assert!(!trie.contains_f64(&[1.0, -0.0, f64::NAN]));

    assert!(trie.insert_with_value("persistent", 17));
    assert!(trie.contains("persistent"));
    assert_eq!(trie.get_value("persistent"), Some(17));

    let units = <u64 as CharUnit>::from_str("persistent");
    assert!(trie.contains_sequence(&units));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn persistent_u64_property_parity(
        sequences in prop::collection::vec(prop::collection::vec(0u64..8, 0..5), 0..12),
        probes in prop::collection::vec(prop::collection::vec(0u64..8, 0..5), 0..12),
    ) {
        assert_sequence_parity(sequences, probes);
    }
}
