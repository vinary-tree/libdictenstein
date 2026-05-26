//! Public read traversal correspondence checks.
//!
//! These tests exercise the Rust side of `PublicReadSnapshotTraversal.tla` and
//! `PersistentReadTraversalSpec.v`: public iterators, prefix iterators, and
//! vocab traversal must return exactly the visible snapshot after
//! checkpoint/reopen, and lazy-load failures must fail closed without appending
//! WAL records.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{PersistentARTrie, SwizzledPtr, BLOCK_SIZE};
use libdictenstein::persistent_artrie_char::serialization_char::{
    deserialize_char_node_v2, DeserializationContext,
};
use libdictenstein::persistent_artrie_char::{
    ArenaSlot as CharArenaSlot, CharNodeArena, PersistentARTrieChar,
};
use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;
use tempfile::tempdir;

const DESCRIPTOR_OFFSET: u64 = 64;
const DESCRIPTOR_LEN: usize = 18;

fn byte_fixture() -> BTreeMap<String, Option<i64>> {
    BTreeMap::from([
        ("alpha".to_string(), Some(1)),
        ("alpine".to_string(), Some(2)),
        ("application".to_string(), Some(3)),
        ("banana".to_string(), Some(4)),
        ("band".to_string(), Some(5)),
        ("emoji😀".to_string(), Some(6)),
        ("日本語".to_string(), Some(7)),
        ("term-only".to_string(), None),
    ])
}

fn char_fixture() -> BTreeMap<String, i64> {
    BTreeMap::from([
        ("alpha".to_string(), 10),
        ("alpine".to_string(), 20),
        ("application".to_string(), 30),
        ("banana".to_string(), 40),
        ("band".to_string(), 50),
        ("café".to_string(), 60),
        ("emoji😀".to_string(), 70),
        ("日本語".to_string(), 80),
    ])
}

fn terms_with_prefix<V: Clone>(map: &BTreeMap<String, V>, prefix: &str) -> BTreeSet<String> {
    map.keys()
        .filter(|term| term.starts_with(prefix))
        .cloned()
        .collect()
}

fn values_with_prefix(map: &BTreeMap<String, Option<i64>>, prefix: &str) -> BTreeMap<String, i64> {
    map.iter()
        .filter(|(term, value)| term.starts_with(prefix) && value.is_some())
        .filter_map(|(term, value)| value.map(|v| (term.clone(), v)))
        .collect()
}

fn string_set<I>(terms: I) -> BTreeSet<String>
where
    I: IntoIterator<Item = String>,
{
    terms.into_iter().collect()
}

fn byte_set<I>(terms: I) -> BTreeSet<String>
where
    I: IntoIterator<Item = Vec<u8>>,
{
    terms
        .into_iter()
        .map(|term| String::from_utf8(term).expect("UTF-8 fixture term"))
        .collect()
}

fn byte_value_map<I>(entries: I) -> BTreeMap<String, Option<i64>>
where
    I: IntoIterator<Item = (Vec<u8>, Option<i64>)>,
{
    entries
        .into_iter()
        .map(|(term, value)| (String::from_utf8(term).expect("UTF-8 fixture term"), value))
        .collect()
}

fn byte_present_value_map<I>(entries: I) -> BTreeMap<String, i64>
where
    I: IntoIterator<Item = (Vec<u8>, i64)>,
{
    entries
        .into_iter()
        .map(|(term, value)| (String::from_utf8(term).expect("UTF-8 fixture term"), value))
        .collect()
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

fn wal_len(path: &Path) -> u64 {
    fs::metadata(path.with_extension("wal"))
        .expect("WAL metadata")
        .len()
}

fn build_checkpointed_char_trie(path: &Path) {
    let mut trie = PersistentARTrieChar::<i64>::create(path).expect("create char trie");
    for (term, value) in char_fixture() {
        trie.insert_with_value(&term, value)
            .expect("insert char value");
    }
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
        'b' => "banana",
        'c' => "café",
        'e' => "emoji😀",
        '日' => "日本語",
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
fn byte_public_iterators_reopen_to_exact_snapshot() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("byte_read_snapshot.part");
    let reference = byte_fixture();

    {
        let mut trie = PersistentARTrie::<i64>::create(&path).expect("create byte trie");
        for (term, value) in &reference {
            match value {
                Some(value) => assert!(trie.insert_with_value(term, *value)),
                None => assert!(trie.insert(term)),
            }
        }
        trie.checkpoint().expect("checkpoint byte trie");
    }

    let reopened = PersistentARTrie::<i64>::open(&path).expect("reopen byte trie");

    let expected_terms: BTreeSet<String> = reference.keys().cloned().collect();
    assert_eq!(byte_set(reopened.iter()), expected_terms);
    assert_eq!(byte_value_map(reopened.iter_with_values()), reference);

    for prefix in ["", "alp", "app", "ban", "emoji", "日本", "missing"] {
        let expected_prefix: BTreeSet<String> = reference
            .keys()
            .filter(|term| term.starts_with(prefix))
            .cloned()
            .collect();
        let actual_terms = reopened
            .iter_prefix(prefix.as_bytes())
            .map(byte_set)
            .unwrap_or_default();
        assert_eq!(actual_terms, expected_prefix, "byte prefix {prefix:?}");

        let actual_values = reopened
            .iter_prefix_with_values(prefix.as_bytes())
            .map(byte_present_value_map)
            .unwrap_or_default();
        assert_eq!(
            actual_values,
            values_with_prefix(&reference, prefix),
            "byte prefix values {prefix:?}"
        );
    }
}

#[test]
fn char_public_iterators_reopen_lazy_snapshot_exactly() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("char_read_snapshot.part");
    let reference = char_fixture();

    {
        let mut trie = PersistentARTrieChar::<i64>::create(&path).expect("create char trie");
        for (term, value) in &reference {
            trie.insert_with_value(term, *value)
                .expect("insert char value");
        }
        trie.checkpoint().expect("checkpoint char trie");
    }

    let reopened = PersistentARTrieChar::<i64>::open(&path).expect("lazy reopen char trie");

    let expected_terms: BTreeSet<String> = reference.keys().cloned().collect();
    assert_eq!(string_set(reopened.iter()), expected_terms);
    assert_eq!(
        reopened.iter_with_values().collect::<BTreeMap<_, _>>(),
        reference
    );

    for prefix in ["", "alp", "app", "ban", "café", "emoji", "日本", "missing"] {
        let actual_terms = reopened
            .iter_prefix(prefix)
            .expect("char iter_prefix")
            .map(string_set)
            .unwrap_or_default();
        assert_eq!(
            actual_terms,
            terms_with_prefix(&reference, prefix),
            "char prefix {prefix:?}"
        );

        let actual_values = reopened
            .iter_prefix_with_values(prefix)
            .expect("char iter_prefix_with_values")
            .unwrap_or_default()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let expected_values: BTreeMap<String, i64> = reference
            .iter()
            .filter(|(term, _)| term.starts_with(prefix))
            .map(|(term, value)| (term.clone(), *value))
            .collect();
        assert_eq!(actual_values, expected_values, "char values {prefix:?}");
    }
}

#[test]
fn char_lazy_traversal_failure_is_error_and_does_not_append_wal() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("char_lazy_read_failure.part");
    build_checkpointed_char_trie(&path);
    let corrupted_query = corrupt_first_lazy_char_child(&path);
    let wal_len_before = wal_len(&path);

    let reopened = PersistentARTrieChar::<i64>::open(&path).expect("lazy reopen char trie");
    assert!(
        reopened.iter_prefix(&corrupted_query).is_err(),
        "prefix traversal should surface lazy-load corruption"
    );
    assert!(
        reopened.iter_prefix_with_values(&corrupted_query).is_err(),
        "valued prefix traversal should surface lazy-load corruption"
    );
    assert_eq!(
        wal_len(&path),
        wal_len_before,
        "failed public read traversal must not append WAL"
    );

    let public_snapshot = string_set(reopened.iter());
    let reference_terms: BTreeSet<String> = char_fixture().keys().cloned().collect();
    assert!(
        public_snapshot.is_subset(&reference_terms),
        "fail-closed public iteration must not fabricate terms"
    );
}

#[test]
fn vocab_public_iterators_reopen_to_exact_snapshot() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("vocab_read_snapshot.vocab");
    let terms = ["alpha", "alpine", "banana", "βeta", "emoji😀", "日本語"];

    {
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab trie");
        for (index, term) in terms.iter().enumerate() {
            assert_eq!(vocab.insert(term).expect("insert vocab term"), index as u64);
        }
        assert_eq!(vocab.insert("alpha").expect("duplicate vocab term"), 0);
        vocab.checkpoint().expect("checkpoint vocab trie");
    }

    let reopened = PersistentVocabARTrie::open(&path).expect("reopen vocab trie");
    let expected: BTreeSet<String> = terms.iter().map(|term| (*term).to_string()).collect();
    assert_eq!(string_set(reopened.iter_terms()), expected);
    assert_eq!(
        string_set(reopened.iter_terms_with_prefix("alp")),
        BTreeSet::from(["alpha".to_string(), "alpine".to_string()])
    );
    assert_eq!(
        string_set(reopened.iter_terms_with_prefix("missing")),
        BTreeSet::new()
    );

    for (index, term) in terms.iter().enumerate() {
        assert_eq!(reopened.get_index(term), Some(index as u64));
        assert_eq!(reopened.get_term(index as u64), Some((*term).to_string()));
    }
}
