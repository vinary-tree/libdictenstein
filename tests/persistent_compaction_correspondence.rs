//! Correspondence tests for persistent byte-trie compaction.
//!
//! The Rocq model treats compaction as a semantic identity over the durable
//! key/value map. These tests exercise the implementation boundaries that can
//! otherwise violate that identity: WAL sidecar collisions, term-only entries,
//! byte keys that are not UTF-8, stale WAL replay, and output-file compaction.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{CompactionConfig, PersistentARTrie};
use libdictenstein::{Dictionary, MappedDictionary};
use tempfile::tempdir;

#[test]
fn in_place_compaction_preserves_unsynced_wal_values_after_reopen() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("unsynced_values.artrie");

    {
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create trie");
        assert!(trie.insert_with_value("alpha", 1));
        assert!(trie.insert_with_value("beta", 2));
        assert!(trie.insert_with_value("gamma", 3));

        trie.compact(CompactionConfig::default(), |_| {})
            .expect("compact unsynced values");

        assert_eq!(trie.get_value("alpha"), Some(1));
        assert_eq!(trie.get_value("beta"), Some(2));
        assert_eq!(trie.get_value("gamma"), Some(3));
    }

    let reopened = PersistentARTrie::<u64>::open(&path).expect("reopen compacted trie");
    assert_eq!(reopened.get_value("alpha"), Some(1));
    assert_eq!(reopened.get_value("beta"), Some(2));
    assert_eq!(reopened.get_value("gamma"), Some(3));
}

#[test]
fn compaction_rejects_wal_sidecar_collision_without_losing_recovery_wal() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("collision.artrie");
    let colliding_output_path = path.with_extension("compacting");

    {
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create trie");
        assert!(trie.insert_with_value("wal-only", 7));
        assert!(path.with_extension("wal").exists());

        let config = CompactionConfig {
            output_path: Some(colliding_output_path),
            ..Default::default()
        };

        let err = trie
            .compact(config, |_| {})
            .expect_err("compaction must reject sidecar collision");
        assert!(
            err.to_string().contains("would collide with original WAL"),
            "unexpected error: {err}"
        );
        assert!(
            path.with_extension("wal").exists(),
            "original WAL must survive rejected compaction"
        );
    }

    let reopened = PersistentARTrie::<u64>::open(&path).expect("reopen original trie");
    assert_eq!(reopened.get_value("wal-only"), Some(7));
}

#[test]
fn set_like_term_only_entries_survive_compaction() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("term_only.artrie");

    {
        let mut trie = PersistentARTrie::<()>::create(&path).expect("create trie");
        assert!(trie.insert("alpha"));
        assert!(trie.insert("alphabet"));
        assert!(trie.insert("beta"));

        trie.compact(CompactionConfig::default(), |_| {})
            .expect("compact term-only trie");

        assert!(trie.contains("alpha"));
        assert!(trie.contains("alphabet"));
        assert!(trie.contains("beta"));
    }

    let reopened = PersistentARTrie::<()>::open(&path).expect("reopen compacted set trie");
    assert!(reopened.contains("alpha"));
    assert!(reopened.contains("alphabet"));
    assert!(reopened.contains("beta"));
}

#[test]
fn non_utf8_byte_keys_survive_compaction() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("byte_keys.artrie");
    let raw_key = [0xff, 0x00, b'a'];

    {
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create trie");
        let inserted = trie.insert_batch_bytes(&[
            (b"ascii".as_slice(), Some(1)),
            (raw_key.as_slice(), Some(2)),
        ]);
        assert_eq!(inserted, 2);

        trie.compact(CompactionConfig::default(), |_| {})
            .expect("compact byte-key trie");

        assert_eq!(trie.get_value_bytes(b"ascii"), Some(1));
        assert_eq!(trie.get_value_bytes(&raw_key), Some(2));
    }

    let reopened = PersistentARTrie::<u64>::open(&path).expect("reopen compacted byte-key trie");
    assert_eq!(reopened.get_value_bytes(b"ascii"), Some(1));
    assert_eq!(reopened.get_value_bytes(&raw_key), Some(2));
}

#[test]
fn successful_in_place_compaction_does_not_replay_stale_original_wal() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("stale_wal.artrie");

    {
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create trie");
        assert!(trie.insert_with_value("original", 1));
        trie.checkpoint().expect("checkpoint original");

        assert!(trie.insert_with_value("stale-suffix", 2));
        assert!(trie.remove("stale-suffix"));
        assert!(trie.insert_with_value("survivor", 3));

        trie.compact(CompactionConfig::default(), |_| {})
            .expect("compact with stale WAL history");

        assert_eq!(trie.get_value("original"), Some(1));
        assert_eq!(trie.get_value("survivor"), Some(3));
        assert!(!trie.contains("stale-suffix"));
    }

    let reopened = PersistentARTrie::<u64>::open(&path).expect("reopen after stale WAL compact");
    assert_eq!(reopened.get_value("original"), Some(1));
    assert_eq!(reopened.get_value("survivor"), Some(3));
    assert!(
        !reopened.contains("stale-suffix"),
        "stale original WAL must not be replayed after compacted snapshot"
    );
}

#[test]
fn output_file_compaction_preserves_key_value_snapshot() {
    let dir = tempdir().expect("temp dir");
    let original_path = dir.path().join("original.artrie");
    let compacted_path = dir.path().join("snapshot.artrie");

    let mut trie = PersistentARTrie::<u64>::create(&original_path).expect("create trie");
    assert!(trie.insert_with_value("alpha", 10));
    assert!(trie.insert_with_value("beta", 20));
    assert!(!trie.insert_with_value("alpha", 11));

    let config = CompactionConfig {
        output_path: Some(compacted_path.clone()),
        ..Default::default()
    };
    trie.compact(config, |_| {})
        .expect("compact to output file");

    assert_eq!(trie.get_value("alpha"), Some(11));
    assert_eq!(trie.get_value("beta"), Some(20));

    let compacted = PersistentARTrie::<u64>::open(&compacted_path).expect("open output snapshot");
    assert_eq!(compacted.get_value("alpha"), Some(11));
    assert_eq!(compacted.get_value("beta"), Some(20));
}
