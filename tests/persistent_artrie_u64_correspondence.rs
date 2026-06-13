#![cfg(feature = "persistent-artrie")]

use libdictenstein::dynamic_dawg::DynamicDawgU64;
use libdictenstein::persistent_artrie::{
    PersistentARTrieU64, PersistentARTrieU64Compact, WalReader, WalRecord, WalWriter,
};
use libdictenstein::{CharUnit, Dictionary, DictionaryNode, MappedDictionaryNode, SyncStrategy};
use proptest::prelude::*;
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
