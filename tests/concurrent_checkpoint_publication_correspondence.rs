//! Correspondence checks for concurrent vocabulary checkpoint publication.
//!
//! These tests exercise the Rust side of `ConcurrentCheckpointPublication.tla`:
//! queued and lock-free inserts must not be lost or assigned conflicting
//! indexes across checkpoint publication, and public `checkpoint()`/`flush()`
//! must publish a durable checkpoint rather than acting as a WAL-only sync.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_vocab_artrie::{ConcurrentVocabARTrie, PersistentVocabARTrie};
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::tempdir;

#[test]
fn queue_batch_duplicates_checkpoint_reopen_with_stable_index() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("queue_batch_duplicates.vocab");

    let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
    let concurrent = ConcurrentVocabARTrie::new(vocab);

    let indices = concurrent.insert_batch_concurrent(&["dup", "dup", "other", "dup"]);
    assert_eq!(
        indices,
        vec![0, 0, 1, 0],
        "queue batching must not allocate multiple visible indexes for duplicates"
    );

    let merged = concurrent.checkpoint().expect("publish checkpoint");
    assert_eq!(merged, 2);
    std::mem::forget(concurrent);

    let (reopened, report) =
        PersistentVocabARTrie::open_with_recovery(&path).expect("recover checkpointed vocab");
    assert_eq!(
        report.records_replayed, 0,
        "concurrent checkpoint should publish the snapshot, not require WAL replay"
    );
    assert_eq!(reopened.get_index("dup"), Some(0));
    assert_eq!(reopened.get_term(0), Some("dup".to_string()));
    assert_eq!(reopened.get_index("other"), Some(1));
    assert_eq!(reopened.get_term(1), Some("other".to_string()));
    assert!(!reopened.contains_index(2));
}

#[test]
fn queue_flush_publishes_pending_inserts_as_checkpoint() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("queue_flush.vocab");

    let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
    let concurrent = ConcurrentVocabARTrie::new(vocab);

    assert_eq!(concurrent.insert_cas("alpha"), 0);
    assert_eq!(concurrent.insert_cas("beta"), 1);

    concurrent.flush().expect("flush publishes checkpoint");
    std::mem::forget(concurrent);

    let (reopened, report) =
        PersistentVocabARTrie::open_with_recovery(&path).expect("recover flushed vocab");
    assert_eq!(
        report.records_replayed, 0,
        "flush should not leave checkpointed queue inserts dependent on WAL replay"
    );
    assert_eq!(reopened.get_index("alpha"), Some(0));
    assert_eq!(reopened.get_index("beta"), Some(1));
}

#[test]
fn lockfree_checkpoint_publishes_without_wal_replay() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("lockfree_checkpoint.vocab");

    let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
    let concurrent = ConcurrentVocabARTrie::new_lockfree(vocab);

    assert_eq!(concurrent.insert_cas("alpha"), 0);
    assert_eq!(concurrent.insert_cas("beta"), 1);
    assert_eq!(concurrent.insert_cas("gamma"), 2);

    let merged = concurrent.checkpoint().expect("publish checkpoint");
    assert_eq!(merged, 3);
    std::mem::forget(concurrent);

    let (reopened, report) =
        PersistentVocabARTrie::open_with_recovery(&path).expect("recover lockfree checkpoint");
    assert_eq!(
        report.records_replayed, 0,
        "lock-free checkpoint should merge and publish a checkpoint before truncating WAL"
    );
    assert_eq!(reopened.get_index("alpha"), Some(0));
    assert_eq!(reopened.get_index("beta"), Some(1));
    assert_eq!(reopened.get_index("gamma"), Some(2));
}

#[test]
fn lockfree_batch_duplicates_checkpoint_reopen_with_stable_indices() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("lockfree_batch_duplicates.vocab");

    let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
    let concurrent = ConcurrentVocabARTrie::new_lockfree(vocab);

    let indices = concurrent.insert_batch_concurrent(&["dup", "dup", "other", "dup"]);
    assert_eq!(
        indices,
        vec![0, 0, 1, 0],
        "lock-free public batch must be left-to-right linearizable for duplicates"
    );
    assert_eq!(concurrent.get_index("dup"), Some(0));
    assert_eq!(concurrent.get_index("other"), Some(1));

    let merged = concurrent.checkpoint().expect("publish checkpoint");
    assert_eq!(merged, 2);
    std::mem::forget(concurrent);

    let (reopened, report) =
        PersistentVocabARTrie::open_with_recovery(&path).expect("recover lockfree batch");
    assert_eq!(report.records_replayed, 0);
    assert_eq!(reopened.get_index("dup"), Some(0));
    assert_eq!(reopened.get_term(0), Some("dup".to_string()));
    assert_eq!(reopened.get_index("other"), Some(1));
    assert_eq!(reopened.get_term(1), Some("other".to_string()));
    assert!(!reopened.contains_index(2));
}

#[test]
fn queue_visible_insert_before_checkpoint_reopens_with_returned_index() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("queue_visible_before_checkpoint.vocab");

    let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
    let concurrent = ConcurrentVocabARTrie::new(vocab);

    let alpha = concurrent.insert_cas("alpha");
    let beta = concurrent.insert_cas("beta");
    assert_eq!(alpha, 0);
    assert_eq!(beta, 1);
    assert_eq!(
        concurrent.get_index("alpha"),
        Some(alpha),
        "queue cache makes returned inserts immediately visible"
    );

    let merged = concurrent.checkpoint().expect("publish queued checkpoint");
    assert_eq!(merged, 2);
    std::mem::forget(concurrent);

    let (reopened, report) =
        PersistentVocabARTrie::open_with_recovery(&path).expect("recover queue checkpoint");
    assert_eq!(report.records_replayed, 0);
    assert_eq!(reopened.get_index("alpha"), Some(alpha));
    assert_eq!(reopened.get_term(alpha), Some("alpha".to_string()));
    assert_eq!(reopened.get_index("beta"), Some(beta));
    assert_eq!(reopened.get_term(beta), Some("beta".to_string()));
}

#[test]
fn lockfree_duplicate_race_checkpoint_reopens_single_index() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("lockfree_duplicate_race.vocab");

    let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
    let concurrent = Arc::new(ConcurrentVocabARTrie::new_lockfree(vocab));
    let barrier = Arc::new(Barrier::new(3));

    let mut handles = Vec::new();
    for _ in 0..2 {
        let concurrent = Arc::clone(&concurrent);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            concurrent.insert_cas("shared")
        }));
    }

    barrier.wait();
    let first = handles.remove(0).join().expect("first insert thread");
    let second = handles.remove(0).join().expect("second insert thread");
    assert_eq!(first, second);

    let merged = concurrent.checkpoint().expect("publish checkpoint");
    assert_eq!(merged, 1);

    let concurrent = Arc::try_unwrap(concurrent)
        .ok()
        .expect("no remaining concurrent vocab references");
    std::mem::forget(concurrent);

    let (reopened, report) =
        PersistentVocabARTrie::open_with_recovery(&path).expect("recover duplicate race");
    assert_eq!(report.records_replayed, 0);
    assert_eq!(reopened.get_index("shared"), Some(first));
    assert_eq!(reopened.get_term(first), Some("shared".to_string()));
    assert!(!reopened.contains_index(first + 1));
}
