//! Regression tests for ARTrie writer-lock-free WAL publication.
//!
//! Byte, char, vocab, and u64 durable writes should publish replayable WAL
//! records without serializing through the active WAL file append lock.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::char::PersistentARTrieChar;
use libdictenstein::persistent_artrie::vocab::PersistentVocabARTrie;
use libdictenstein::persistent_artrie::{PersistentARTrie, PersistentARTrieU64, WalReader};
use libdictenstein::MappedDictionary;
use std::path::Path;
use tempfile::tempdir;

fn active_record_count(wal_path: &Path) -> usize {
    let reader = WalReader::new(wal_path).expect("open active WAL");
    reader
        .iter()
        .map(|result| result.expect("read WAL record"))
        .count()
}

#[test]
fn byte_replays_lockfree_wal_records() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("byte_segments.part");
    let wal_path = path.with_extension("wal");

    {
        let trie = PersistentARTrie::<i64>::create(&path).expect("create byte trie");
        assert!(trie.insert_with_value("alpha", 11));
        assert!(trie.insert_with_value("beta", 22));
        trie.sync().expect("sync byte WAL");
        assert!(active_record_count(&wal_path) >= 4);
        std::mem::forget(trie);
    }

    let reopened = PersistentARTrie::<i64>::open(&path).expect("reopen byte trie");
    assert_eq!(MappedDictionary::get_value(&reopened, "alpha"), Some(11));
    assert_eq!(MappedDictionary::get_value(&reopened, "beta"), Some(22));
}

#[test]
fn char_replays_lockfree_wal_records() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("char_segments.part");
    let wal_path = path.with_extension("wal");

    {
        let trie = PersistentARTrieChar::<i64>::create(&path).expect("create char trie");
        assert!(trie
            .insert_with_value("cafe", 1)
            .expect("insert ascii char key"));
        assert!(trie
            .insert_with_value("東京", 2)
            .expect("insert unicode char key"));
        trie.sync().expect("sync char WAL");
        assert!(active_record_count(&wal_path) >= 4);
        std::mem::forget(trie);
    }

    let reopened = PersistentARTrieChar::<i64>::open(&path).expect("reopen char trie");
    assert_eq!(MappedDictionary::get_value(&reopened, "cafe"), Some(1));
    assert_eq!(MappedDictionary::get_value(&reopened, "東京"), Some(2));
}

#[test]
fn vocab_replays_lockfree_wal_records() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vocab_segments.vocab");
    let wal_path = path.with_extension("vocab.wal");

    {
        let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        assert_eq!(vocab.insert("alpha").expect("insert alpha"), 0);
        assert!(vocab
            .insert_with_index("manual", 42)
            .expect("insert manual index"));
        vocab.sync().expect("sync vocab WAL");
        assert!(active_record_count(&wal_path) >= 4);
        std::mem::forget(vocab);
    }

    let (vocab, report) = PersistentVocabARTrie::open_with_recovery(&path).expect("recover vocab");
    assert!(report.records_replayed > 0, "expected vocab WAL replay");
    assert_eq!(vocab.get_index("alpha"), Some(0));
    assert_eq!(vocab.get_index("manual"), Some(42));
    assert_eq!(vocab.get_term(42), Some("manual".to_string()));
}

#[test]
fn u64_replays_lockfree_wal_records() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("u64_segments.partu64");
    let wal_path = path.with_extension("wal");

    {
        let trie = PersistentARTrieU64::<i64>::create(&path).expect("create u64 trie");
        assert!(trie.insert_sequence_with_value(&[1, 2, 3], 123));
        assert!(trie.insert_sequence_with_value(&[7, 8], 78));
        assert!(trie.remove_sequence(&[7, 8]));
        assert!(active_record_count(&wal_path) >= 6);
        std::mem::forget(trie);
    }

    let (reopened, report) =
        PersistentARTrieU64::<i64>::open_with_recovery(&path).expect("recover u64 trie");
    assert!(report.records_replayed > 0, "expected u64 WAL replay");
    assert_eq!(reopened.get_sequence_value(&[1, 2, 3]), Some(123));
    assert!(!reopened.contains_sequence(&[7, 8]));
}
