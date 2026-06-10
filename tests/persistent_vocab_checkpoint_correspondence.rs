//! Executable correspondence checks for persistent vocabulary checkpoint safety.
//!
//! These tests exercise the Rust side of `PersistentVocabCheckpointSpec.v`:
//! checkpoint/reopen bijection, WAL retention until a real checkpoint, LSN
//! continuity after checkpoint truncation, sidecar rebuild, and dirty-state
//! preservation after failed publication.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::wal::WalReader;
use libdictenstein::persistent_vocab_artrie::{NodeRef, PersistentVocabARTrie, VocabReverseIndex};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn wal_record_count(vocab_path: &Path) -> usize {
    let wal_path = vocab_path.with_extension("vocab.wal");
    WalReader::new(&wal_path).expect("open WAL").iter().count()
}

#[test]
fn checkpoint_reopen_preserves_unicode_sparse_duplicates_and_batch() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("unicode_sparse.vocab");

    {
        let mut vocab =
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
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
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
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
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

#[test]
#[ignore = "owned WAL-rotate behavior; obsolete under the V4c overlay flip (overlay retains its WAL); removed at V6/single-lock-free"]
fn rotate_wal_followed_by_reopen_still_recovers_from_wal() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("rotate_recovery.vocab");

    {
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        assert_eq!(
            vocab
                .insert_batch(&["apple", "banana", "cherry"])
                .expect("batch"),
            vec![0, 1, 2]
        );
        vocab.rotate_wal().expect("rotate WAL");
        assert!(vocab.is_dirty(), "rotate_wal is not a checkpoint");
        std::mem::forget(vocab);
    }

    let (vocab, report) = PersistentVocabARTrie::open_with_recovery(&path).expect("recover vocab");
    assert!(report.records_replayed > 0, "expected WAL replay");
    assert_eq!(vocab.get_index("apple"), Some(0));
    assert_eq!(vocab.get_index("banana"), Some(1));
    assert_eq!(vocab.get_index("cherry"), Some(2));
}

#[test]
#[ignore = "owned sync/checkpoint distinction; obsolete under the V4c overlay flip (overlay sync flushes the WAL, reopen replays it via V3); removed at V6/single-lock-free"]
fn sync_to_disk_does_not_act_as_checkpoint() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("sync_not_checkpoint.vocab");

    {
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        assert_eq!(vocab.insert("alpha").expect("insert alpha"), 0);
        vocab.sync_to_disk().expect("sync WAL");
        assert!(vocab.is_dirty(), "sync_to_disk is not a checkpoint");
        std::mem::forget(vocab);
    }

    let (vocab, report) = PersistentVocabARTrie::open_with_recovery(&path).expect("recover vocab");
    assert!(report.records_replayed > 0, "expected WAL replay");
    assert_eq!(vocab.get_index("alpha"), Some(0));
}

#[test]
fn missing_reverse_index_rebuilds_exact_reverse_mapping() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("missing_reverse.vocab");

    {
        let mut vocab =
            PersistentVocabARTrie::create_with_start_index(&path, 100).expect("create vocab");
        assert!(vocab.insert_with_index("hundred", 100).expect("insert"));
        assert!(vocab.insert_with_index("sparse", 150).expect("insert"));
        vocab.checkpoint().expect("checkpoint");
    }

    fs::remove_file(path.with_extension("vocab.idx")).expect("remove reverse index");

    let vocab = PersistentVocabARTrie::open(&path).expect("open with rebuilt reverse index");
    assert_eq!(vocab.get_term(100), Some("hundred".to_string()));
    assert_eq!(vocab.get_term(150), Some("sparse".to_string()));
    assert!(!vocab.contains_index(101));
}

#[test]
fn corrupt_reverse_index_rebuilds_exact_reverse_mapping() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("corrupt_reverse.vocab");

    {
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        assert_eq!(vocab.insert("alpha").expect("insert"), 0);
        assert_eq!(vocab.insert("beta").expect("insert"), 1);
        vocab.checkpoint().expect("checkpoint");
    }

    fs::write(path.with_extension("vocab.idx"), b"bad reverse sidecar")
        .expect("corrupt reverse index");

    let vocab = PersistentVocabARTrie::open(&path).expect("open with corrupt reverse index");
    assert_eq!(vocab.get_term(0), Some("alpha".to_string()));
    assert_eq!(vocab.get_term(1), Some("beta".to_string()));
}

#[test]
fn stale_reverse_index_entries_are_not_trusted() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("stale_reverse.vocab");
    let idx_path = path.with_extension("vocab.idx");

    {
        let mut vocab =
            PersistentVocabARTrie::create_with_start_index(&path, 10).expect("create vocab");
        assert!(vocab.insert_with_index("ten", 10).expect("insert ten"));
        assert!(vocab
            .insert_with_index("twelve", 12)
            .expect("insert twelve"));
        vocab.checkpoint().expect("checkpoint");
    }

    {
        let mut stale = VocabReverseIndex::create(&idx_path, 10, 1024).expect("create stale idx");
        stale
            .set(11, NodeRef::new(0, 0))
            .expect("inject stale gap entry");
        stale.flush().expect("flush stale idx");
    }

    let vocab = PersistentVocabARTrie::open(&path).expect("open with rebuilt reverse index");
    assert_eq!(vocab.get_term(10), Some("ten".to_string()));
    assert_eq!(vocab.get_term(11), None);
    assert_eq!(vocab.get_term(12), Some("twelve".to_string()));
    assert!(!vocab.contains_index(11));
}

#[test]
#[ignore = "owned bloom sidecar; obsolete under the V4c overlay flip (overlay reads use get_index_lockfree, not the bloom; reopen does not rebuild it); removed at V6/single-lock-free"]
fn missing_bloom_sidecar_rebuilds_without_false_negatives() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("missing_bloom.vocab");
    let terms = ["alpha", "日本語", "emoji😀"];

    {
        let mut vocab = PersistentVocabARTrie::create_with_bloom(&path, 32).expect("create vocab");
        for term in terms {
            vocab.insert(term).expect("insert");
        }
        vocab.checkpoint().expect("checkpoint");
    }

    fs::remove_file(path.with_extension("vocab.bloom")).expect("remove bloom sidecar");

    let vocab = PersistentVocabARTrie::open(&path).expect("open with rebuilt bloom");
    assert!(vocab.has_bloom_filter());
    for term in terms {
        assert!(vocab.might_contain(term), "rebuilt bloom rejected {term}");
        assert!(vocab.get_index_with_bloom(term).is_some());
    }
}

#[test]
#[ignore = "owned bloom sidecar; obsolete under the V4c overlay flip (overlay reads use get_index_lockfree, not the bloom; reopen does not rebuild it); removed at V6/single-lock-free"]
fn corrupt_bloom_sidecar_rebuilds_without_false_negatives() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("corrupt_bloom.vocab");
    let terms = ["alpha", "beta", "γamma"];

    {
        let mut vocab = PersistentVocabARTrie::create_with_bloom(&path, 32).expect("create vocab");
        for term in terms {
            vocab.insert(term).expect("insert");
        }
        vocab.checkpoint().expect("checkpoint");
    }

    fs::write(path.with_extension("vocab.bloom"), b"not a bloom filter")
        .expect("corrupt bloom sidecar");

    let vocab = PersistentVocabARTrie::open(&path).expect("open with rebuilt bloom");
    assert!(vocab.has_bloom_filter());
    for term in terms {
        assert!(vocab.might_contain(term), "rebuilt bloom rejected {term}");
        assert!(vocab.get_index_with_bloom(term).is_some());
    }
}

#[test]
#[ignore = "owned bloom sidecar publication; obsolete under the V4c overlay flip (overlay has no owned bloom sidecar); removed at V6/single-lock-free"]
fn failed_bloom_sidecar_publication_keeps_dirty_and_wal_replayable() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("failed_bloom_checkpoint.vocab");
    let bloom_path = path.with_extension("vocab.bloom");

    let mut vocab = PersistentVocabARTrie::create_with_bloom(&path, 32).expect("create vocab");
    assert_eq!(vocab.insert("alpha").expect("insert alpha"), 0);
    fs::create_dir(&bloom_path).expect("create blocking bloom directory");

    let err = vocab
        .checkpoint()
        .expect_err("checkpoint must fail when bloom sidecar cannot be written");
    assert!(err.to_string().contains("bloom"));
    assert!(vocab.is_dirty(), "failed checkpoint must remain dirty");
    assert_eq!(vocab.get_index("alpha"), Some(0));
    assert!(
        wal_record_count(&path) > 0,
        "failed checkpoint must retain WAL"
    );

    fs::remove_dir(&bloom_path).expect("remove blocking bloom directory");
    vocab.checkpoint().expect("checkpoint after fixing sidecar");
    assert!(!vocab.is_dirty());
}
