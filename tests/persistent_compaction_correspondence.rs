//! Correspondence tests for persistent byte-trie compaction.
//!
//! The Rocq model treats compaction as a semantic identity over the durable
//! key/value map. These tests exercise the implementation boundaries that can
//! otherwise violate that identity: WAL sidecar collisions, term-only entries,
//! byte keys that are not UTF-8, stale WAL replay, and output-file compaction.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{CompactionConfig, PersistentARTrie};
use libdictenstein::{Dictionary, MappedDictionary};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn wal_sidecar_path(path: &Path) -> PathBuf {
    path.with_extension("wal")
}

fn in_place_temp_path(original_path: &Path) -> PathBuf {
    let mut file_name = original_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("compact"));
    file_name.push(".compacting");
    original_path.with_file_name(file_name)
}

fn stale_wal_backup_path(original_wal_path: &Path) -> PathBuf {
    let mut file_name = original_wal_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("compact.wal"));
    file_name.push(".compacting-stale");
    original_wal_path.with_file_name(file_name)
}

fn create_compacted_counter_snapshot(path: &Path, value: i64) {
    let mut compacted = PersistentARTrie::<i64>::create(path).expect("create compacted snapshot");
    assert!(compacted.insert_with_value("counter", value));
    compacted
        .checkpoint()
        .expect("checkpoint compacted snapshot");
}

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
        // **M4b REFRAME.** A fresh `create::<()>()` now create-flips to the overlay,
        // but this test exercises `compact()`, which the overlay REJECTS (compaction
        // rebuilds from the owned tree and atomically replaces the file, which would
        // clobber the durable overlay/WAL). Force the owned regime.
        trie.kill_switch_to_owned();
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
    assert!(
        !stale_wal_backup_path(&wal_sidecar_path(&path)).exists(),
        "successful compaction must not leave stale WAL backup artifacts"
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

#[test]
fn crash_before_data_rename_restores_original_wal_for_recovery() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("restore_before_rename.artrie");
    let temp_path = in_place_temp_path(&path);
    let original_wal_path = wal_sidecar_path(&path);
    let backup_wal_path = stale_wal_backup_path(&original_wal_path);

    {
        let mut trie = PersistentARTrie::<i64>::create(&path).expect("create trie");
        // **M4b REFRAME.** A fresh `create::<i64>()` now create-flips to the overlay,
        // but this test does a NEGATIVE increment ("counter", -5), which the overlay
        // counter domain REJECTS (the overlay counter is a non-negative i64; the
        // Order-A `bound_increment_delta` fails-loud on a negative delta). Force the
        // owned regime, where decrements are allowed.
        trie.kill_switch_to_owned();
        assert_eq!(trie.increment("counter", 5).expect("increment to five"), 5);
        trie.checkpoint().expect("checkpoint old data");
        assert_eq!(
            trie.increment("counter", -5)
                .expect("increment back to zero"),
            0
        );
    }

    create_compacted_counter_snapshot(&temp_path, 0);
    let _ = std::fs::remove_file(wal_sidecar_path(&temp_path));
    std::fs::rename(&original_wal_path, &backup_wal_path).expect("backup original WAL");

    let reopened = PersistentARTrie::<i64>::open(&path).expect("reopen before data rename");
    assert_eq!(
        reopened.get_value("counter"),
        Some(0),
        "old data must recover through the restored WAL if data rename did not happen"
    );
    assert!(
        original_wal_path.exists(),
        "original WAL must be restored for ordinary recovery"
    );
    assert!(
        !backup_wal_path.exists(),
        "backup WAL must be consumed after restoring the pre-rename state"
    );
    assert!(
        !temp_path.exists(),
        "unfinished compacted data must be discarded when original data remains published"
    );
}

#[test]
fn crash_after_data_rename_ignores_stale_wal_backup() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("ignore_after_rename.artrie");
    let temp_path = in_place_temp_path(&path);
    let original_wal_path = wal_sidecar_path(&path);
    let backup_wal_path = stale_wal_backup_path(&original_wal_path);

    {
        let mut trie = PersistentARTrie::<i64>::create(&path).expect("create trie");
        // **M4b REFRAME.** A fresh `create::<i64>()` now create-flips to the overlay,
        // but this test does a NEGATIVE increment ("counter", -5), which the overlay
        // counter domain REJECTS (the overlay counter is a non-negative i64; the
        // Order-A `bound_increment_delta` fails-loud on a negative delta). Force the
        // owned regime, where decrements are allowed.
        trie.kill_switch_to_owned();
        assert_eq!(trie.increment("counter", 5).expect("increment to five"), 5);
        trie.checkpoint().expect("checkpoint old data");
        assert_eq!(
            trie.increment("counter", -5)
                .expect("increment back to zero"),
            0
        );
    }

    create_compacted_counter_snapshot(&temp_path, 0);
    let _ = std::fs::remove_file(wal_sidecar_path(&temp_path));
    std::fs::rename(&original_wal_path, &backup_wal_path).expect("backup original WAL");
    std::fs::rename(&temp_path, &path).expect("publish compacted data");

    let reopened = PersistentARTrie::<i64>::open(&path).expect("reopen after data rename");
    assert_eq!(
        reopened.get_value("counter"),
        Some(0),
        "compacted data must not replay the stale increment WAL after publication"
    );
    assert!(
        !backup_wal_path.exists(),
        "stale WAL backup must be removed after compacted data is published"
    );
    assert!(
        original_wal_path.exists(),
        "reopen should create a fresh WAL after consuming the stale backup"
    );
}
