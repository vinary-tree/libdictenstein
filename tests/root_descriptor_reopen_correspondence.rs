//! Root-descriptor and reopen correspondence checks for the persistent byte and
//! char ART backends.
//!
//! These tests exercise the Rust side of `RootDescriptorReopenSpec.v`: valid
//! root descriptors reopen to the checkpointed map, malformed descriptors and
//! invalid arena counts do not trust checkpoint skip thresholds, and char
//! lazy-load errors are exposed by `try_*` while public reads fail closed.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{
    PersistentARTrie, SwizzledPtr, WalRecord, WalWriter, BLOCK_SIZE,
};
use libdictenstein::persistent_artrie_char::serialization_char::{
    deserialize_char_node_v2, DeserializationContext,
};
use libdictenstein::persistent_artrie_char::{
    ArenaSlot as CharArenaSlot, CharNodeArena, PersistentARTrieChar,
};
use libdictenstein::serialization::bincode_compat;
use libdictenstein::{Dictionary, MappedDictionary};
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;
use tempfile::tempdir;

const DESCRIPTOR_OFFSET: u64 = 64;
const DESCRIPTOR_LEN: usize = 18;
const ROOT_TYPE_UNKNOWN: u8 = 99;

fn read_descriptor(path: &Path) -> [u8; DESCRIPTOR_LEN] {
    let mut file = File::open(path).expect("open trie file");
    file.seek(SeekFrom::Start(DESCRIPTOR_OFFSET))
        .expect("seek descriptor");
    let mut descriptor = [0u8; DESCRIPTOR_LEN];
    file.read_exact(&mut descriptor).expect("read descriptor");
    descriptor
}

fn write_descriptor_byte(path: &Path, offset: usize, byte: u8) {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open trie file for descriptor byte write");
    file.seek(SeekFrom::Start(DESCRIPTOR_OFFSET + offset as u64))
        .expect("seek descriptor byte");
    file.write_all(&[byte]).expect("write descriptor byte");
    file.sync_all().expect("sync descriptor byte");
}

fn write_descriptor_arena_count(path: &Path, arena_count: u32) {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open trie file for arena_count write");
    file.seek(SeekFrom::Start(DESCRIPTOR_OFFSET + 6))
        .expect("seek descriptor arena_count");
    file.write_all(&arena_count.to_le_bytes())
        .expect("write descriptor arena_count");
    file.sync_all().expect("sync descriptor arena_count");
}

fn replace_wal_with_checkpointed_insert(path: &Path, term: &str, value: i32) {
    let wal_path = path.with_extension("wal");
    let _ = fs::remove_file(&wal_path);

    let value_bytes = bincode_compat::serialize(&value).expect("serialize WAL value");
    let writer = WalWriter::create(&wal_path).expect("create replacement WAL");
    let insert_lsn = writer
        .append(WalRecord::Insert {
            term: term.as_bytes().to_vec(),
            value: Some(value_bytes),
        })
        .expect("append insert");
    let checkpoint_lsn = writer
        .append(WalRecord::Checkpoint {
            checkpoint_lsn: insert_lsn + 1,
            timestamp: 1,
        })
        .expect("append checkpoint");
    assert_eq!(insert_lsn, 1);
    assert_eq!(checkpoint_lsn, 2);
    writer.sync().expect("sync replacement WAL");
}

fn assert_byte_value(dict: &PersistentARTrie<i32>, term: &str, value: i32) {
    assert!(dict.contains(term), "expected byte term: {term}");
    assert_eq!(dict.get_value(term), Some(value), "byte value for {term}");
}

fn assert_char_value(dict: &PersistentARTrieChar<i32>, term: &str, value: i32) {
    assert!(dict.contains(term), "expected char term: {term}");
    // F2-migrate: Bucket A — `get()` lends no reference out of the lock-free overlay
    // (returns None under `route_overlay()`); read the value via `get_value` (owned
    // `Option<V>`, overlay-routed). In owned mode it falls through to the owned tree, so
    // this is correct for both the overlay-routed and owned-pinned callers.
    assert_eq!(dict.get_value(term), Some(value), "char value for {term}");
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

fn storage_block_count(path: &Path) -> u32 {
    let bytes = fs::metadata(path).expect("metadata").len();
    (bytes / BLOCK_SIZE as u64) as u32
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
    assert!(
        child_location.node_type.is_char_level(),
        "char root child should be a char node pointer"
    );
    let child_block = read_block(path, child_location.block_id);
    let mut child_arena =
        CharNodeArena::from_bytes_unchecked(&child_block, child_location.block_id)
            .expect("load child arena unchecked");
    let child_len = child_arena
        .read(child_location.offset)
        .expect("read child slot")
        .len();
    let corrupt_payload = vec![0u8; child_len];
    child_arena
        .update(child_location.offset, &corrupt_payload)
        .expect("overwrite child slot with malformed payload");
    child_arena.finalize_checksums();
    write_block(path, child_location.block_id, child_arena.as_bytes());

    query
}

#[test]
fn byte_descriptor_reopen_roundtrip_preserves_reference_map() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("byte_roundtrip.part");

    {
        let mut trie = PersistentARTrie::<i32>::create(&path).expect("create byte trie");
        assert!(trie.insert_with_value("alpha", 10));
        assert!(trie.insert_with_value("beta", 20));
        assert!(trie.insert_with_value("alpine", 30));
        trie.checkpoint().expect("checkpoint byte trie");
    }

    let reopened = PersistentARTrie::<i32>::open(&path).expect("reopen byte trie");
    assert_byte_value(&reopened, "alpha", 10);
    assert_byte_value(&reopened, "beta", 20);
    assert_byte_value(&reopened, "alpine", 30);
    assert!(!reopened.contains("missing"));
}

#[test]
fn char_descriptor_reopen_roundtrip_preserves_reference_map_across_depths() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("char_roundtrip.part");

    {
        let mut trie = PersistentARTrieChar::<i32>::create(&path).expect("create char trie");
        trie.insert_with_value("alpha", 1).expect("insert alpha");
        trie.insert_with_value("beta", 2).expect("insert beta");
        trie.insert_with_value("café", 3).expect("insert cafe");
        trie.insert_with_value("東京", 4).expect("insert tokyo");
        trie.checkpoint().expect("checkpoint char trie");
    }

    {
        let reopened = PersistentARTrieChar::<i32>::open(&path).expect("lazy reopen char trie");
        assert_char_value(&reopened, "alpha", 1);
        assert_char_value(&reopened, "beta", 2);
        assert_char_value(&reopened, "café", 3);
        assert_char_value(&reopened, "東京", 4);
        assert!(!reopened.contains("missing"));
    }

    {
        let reopened = PersistentARTrieChar::<i32>::open_with_depth(&path, Some(usize::MAX))
            .expect("eager reopen char trie");
        assert_char_value(&reopened, "alpha", 1);
        assert_char_value(&reopened, "beta", 2);
        assert_char_value(&reopened, "café", 3);
        assert_char_value(&reopened, "東京", 4);
    }
}

#[test]
fn byte_invalid_root_descriptor_replays_wal_without_checkpoint_skip() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("byte_bad_descriptor.part");

    {
        let mut trie = PersistentARTrie::<i32>::create(&path).expect("create byte trie");
        assert!(trie.insert_with_value("disk-only", 1));
        trie.checkpoint().expect("checkpoint byte trie");
    }

    replace_wal_with_checkpointed_insert(&path, "wal-only", 7);
    write_descriptor_byte(&path, 0, ROOT_TYPE_UNKNOWN);

    let reopened = PersistentARTrie::<i32>::open(&path).expect("reopen through WAL");
    assert_byte_value(&reopened, "wal-only", 7);
    assert!(
        !reopened.contains("disk-only"),
        "malformed descriptor must not fabricate checkpointed disk contents"
    );
}

#[test]
fn char_invalid_root_descriptor_replays_wal_without_checkpoint_skip() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("char_bad_descriptor.part");

    {
        let mut trie = PersistentARTrieChar::<i32>::create(&path).expect("create char trie");
        trie.insert_with_value("disk-only", 1)
            .expect("insert disk-only");
        trie.checkpoint().expect("checkpoint char trie");
    }

    replace_wal_with_checkpointed_insert(&path, "wal-only", 7);
    write_descriptor_byte(&path, 0, ROOT_TYPE_UNKNOWN);

    let reopened = PersistentARTrieChar::<i32>::open(&path).expect("reopen through WAL");
    assert_char_value(&reopened, "wal-only", 7);
    assert!(
        !reopened.contains("disk-only"),
        "malformed descriptor must not fabricate checkpointed disk contents"
    );
}

#[test]
fn invalid_arena_count_replays_wal_instead_of_trusting_checkpoint() {
    let dir = tempdir().expect("tempdir");
    let byte_path = dir.path().join("byte_bad_arena_count.part");
    let char_path = dir.path().join("char_bad_arena_count.part");

    {
        let mut trie = PersistentARTrie::<i32>::create(&byte_path).expect("create byte trie");
        assert!(trie.insert_with_value("disk-only", 1));
        trie.checkpoint().expect("checkpoint byte trie");
    }
    replace_wal_with_checkpointed_insert(&byte_path, "wal-only", 11);
    write_descriptor_arena_count(&byte_path, storage_block_count(&byte_path));
    let reopened = PersistentARTrie::<i32>::open(&byte_path).expect("byte WAL fallback");
    assert_byte_value(&reopened, "wal-only", 11);

    {
        let mut trie = PersistentARTrieChar::<i32>::create(&char_path).expect("create char trie");
        trie.insert_with_value("disk-only", 1)
            .expect("insert disk-only");
        trie.checkpoint().expect("checkpoint char trie");
    }
    replace_wal_with_checkpointed_insert(&char_path, "wal-only", 12);
    write_descriptor_arena_count(&char_path, storage_block_count(&char_path));
    let reopened = PersistentARTrieChar::<i32>::open(&char_path).expect("char WAL fallback");
    assert_char_value(&reopened, "wal-only", 12);
}

#[test]
fn char_lazy_load_errors_are_result_errors_and_public_reads_fail_closed() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("char_lazy_corrupt.part");

    {
        let mut trie = PersistentARTrieChar::<i32>::create(&path).expect("create char trie");
        // F2-migrate: Bucket B — `corrupt_first_lazy_char_child` corrupts an on-disk
        // OWNED lazy child; the reopen must lazily fault owned children and surface the
        // corruption as an error. Pin the Owned regime so the owned-tree layout exists on
        // disk and the reopen stays owned. No-op feature-off.
        trie.kill_switch_to_owned();
        trie.insert_with_value("alpha", 1).expect("insert alpha");
        trie.insert_with_value("alpine", 2).expect("insert alpine");
        trie.insert_with_value("beta", 3).expect("insert beta");
        trie.checkpoint().expect("checkpoint char trie");
    }

    let corrupted_query = corrupt_first_lazy_char_child(&path);
    let reopened = PersistentARTrieChar::<i32>::open(&path).expect("lazy reopen char trie");

    assert!(
        reopened.try_contains(&corrupted_query).is_err(),
        "try_contains should surface lazy-load corruption"
    );
    assert!(
        reopened.try_get(&corrupted_query).is_err(),
        "try_get should surface lazy-load corruption"
    );
    assert!(
        !reopened.contains(&corrupted_query),
        "public contains should fail closed"
    );
    assert_eq!(
        reopened.get(&corrupted_query),
        None,
        "public get should fail closed"
    );
}
