//! Correspondence checks for char/vocab persistent rewrite publication.
//!
//! The Rocq model treats char and vocab checkpointing as the public
//! rewrite-compaction surface for those backends: a successful rewrite preserves
//! the exact visible snapshot, while failed publication keeps dirty/WAL evidence
//! retryable.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::wal::{WalConfig, WalReader};
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn char_wal_record_count(path: &Path) -> usize {
    WalReader::new(&path.with_extension("wal"))
        .expect("open char WAL")
        .iter()
        .count()
}

fn vocab_wal_record_count(path: &Path) -> usize {
    WalReader::new(&path.with_extension("vocab.wal"))
        .expect("open vocab WAL")
        .iter()
        .count()
}

fn assert_char_value(dict: &PersistentARTrieChar<i32>, term: &str, value: i32) {
    // F2-migrate: Bucket A — `get()` returns None under the overlay; read via `get_value`
    // (correct in owned mode too — falls through to the owned tree).
    assert_eq!(
        dict.get_value(term),
        Some(value),
        "unexpected char value for {term:?}"
    );
}

#[test]
fn char_rewrite_checkpoint_preserves_unicode_values_lazy_and_eager_reopen() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("char_unicode_rewrite.part");

    {
        let trie = PersistentARTrieChar::<i32>::create(&path).expect("create char trie");
        trie.insert_with_value("alpha", 1).expect("insert alpha");
        trie.insert_with_value("café", 2).expect("insert cafe");
        trie.insert_with_value("東京", 3).expect("insert tokyo");
        trie.insert_with_value("emoji😀", 4).expect("insert emoji");
        trie.checkpoint().expect("checkpoint char trie");
        assert!(!trie.is_dirty(), "successful checkpoint should clear dirty");
    }

    let lazy = PersistentARTrieChar::<i32>::open(&path).expect("lazy reopen char trie");
    assert_char_value(&lazy, "alpha", 1);
    assert_char_value(&lazy, "café", 2);
    assert_char_value(&lazy, "東京", 3);
    assert_char_value(&lazy, "emoji😀", 4);
    // L1.3: the eager-depth `open_with_depth` reopen was deleted (F5 materializes the whole overlay,
    // so eager depth is moot); the lazy `open()` reopen above is the production path.
}

#[test]
fn char_checkpoint_rewrite_keeps_post_checkpoint_wal_tail_replayable() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("char_rewrite_tail.part");

    {
        let trie = PersistentARTrieChar::<i32>::create(&path).expect("create char trie");
        trie.insert_with_value("checkpointed", 10)
            .expect("insert checkpointed");
        trie.checkpoint().expect("checkpoint char trie");

        trie.insert_with_value("wal-tail", 20)
            .expect("insert WAL tail");
        trie.sync().expect("sync WAL tail");
    }

    let reopened = PersistentARTrieChar::<i32>::open(&path).expect("reopen char trie");
    assert_char_value(&reopened, "checkpointed", 10);
    assert_char_value(&reopened, "wal-tail", 20);
}

#[test]
fn char_persist_to_disk_alone_does_not_clear_checkpoint_dirty_state() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("char_persist_only.part");

    let mut trie = PersistentARTrieChar::<i32>::create(&path).expect("create char trie");
    // F2-migrate: Bucket B — `is_dirty()`/`persist_to_disk`/checkpoint dirty-state is an
    // OWNED-tree checkpoint concept (the overlay tracks durability via its own WAL
    // watermark, not the owned dirty flag). Pin OwnedTree. No-op feature-off.
    trie.kill_switch_to_owned();
    trie.insert_with_value("descriptor", 10)
        .expect("insert descriptor term");
    assert!(trie.is_dirty(), "mutation should mark trie dirty");

    trie.persist_to_disk().expect("publish descriptor only");
    assert!(
        trie.is_dirty(),
        "persist_to_disk is not full checkpoint publication"
    );

    trie.checkpoint()
        .expect("checkpoint after descriptor publish");
    assert!(!trie.is_dirty(), "checkpoint should clear dirty");
}

#[test]
fn char_failed_wal_archive_after_rewrite_keeps_dirty_until_retry() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("char_archive_retry.part");
    let archive_dir_name = "char_archive_retry";
    let archive_dir = dir.path().join(archive_dir_name);
    let wal_config = WalConfig::with_archive_dir(archive_dir_name);

    let trie =
        PersistentARTrieChar::<i32>::create_with_config(&path, wal_config).expect("create trie");
    // F2-migrate: Bucket B — failed-WAL-archive dirty-retry is an OWNED-tree checkpoint
    // concept (the overlay does not gate visibility on the owned dirty flag). Pin
    // OwnedTree. No-op feature-off.
    trie.kill_switch_to_owned();
    trie.insert_with_value("alpha", 1).expect("insert alpha");

    fs::remove_dir(&archive_dir).expect("remove empty archive dir");
    fs::write(&archive_dir, b"not a directory").expect("block archive directory");

    trie.checkpoint()
        .expect_err("checkpoint must fail while archive dir is blocked");
    assert!(
        trie.is_dirty(),
        "failed WAL archive after rewrite must leave checkpoint dirty"
    );
    assert!(
        char_wal_record_count(&path) > 0,
        "failed checkpoint must leave active WAL replay evidence"
    );

    fs::remove_file(&archive_dir).expect("remove archive blocker");
    trie.checkpoint().expect("retry checkpoint");
    assert!(!trie.is_dirty(), "retry checkpoint should clear dirty");
    drop(trie);

    let reopened = PersistentARTrieChar::<i32>::open(&path).expect("reopen char trie");
    assert_char_value(&reopened, "alpha", 1);
}

#[test]
fn vocab_rewrite_checkpoint_preserves_sparse_unicode_duplicate_bijection() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vocab_rewrite_sparse.vocab");

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
        vocab.checkpoint().expect("checkpoint vocab");
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
fn vocab_checkpoint_rewrite_keeps_post_checkpoint_wal_tail_replayable() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vocab_rewrite_tail.vocab");

    {
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        assert_eq!(vocab.insert("checkpointed").expect("insert"), 0);
        vocab.checkpoint().expect("checkpoint vocab");

        assert_eq!(vocab.insert("wal-tail").expect("insert tail"), 1);
        vocab.sync().expect("sync WAL tail");
        std::mem::forget(vocab);
    }

    let (vocab, report) = PersistentVocabARTrie::open_with_recovery(&path).expect("recover vocab");
    assert!(
        report.records_replayed > 0,
        "post-checkpoint WAL tail must be replayed"
    );
    assert_eq!(vocab.get_index("checkpointed"), Some(0));
    assert_eq!(vocab.get_term(0), Some("checkpointed".to_string()));
    assert_eq!(vocab.get_index("wal-tail"), Some(1));
    assert_eq!(vocab.get_term(1), Some("wal-tail".to_string()));
}

#[test]
fn vocab_failed_sidecar_publication_keeps_dirty_and_wal_replayable_until_retry() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vocab_rewrite_bloom_fail.vocab");
    let bloom_path = path.with_extension("vocab.bloom");

    let mut vocab = PersistentVocabARTrie::create_with_bloom(&path, 32).expect("create vocab");
    assert_eq!(vocab.insert("alpha").expect("insert alpha"), 0);
    fs::create_dir(&bloom_path).expect("create blocking bloom directory");

    let err = vocab
        .checkpoint()
        .expect_err("checkpoint must fail while bloom sidecar is blocked");
    assert!(err.to_string().contains("bloom"), "unexpected error: {err}");
    assert!(vocab.is_dirty(), "failed checkpoint must remain dirty");
    assert_eq!(vocab.get_index("alpha"), Some(0));
    assert!(
        vocab_wal_record_count(&path) > 0,
        "failed sidecar publication must retain active WAL"
    );

    fs::remove_dir(&bloom_path).expect("remove bloom blocker");
    vocab.checkpoint().expect("retry checkpoint");
    assert!(!vocab.is_dirty(), "retry checkpoint should clear dirty");
}
