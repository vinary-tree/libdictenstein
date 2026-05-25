//! Correspondence tests for persistent vocabulary WAL atomicity and bijection.
//!
//! The Rocq model for this target treats vocabulary mutations as append-only
//! term-index assignments. Public mutations must accept the WAL record before
//! changing the trie, and the forward/reverse index relation must stay exact.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
use tempfile::tempdir;

#[test]
fn single_insert_recovers_by_wal_replay_without_checkpoint() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("single_replay.vocab");

    {
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        assert_eq!(vocab.insert("alpha").expect("insert alpha"), 0);
        vocab.sync().expect("sync WAL");
        std::mem::forget(vocab);
    }

    let (vocab, report) = PersistentVocabARTrie::open_with_recovery(&path).expect("recover vocab");
    assert!(report.records_replayed > 0, "expected WAL replay");
    assert_eq!(vocab.get_index("alpha"), Some(0));
    assert_eq!(vocab.get_term(0), Some("alpha".to_string()));
    assert!(vocab.contains_index(0));
}

#[test]
fn manual_index_insert_recovers_by_wal_replay_without_checkpoint() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("manual_replay.vocab");

    {
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        assert!(vocab
            .insert_with_index("manual", 42)
            .expect("insert manual index"));
        vocab.sync().expect("sync WAL");
        std::mem::forget(vocab);
    }

    let (vocab, report) = PersistentVocabARTrie::open_with_recovery(&path).expect("recover vocab");
    assert!(report.records_replayed > 0, "expected WAL replay");
    assert_eq!(vocab.get_index("manual"), Some(42));
    assert_eq!(vocab.get_term(42), Some("manual".to_string()));
    assert!(vocab.contains_index(42));
    assert!(!vocab.contains_index(0));
}

#[test]
fn manual_nonzero_index_round_trips_bijection() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("manual_bijection.vocab");

    {
        let mut vocab =
            PersistentVocabARTrie::create_with_start_index(&path, 10).expect("create vocab");
        assert!(vocab.insert_with_index("ten", 10).expect("insert ten"));
        assert!(vocab.insert_with_index("dozen", 12).expect("insert dozen"));
        vocab.checkpoint().expect("checkpoint vocab");
    }

    let vocab = PersistentVocabARTrie::open(&path).expect("open vocab");
    assert_eq!(vocab.get_index("ten"), Some(10));
    assert_eq!(vocab.get_term(10), Some("ten".to_string()));
    assert_eq!(vocab.get_index("dozen"), Some(12));
    assert_eq!(vocab.get_term(12), Some("dozen".to_string()));
    assert!(vocab.contains_index(10));
    assert!(!vocab.contains_index(11));
    assert!(vocab.contains_index(12));
}

#[test]
fn batch_duplicate_terms_share_index_without_gaps() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("batch_duplicates.vocab");

    let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
    let indices = vocab
        .insert_batch(&["alpha", "beta", "alpha", "gamma", "beta"])
        .expect("batch insert");

    assert_eq!(indices, vec![0, 1, 0, 2, 1]);
    assert_eq!(vocab.len(), 3);
    assert_eq!(vocab.next_index(), 3);
    assert_eq!(vocab.get_index("alpha"), Some(0));
    assert_eq!(vocab.get_index("beta"), Some(1));
    assert_eq!(vocab.get_index("gamma"), Some(2));
    assert_eq!(vocab.get_term(0), Some("alpha".to_string()));
    assert_eq!(vocab.get_term(1), Some("beta".to_string()));
    assert_eq!(vocab.get_term(2), Some("gamma".to_string()));
}

#[test]
fn duplicate_terms_across_batches_keep_stable_after_reopen() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("batch_reopen.vocab");

    {
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        assert_eq!(
            vocab.insert_batch(&["alpha", "beta"]).expect("first batch"),
            vec![0, 1]
        );
        assert_eq!(
            vocab
                .insert_batch(&["beta", "gamma", "alpha"])
                .expect("second batch"),
            vec![1, 2, 0]
        );
        vocab.checkpoint().expect("checkpoint vocab");
    }

    let vocab = PersistentVocabARTrie::open(&path).expect("open vocab");
    assert_eq!(vocab.len(), 3);
    assert_eq!(vocab.next_index(), 3);
    assert_eq!(vocab.get_index("alpha"), Some(0));
    assert_eq!(vocab.get_index("beta"), Some(1));
    assert_eq!(vocab.get_index("gamma"), Some(2));
}

#[test]
fn index_collision_rejected_without_mutating() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("collision.vocab");

    let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
    assert!(vocab.insert_with_index("alpha", 5).expect("insert alpha"));
    let before_len = vocab.len();
    let before_next = vocab.next_index();

    let err = vocab
        .insert_with_index("beta", 5)
        .expect_err("index collision must be rejected");
    assert!(err.to_string().contains("already assigned"));
    assert_eq!(vocab.len(), before_len);
    assert_eq!(vocab.next_index(), before_next);
    assert_eq!(vocab.get_index("alpha"), Some(5));
    assert_eq!(vocab.get_index("beta"), None);
    assert_eq!(vocab.get_term(5), Some("alpha".to_string()));

    let err = vocab
        .insert_with_index("alpha", 7)
        .expect_err("term reindex must be rejected");
    assert!(err.to_string().contains("already assigned"));
    assert_eq!(vocab.get_index("alpha"), Some(5));
    assert_eq!(vocab.get_term(7), None);
}
