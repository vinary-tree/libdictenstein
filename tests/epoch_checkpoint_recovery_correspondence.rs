#![cfg(feature = "persistent-artrie")]

use std::path::Path;
use std::time::Duration;

use libdictenstein::persistent_artrie::char::PersistentARTrieChar;
use libdictenstein::persistent_artrie::epoch::EpochConfig;
use tempfile::tempdir;

fn correspondence_epoch_config() -> EpochConfig {
    EpochConfig {
        epoch_duration: Duration::from_secs(60),
        max_ops_per_epoch: 100,
        max_wal_size_bytes: 1024 * 1024,
        retention_epochs: 1,
        background_checkpoint: false,
        incremental_checkpoint: true,
    }
}

fn remove_file_if_exists(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("remove {}: {}", path.display(), error),
    }
}

fn remove_dir_if_exists(path: &Path) {
    match std::fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("remove {}: {}", path.display(), error),
    }
}

#[test]
fn forced_epoch_checkpoint_reopens_without_wal_tail() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("force_checkpoint.trie");

    let trie: PersistentARTrieChar<i64> = PersistentARTrieChar::create(&path).expect("create");
    trie.enable_epoch_checkpointing(correspondence_epoch_config())
        .expect("enable epoch checkpointing");

    assert!(trie.insert_with_value("durable", 42).expect("insert"));
    assert!(trie
        .insert_with_value("removed", 11)
        .expect("insert removed"));
    assert!(trie.remove("removed").expect("remove"));

    let checkpointed_epoch = trie.current_epoch_id().expect("current epoch");
    let new_epoch = trie
        .force_epoch_checkpoint()
        .expect("epoch checkpointing enabled")
        .expect("force epoch checkpoint");

    assert_eq!(new_epoch, checkpointed_epoch + 1);
    assert_eq!(trie.last_durable_epoch(), Some(checkpointed_epoch));
    drop(trie);

    remove_file_if_exists(&path.with_extension("wal"));
    remove_dir_if_exists(&dir.path().join("wal_archive"));

    let reopened: PersistentARTrieChar<i64> = PersistentARTrieChar::open(&path).expect("reopen");
    assert_eq!(reopened.get("durable"), Some(42));
    assert_eq!(reopened.get("removed"), None);
}

#[test]
fn corrupt_epoch_metadata_fails_closed_while_trie_checkpoint_recovers() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("corrupt_epoch_meta.trie");

    let trie: PersistentARTrieChar<i64> = PersistentARTrieChar::create(&path).expect("create");
    trie.enable_epoch_checkpointing(correspondence_epoch_config())
        .expect("enable epoch checkpointing");
    assert!(trie.insert_with_value("survives", 7).expect("insert"));
    trie.force_epoch_checkpoint()
        .expect("epoch checkpointing enabled")
        .expect("force epoch checkpoint");
    drop(trie);

    let meta_path = path
        .with_extension("epoch")
        .join("wal")
        .join("checkpoint.meta");
    std::fs::write(&meta_path, b"not valid checkpoint metadata").expect("corrupt metadata");

    let reopened: PersistentARTrieChar<i64> =
        PersistentARTrieChar::open(&path).expect("reopen trie");
    // F2-migrate: Bucket A — `get()` returns None under the overlay; read via `get_value`.
    assert_eq!(reopened.get_value("survives"), Some(7));

    reopened
        .enable_epoch_checkpointing(correspondence_epoch_config())
        .expect("enable epoch checkpointing with corrupt metadata");
    assert_eq!(
        reopened.last_durable_epoch(),
        None,
        "corrupt epoch metadata must not be trusted as a durable epoch"
    );
}
