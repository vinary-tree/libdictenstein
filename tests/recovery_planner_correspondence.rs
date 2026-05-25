//! Executable correspondence checks for recovery-planner safety.
//!
//! The recovery planner model requires WAL replay to stop at the first corrupt
//! record: records before that point are a durable prefix, while later bytes are
//! not trusted even if they parse as valid records.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{
    rebuild_from_wal_segments, IncrementalRecovery, PersistentARTrie, RecoveredOperation,
    RecoveryManager, RecoveryMode, WalConfig, WalHeader, WalRecord, WalWriter,
};
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::{Dictionary, MappedDictionary};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn wal_insert(term: &str, value: i32) -> WalRecord {
    WalRecord::Insert {
        term: term.as_bytes().to_vec(),
        value: Some(libdictenstein::serialization::bincode_compat::serialize(&value).unwrap()),
    }
}

fn recovery_config() -> WalConfig {
    WalConfig {
        archive_enabled: true,
        archive_dir: PathBuf::from("wal_archive"),
        max_segments: 16,
        max_archive_bytes: 16 * 1024 * 1024,
    }
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

fn insert_terms_into_wal(path: &Path) {
    let writer = WalWriter::create(path).expect("create WAL");
    writer.append(wal_insert("before", 1)).expect("before");
    writer.append(wal_insert("corrupt", 2)).expect("corrupt");
    writer.append(wal_insert("after", 3)).expect("after");
    writer.sync().expect("sync WAL");
}

#[test]
fn rebuild_from_segments_stops_at_first_corrupt_record() {
    let dir = tempdir().expect("tempdir");
    let wal_path = dir.path().join("prefix.wal");
    insert_terms_into_wal(&wal_path);
    corrupt_record_crc(&wal_path, 1);

    let mut applied = Vec::new();
    let (records, terms) = rebuild_from_wal_segments(&[wal_path], |op| {
        if let RecoveredOperation::Insert { term, .. } = op {
            applied.push(String::from_utf8(term).expect("utf8 term"));
        }
        Ok(())
    })
    .expect("rebuild prefix");

    assert_eq!(records, 1);
    assert_eq!(terms, 1);
    assert_eq!(applied, vec!["before"]);
}

#[test]
fn recovery_manager_stops_at_first_corrupt_record() {
    let dir = tempdir().expect("tempdir");
    let wal_path = dir.path().join("manager.wal");
    insert_terms_into_wal(&wal_path);
    corrupt_record_crc(&wal_path, 1);

    let state = RecoveryManager::new(&wal_path)
        .recover()
        .expect("recover durable prefix");
    let terms: Vec<_> = state
        .operations()
        .filter_map(|op| match op {
            RecoveredOperation::Insert { term, .. } => {
                Some(String::from_utf8(term.clone()).expect("utf8 term"))
            }
            _ => None,
        })
        .collect();

    assert_eq!(state.operation_count(), 1);
    assert_eq!(state.next_lsn, 2);
    assert_eq!(state.stats.corrupted_records, 1);
    assert_eq!(terms, vec!["before"]);
}

#[test]
fn incremental_recovery_stops_permanently_at_first_corrupt_record() {
    let dir = tempdir().expect("tempdir");
    let wal_path = dir.path().join("incremental.wal");
    insert_terms_into_wal(&wal_path);
    corrupt_record_crc(&wal_path, 1);

    let mut recovery = IncrementalRecovery::new(&wal_path).expect("incremental recovery");
    let first = recovery
        .next_batch(1)
        .expect("first batch")
        .expect("durable prefix batch");
    assert_eq!(first.len(), 1);

    assert!(
        recovery.next_batch(10).expect("corrupt batch").is_none(),
        "corrupt record terminates the durable prefix"
    );
    assert!(
        recovery
            .next_batch(10)
            .expect("post-corruption batch")
            .is_none(),
        "later calls must not read suffix records after corruption"
    );
}

#[test]
fn byte_corruption_rebuild_uses_only_durable_wal_prefix() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("byte_prefix.part");
    {
        let mut trie = PersistentARTrie::<i32>::create(&path).expect("create byte trie");
        assert!(trie.insert_with_value("before", 1));
        assert!(trie.insert_with_value("corrupt", 2));
        assert!(trie.insert_with_value("after", 3));
        trie.sync().expect("sync byte WAL");
    }

    corrupt_record_crc(&path.with_extension("wal"), 1);
    corrupt_header_magic(&path);

    let (recovered, report) =
        PersistentARTrie::<i32>::open_with_recovery_config(&path, recovery_config())
            .expect("rebuild byte trie");

    assert_eq!(report.mode, RecoveryMode::RebuildFromWal);
    assert_eq!(report.records_replayed, 1);
    assert_eq!(recovered.get_value("before"), Some(1));
    assert!(!recovered.contains("corrupt"));
    assert!(!recovered.contains("after"));
}

#[test]
fn char_corruption_rebuild_uses_only_durable_wal_prefix() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("char_prefix.artc");
    {
        let mut trie = PersistentARTrieChar::<i32>::create(&path).expect("create char trie");
        trie.insert_with_value("before", 1).expect("before");
        trie.insert_with_value("corrupt", 2).expect("corrupt");
        trie.insert_with_value("after", 3).expect("after");
        trie.sync().expect("sync char WAL");
    }

    corrupt_record_crc(&path.with_extension("wal"), 1);
    corrupt_header_magic(&path);

    let (recovered, report) =
        PersistentARTrieChar::<i32>::open_with_recovery_config(&path, recovery_config())
            .expect("rebuild char trie");

    assert_eq!(report.mode, RecoveryMode::RebuildFromWal);
    assert_eq!(report.records_replayed, 1);
    assert_eq!(recovered.get("before").copied(), Some(1));
    assert!(!recovered.contains("corrupt"));
    assert!(!recovered.contains("after"));
}
