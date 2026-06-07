//! Correspondence checks for the persistent WAL write-atomicity model.
//!
//! The model requires value serialization and WAL append to happen before any
//! visible in-memory mutation. These tests inject serialization failures into
//! value-carrying writes and verify that the trie fails closed.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::value::DictionaryValue;
use libdictenstein::{Dictionary, MappedDictionary};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::Path;
use tempfile::TempDir;

#[derive(Clone, Debug, Default, PartialEq)]
struct FailingSerializeValue(i32);

impl Serialize for FailingSerializeValue {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Err(<S::Error as serde::ser::Error>::custom(
            "injected serialization failure",
        ))
    }
}

impl<'de> Deserialize<'de> for FailingSerializeValue {
    fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self::default())
    }
}

impl DictionaryValue for FailingSerializeValue {}

#[derive(Clone, Debug, Default, PartialEq)]
struct MaybeFailValue {
    value: i32,
    fail_serialize: bool,
}

impl MaybeFailValue {
    fn ok(value: i32) -> Self {
        Self {
            value,
            fail_serialize: false,
        }
    }

    fn failing(value: i32) -> Self {
        Self {
            value,
            fail_serialize: true,
        }
    }
}

impl Serialize for MaybeFailValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.fail_serialize {
            return Err(<S::Error as serde::ser::Error>::custom(
                "injected serialization failure",
            ));
        }

        self.value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MaybeFailValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self::ok(i32::deserialize(deserializer)?))
    }
}

impl DictionaryValue for MaybeFailValue {}

fn wal_len(path: &Path) -> u64 {
    std::fs::metadata(path.with_extension("wal"))
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

#[test]
fn byte_value_insert_serialization_failure_preserves_memory_and_wal() {
    let temp_dir = TempDir::new().expect("tempdir");
    let path = temp_dir.path().join("byte_insert.part");
    let mut trie =
        PersistentARTrie::<FailingSerializeValue>::create(&path).expect("create byte trie");
    let before_wal = wal_len(&path);

    assert!(!trie.insert_with_value("bad", FailingSerializeValue(7)));
    assert_eq!(
        trie.insert_batch(&[("also_bad".to_string(), Some(FailingSerializeValue(8)))]),
        0
    );

    assert!(!trie.contains("bad"));
    assert!(!trie.contains("also_bad"));
    assert_eq!(wal_len(&path), before_wal);
}

#[test]
fn byte_atomic_serialization_failures_preserve_memory_and_wal() {
    let temp_dir = TempDir::new().expect("tempdir");
    let path = temp_dir.path().join("byte_atomic.part");
    let mut trie = PersistentARTrie::<MaybeFailValue>::create(&path).expect("create byte trie");

    assert!(trie.insert_with_value("key", MaybeFailValue::ok(1)));
    let before_wal = wal_len(&path);

    assert!(trie.upsert("bad", MaybeFailValue::failing(2)).is_err());
    assert!(trie
        .get_or_insert("default", MaybeFailValue::failing(3))
        .is_err());
    assert!(trie
        .compare_and_swap(
            "key",
            Some(MaybeFailValue::failing(1)),
            MaybeFailValue::ok(4),
        )
        .is_err());

    assert!(!trie.contains("bad"));
    assert!(!trie.contains("default"));
    assert_eq!(trie.get_value("key").map(|value| value.value), Some(1));
    assert_eq!(wal_len(&path), before_wal);
}

#[test]
fn byte_document_commit_serialization_failure_preserves_memory_and_wal() {
    let temp_dir = TempDir::new().expect("tempdir");
    let path = temp_dir.path().join("byte_document.part");
    let mut trie =
        PersistentARTrie::<FailingSerializeValue>::create(&path).expect("create byte trie");
    let mut tx = trie.begin_document("doc").expect("begin tx");
    trie.tx_insert(&mut tx, "bad", Some(FailingSerializeValue(9)));
    let before_commit_wal = wal_len(&path);

    assert!(trie.commit_document(tx).is_err());
    assert!(!trie.contains("bad"));
    assert_eq!(wal_len(&path), before_commit_wal);

    drop(trie);
    let reopened =
        PersistentARTrie::<FailingSerializeValue>::open(&path).expect("reopen byte trie");
    assert!(!reopened.contains("bad"));
}

#[test]
fn byte_atomic_writes_replay_after_reopen() {
    let temp_dir = TempDir::new().expect("tempdir");
    let path = temp_dir.path().join("byte_replay.part");
    {
        let mut trie = PersistentARTrie::<i64>::create(&path).expect("create byte trie");
        // **M4b REFRAME.** A fresh `create::<i64>()` now create-flips to the overlay,
        // but this test exercises `compare_and_swap` (value-level CAS-with-expected),
        // which the byte overlay REJECTS (no value-level CAS primitive on the overlay).
        // Force the owned regime with the kill-switch (the M4b CAS precedent); the
        // kill-switch restamps the WAL Owned on the fresh trie, so the reopen below
        // stays owned and `get_value` reads the owned CAS result.
        trie.kill_switch_to_owned();
        assert!(trie.upsert("count", 10).expect("upsert"));
        assert_eq!(trie.increment("count", 5).expect("increment"), 15);
        assert_eq!(trie.fetch_add("count", 2).expect("fetch_add"), 15);
        assert!(trie.compare_and_swap("count", Some(17), 20).expect("cas"));
        assert!(!trie
            .compare_and_swap("count", Some(999), 0)
            .expect("cas miss"));
        assert_eq!(trie.get_or_insert("new", 7).expect("get_or_insert"), 7);
    }

    let reopened = PersistentARTrie::<i64>::open(&path).expect("reopen byte trie");
    assert_eq!(reopened.get_value("count"), Some(20));
    assert_eq!(reopened.get_value("new"), Some(7));
}

#[test]
fn char_value_insert_serialization_failure_preserves_memory_and_wal() {
    let temp_dir = TempDir::new().expect("tempdir");
    let path = temp_dir.path().join("char_insert.artc");
    let mut trie =
        PersistentARTrieChar::<FailingSerializeValue>::create(&path).expect("create char trie");
    let before_wal = wal_len(&path);

    assert!(trie
        .insert_with_value("bad", FailingSerializeValue(7))
        .is_err());
    assert_eq!(
        trie.insert_batch(&[("also_bad".to_string(), Some(FailingSerializeValue(8)))]),
        0
    );

    assert!(!trie.contains("bad"));
    assert!(!trie.contains("also_bad"));
    assert_eq!(wal_len(&path), before_wal);
}

#[test]
fn char_atomic_serialization_failures_preserve_memory_and_wal() {
    let temp_dir = TempDir::new().expect("tempdir");
    let path = temp_dir.path().join("char_atomic.artc");
    let mut trie = PersistentARTrieChar::<MaybeFailValue>::create(&path).expect("create char trie");

    assert!(trie
        .insert_with_value("key", MaybeFailValue::ok(1))
        .expect("insert"));
    let before_wal = wal_len(&path);

    assert!(trie.upsert("bad", MaybeFailValue::failing(2)).is_err());
    assert!(trie
        .get_or_insert("default", MaybeFailValue::failing(3))
        .is_err());
    assert!(trie
        .compare_and_swap(
            "key",
            Some(MaybeFailValue::failing(1)),
            MaybeFailValue::ok(4),
        )
        .is_err());

    assert!(!trie.contains("bad"));
    assert!(!trie.contains("default"));
    // F2-migrate: Bucket A — `get()` returns None under the overlay; read the surviving
    // value via `get_value` (owned `Option<V>`).
    assert_eq!(trie.get_value("key").map(|value| value.value), Some(1));
    assert_eq!(wal_len(&path), before_wal);
}

#[test]
fn char_document_commit_serialization_failure_preserves_memory_and_wal() {
    let temp_dir = TempDir::new().expect("tempdir");
    let path = temp_dir.path().join("char_document.artc");
    let mut trie =
        PersistentARTrieChar::<FailingSerializeValue>::create(&path).expect("create char trie");
    let mut tx = trie.begin_document("doc").expect("begin tx");
    trie.tx_insert(&mut tx, "bad", Some(FailingSerializeValue(9)));
    let before_commit_wal = wal_len(&path);

    assert!(trie.commit_document(tx).is_err());
    assert!(!trie.contains("bad"));
    assert_eq!(wal_len(&path), before_commit_wal);

    drop(trie);
    let reopened =
        PersistentARTrieChar::<FailingSerializeValue>::open(&path).expect("reopen char trie");
    assert!(!reopened.contains("bad"));
}

#[test]
fn char_atomic_writes_replay_after_reopen() {
    let temp_dir = TempDir::new().expect("tempdir");
    let path = temp_dir.path().join("char_replay.artc");
    {
        let mut trie = PersistentARTrieChar::<i64>::create(&path).expect("create char trie");
        assert!(trie.upsert("count", 10).expect("upsert"));
        assert_eq!(trie.increment("count", 5).expect("increment"), 15);
        assert_eq!(trie.fetch_add("count", 2).expect("fetch_add"), 15);
        assert!(trie.compare_and_swap("count", Some(17), 20).expect("cas"));
        assert!(!trie
            .compare_and_swap("count", Some(999), 0)
            .expect("cas miss"));
        assert_eq!(trie.get_or_insert("new", 7).expect("get_or_insert"), 7);
    }

    let reopened = PersistentARTrieChar::<i64>::open(&path).expect("reopen char trie");
    // F2-migrate: Bucket A — `get()` returns None under the overlay; read via `get_value`.
    assert_eq!(reopened.get_value("count"), Some(20));
    assert_eq!(reopened.get_value("new"), Some(7));
}
