//! Executable correspondence checks for public WAL lifecycle safety.
//!
//! These tests exercise the Rust side of `PersistentPublicWalLifecycleSpec.v`
//! and the ordered `AsyncWalGroupCommit.tla` model: public open must recover
//! synced WAL tails after checkpoints, and group commit must write the record
//! associated with each returned LSN at that same durable LSN.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
use libdictenstein::MappedDictionary;
use tempfile::tempdir;

#[test]
fn byte_public_open_replays_synced_wal_tail_without_checkpoint() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("byte_tail.part");

    {
        let mut trie: PersistentARTrie<i64> =
            PersistentARTrie::create(&path).expect("create byte trie");
        assert!(trie.insert_with_value("alpha", 11));
        assert!(trie.insert_with_value("beta", 22));
        trie.sync().expect("sync WAL tail");
    }

    let reopened = PersistentARTrie::<i64>::open(&path).expect("reopen byte trie");
    assert_eq!(reopened.get_value("alpha"), Some(11));
    assert_eq!(reopened.get_value("beta"), Some(22));
}

#[test]
fn byte_public_open_replays_checkpoint_plus_synced_tail() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("byte_checkpoint_tail.part");

    {
        let mut trie: PersistentARTrie<i64> =
            PersistentARTrie::create(&path).expect("create byte trie");
        assert!(trie.insert_with_value("checkpointed", 1));
        trie.sync().expect("sync checkpointed record");
        trie.checkpoint().expect("checkpoint byte trie");

        assert!(trie.insert_with_value("tail", 2));
        assert!(trie.insert_with_value("tail-2", 3));
        trie.sync().expect("sync post-checkpoint tail");
    }

    let reopened = PersistentARTrie::<i64>::open(&path).expect("reopen byte trie");
    assert_eq!(reopened.get_value("checkpointed"), Some(1));
    assert_eq!(reopened.get_value("tail"), Some(2));
    assert_eq!(reopened.get_value("tail-2"), Some(3));
}

#[test]
fn char_public_open_replays_unicode_checkpoint_plus_synced_tail() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("char_checkpoint_tail.part");

    {
        let mut trie: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path).expect("create char trie");
        assert!(trie.insert_with_value("café", 7).expect("insert café"));
        trie.sync().expect("sync checkpointed char record");
        trie.checkpoint().expect("checkpoint char trie");

        assert!(trie.insert_with_value("東京", 8).expect("insert Tokyo"));
        assert!(trie.insert_with_value("emoji😀", 9).expect("insert emoji"));
        trie.sync().expect("sync char tail");
    }

    let reopened = PersistentARTrieChar::<i64>::open(&path).expect("reopen char trie");
    assert_eq!(reopened.get("café").copied(), Some(7));
    assert_eq!(reopened.get("東京").copied(), Some(8));
    assert_eq!(reopened.get("emoji😀").copied(), Some(9));
}

#[test]
fn vocab_public_open_replays_checkpoint_plus_synced_tail_indices() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vocab_checkpoint_tail.vocab");

    let (alpha, beta, emoji) = {
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab trie");
        let alpha = vocab.insert("alpha").expect("insert alpha");
        vocab.sync().expect("sync checkpointed vocab record");
        vocab.checkpoint().expect("checkpoint vocab trie");

        let beta = vocab.insert("βeta").expect("insert beta");
        let emoji = vocab.insert("emoji😀").expect("insert emoji");
        vocab.sync().expect("sync post-checkpoint vocab tail");

        // Drop checkpoints the trie and would hide whether open replayed the
        // retained WAL tail. Keep this test focused on public open recovery.
        std::mem::forget(vocab);
        (alpha, beta, emoji)
    };

    let reopened = PersistentVocabARTrie::open(&path).expect("reopen vocab trie");
    assert_eq!(reopened.get_index("alpha"), Some(alpha));
    assert_eq!(reopened.get_index("βeta"), Some(beta));
    assert_eq!(reopened.get_index("emoji😀"), Some(emoji));
    assert_eq!(reopened.get_term(alpha), Some("alpha".to_string()));
    assert_eq!(reopened.get_term(beta), Some("βeta".to_string()));
    assert_eq!(reopened.get_term(emoji), Some("emoji😀".to_string()));
}

#[cfg(feature = "group-commit")]
#[test]
fn group_commit_concurrent_writes_return_lsn_written_for_same_record() {
    use libdictenstein::persistent_artrie::{
        AsyncWalConfig, AsyncWalWriter, GroupCommitConfig, GroupCommitCoordinator, WalConfig,
        WalReader, WalRecord,
    };
    use std::collections::BTreeMap;
    use std::sync::{mpsc, Arc, Barrier};
    use std::thread;

    let dir = tempdir().expect("temp dir");
    let wal_path = dir.path().join("group_commit_ordered.wal");
    let async_config = AsyncWalConfig::with_pending_dir(dir.path().join("pending"));
    let archive_config = WalConfig {
        archive_dir: dir.path().join("archive"),
        ..Default::default()
    };
    let wal = Arc::new(
        AsyncWalWriter::create(&wal_path, async_config, archive_config).expect("create async WAL"),
    );

    let writer_count = 8usize;
    let coordinator = Arc::new(
        GroupCommitCoordinator::new(
            Arc::clone(&wal),
            GroupCommitConfig {
                max_batch_size: writer_count,
                max_batch_delay_us: 100_000,
                dedicated_commit_thread: true,
                adaptive_batching: false,
                ..Default::default()
            },
        )
        .expect("create group commit coordinator"),
    );

    let barrier = Arc::new(Barrier::new(writer_count + 1));
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::with_capacity(writer_count);

    for writer_id in 0..writer_count {
        let coordinator = Arc::clone(&coordinator);
        let barrier = Arc::clone(&barrier);
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            let term = format!("writer-{writer_id}");
            barrier.wait();
            let lsn = coordinator
                .append_with_sync(WalRecord::Insert {
                    term: term.as_bytes().to_vec(),
                    value: None,
                })
                .expect("append with group commit");
            tx.send((term, lsn)).expect("send returned LSN");
        }));
    }
    drop(tx);

    barrier.wait();
    for handle in handles {
        handle.join().expect("writer thread");
    }

    let returned: BTreeMap<String, u64> = rx.into_iter().collect();
    assert_eq!(returned.len(), writer_count);
    let mut returned_lsns: Vec<_> = returned.values().copied().collect();
    returned_lsns.sort_unstable();
    assert_eq!(returned_lsns, (1..=writer_count as u64).collect::<Vec<_>>());

    drop(coordinator);
    drop(wal);

    let records: Vec<_> = WalReader::new(&wal_path)
        .expect("open WAL reader")
        .iter()
        .collect::<Result<_, _>>()
        .expect("read WAL records");
    assert_eq!(records.len(), writer_count);

    for (lsn, record) in records {
        match record {
            WalRecord::Insert { term, .. } => {
                let term = String::from_utf8(term).expect("UTF-8 test term");
                assert_eq!(
                    returned.get(&term),
                    Some(&lsn),
                    "record {term} must be written at its returned LSN"
                );
            }
            other => panic!("unexpected WAL record: {:?}", other),
        }
    }
}
