//! Executable correspondence checks for recovery replay completeness.
//!
//! The replay model requires every mutating WAL variant to map to the same
//! no-WAL trie mutation across core rebuild, byte trie recovery, and char trie
//! archive recovery. Corrupt WAL suffixes remain outside the durable prefix.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::char::PersistentARTrieChar;
use libdictenstein::persistent_artrie::{
    rebuild_from_wal_segments, recovered_operations_from_record, PersistentARTrie,
    RecoveredOperation, RecoveryMode, WalConfig, WalHeader, WalRecord, WalWriter,
};
use libdictenstein::MappedDictionary;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn ser_i64(value: i64) -> Vec<u8> {
    libdictenstein::serialization::bincode_compat::serialize(&value).expect("serialize i64")
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

fn corrupt_record_crc(path: &Path, record_index: usize) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open WAL for corruption");

    let mut record_offset = WalHeader::SIZE as u64;
    for _ in 0..record_index {
        file.seek(SeekFrom::Start(record_offset + 4))
            .expect("seek record length");
        let mut length = [0u8; 4];
        file.read_exact(&mut length).expect("read record length");
        record_offset += u32::from_le_bytes(length) as u64;
    }

    file.seek(SeekFrom::Start(record_offset))
        .expect("seek record crc");
    let mut crc_byte = [0u8; 1];
    file.read_exact(&mut crc_byte).expect("read record crc");
    crc_byte[0] ^= 0x80;
    file.seek(SeekFrom::Start(record_offset))
        .expect("seek record crc for write");
    file.write_all(&crc_byte).expect("write bad crc");
    file.sync_all().expect("sync bad crc");
}

#[test]
fn core_record_mapping_expands_every_replayable_variant() {
    let records = vec![
        WalRecord::Insert {
            term: b"insert".to_vec(),
            value: Some(ser_i64(1)),
        },
        WalRecord::Remove {
            term: b"remove".to_vec(),
        },
        WalRecord::Increment {
            term: b"increment".to_vec(),
            delta: 2,
            result: 7,
        },
        WalRecord::Upsert {
            term: b"upsert".to_vec(),
            value: ser_i64(9),
        },
        WalRecord::CompareAndSwap {
            term: b"cas-ok".to_vec(),
            expected: None,
            new_value: ser_i64(11),
            success: true,
        },
        WalRecord::CompareAndSwap {
            term: b"cas-fail".to_vec(),
            expected: None,
            new_value: ser_i64(13),
            success: false,
        },
        WalRecord::BatchInsert {
            entries: vec![
                (b"batch-a".to_vec(), Some(ser_i64(17))),
                (b"batch-b".to_vec(), None),
            ],
        },
        WalRecord::BatchIncrement {
            entries: vec![(b"batch-inc-a".to_vec(), 3), (b"batch-inc-b".to_vec(), 5)],
        },
        WalRecord::Checkpoint {
            checkpoint_lsn: 99,
            timestamp: 1,
        },
    ];

    let mut terms = Vec::new();
    for (offset, record) in records.into_iter().enumerate() {
        for op in recovered_operations_from_record((offset + 1) as u64, record) {
            match op {
                RecoveredOperation::Insert { term, .. }
                | RecoveredOperation::Remove { term, .. }
                | RecoveredOperation::Increment { term, .. }
                | RecoveredOperation::Upsert { term, .. }
                | RecoveredOperation::CompareAndSwap { term, .. } => {
                    terms.push(String::from_utf8(term).expect("utf8 term"));
                }
            }
        }
    }

    assert_eq!(
        terms,
        vec![
            "insert",
            "remove",
            "increment",
            "upsert",
            "cas-ok",
            "batch-a",
            "batch-b",
            "batch-inc-a",
            "batch-inc-b",
        ]
    );
}

#[test]
fn core_rebuild_replays_batch_increment_and_successful_cas() {
    let dir = tempdir().expect("tempdir");
    let wal_path = dir.path().join("complete.wal");
    write_wal(
        &wal_path,
        vec![
            WalRecord::BatchInsert {
                entries: vec![(b"counter".to_vec(), Some(ser_i64(1)))],
            },
            WalRecord::BatchIncrement {
                entries: vec![(b"counter".to_vec(), 4)],
            },
            WalRecord::CompareAndSwap {
                term: b"cas".to_vec(),
                expected: None,
                new_value: ser_i64(8),
                success: true,
            },
            WalRecord::CompareAndSwap {
                term: b"cas-fail".to_vec(),
                expected: None,
                new_value: ser_i64(9),
                success: false,
            },
        ],
    );

    let mut ops = Vec::new();
    let (records, terms) = rebuild_from_wal_segments(&[wal_path], |op| {
        ops.push(op);
        Ok(())
    })
    .expect("rebuild WAL segment");

    assert_eq!(records, 4);
    assert_eq!(terms, 3);
    assert_eq!(ops.len(), 3);
    assert!(matches!(
        ops[1],
        RecoveredOperation::Increment { delta: 4, .. }
    ));
    assert!(matches!(
        ops[2],
        RecoveredOperation::CompareAndSwap { success: true, .. }
    ));
}

#[test]
fn byte_corruption_rebuild_replays_batch_increment_and_cas() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("byte_replay.part");
    {
        let _trie = PersistentARTrie::<i64>::create(&path).expect("create byte trie");
    }

    fs::remove_file(path.with_extension("wal")).expect("replace active WAL");
    write_wal(
        &path.with_extension("wal"),
        vec![
            WalRecord::BatchInsert {
                entries: vec![(b"counter".to_vec(), Some(ser_i64(1)))],
            },
            WalRecord::BatchIncrement {
                entries: vec![(b"counter".to_vec(), 4)],
            },
            WalRecord::CompareAndSwap {
                term: b"cas".to_vec(),
                expected: None,
                new_value: ser_i64(8),
                success: true,
            },
        ],
    );
    corrupt_header_magic(&path);

    let (recovered, report) =
        PersistentARTrie::<i64>::open_with_recovery_config(&path, recovery_config())
            .expect("rebuild byte trie");

    assert_eq!(report.mode, RecoveryMode::RebuildFromWal);
    assert_eq!(recovered.get_value("counter"), Some(5));
    assert_eq!(recovered.get_value("cas"), Some(8));
}

#[test]
fn char_archive_recovery_replays_every_mutating_variant_without_relogging() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("char_archive.artc");
    let archive_dir = dir.path().join("archive");
    fs::create_dir(&archive_dir).expect("create archive dir");
    let segment = archive_dir.join("wal_0001.segment");

    write_wal(
        &segment,
        vec![
            WalRecord::BatchInsert {
                entries: vec![
                    (b"alpha".to_vec(), Some(ser_i64(1))),
                    (b"remove-me".to_vec(), Some(ser_i64(99))),
                    (b"counter".to_vec(), Some(ser_i64(1))),
                ],
            },
            WalRecord::Remove {
                term: b"remove-me".to_vec(),
            },
            WalRecord::BatchIncrement {
                entries: vec![(b"counter".to_vec(), 4)],
            },
            WalRecord::CompareAndSwap {
                term: b"cas".to_vec(),
                expected: None,
                new_value: ser_i64(8),
                success: true,
            },
        ],
    );

    let (recovered, stats) =
        PersistentARTrieChar::<i64>::recover_from_archives(&path, &archive_dir, recovery_config())
            .expect("recover char trie from archives");

    assert_eq!(stats.records_replayed, 4);
    // F2-migrate: Bucket A — the archive holds OWNED-format records (BatchInsert/Remove/
    // BatchIncrement/CompareAndSwap); the recovered trie create-flips on rebuild, so read
    // the recovered values via `get_value` (the overlay returns None from `get`).
    assert_eq!(recovered.get_value("alpha"), Some(1));
    assert!(!recovered.contains("remove-me"));
    assert_eq!(recovered.get_value("counter"), Some(5));
    assert_eq!(recovered.get_value("cas"), Some(8));

    let active_records =
        libdictenstein::persistent_artrie::WalReader::new(path.with_extension("wal"))
            .expect("open fresh active WAL")
            .iter()
            .count();
    assert_eq!(active_records, 0, "archive recovery must use no-WAL replay");
}

#[test]
fn char_archive_recovery_stops_at_first_corrupt_record() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("char_archive_prefix.artc");
    let archive_dir = dir.path().join("archive");
    fs::create_dir(&archive_dir).expect("create archive dir");
    let segment = archive_dir.join("wal_0001.segment");

    write_wal(
        &segment,
        vec![
            WalRecord::Insert {
                term: b"before".to_vec(),
                value: Some(ser_i64(1)),
            },
            WalRecord::Insert {
                term: b"corrupt".to_vec(),
                value: Some(ser_i64(2)),
            },
            WalRecord::Insert {
                term: b"after".to_vec(),
                value: Some(ser_i64(3)),
            },
        ],
    );
    corrupt_record_crc(&segment, 1);

    let (recovered, stats) =
        PersistentARTrieChar::<i64>::recover_from_archives(&path, &archive_dir, recovery_config())
            .expect("recover durable archive prefix");

    assert_eq!(stats.records_replayed, 1);
    // F2-migrate: Bucket A — the recovered char trie create-flips on rebuild; read via
    // `get_value` (the overlay returns None from `get`).
    assert_eq!(recovered.get_value("before"), Some(1));
    assert!(!recovered.contains("corrupt"));
    assert!(!recovered.contains("after"));
}
