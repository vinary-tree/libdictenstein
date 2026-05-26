//! Correspondence checks for `Shared*` persistent public concurrency.
//!
//! These tests exercise the Rust side of `SharedPersistentConcurrency.tla`:
//! a public checkpoint and a public writer may linearize in either order, but a
//! completed writer must be visible in memory and after reopen once the shared
//! handle is synced.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{PersistentARTrie, SharedARTrie};
use libdictenstein::persistent_artrie_char::{PersistentARTrieChar, SharedCharARTrie};
use libdictenstein::persistent_vocab_artrie::{PersistentVocabARTrie, SharedVocabARTrie};
use libdictenstein::{ARTrie, MappedDictionary};
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::tempdir;

#[test]
fn byte_shared_checkpoint_racing_insert_reopens() {
    for round in 0..8 {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join(format!("byte_shared_race_{round}.part"));
        let trie: SharedARTrie<i64> = ARTrie::create(&path).expect("create shared byte trie");

        for i in 0..64 {
            let term = format!("seed-{round}-{i}");
            assert!(ARTrie::insert_with_value(&trie, &term, i));
        }
        ARTrie::checkpoint(&trie).expect("initial checkpoint");

        let racing_term = format!("racing-byte-{round}");
        let racing_value = 10_000 + i64::from(round);
        let barrier = Arc::new(Barrier::new(3));

        let checkpoint_trie = Arc::clone(&trie);
        let checkpoint_barrier = Arc::clone(&barrier);
        let checkpoint_thread = thread::spawn(move || {
            checkpoint_barrier.wait();
            ARTrie::checkpoint(&checkpoint_trie).expect("racing checkpoint");
        });

        let writer_trie = Arc::clone(&trie);
        let writer_barrier = Arc::clone(&barrier);
        let writer_term = racing_term.clone();
        let writer_thread = thread::spawn(move || {
            writer_barrier.wait();
            assert!(ARTrie::insert_with_value(
                &writer_trie,
                &writer_term,
                racing_value
            ));
        });

        barrier.wait();
        checkpoint_thread.join().expect("checkpoint thread");
        writer_thread.join().expect("writer thread");

        ARTrie::sync(&trie).expect("sync racing tail");
        assert_eq!(ARTrie::get_value(&trie, &racing_term), Some(racing_value));

        drop(trie);
        let reopened = PersistentARTrie::<i64>::open(&path).expect("reopen byte trie");
        assert_eq!(reopened.get_value(&racing_term), Some(racing_value));
    }
}

#[test]
fn char_shared_checkpoint_racing_insert_reopens() {
    for round in 0..4 {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join(format!("char_shared_race_{round}.part"));
        let trie: SharedCharARTrie<i64> = ARTrie::create(&path).expect("create shared char trie");

        assert!(ARTrie::insert_with_value(&trie, "alpha", 1));
        assert!(ARTrie::insert_with_value(&trie, "café", 2));
        ARTrie::checkpoint(&trie).expect("initial checkpoint");

        let racing_term = format!("東京-{round}");
        let racing_value = 20_000 + i64::from(round);
        let barrier = Arc::new(Barrier::new(3));

        let checkpoint_trie = Arc::clone(&trie);
        let checkpoint_barrier = Arc::clone(&barrier);
        let checkpoint_thread = thread::spawn(move || {
            checkpoint_barrier.wait();
            ARTrie::checkpoint(&checkpoint_trie).expect("racing checkpoint");
        });

        let writer_trie = Arc::clone(&trie);
        let writer_barrier = Arc::clone(&barrier);
        let writer_term = racing_term.clone();
        let writer_thread = thread::spawn(move || {
            writer_barrier.wait();
            assert!(ARTrie::insert_with_value(
                &writer_trie,
                &writer_term,
                racing_value
            ));
        });

        barrier.wait();
        checkpoint_thread.join().expect("checkpoint thread");
        writer_thread.join().expect("writer thread");

        ARTrie::sync(&trie).expect("sync racing tail");
        assert_eq!(ARTrie::get_value(&trie, &racing_term), Some(racing_value));

        drop(trie);
        let reopened = PersistentARTrieChar::<i64>::open(&path).expect("reopen char trie");
        assert_eq!(reopened.get(&racing_term).copied(), Some(racing_value));
    }
}

#[test]
fn vocab_shared_checkpoint_racing_insert_reopens_with_same_index() {
    for round in 0..4 {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join(format!("vocab_shared_race_{round}.part"));
        let vocab: SharedVocabARTrie = ARTrie::create(&path).expect("create shared vocab trie");

        assert!(ARTrie::insert(&vocab, "alpha"));
        assert!(ARTrie::insert(&vocab, "βeta"));
        ARTrie::checkpoint(&vocab).expect("initial checkpoint");

        let racing_term = format!("emoji😀-{round}");
        let barrier = Arc::new(Barrier::new(3));

        let checkpoint_vocab = Arc::clone(&vocab);
        let checkpoint_barrier = Arc::clone(&barrier);
        let checkpoint_thread = thread::spawn(move || {
            checkpoint_barrier.wait();
            ARTrie::checkpoint(&checkpoint_vocab).expect("racing checkpoint");
        });

        let writer_vocab = Arc::clone(&vocab);
        let writer_barrier = Arc::clone(&barrier);
        let writer_term = racing_term.clone();
        let writer_thread = thread::spawn(move || {
            writer_barrier.wait();
            assert!(ARTrie::insert(&writer_vocab, &writer_term));
        });

        barrier.wait();
        checkpoint_thread.join().expect("checkpoint thread");
        writer_thread.join().expect("writer thread");

        ARTrie::sync(&vocab).expect("sync racing vocab tail");
        let returned_index = ARTrie::get_value(&vocab, &racing_term).expect("visible index");

        std::mem::forget(vocab);
        let reopened = PersistentVocabARTrie::open(&path).expect("reopen vocab trie");
        assert_eq!(reopened.get_index(&racing_term), Some(returned_index));
        assert_eq!(reopened.get_term(returned_index), Some(racing_term));
    }
}
