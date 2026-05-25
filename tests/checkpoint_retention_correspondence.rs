//! Executable correspondence checks for checkpoint/WAL retention safety.
//!
//! These tests exercise the Rust side of the checkpoint-retention model:
//! corruption rebuild must retain every replayable WAL segment, including the
//! active WAL records written after the last checkpoint.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::recovery::RecoveryMode;
use libdictenstein::persistent_artrie::{PersistentARTrie, WalConfig};
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::{Dictionary, MappedDictionary};
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn corrupt_header_magic(path: &Path) {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open trie file for corruption");
    file.seek(SeekFrom::Start(0)).expect("seek header magic");
    file.write_all(b"BAD!").expect("corrupt header magic");
    file.sync_all().expect("sync header corruption");
}

fn recovery_config() -> WalConfig {
    WalConfig {
        archive_enabled: true,
        archive_dir: PathBuf::from("wal_archive"),
        max_segments: 16,
        max_archive_bytes: 16 * 1024 * 1024,
    }
}

#[test]
fn byte_corruption_rebuild_retains_active_wal_batch_and_remove() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("byte_active.part");

    {
        let mut trie = PersistentARTrie::<i32>::create(&path).expect("create byte trie");
        let inserted = trie.insert_batch(&[
            ("keep".to_string(), Some(10)),
            ("remove-me".to_string(), Some(20)),
        ]);
        assert_eq!(inserted, 2);
        assert!(trie.remove("remove-me"));
        trie.sync().expect("sync active WAL");
    }

    corrupt_header_magic(&path);

    let (recovered, report) =
        PersistentARTrie::<i32>::open_with_recovery_config(&path, recovery_config())
            .expect("recover byte trie from active WAL");

    assert_eq!(report.mode, RecoveryMode::RebuildFromWal);
    assert_eq!(recovered.get_value("keep"), Some(10));
    assert!(!recovered.contains("remove-me"));
    assert!(
        report.archive_segments_used.len() >= 1,
        "active WAL should be preserved as a rebuild segment"
    );
}

#[test]
fn char_corruption_rebuild_replays_archived_checkpoint_and_active_tail() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("char_active.artc");
    let config = recovery_config();

    {
        let mut trie =
            PersistentARTrieChar::<i32>::create_with_config(&path, config.clone()).expect("create");
        trie.insert_with_value("checkpointed", 1)
            .expect("insert checkpointed");
        trie.checkpoint().expect("checkpoint to archive");

        let inserted = trie.insert_batch(&[
            ("active-keep".to_string(), Some(2)),
            ("active-remove".to_string(), Some(3)),
        ]);
        assert_eq!(inserted, 2);
        assert!(trie.remove("active-remove").expect("remove active"));
        trie.sync().expect("sync active WAL");
    }

    corrupt_header_magic(&path);

    let (recovered, report) = PersistentARTrieChar::<i32>::open_with_recovery_config(&path, config)
        .expect("recover char trie from archive plus active WAL");

    assert_eq!(report.mode, RecoveryMode::RebuildFromWal);
    assert_eq!(recovered.get("checkpointed").copied(), Some(1));
    assert_eq!(recovered.get("active-keep").copied(), Some(2));
    assert!(!recovered.contains("active-remove"));
    assert!(
        report.archive_segments_used.len() >= 2,
        "rebuild should consume both archived checkpoint and active tail"
    );
}
