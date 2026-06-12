//! Concurrency coverage for persistent suffix automata.
//!
//! These tests exercise the public shared-reference behavior over the same
//! ARTrie overlay/checkpoint substrate used by the byte and char persistent
//! tries.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{
    PersistentSuffixAutomaton, PersistentSuffixAutomatonChar, PersistentSuffixAutomatonCharNode,
    PersistentSuffixAutomatonNode,
};
use libdictenstein::{Dictionary, MappedDictionary};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::TempDir;

fn scratch_dir(prefix: &str) -> TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn persistent_suffix_types_are_send_sync() {
    assert_send_sync::<PersistentSuffixAutomaton<i32>>();
    assert_send_sync::<PersistentSuffixAutomatonChar<i32>>();
    assert_send_sync::<PersistentSuffixAutomatonNode<i32>>();
    assert_send_sync::<PersistentSuffixAutomatonCharNode<i32>>();
}

#[test]
fn byte_concurrent_writers_readers_and_checkpoint_survive_reopen() {
    let dir = scratch_dir("persistent-suffix-byte-concurrency");
    let path = dir.path().join("byte_suffix.art");
    let dict = Arc::new(PersistentSuffixAutomaton::<i32>::create(&path).expect("create"));
    let done = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(7));

    let mut expected = Vec::new();
    for writer in 0..4 {
        for idx in 0..8 {
            expected.push((
                format!("writer-{writer}-term-{idx}-suffix"),
                writer * 100 + idx,
            ));
        }
    }

    let mut handles = Vec::new();
    for writer in 0..4 {
        let dict = Arc::clone(&dict);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for idx in 0..8 {
                let term = format!("writer-{writer}-term-{idx}-suffix");
                assert!(dict.insert_with_value(&term, writer * 100 + idx));
                assert!(dict.contains("suffix"));
            }
        }));
    }

    for _ in 0..2 {
        let dict = Arc::clone(&dict);
        let done = Arc::clone(&done);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            while !done.load(Ordering::Acquire) {
                assert!(dict.contains(""));
                let _ = dict.contains("suffix");
                let _ = dict.match_positions("term");
                let _ = dict.get_value("writer-0-term-0-suffix");
                thread::yield_now();
            }
        }));
    }

    {
        let dict = Arc::clone(&dict);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..8 {
                dict.checkpoint().expect("concurrent checkpoint");
                thread::yield_now();
            }
        }));
    }

    for handle in handles.drain(..4) {
        handle.join().expect("writer thread");
    }
    done.store(true, Ordering::Release);
    for handle in handles {
        handle.join().expect("reader/checkpoint thread");
    }

    dict.checkpoint().expect("final checkpoint");
    dict.close();
    drop(dict);

    let reopened = PersistentSuffixAutomaton::<i32>::open(&path).expect("reopen");
    assert_eq!(reopened.string_count(), expected.len());
    assert!(reopened.contains("term-0"));
    assert!(reopened.contains("suffix"));
    for (term, value) in expected {
        assert!(reopened.contains(&term), "missing {term:?}");
        assert_eq!(reopened.get_value(&term), Some(value), "value for {term:?}");
    }
}

#[test]
fn char_concurrent_duplicate_removes_filter_active_sources() {
    let dir = scratch_dir("persistent-suffix-char-concurrency");
    let path = dir.path().join("char_suffix.art");
    let dict = Arc::new(PersistentSuffixAutomatonChar::<()>::create(&path).expect("create"));
    let terms = ["東京カフェ", "naïve café", "🙂🙂", "a\u{E000}b"];
    let barrier = Arc::new(Barrier::new(4));

    let mut handles = Vec::new();
    for _ in 0..4 {
        let dict = Arc::clone(&dict);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for term in terms {
                assert!(dict.insert(term));
            }
        }));
    }
    for handle in handles {
        handle.join().expect("insert thread");
    }

    assert_eq!(dict.string_count(), 16);
    assert_eq!(dict.match_positions("東京カフェ").len(), 4);
    assert!(dict.contains("カフェ"));
    assert!(dict.contains("\u{E000}b"));

    let barrier = Arc::new(Barrier::new(4));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let dict = Arc::clone(&dict);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            assert!(dict.remove("東京カフェ"));
        }));
    }
    for handle in handles {
        handle.join().expect("remove thread");
    }

    assert_eq!(dict.string_count(), 12);
    assert!(dict.needs_compaction());
    assert_eq!(
        dict.match_positions("東京カフェ"),
        Vec::<(usize, usize)>::new()
    );
    dict.compact();
    assert!(!dict.contains("東京"));
    assert!(dict.contains("café"));
    dict.checkpoint().expect("checkpoint compacted char suffix");
    dict.close();
    drop(dict);

    let reopened = PersistentSuffixAutomatonChar::<()>::open(&path).expect("reopen");
    assert_eq!(reopened.string_count(), 12);
    assert!(!reopened.contains("東京"));
    assert!(reopened.contains("🙂"));
    assert!(reopened.contains("\u{E000}b"));
}
