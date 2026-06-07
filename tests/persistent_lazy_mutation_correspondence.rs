//! Correspondence checks for persistent lazy mutation atomicity.
//!
//! These tests exercise the Rust side of `PersistentLazyMutationSpec.v`:
//! public char mutations must preflight lazy child loading before appending WAL,
//! lazy-load errors must be returned as `Err` instead of panicking, and successful
//! lazy mutations must still replay to the same map after reopen.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{SwizzledPtr, BLOCK_SIZE};
use libdictenstein::persistent_artrie_char::serialization_char::{
    deserialize_char_node_v2, DeserializationContext,
};
use libdictenstein::persistent_artrie_char::{
    ArenaSlot as CharArenaSlot, CharNodeArena, PersistentARTrieChar,
};
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;
use tempfile::tempdir;

const DESCRIPTOR_OFFSET: u64 = 64;
const DESCRIPTOR_LEN: usize = 18;

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

fn wal_len(path: &Path) -> u64 {
    fs::metadata(path.with_extension("wal"))
        .expect("WAL metadata")
        .len()
}

fn assert_char_value(dict: &PersistentARTrieChar<i32>, term: &str, value: i32) {
    assert!(dict.contains(term), "expected char term: {term}");
    assert_eq!(dict.get(term), Some(value), "char value for {term}");
}

fn build_checkpointed_char_trie(path: &Path) {
    let trie = PersistentARTrieChar::<i32>::create(path).expect("create char trie");
    // F2-migrate: Bucket B — this whole suite exercises the OWNED-tree lazy-load path
    // (`corrupt_first_lazy_char_child` navigates the on-disk owned arena; the reopen must
    // lazily fault owned children + surface their corruption). Under the lock-free
    // overlay the owned tree is empty/unused, so pin the Owned regime (stamps an Owned
    // WAL ⇒ every reopen stays owned, and `get`/`assert_char_value` read the owned tree).
    // No-op feature-off (`i32` is ineligible and stays owned anyway).
    trie.kill_switch_to_owned();
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
fn char_lazy_insert_error_returns_err_before_wal_append() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("char_lazy_insert_error.part");
    build_checkpointed_char_trie(&path);
    let corrupted_query = corrupt_first_lazy_char_child(&path);
    let new_term = format!("{corrupted_query}-new");
    let wal_len_before = wal_len(&path);

    let reopened = PersistentARTrieChar::<i32>::open(&path).expect("lazy reopen char trie");
    assert!(
        reopened.insert(&new_term).is_err(),
        "insert should surface lazy-load corruption"
    );
    assert_eq!(
        wal_len(&path),
        wal_len_before,
        "failed lazy insert must not append WAL"
    );

    let unaffected = if corrupted_query.starts_with('a') {
        ("beta", 3)
    } else {
        ("alpha", 1)
    };
    assert_char_value(&reopened, unaffected.0, unaffected.1);
    assert!(
        !reopened.contains(&new_term),
        "failed insert must not become visible"
    );
}

#[test]
fn char_lazy_value_insert_and_remove_errors_do_not_append_wal() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("char_lazy_value_remove_error.part");
    build_checkpointed_char_trie(&path);
    let corrupted_query = corrupt_first_lazy_char_child(&path);
    let new_term = format!("{corrupted_query}-valued");
    let wal_len_before = wal_len(&path);

    let reopened = PersistentARTrieChar::<i32>::open(&path).expect("lazy reopen char trie");
    assert!(
        reopened.insert_with_value(&new_term, 99).is_err(),
        "value insert should surface lazy-load corruption"
    );
    assert_eq!(
        wal_len(&path),
        wal_len_before,
        "failed lazy value insert must not append WAL"
    );

    assert!(
        reopened.remove(&corrupted_query).is_err(),
        "remove should surface lazy-load corruption"
    );
    assert_eq!(
        wal_len(&path),
        wal_len_before,
        "failed lazy remove must not append WAL"
    );
}

#[test]
fn char_lazy_duplicate_insert_is_noop_without_wal_append() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("char_lazy_duplicate_noop.part");
    build_checkpointed_char_trie(&path);
    let wal_len_before = wal_len(&path);

    let reopened = PersistentARTrieChar::<i32>::open(&path).expect("lazy reopen char trie");
    assert_eq!(
        reopened.insert("alpha").expect("duplicate insert"),
        false,
        "duplicate term-only insert should be a no-op"
    );
    assert_eq!(
        wal_len(&path),
        wal_len_before,
        "duplicate term-only insert must not append WAL"
    );
    assert_char_value(&reopened, "alpha", 1);
}

#[test]
fn char_successful_lazy_mutations_replay_after_reopen() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("char_lazy_success.part");
    build_checkpointed_char_trie(&path);

    {
        let reopened = PersistentARTrieChar::<i32>::open(&path).expect("lazy reopen char trie");
        assert_eq!(
            reopened
                .insert_with_value("alphabet", 10)
                .expect("insert alphabet"),
            true
        );
        assert_eq!(
            reopened
                .insert_with_value("alpha", 11)
                .expect("update alpha"),
            false
        );
        assert_eq!(reopened.insert("gamma").expect("insert gamma"), true);
        assert_eq!(reopened.remove("beta").expect("remove beta"), true);
        reopened.sync().expect("sync lazy mutations");
    }

    let reopened = PersistentARTrieChar::<i32>::open(&path).expect("reopen after lazy mutations");
    assert_char_value(&reopened, "alpha", 11);
    assert_char_value(&reopened, "alpine", 2);
    assert_char_value(&reopened, "alphabet", 10);
    assert!(reopened.contains("gamma"));
    assert!(
        !reopened.contains("beta"),
        "successful remove must replay after reopen"
    );
}
