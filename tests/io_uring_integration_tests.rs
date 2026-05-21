//! Integration tests for io_uring backend.
//!
//! These tests exercise the `create_with_io_uring` / `open_with_io_uring` convenience
//! constructors end-to-end on all three persistent trie types:
//! - `PersistentARTrie<V, IoUringDiskManager>` (byte-level)
//! - `PersistentARTrieChar<V, IoUringDiskManager>` (char-level / UTF-8)
//! - `PersistentVocabARTrie<IoUringDiskManager>` (vocabulary with indices)
//!
//! Requires the `io-uring-backend` feature to be enabled.

#![cfg(feature = "io-uring-backend")]

use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
use libdictenstein::Dictionary;
use tempfile::tempdir;

// =============================================================================
// PersistentARTrie<(), IoUringDiskManager> — byte-level trie
// =============================================================================

#[test]
fn test_byte_trie_create_insert_contains() {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("byte_trie.part");

    let mut dict: PersistentARTrie<(), _> =
        PersistentARTrie::create_with_io_uring(&path).expect("create with io_uring");

    assert!(dict.insert("apple"));
    assert!(dict.insert("banana"));
    assert!(dict.insert("cherry"));
    assert!(!dict.insert("apple")); // duplicate

    assert!(dict.contains("apple"));
    assert!(dict.contains("banana"));
    assert!(dict.contains("cherry"));
    assert!(!dict.contains("date"));

    assert_eq!(dict.len(), Some(3));
}

#[test]
fn test_byte_trie_create_sync_reopen() {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("byte_reopen.part");

    let terms = vec!["alpha", "bravo", "charlie", "delta", "echo"];

    // Create and insert
    {
        let mut dict: PersistentARTrie<(), _> =
            PersistentARTrie::create_with_io_uring(&path).expect("create");
        for term in &terms {
            dict.insert(term);
        }
        dict.sync().expect("sync");
    }

    // Reopen and verify
    {
        let dict: PersistentARTrie<(), _> =
            PersistentARTrie::open_with_io_uring(&path).expect("open");
        for term in &terms {
            assert!(
                dict.contains(term),
                "Term '{}' should be present after reopen",
                term
            );
        }
        assert_eq!(dict.len(), Some(terms.len()));
    }
}

#[test]
fn test_byte_trie_wal_recovery() {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("byte_wal.part");

    let terms = vec!["foo", "bar", "baz", "qux"];

    // Create and insert — drop without sync (simulate crash)
    {
        let mut dict: PersistentARTrie<(), _> =
            PersistentARTrie::create_with_io_uring(&path).expect("create");
        for term in &terms {
            dict.insert(term);
        }
        // No sync — WAL should buffer writes
    }

    // Reopen — WAL replay should recover terms
    {
        let dict: PersistentARTrie<(), _> =
            PersistentARTrie::open_with_io_uring(&path).expect("open");
        for term in &terms {
            assert!(
                dict.contains(term),
                "Term '{}' should be recovered from WAL",
                term
            );
        }
        assert_eq!(dict.len(), Some(terms.len()));
    }
}

#[test]
fn test_byte_trie_remove_and_reopen() {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("byte_remove.part");

    {
        let mut dict: PersistentARTrie<(), _> =
            PersistentARTrie::create_with_io_uring(&path).expect("create");
        dict.insert("keep");
        dict.insert("remove_me");
        dict.insert("also_keep");
        dict.remove("remove_me");
        dict.sync().expect("sync");
    }

    {
        let dict: PersistentARTrie<(), _> =
            PersistentARTrie::open_with_io_uring(&path).expect("open");
        assert!(dict.contains("keep"));
        assert!(dict.contains("also_keep"));
        assert!(!dict.contains("remove_me"));
        assert_eq!(dict.len(), Some(2));
    }
}

#[test]
fn test_byte_trie_large_dataset() {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("byte_large.part");

    let count = 5000;

    {
        let mut dict: PersistentARTrie<(), _> =
            PersistentARTrie::create_with_io_uring(&path).expect("create");
        for i in 0..count {
            dict.insert(&format!("term_{:06}", i));
        }
        dict.sync().expect("sync");
    }

    {
        let dict: PersistentARTrie<(), _> =
            PersistentARTrie::open_with_io_uring(&path).expect("open");
        assert_eq!(dict.len(), Some(count));
        for i in 0..count {
            assert!(
                dict.contains(&format!("term_{:06}", i)),
                "Missing term_{:06}",
                i
            );
        }
    }
}

// =============================================================================
// PersistentARTrieChar<(), IoUringDiskManager> — char-level trie (UTF-8)
// =============================================================================

#[test]
fn test_char_trie_create_insert_contains() {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("char_trie.part");

    let mut trie: PersistentARTrieChar<(), _> =
        PersistentARTrieChar::create_with_io_uring(&path).expect("create with io_uring");

    trie.insert("hello").expect("insert hello");
    trie.insert("world").expect("insert world");
    trie.insert("héllo").expect("insert héllo"); // Unicode

    assert!(trie.contains("hello"));
    assert!(trie.contains("world"));
    assert!(trie.contains("héllo"));
    assert!(!trie.contains("missing"));

    assert_eq!(trie.len(), 3);
}

#[test]
fn test_char_trie_create_sync_reopen() {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("char_reopen.part");

    let terms = vec!["café", "naïve", "résumé", "über", "日本語"];

    {
        let mut trie: PersistentARTrieChar<(), _> =
            PersistentARTrieChar::create_with_io_uring(&path).expect("create");
        for term in &terms {
            trie.insert(term).expect("insert");
        }
        trie.sync().expect("sync");
    }

    {
        let trie: PersistentARTrieChar<(), _> =
            PersistentARTrieChar::open_with_io_uring(&path).expect("open");
        for term in &terms {
            assert!(
                trie.contains(term),
                "Term '{}' should be present after reopen",
                term
            );
        }
        assert_eq!(trie.len(), terms.len());
    }
}

#[test]
fn test_char_trie_wal_recovery() {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("char_wal.part");

    let terms = vec!["alpha", "beta", "gamma", "δ"];

    // Drop without sync
    {
        let mut trie: PersistentARTrieChar<(), _> =
            PersistentARTrieChar::create_with_io_uring(&path).expect("create");
        for term in &terms {
            trie.insert(term).expect("insert");
        }
    }

    // WAL replay should recover
    {
        let trie: PersistentARTrieChar<(), _> =
            PersistentARTrieChar::open_with_io_uring(&path).expect("open");
        for term in &terms {
            assert!(
                trie.contains(term),
                "Term '{}' should be recovered from WAL",
                term
            );
        }
        assert_eq!(trie.len(), terms.len());
    }
}

// =============================================================================
// PersistentVocabARTrie<IoUringDiskManager> — vocabulary with indices
// =============================================================================

#[test]
fn test_vocab_trie_create_insert_lookup() {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("vocab_trie.part");

    let mut vocab: PersistentVocabARTrie<_> =
        PersistentVocabARTrie::create_with_io_uring(&path).expect("create with io_uring");

    let idx0 = vocab.insert("hello");
    let idx1 = vocab.insert("world");
    let idx_dup = vocab.insert("hello"); // duplicate returns existing index

    assert_eq!(idx0, 0);
    assert_eq!(idx1, 1);
    assert_eq!(idx_dup, 0);

    assert_eq!(vocab.get_index("hello"), Some(0));
    assert_eq!(vocab.get_index("world"), Some(1));
    assert_eq!(vocab.get_index("missing"), None);

    assert_eq!(vocab.get_term(0), Some("hello".to_string()));
    assert_eq!(vocab.get_term(1), Some("world".to_string()));

    assert!(vocab.contains("hello"));
    assert!(vocab.contains("world"));
    assert!(!vocab.contains("missing"));

    assert_eq!(vocab.len(), 2);
}

#[test]
fn test_vocab_trie_create_sync_reopen() {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("vocab_reopen.part");

    let terms = vec!["apple", "banana", "cherry", "date", "elderberry"];

    {
        let mut vocab: PersistentVocabARTrie<_> =
            PersistentVocabARTrie::create_with_io_uring(&path).expect("create");
        for term in &terms {
            vocab.insert(term);
        }
        vocab.sync().expect("sync");
    }

    {
        let vocab: PersistentVocabARTrie<_> =
            PersistentVocabARTrie::open_with_io_uring(&path).expect("open");
        for (i, term) in terms.iter().enumerate() {
            assert_eq!(
                vocab.get_index(term),
                Some(i as u64),
                "Term '{}' should have index {}",
                term,
                i
            );
            assert_eq!(
                vocab.get_term(i as u64),
                Some(term.to_string()),
                "Index {} should map to '{}'",
                i,
                term
            );
        }
        assert_eq!(vocab.len(), terms.len());
    }
}

#[test]
fn test_vocab_trie_wal_recovery() {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("vocab_wal.part");

    let terms = vec!["one", "two", "three"];

    // Drop without sync
    {
        let mut vocab: PersistentVocabARTrie<_> =
            PersistentVocabARTrie::create_with_io_uring(&path).expect("create");
        for term in &terms {
            vocab.insert(term);
        }
    }

    // WAL replay should recover
    {
        let vocab: PersistentVocabARTrie<_> =
            PersistentVocabARTrie::open_with_io_uring(&path).expect("open");
        for (i, term) in terms.iter().enumerate() {
            assert_eq!(
                vocab.get_index(term),
                Some(i as u64),
                "Term '{}' should be recovered with index {}",
                term,
                i
            );
        }
        assert_eq!(vocab.len(), terms.len());
    }
}

#[test]
fn test_vocab_trie_large_dataset() {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("vocab_large.part");

    let count = 5000;

    {
        let mut vocab: PersistentVocabARTrie<_> =
            PersistentVocabARTrie::create_with_io_uring(&path).expect("create");
        for i in 0..count {
            let idx = vocab.insert(&format!("term_{:06}", i));
            assert_eq!(
                idx, i as u64,
                "First insert of term_{:06} should get index {}",
                i, i
            );
        }
        vocab.sync().expect("sync");
    }

    {
        let vocab: PersistentVocabARTrie<_> =
            PersistentVocabARTrie::open_with_io_uring(&path).expect("open");
        assert_eq!(vocab.len(), count);
        for i in 0..count {
            let term = format!("term_{:06}", i);
            assert_eq!(
                vocab.get_index(&term),
                Some(i as u64),
                "Missing forward mapping for {}",
                term
            );
            assert_eq!(
                vocab.get_term(i as u64),
                Some(term.clone()),
                "Missing reverse mapping for index {}",
                i
            );
        }
    }
}
