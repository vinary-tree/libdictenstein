#![cfg(feature = "persistent-artrie")]

use libdictenstein::dynamic_dawg::DynamicDawgU64;
use libdictenstein::persistent_artrie::{
    PersistentARTrieU64, PersistentARTrieU64Compact, WalReader, WalRecord, WalWriter,
};
use libdictenstein::{CharUnit, Dictionary, DictionaryNode, MappedDictionaryNode, SyncStrategy};
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn sorted_sequences(mut sequences: Vec<Vec<u64>>) -> Vec<Vec<u64>> {
    sequences.sort();
    sequences
}

fn encode_sequence(sequence: &[u64]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(sequence.len() * 8);
    for unit in sequence {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn u64_wal_path(path: &Path) -> PathBuf {
    let mut wal = path.to_path_buf();
    wal.set_extension("wal");
    wal
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

    let persistent_sequences = persistent.iter_sequences().collect::<Vec<_>>();
    assert!(
        persistent_sequences
            .windows(2)
            .all(|pair| pair[0] < pair[1]),
        "native persistent iteration must be strictly lexicographic"
    );
    assert_eq!(
        persistent_sequences,
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
fn native_iteration_is_lexicographic_and_prefix_local() {
    let trie = PersistentARTrieU64Compact::<u64>::new();
    for (sequence, value) in [
        (vec![2], 20),
        (vec![], 0),
        (vec![1, 3], 13),
        (vec![1], 10),
        (vec![1, 2, 0], 120),
        (vec![1, 2], 12),
    ] {
        assert!(trie.insert_sequence_with_value(&sequence, value));
    }

    let mut snapshot = trie.iter_sequences();
    assert_eq!(snapshot.next(), Some(vec![]));
    assert!(trie.insert_sequence_with_value(&[0], 1));
    assert_eq!(
        snapshot.collect::<Vec<_>>(),
        vec![vec![1], vec![1, 2], vec![1, 2, 0], vec![1, 3], vec![2],]
    );
    let mut current = trie.iter_sequences();
    assert_eq!(
        current.by_ref().collect::<Vec<_>>(),
        vec![
            vec![],
            vec![0],
            vec![1],
            vec![1, 2],
            vec![1, 2, 0],
            vec![1, 3],
            vec![2],
        ]
    );
    assert_eq!(current.next(), None);
    assert_eq!(current.next(), None);
    assert_eq!(
        trie.iter_sequence_prefix_with_values(&[1, 2])
            .collect::<Vec<_>>(),
        vec![(vec![1, 2], Some(12)), (vec![1, 2, 0], Some(120))]
    );
    assert!(trie.iter_sequence_prefix(&[9, 9]).next().is_none());
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
fn native_u64_wal_replays_uncheckpointed_operations() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_artrie_u64_wal.partu64");

    {
        let trie = PersistentARTrieU64::<i32>::create(&path).expect("create u64 trie");
        assert!(trie.insert_sequence_with_value(&[1, 2, 3], 123));
        assert!(trie.insert_sequence(&[4, 5, 6]));
        assert!(trie.insert_sequence_with_value(&[9], 9));
        assert!(trie.remove_sequence(&[9]));
        // Intentionally skip checkpoint/close so reopen must use the native WAL.
    }

    let (reopened, report) =
        PersistentARTrieU64::<i32>::open_with_recovery(&path).expect("recover u64 trie");
    assert!(report.records_replayed >= 4);
    assert!(reopened.contains_sequence(&[1, 2, 3]));
    assert!(reopened.contains_sequence(&[4, 5, 6]));
    assert!(!reopened.contains_sequence(&[9]));
    assert_eq!(reopened.get_sequence_value(&[1, 2, 3]), Some(123));
}

#[test]
fn native_u64_checkpoint_records_watermark_and_replays_only_tail() {
    let dir = tempdir().expect("temp dir");
    let path = dir
        .path()
        .join("persistent_artrie_u64_checkpoint_tail.partu64");

    {
        let trie = PersistentARTrieU64::<i32>::create(&path).expect("create u64 trie");
        assert!(trie.insert_sequence_with_value(&[1], 10));
        assert!(trie.insert_sequence_with_value(&[2], 20));
        trie.checkpoint().expect("checkpoint u64 trie");
        assert!(trie.insert_sequence_with_value(&[3], 30));
    }

    let header = WalReader::read_header(u64_wal_path(&path)).expect("read u64 wal header");
    assert!(
        header.checkpoint_lsn > 0,
        "u64 checkpoint must record a committed-watermark checkpoint_lsn"
    );

    let (reopened, report) =
        PersistentARTrieU64::<i32>::open_with_recovery(&path).expect("recover u64 trie");
    assert_eq!(
        report.records_replayed, 1,
        "only the post-checkpoint tail data record should replay"
    );
    assert_eq!(reopened.get_sequence_value(&[1]), Some(10));
    assert_eq!(reopened.get_sequence_value(&[2]), Some(20));
    assert_eq!(reopened.get_sequence_value(&[3]), Some(30));
}

#[test]
fn native_u64_recovery_honors_commit_rank_generation_order() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_artrie_u64_commit_rank.partu64");
    {
        let _trie = PersistentARTrieU64::<i32>::create(&path).expect("create u64 trie");
    }

    let wal_path = u64_wal_path(&path);
    let writer = WalWriter::open(&wal_path).expect("open u64 wal");
    let term = encode_sequence(&[9, 9]);
    let value_one =
        libdictenstein::serialization::bincode_compat::serialize(&1i32).expect("serialize value");
    let value_two =
        libdictenstein::serialization::bincode_compat::serialize(&2i32).expect("serialize value");

    let lsn_one = writer
        .append(WalRecord::Upsert {
            term: term.clone(),
            value: value_one,
        })
        .expect("append first upsert");
    let lsn_two = writer
        .append(WalRecord::Upsert {
            term: term.clone(),
            value: value_two,
        })
        .expect("append second upsert");
    writer
        .append(WalRecord::CommitRank {
            data_lsn: lsn_two,
            term: term.clone(),
            generation: 1,
        })
        .expect("rank second upsert first");
    writer
        .append(WalRecord::CommitRank {
            data_lsn: lsn_one,
            term,
            generation: 2,
        })
        .expect("rank first upsert second");
    writer.sync().expect("sync u64 wal");

    let (reopened, report) =
        PersistentARTrieU64::<i32>::open_with_recovery(&path).expect("recover ranked u64 trie");
    assert_eq!(report.records_replayed, 2);
    assert_eq!(
        reopened.get_sequence_value(&[9, 9]),
        Some(1),
        "replay must follow CommitRank generation order, not raw WAL LSN order"
    );
}

#[test]
fn cx_prefix_four_checkpoint_reopens() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_artrie_u64_prefix4.artrie");

    {
        let trie = PersistentARTrieU64Compact::<u64>::create(&path).expect("create u64 trie");
        for i in 0..32u64 {
            let sequence = vec![7, 11, 13, 17, i, i.wrapping_mul(3), i.wrapping_mul(9)];
            assert!(trie.insert_sequence_with_value(&sequence, i));
        }
        trie.checkpoint().expect("checkpoint prefix4 u64 trie");
    }

    let reopened = PersistentARTrieU64Compact::<u64>::open(&path).expect("open prefix4 u64 trie");
    for i in 0..32u64 {
        let sequence = vec![7, 11, 13, 17, i, i.wrapping_mul(3), i.wrapping_mul(9)];
        assert!(reopened.contains_sequence(&sequence));
        assert_eq!(reopened.get_sequence_value(&sequence), Some(i));
    }
}

#[test]
fn deep_native_u64_lifecycle_is_stack_safe() {
    const SEQUENCE_LEN: usize = 100_000;

    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("persistent_artrie_u64_deep.artrie");
    let first: Vec<u64> = (0..SEQUENCE_LEN)
        .map(|index| u64::try_from(index).expect("sequence index fits u64"))
        .collect();
    let mut sibling = first.clone();
    sibling[SEQUENCE_LEN - 1] = u64::MAX;

    {
        let trie = PersistentARTrieU64Compact::<u64>::create(&path).expect("create deep u64 trie");
        assert!(!trie.contains_sequence(&first));
        assert_eq!(trie.get_sequence_value(&first), None);
        assert!(trie.insert_sequence_with_value(&first, 1));
        assert!(trie.insert_sequence_with_value(&sibling, 2));
        assert!(!trie.insert_sequence_with_value(&first, 3));
        assert_eq!(trie.get_sequence_value(&first), Some(3));
        assert_eq!(trie.get_sequence_value(&sibling), Some(2));

        // Dropping never-started and partially consumed iterators must release an
        // arbitrarily deep active machine without recursive `Arc` destruction.
        let never_started = trie.iter_sequences_with_values();
        drop(never_started);
        let mut partially_consumed = trie.iter_sequences_with_values();
        assert_eq!(partially_consumed.next(), Some((first.clone(), Some(3))));
        drop(partially_consumed);
        let mut prefix_partially_consumed = trie.iter_sequence_prefix_with_values(&first[..50_000]);
        assert_eq!(
            prefix_partially_consumed.next(),
            Some((first.clone(), Some(3)))
        );
        drop(prefix_partially_consumed);

        assert_eq!(
            trie.iter_sequences_with_values().collect::<Vec<_>>(),
            vec![(first.clone(), Some(3)), (sibling.clone(), Some(2))]
        );
        assert!(trie.remove_sequence(&first));
        assert!(!trie.contains_sequence(&first));
        assert!(trie.contains_sequence(&sibling));
        assert_eq!(trie.term_count(), 1);
        trie.checkpoint().expect("checkpoint deep u64 trie");
    }

    {
        let trie = PersistentARTrieU64Compact::<u64>::open(&path).expect("reopen deep u64 trie");
        assert!(!trie.contains_sequence(&first));
        assert_eq!(trie.get_sequence_value(&sibling), Some(2));
        assert!(trie.remove_sequence(&sibling));
        assert_eq!(trie.term_count(), 0);
        trie.checkpoint().expect("checkpoint emptied deep u64 trie");
    }

    let reopened =
        PersistentARTrieU64Compact::<u64>::open(&path).expect("reopen emptied deep u64 trie");
    assert_eq!(reopened.term_count(), 0);
    assert!(!reopened.contains_sequence(&first));
    assert!(!reopened.contains_sequence(&sibling));
}

#[test]
fn cx_checkpoint_bytes_are_stable_across_reopen() {
    let dir = tempdir().expect("temp dir");

    let compact_path = dir.path().join("stable-prefix4.artrie");
    {
        let trie = PersistentARTrieU64Compact::<u64>::create(&compact_path)
            .expect("create prefix4 stability trie");
        assert!(trie.insert_sequence_with_value(&[], 11));
        assert!(trie.insert_sequence_with_value(&[3, 5, 8, 13, 21], 34));
        assert!(trie.insert_sequence_with_value(&[3, 5, 8, 13, 22], 35));
        assert!(trie.insert_sequence_with_value(&[9, 9, 9], 99));
        assert!(trie.remove_sequence(&[9, 9, 9]));
        trie.checkpoint().expect("write first prefix4 checkpoint");
    }
    let compact_before = std::fs::read(&compact_path).expect("read first prefix4 checkpoint");
    {
        let trie =
            PersistentARTrieU64Compact::<u64>::open(&compact_path).expect("reopen prefix4 trie");
        trie.checkpoint().expect("write second prefix4 checkpoint");
    }
    let compact_after = std::fs::read(&compact_path).expect("read second prefix4 checkpoint");
    assert_eq!(compact_after, compact_before);
    assert_eq!(
        xxhash_rust::xxh3::xxh3_64(&compact_after),
        0xb7ad_f877_da1a_6bc1,
        "prefix4 AR64CX01 bytes changed"
    );

    let prefix3_path = dir.path().join("stable-prefix3.artrie");
    {
        let trie =
            libdictenstein::persistent_artrie::PersistentARTrieU64Prefix3Compat::<u64>::create(
                &prefix3_path,
            )
            .expect("create prefix3 stability trie");
        assert!(trie.insert_sequence_with_value(&[1, 1, 2, 3, 5, 8], 13));
        assert!(trie.insert_sequence_with_value(&[1, 1, 2, 3, 5, 9], 14));
        trie.checkpoint().expect("write first prefix3 checkpoint");
    }
    let prefix3_before = std::fs::read(&prefix3_path).expect("read first prefix3 checkpoint");
    {
        let trie =
            libdictenstein::persistent_artrie::PersistentARTrieU64Prefix3Compat::<u64>::open(
                &prefix3_path,
            )
            .expect("reopen prefix3 trie");
        trie.checkpoint().expect("write second prefix3 checkpoint");
    }
    let prefix3_after = std::fs::read(&prefix3_path).expect("read second prefix3 checkpoint");
    assert_eq!(prefix3_after, prefix3_before);
    assert_eq!(
        xxhash_rust::xxh3::xxh3_64(&prefix3_after),
        0x4155_bf8b_54f6_2a3a,
        "prefix3 AR64CX01 bytes changed"
    );
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

    #[test]
    fn valued_iteration_and_arbitrary_prefixes_match_btree_map(
        entries in prop::collection::vec(
            (prop::collection::vec(any::<u64>(), 0..7), any::<u64>()),
            0..32,
        ),
        prefixes in prop::collection::vec(prop::collection::vec(any::<u64>(), 0..5), 0..16),
    ) {
        let trie = PersistentARTrieU64Compact::<u64>::new();
        let mut oracle = BTreeMap::new();
        for (key, value) in entries {
            trie.insert_sequence_with_value(&key, value);
            oracle.insert(key, value);
        }

        let emitted = trie
            .try_iter_sequences_with_values()
            .collect::<libdictenstein::persistent_artrie::Result<Vec<_>>>()
            .expect("public native-u64 topology is resident");
        let expected = oracle
            .iter()
            .map(|(key, value)| (key.clone(), Some(*value)))
            .collect::<Vec<_>>();
        prop_assert_eq!(emitted, expected);

        for prefix in prefixes {
            let emitted = trie
                .try_iter_sequence_prefix_with_values(&prefix)
                .expect("prefix lookup must preserve resident topology")
                .collect::<libdictenstein::persistent_artrie::Result<Vec<_>>>()
                .expect("prefix traversal must preserve resident topology");
            let expected = oracle
                .iter()
                .filter(|(key, _)| key.starts_with(&prefix))
                .map(|(key, value)| (key.clone(), Some(*value)))
                .collect::<Vec<_>>();
            prop_assert_eq!(emitted, expected, "prefix {:?}", prefix);
        }
    }
}
