//! Correspondence checks for checked transaction increments and replayed
//! `BatchIncrement` records.
//!
//! The formal model requires overflow to fail before commit/batch WAL records
//! are appended and requires recovery to stop at the durable prefix when replay
//! arithmetic is invalid.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::char::PersistentARTrieChar;
use libdictenstein::persistent_artrie::{
    PersistentARTrie, RecoveryMode, WalConfig, WalRecord, WalWriter,
};
use libdictenstein::{Dictionary, MappedDictionary};
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn ser_i64(value: i64) -> Vec<u8> {
    libdictenstein::serialization::bincode_compat::serialize(&value).expect("serialize i64")
}

fn wal_len(path: &Path) -> u64 {
    fs::metadata(path.with_extension("wal"))
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn recovery_config() -> WalConfig {
    WalConfig {
        archive_enabled: true,
        archive_dir: PathBuf::from("wal_archive"),
        max_segments: 16,
        max_archive_bytes: 16 * 1024 * 1024,
    }
}

fn write_wal(path: &Path, records: Vec<WalRecord>) {
    let writer = WalWriter::create(path).expect("create WAL");
    for record in records {
        writer.append(record).expect("append WAL record");
    }
    writer.sync().expect("sync WAL");
}

fn corrupt_header_magic(path: &Path) {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open trie file for corruption");
    file.seek(SeekFrom::Start(0)).expect("seek header magic");
    file.write_all(b"BAD!").expect("corrupt header magic");
    file.sync_all().expect("sync header corruption");
}

#[test]
fn char_tx_increment_aggregate_overflow_poisons_transaction() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("char_tx_aggregate_overflow.artc");
    let trie = PersistentARTrieChar::<i64>::create(&path).expect("create char trie");

    let mut tx = trie.begin_document("overflow-doc").expect("begin tx");
    let before_commit_wal = wal_len(&path);

    trie.try_tx_increment(&mut tx, "counter", i64::MAX)
        .expect("first staged increment fits");
    assert!(
        trie.try_tx_increment(&mut tx, "counter", 1).is_err(),
        "aggregate overflow must be reported while staging"
    );

    let error = trie
        .commit_document(tx)
        .expect_err("poisoned transaction must not commit");
    assert!(
        error.to_string().contains("overflow"),
        "unexpected error: {error}"
    );
    assert_eq!(wal_len(&path), before_commit_wal);
    assert!(!trie.contains("counter"));
}

#[test]
fn byte_rebuild_stops_before_overflowed_batch_increment_suffix() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("byte_replay_overflow.part");
    {
        let _trie = PersistentARTrie::<i64>::create(&path).expect("create byte trie");
    }

    fs::remove_file(path.with_extension("wal")).expect("replace active WAL");
    write_wal(
        &path.with_extension("wal"),
        vec![
            WalRecord::BatchInsert {
                entries: vec![(b"counter".to_vec(), Some(ser_i64(i64::MAX)))],
            },
            WalRecord::BatchIncrement {
                entries: vec![(b"counter".to_vec(), 1)],
            },
            WalRecord::Insert {
                term: b"after".to_vec(),
                value: Some(ser_i64(7)),
            },
        ],
    );
    corrupt_header_magic(&path);

    let (recovered, report) =
        PersistentARTrie::<i64>::open_with_recovery_config(&path, recovery_config())
            .expect("rebuild byte trie");

    assert_eq!(report.mode, RecoveryMode::RebuildFromWal);
    assert_eq!(recovered.get_value("counter"), Some(i64::MAX));
    assert!(!recovered.contains("after"));
}

#[test]
fn char_archive_recovery_stops_before_overflowed_batch_increment_suffix() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("char_replay_overflow.artc");
    let archive_dir = dir.path().join("archive");
    fs::create_dir(&archive_dir).expect("create archive dir");
    let segment = archive_dir.join("wal_0001.segment");

    write_wal(
        &segment,
        vec![
            WalRecord::BatchInsert {
                entries: vec![(b"counter".to_vec(), Some(ser_i64(i64::MAX)))],
            },
            WalRecord::BatchIncrement {
                entries: vec![(b"counter".to_vec(), 1)],
            },
            WalRecord::Insert {
                term: b"after".to_vec(),
                value: Some(ser_i64(7)),
            },
        ],
    );

    let (recovered, stats) =
        PersistentARTrieChar::<i64>::recover_from_archives(&path, &archive_dir, recovery_config())
            .expect("recover durable archive prefix");

    assert_eq!(stats.records_replayed, 2);
    // F2-migrate: Bucket A — the archive holds OWNED-format records; the recovered char
    // trie create-flips on rebuild, so read the recovered value via `get_value` (the
    // overlay returns None from `get`).
    assert_eq!(recovered.get_value("counter"), Some(i64::MAX));
    assert!(!recovered.contains("after"));
}
