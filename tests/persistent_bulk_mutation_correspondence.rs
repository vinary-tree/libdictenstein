//! Correspondence checks for persistent bulk-mutation recovery.
//!
//! These tests connect the prefix-deletion and checked-RMW proof obligations to
//! Rust behavior: char prefix deletion replays as a durable prefix of individual
//! WAL removes, lazy collection failures do not append WAL records, and numeric
//! increments fail before WAL/memory mutation on i64 overflow.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{
    PersistentARTrie, SwizzledPtr, WalHeader, WalReader, WalRecord, WalWriter, BLOCK_SIZE,
};
use libdictenstein::persistent_artrie_char::serialization_char::{
    deserialize_char_node_v2, DeserializationContext,
};
use libdictenstein::persistent_artrie_char::{
    ArenaSlot as CharArenaSlot, CharNodeArena, PersistentARTrieChar,
};
use libdictenstein::MappedDictionary;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tempfile::tempdir;

const DESCRIPTOR_OFFSET: u64 = 64;
const DESCRIPTOR_LEN: usize = 18;

fn bulk_reference() -> BTreeMap<String, i32> {
    [
        ("", 0),
        ("app", 1),
        ("apple", 2),
        ("application", 3),
        ("apply", 4),
        ("apt", 5),
        ("banana", 6),
        ("band", 7),
        ("emoji🙂", 8),
        ("日本", 9),
        ("日本語", 10),
    ]
    .into_iter()
    .map(|(term, value)| (term.to_string(), value))
    .collect()
}

fn wal_path(path: &Path) -> PathBuf {
    path.with_extension("wal")
}

fn wal_len(path: &Path) -> u64 {
    fs::metadata(wal_path(path))
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn seed_checkpointed_char_trie(path: &Path, reference: &BTreeMap<String, i32>) {
    let mut trie = PersistentARTrieChar::<i32>::create(path).expect("create char trie");
    for (term, value) in reference {
        trie.upsert(term, *value).expect("seed char trie");
    }
    trie.checkpoint().expect("checkpoint seed trie");
}

fn assert_char_map(path: &Path, expected: &BTreeMap<String, i32>) {
    let trie = PersistentARTrieChar::<i32>::open(path).expect("open char trie");
    assert_eq!(trie.len(), expected.len(), "char trie length");

    for (term, value) in expected {
        assert!(trie.contains(term), "expected term {term:?}");
        assert_eq!(trie.get(term).copied(), Some(*value), "value for {term:?}");
    }

    for term in bulk_reference().keys() {
        if !expected.contains_key(term) {
            assert!(
                !trie.contains(term),
                "removed term stayed visible: {term:?}"
            );
        }
    }
}

fn wal_record_spans(wal_bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut offset = WalHeader::SIZE;
    let mut spans = Vec::new();

    assert!(
        wal_bytes.len() >= WalHeader::SIZE,
        "WAL bytes must include a header"
    );

    while offset < wal_bytes.len() {
        assert!(
            offset + WalWriter::RECORD_HEADER_SIZE <= wal_bytes.len(),
            "WAL fixture ended inside a record header"
        );

        let length = u32::from_le_bytes([
            wal_bytes[offset + 4],
            wal_bytes[offset + 5],
            wal_bytes[offset + 6],
            wal_bytes[offset + 7],
        ]) as usize;
        assert!(
            length >= WalWriter::RECORD_HEADER_SIZE,
            "WAL fixture record length is too small"
        );

        let end = offset
            .checked_add(length)
            .expect("WAL record end offset overflowed");
        assert!(end <= wal_bytes.len(), "WAL fixture record exceeds file");

        spans.push((offset, end));
        offset = end;
    }

    spans
}

fn wal_records(path: &Path) -> Vec<WalRecord> {
    WalReader::new(&wal_path(path))
        .expect("open WAL reader")
        .iter()
        .map(|record| record.expect("read WAL record").1)
        .collect()
}

fn write_case(base_file_bytes: &[u8], wal_bytes: &[u8], parent: &Path, name: &str) -> PathBuf {
    let case_dir = parent.join(name);
    fs::create_dir_all(&case_dir).expect("create case directory");
    let case_path = case_dir.join("case.artc");
    fs::write(&case_path, base_file_bytes).expect("write case data file");
    fs::write(wal_path(&case_path), wal_bytes).expect("write case WAL");
    case_path
}

#[test]
fn char_remove_prefix_batched_survives_reopen_without_checkpoint_for_batch_sizes() {
    for batch_size in [0usize, 1, 2, 1024] {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join(format!("batch_{batch_size}.artc"));
        let mut expected = bulk_reference();
        let expected_removed = expected
            .keys()
            .filter(|term| term.starts_with("app"))
            .count();

        seed_checkpointed_char_trie(&path, &expected);

        {
            let mut trie = PersistentARTrieChar::<i32>::open(&path).expect("open char trie");
            let removed = trie
                .remove_prefix_batched("app", batch_size)
                .expect("remove prefix");
            assert_eq!(
                removed, expected_removed,
                "removed count for batch size {batch_size}"
            );
            trie.sync().expect("sync delete WAL");
        }

        expected.retain(|term, _| !term.starts_with("app"));
        assert_char_map(&path, &expected);
    }
}

#[test]
fn char_remove_prefix_batched_replays_every_durable_wal_prefix() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("durable_prefix.artc");
    let reference = bulk_reference();
    seed_checkpointed_char_trie(&path, &reference);

    let base_file_bytes = fs::read(&path).expect("read checkpointed data file");
    let base_wal_bytes = fs::read(wal_path(&path)).expect("read checkpointed WAL");
    let base_record_count = wal_record_spans(&base_wal_bytes).len();

    {
        let mut trie = PersistentARTrieChar::<i32>::open(&path).expect("open char trie");
        let expected_removed = reference
            .keys()
            .filter(|term| term.starts_with("app"))
            .count();
        assert_eq!(
            trie.remove_prefix_batched("app", 1)
                .expect("remove prefix one at a time"),
            expected_removed
        );
        trie.sync().expect("sync remove WAL");
    }

    let final_wal_bytes = fs::read(wal_path(&path)).expect("read final WAL");
    let spans = wal_record_spans(&final_wal_bytes);
    let delete_terms: Vec<String> = wal_records(&path)
        .into_iter()
        .skip(base_record_count)
        .filter_map(|record| match record {
            WalRecord::Remove { term } => {
                Some(String::from_utf8(term).expect("test terms are valid UTF-8"))
            }
            other => panic!("unexpected bulk-delete WAL record after baseline: {other:?}"),
        })
        .collect();

    assert!(
        !delete_terms.is_empty(),
        "bulk prefix delete must emit remove records"
    );
    assert!(
        delete_terms.iter().all(|term| term.starts_with("app")),
        "bulk delete WAL must not remove terms outside the requested prefix"
    );

    for removed_count in 0..=delete_terms.len() {
        let wal_end = if removed_count == 0 {
            base_wal_bytes.len()
        } else {
            spans[base_record_count + removed_count - 1].1
        };
        let case_path = write_case(
            &base_file_bytes,
            &final_wal_bytes[..wal_end],
            dir.path(),
            &format!("prefix_{removed_count}"),
        );

        let mut expected = reference.clone();
        for term in delete_terms.iter().take(removed_count) {
            expected.remove(term);
        }
        assert_char_map(&case_path, &expected);
    }
}

fn read_descriptor(path: &Path) -> [u8; DESCRIPTOR_LEN] {
    let mut file = File::open(path).expect("open trie file");
    file.seek(SeekFrom::Start(DESCRIPTOR_OFFSET))
        .expect("seek descriptor");
    let mut descriptor = [0u8; DESCRIPTOR_LEN];
    file.read_exact(&mut descriptor).expect("read descriptor");
    descriptor
}

fn read_block(path: &Path, block_id: u32) -> Vec<u8> {
    let mut file = File::open(path).expect("open trie file");
    file.seek(SeekFrom::Start(block_id as u64 * BLOCK_SIZE as u64))
        .expect("seek block");
    let mut block = vec![0u8; BLOCK_SIZE];
    file.read_exact(&mut block).expect("read block");
    block
}

fn write_block(path: &Path, block_id: u32, block: &[u8]) {
    assert_eq!(block.len(), BLOCK_SIZE);
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open trie file for block write");
    file.seek(SeekFrom::Start(block_id as u64 * BLOCK_SIZE as u64))
        .expect("seek block");
    file.write_all(block).expect("write block");
    file.sync_all().expect("sync block");
}

fn build_checkpointed_lazy_fixture(path: &Path) {
    let mut trie = PersistentARTrieChar::<i32>::create(path).expect("create char trie");
    trie.insert_with_value("alpha", 1).expect("insert alpha");
    trie.insert_with_value("alpine", 2).expect("insert alpine");
    trie.insert_with_value("beta", 3).expect("insert beta");
    trie.checkpoint().expect("checkpoint char trie");
}

fn corrupt_first_lazy_char_child(path: &Path) -> String {
    let descriptor = read_descriptor(path);
    let root_ptr = u64::from_le_bytes(descriptor[10..18].try_into().unwrap());
    let root_location = SwizzledPtr::from_raw(root_ptr)
        .disk_location()
        .expect("root pointer is on disk");

    let root_block = read_block(path, root_location.block_id);
    let root_arena =
        CharNodeArena::from_bytes(&root_block, root_location.block_id).expect("load root arena");
    let root_data = root_arena
        .read(root_location.offset)
        .expect("read root slot");

    let root_slot = CharArenaSlot::new(root_location.block_id - 1, root_location.offset);
    let ctx = DeserializationContext::new(root_slot);
    let mut cursor = Cursor::new(root_data);
    let root_node = deserialize_char_node_v2(&mut cursor, &ctx).expect("deserialize root node");
    let (edge, child_ptr) = root_node
        .iter_children()
        .next()
        .expect("root should have a child pointer");
    let edge_char = char::from_u32(edge).expect("valid test edge");
    let query = match edge_char {
        'a' => "alpha",
        'b' => "beta",
        other => panic!("unexpected root edge in test fixture: {other}"),
    }
    .to_string();

    let child_location = child_ptr
        .disk_location()
        .expect("child pointer should still be on disk");
    let child_block = read_block(path, child_location.block_id);
    let mut child_arena =
        CharNodeArena::from_bytes_unchecked(&child_block, child_location.block_id)
            .expect("load child arena unchecked");
    let child_len = child_arena
        .read(child_location.offset)
        .expect("read child slot")
        .len();
    child_arena
        .update(child_location.offset, &vec![0u8; child_len])
        .expect("overwrite child slot with malformed payload");
    child_arena.finalize_checksums();
    write_block(path, child_location.block_id, child_arena.as_bytes());

    query
}

#[test]
fn char_remove_prefix_lazy_collection_error_preserves_wal_and_unaffected_terms() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("lazy_prefix_failure.artc");
    build_checkpointed_lazy_fixture(&path);
    let corrupted_query = corrupt_first_lazy_char_child(&path);
    let prefix = corrupted_query
        .chars()
        .next()
        .expect("corrupted query is non-empty")
        .to_string();
    let before_wal = wal_len(&path);

    let mut reopened = PersistentARTrieChar::<i32>::open(&path).expect("lazy reopen char trie");
    assert!(
        reopened.remove_prefix_batched(&prefix, 1).is_err(),
        "prefix removal should surface lazy collection corruption"
    );
    assert_eq!(
        wal_len(&path),
        before_wal,
        "failed prefix collection must not append WAL records"
    );

    let unaffected = if prefix == "a" {
        ("beta", 3)
    } else {
        ("alpha", 1)
    };
    assert!(reopened.contains(unaffected.0), "unaffected term remains");
    assert_eq!(reopened.get(unaffected.0).copied(), Some(unaffected.1));
}

#[test]
fn byte_and_char_checked_increment_overflow_preserves_wal_and_memory() {
    let dir = tempdir().expect("tempdir");
    let byte_path = dir.path().join("byte_overflow.part");
    let char_path = dir.path().join("char_overflow.artc");

    let mut byte_trie = PersistentARTrie::<i64>::create(&byte_path).expect("create byte trie");
    assert!(byte_trie.upsert("high", i64::MAX).expect("upsert high"));
    assert!(byte_trie.upsert("low", i64::MIN).expect("upsert low"));
    let before_byte_wal = wal_len(&byte_path);

    assert!(byte_trie.increment("high", 1).is_err());
    assert!(byte_trie.fetch_add("low", -1).is_err());
    assert_eq!(byte_trie.get_value("high"), Some(i64::MAX));
    assert_eq!(byte_trie.get_value("low"), Some(i64::MIN));
    assert_eq!(
        wal_len(&byte_path),
        before_byte_wal,
        "byte overflow must not append WAL"
    );

    let mut char_trie = PersistentARTrieChar::<i64>::create(&char_path).expect("create char trie");
    assert!(char_trie.upsert("high", i64::MAX).expect("upsert high"));
    assert!(char_trie.upsert("low", i64::MIN).expect("upsert low"));
    let before_char_wal = wal_len(&char_path);

    assert!(char_trie.increment("high", 1).is_err());
    assert!(char_trie.fetch_add("low", -1).is_err());
    assert_eq!(char_trie.get("high").copied(), Some(i64::MAX));
    assert_eq!(char_trie.get("low").copied(), Some(i64::MIN));
    assert_eq!(
        wal_len(&char_path),
        before_char_wal,
        "char overflow must not append WAL"
    );
}
