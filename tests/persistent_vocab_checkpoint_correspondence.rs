//! Executable correspondence checks for persistent vocabulary checkpoint safety.
//!
//! These tests exercise the Rust side of `PersistentVocabCheckpointSpec.v`:
//! checkpoint/reopen bijection, WAL retention until a real checkpoint, LSN
//! continuity after checkpoint truncation, reverse-map rebuild, and dirty-state
//! preservation after failed publication.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::vocab::PersistentVocabARTrie;
use tempfile::tempdir;

#[test]
fn checkpoint_reopen_preserves_unicode_sparse_duplicates_and_batch() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("unicode_sparse.vocab");

    {
        let vocab =
            PersistentVocabARTrie::create_with_start_index(&path, 10).expect("create vocab");
        assert!(vocab.insert_with_index("日本語", 10).expect("insert"));
        assert!(vocab.insert_with_index("emoji😀", 42).expect("insert"));
        assert_eq!(vocab.insert("emoji😀").expect("duplicate"), 42);
        assert_eq!(
            vocab
                .insert_batch(&["alpha", "emoji😀", "βeta", "alpha"])
                .expect("batch"),
            vec![43, 42, 44, 43]
        );
        vocab.checkpoint().expect("checkpoint");
    }

    let vocab = PersistentVocabARTrie::open(&path).expect("open vocab");
    assert_eq!(vocab.get_index("日本語"), Some(10));
    assert_eq!(vocab.get_term(10), Some("日本語".to_string()));
    assert_eq!(vocab.get_index("emoji😀"), Some(42));
    assert_eq!(vocab.get_term(42), Some("emoji😀".to_string()));
    assert_eq!(vocab.get_index("alpha"), Some(43));
    assert_eq!(vocab.get_term(43), Some("alpha".to_string()));
    assert_eq!(vocab.get_index("βeta"), Some(44));
    assert_eq!(vocab.get_term(44), Some("βeta".to_string()));
    assert!(!vocab.contains_index(11));
}

#[test]
fn checkpoint_then_later_insert_replays_post_checkpoint_wal() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("post_checkpoint_replay.vocab");

    {
        let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        assert_eq!(vocab.insert("alpha").expect("insert alpha"), 0);
        vocab.checkpoint().expect("checkpoint alpha");

        assert_eq!(vocab.insert("beta").expect("insert beta"), 1);
        vocab.sync().expect("sync beta WAL");
        std::mem::forget(vocab);
    }

    let (vocab, report) = PersistentVocabARTrie::open_with_recovery(&path).expect("recover vocab");
    assert!(
        report.records_replayed > 0,
        "post-checkpoint insert must remain above the checkpoint LSN"
    );
    assert_eq!(vocab.get_index("alpha"), Some(0));
    assert_eq!(vocab.get_term(0), Some("alpha".to_string()));
    assert_eq!(vocab.get_index("beta"), Some(1));
    assert_eq!(vocab.get_term(1), Some("beta".to_string()));
}

#[test]
fn recovery_replay_retains_wal_until_checkpoint() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("recovery_retains_wal.vocab");

    {
        let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        assert_eq!(vocab.insert("alpha").expect("insert alpha"), 0);
        vocab.sync().expect("sync WAL");
        std::mem::forget(vocab);
    }

    {
        let (vocab, report) =
            PersistentVocabARTrie::open_with_recovery(&path).expect("first recovery");
        assert!(report.records_replayed > 0, "expected first WAL replay");
        assert_eq!(vocab.get_index("alpha"), Some(0));
        std::mem::forget(vocab);
    }

    let (vocab, report) =
        PersistentVocabARTrie::open_with_recovery(&path).expect("second recovery");
    assert!(
        report.records_replayed > 0,
        "recovery must not truncate WAL before a checkpoint"
    );
    assert_eq!(vocab.get_index("alpha"), Some(0));
    assert_eq!(vocab.get_term(0), Some("alpha".to_string()));
}
