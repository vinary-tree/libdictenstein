//! Public read traversal correspondence checks.
//!
//! These tests exercise the Rust side of `PublicReadSnapshotTraversal.tla` and
//! `PersistentReadTraversalSpec.v`: public iterators, prefix iterators, and
//! vocab traversal must return exactly the visible snapshot after
//! checkpoint/reopen, and lazy-load failures must fail closed without appending
//! WAL records.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::BLOCK_SIZE;
use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
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
