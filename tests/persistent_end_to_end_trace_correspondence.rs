//! Executable correspondence checks for end-to-end persistent public traces.
//!
//! These tests exercise the Rust side of `PersistentEndToEndTraceSpec.v` and
//! `PersistentEndToEndTrace.tla`: public mutations update the live map,
//! checkpoints and compaction publish a durable snapshot without semantic
//! drift, and crash/reopen reconstructs the visible state from the checkpoint
//! plus retained WAL tail. The vocabulary case checks the same trace shape for
//! the forward/reverse bijection.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{CompactionConfig, PersistentARTrie};
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
use libdictenstein::{Dictionary, MappedDictionary};
use std::collections::BTreeMap;
use tempfile::tempdir;

fn assert_byte_trie_matches(expected: &BTreeMap<String, i64>, trie: &PersistentARTrie<i64>) {
    for (term, value) in expected {
        assert_eq!(
            trie.get_value(term),
            Some(*value),
            "byte trie value mismatch for {term:?}"
        );
    }
    assert_eq!(trie.len(), Some(expected.len()));
}

#[test]
fn byte_trace_survives_checkpoint_compaction_and_reopen() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("byte_end_to_end.part");
    let mut expected = BTreeMap::new();

    {
        let mut trie = PersistentARTrie::<i64>::create(&path).expect("create byte trie");

        assert!(trie.insert_with_value("alpha", 1));
        expected.insert("alpha".to_string(), 1);

        assert!(trie.insert_with_value("app", 2));
        expected.insert("app".to_string(), 2);

        assert_eq!(trie.increment("alpha", 4).expect("increment alpha"), 5);
        expected.insert("alpha".to_string(), 5);

        let mut tx = trie.begin_document("doc").expect("begin document");
        trie.tx_insert(&mut tx, "beta", Some(7));
        trie.tx_insert(&mut tx, "gamma", Some(9));
        assert_eq!(trie.commit_document(tx).expect("commit document"), 2);
        expected.insert("beta".to_string(), 7);
        expected.insert("gamma".to_string(), 9);

        assert!(trie.remove("app"));
        expected.remove("app");

        let batch = vec![
            ("delta".to_string(), Some(11)),
            ("epsilon".to_string(), Some(13)),
        ];
        assert_eq!(trie.insert_batch(&batch), 2);
        expected.insert("delta".to_string(), 11);
        expected.insert("epsilon".to_string(), 13);

        trie.sync().expect("sync pre-checkpoint byte trace");
        trie.checkpoint().expect("checkpoint byte trace");

        assert!(trie.insert_with_value("tail", 17));
        expected.insert("tail".to_string(), 17);

        assert_eq!(trie.increment("beta", 5).expect("increment beta"), 12);
        expected.insert("beta".to_string(), 12);

        assert!(trie.remove("gamma"));
        expected.remove("gamma");

        trie.sync().expect("sync post-checkpoint byte tail");
        trie.compact(CompactionConfig::default(), |_| {})
            .expect("compact byte trace");

        assert_byte_trie_matches(&expected, &trie);
    }

    let reopened = PersistentARTrie::<i64>::open(&path).expect("reopen byte trie");
    assert_byte_trie_matches(&expected, &reopened);
    assert_eq!(reopened.get_value("app"), None);
    assert_eq!(reopened.get_value("gamma"), None);
}

#[test]
fn char_trace_survives_checkpoint_and_wal_tail_reopen() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("char_end_to_end.part");
    let mut expected = BTreeMap::new();

    {
        let mut trie = PersistentARTrieChar::<i64>::create(&path).expect("create char trie");

        assert!(trie
            .insert_with_value("café", 3)
            .expect("insert checkpointed char term"));
        expected.insert("café".to_string(), 3);

        assert!(trie
            .insert_with_value("東京", 5)
            .expect("insert removed char term"));
        expected.insert("東京".to_string(), 5);

        assert_eq!(trie.increment("café", 4).expect("increment café"), 7);
        expected.insert("café".to_string(), 7);

        let batch = vec![
            ("emoji😀".to_string(), Some(11)),
            ("mañana".to_string(), Some(13)),
        ];
        assert_eq!(trie.insert_batch(&batch), 2);
        expected.insert("emoji😀".to_string(), 11);
        expected.insert("mañana".to_string(), 13);

        trie.sync().expect("sync char trace");
        trie.checkpoint().expect("checkpoint char trace");

        assert!(trie.remove("東京").expect("remove Tokyo"));
        expected.remove("東京");

        assert!(trie
            .insert_with_value("tail漢字", 17)
            .expect("insert char WAL tail"));
        expected.insert("tail漢字".to_string(), 17);

        trie.sync().expect("sync char WAL tail");
        std::mem::forget(trie);
    }

    let reopened = PersistentARTrieChar::<i64>::open(&path).expect("reopen char trie");
    for (term, value) in &expected {
        assert_eq!(
            reopened.get(term).copied(),
            Some(*value),
            "char trie value mismatch for {term:?}"
        );
    }
    assert_eq!(reopened.get("東京").copied(), None);
    assert_eq!(reopened.len(), expected.len());
}

#[test]
fn vocab_trace_preserves_bijection_after_checkpoint_and_reopen() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vocab_end_to_end.vocab");

    let mut expected = BTreeMap::new();
    let (alpha, beta, gamma, emoji) = {
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab trie");

        let alpha = vocab.insert("alpha").expect("insert alpha");
        assert_eq!(vocab.insert("alpha").expect("duplicate alpha"), alpha);
        expected.insert("alpha".to_string(), alpha);

        let batch = vocab
            .insert_batch(&["βeta", "gamma"])
            .expect("insert vocab batch");
        let beta = batch[0];
        let gamma = batch[1];
        expected.insert("βeta".to_string(), beta);
        expected.insert("gamma".to_string(), gamma);

        vocab.sync().expect("sync pre-checkpoint vocab trace");
        vocab.checkpoint().expect("checkpoint vocab trace");

        let emoji = vocab.insert("emoji😀").expect("insert emoji tail");
        assert_eq!(vocab.insert("gamma").expect("duplicate gamma"), gamma);
        expected.insert("emoji😀".to_string(), emoji);

        vocab.sync().expect("sync post-checkpoint vocab tail");
        std::mem::forget(vocab);

        (alpha, beta, gamma, emoji)
    };

    let (reopened, report) =
        PersistentVocabARTrie::open_with_recovery(&path).expect("recover vocab trie");
    assert!(
        report.records_replayed > 0,
        "post-checkpoint vocab WAL tail must be replayed"
    );

    for (term, index) in &expected {
        assert_eq!(
            reopened.get_index(term),
            Some(*index),
            "vocab forward lookup mismatch for {term:?}"
        );
        assert_eq!(
            reopened.get_term(*index),
            Some(term.clone()),
            "vocab reverse lookup mismatch for index {index}"
        );
    }

    assert_eq!(reopened.get_index("alpha"), Some(alpha));
    assert_eq!(reopened.get_index("βeta"), Some(beta));
    assert_eq!(reopened.get_index("gamma"), Some(gamma));
    assert_eq!(reopened.get_index("emoji😀"), Some(emoji));
}
