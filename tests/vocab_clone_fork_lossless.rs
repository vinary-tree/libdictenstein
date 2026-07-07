//! Lossless snapshot `Clone` + independent `fork_to` for `PersistentVocabARTrie`.
//!
//! `clone()` is a read-only, storage-detached, point-in-time **snapshot** that shares the
//! immutable overlay and materializes its caches from the frozen root (self-consistent even under
//! concurrent mutation). `fork_to(path)` is a fully independent, writable, separately-persistable
//! on-disk copy. These tests pin: losslessness (for-all term/id agreement), independence,
//! point-in-time freezing, lifetime safety, id-frontier + start-index preservation, and
//! persistence across reopen.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::vocab::PersistentVocabARTrie;
use tempfile::tempdir;

/// A spread of terms exercising the empty string, ASCII, multibyte UTF-8, emoji, and shared
/// prefixes (path compression).
const TERMS: &[&str] = &[
    "",
    "apple",
    "banana",
    "cherry",
    "date",
    "élan",
    "naïve",
    "日本語",
    "emoji😀",
    "a",
    "ab",
    "abc",
    "prefix",
    "prefixed",
    "prefixes",
];

fn seed(vocab: &PersistentVocabARTrie, terms: &[&str]) {
    for t in terms {
        vocab.insert(t).expect("insert term");
    }
}

/// Assert that `copy` reproduces every `(term, id)` mapping of `src`, in both directions.
fn assert_lossless(copy: &PersistentVocabARTrie, src: &PersistentVocabARTrie) {
    assert_eq!(copy.len(), src.len(), "len must match");
    for t in TERMS {
        let id = src.get_index(t).expect("source has term");
        assert_eq!(copy.get_index(t), Some(id), "forward {t:?}");
        assert_eq!(copy.get_term(id).as_deref(), Some(*t), "reverse {id}");
        assert!(copy.contains(t), "contains {t:?}");
    }
    assert_eq!(copy.get_index("definitely-absent"), None);
}

// =============================================================================
// clone() — lossless in-memory snapshot
// =============================================================================

#[test]
fn clone_is_lossless_snapshot() {
    let dir = tempdir().unwrap();
    let vocab = PersistentVocabARTrie::create(dir.path().join("v.vocab")).unwrap();
    seed(&vocab, TERMS);

    let snap = vocab.clone();

    // The old lossy clone reported len()==N but returned None for every lookup; this asserts the
    // clone is a *consistent*, fully-populated snapshot.
    assert_eq!(snap.len(), TERMS.len());
    assert_lossless(&snap, &vocab);

    // `snapshot()` is the named alias for the same behavior.
    assert_lossless(&vocab.snapshot(), &vocab);
}

#[test]
fn clone_snapshot_is_frozen_and_independent() {
    let dir = tempdir().unwrap();
    let vocab = PersistentVocabARTrie::create(dir.path().join("v.vocab")).unwrap();
    seed(&vocab, &["one", "two"]);
    let snap = vocab.clone();

    // Mutating the source AFTER cloning must not be visible to the snapshot (point-in-time).
    vocab.insert("three").unwrap();
    assert_eq!(vocab.get_index("three"), Some(2));
    assert_eq!(
        snap.get_index("three"),
        None,
        "snapshot must not observe the source's later writes"
    );
    assert_eq!(snap.len(), 2);
    assert_eq!(vocab.len(), 3);
}

#[test]
fn clone_snapshot_outlives_source() {
    let dir = tempdir().unwrap();
    let vocab = PersistentVocabARTrie::create(dir.path().join("v.vocab")).unwrap();
    seed(&vocab, &["alpha", "beta"]);
    let snap = vocab.clone();

    // Drop the source first — the snapshot's shared Arc keeps the frozen tree + reverse map alive.
    drop(vocab);
    assert_eq!(snap.get_index("alpha"), Some(0));
    assert_eq!(snap.get_term(1).as_deref(), Some("beta"));
    assert_eq!(snap.len(), 2);
}

#[test]
fn clone_of_empty_trie() {
    let dir = tempdir().unwrap();
    let vocab = PersistentVocabARTrie::create(dir.path().join("v.vocab")).unwrap();
    let snap = vocab.clone();
    assert_eq!(snap.len(), 0);
    assert!(snap.is_empty());
    assert_eq!(snap.get_index("x"), None);
}

#[test]
fn clone_preserves_start_index() {
    let dir = tempdir().unwrap();
    let vocab =
        PersistentVocabARTrie::create_with_start_index(dir.path().join("v.vocab"), 100).unwrap();
    vocab.insert("a").unwrap(); // id 100
    vocab.insert("b").unwrap(); // id 101
    let snap = vocab.clone();
    assert_eq!(snap.start_index(), 100);
    assert_eq!(snap.next_index(), vocab.next_index());
    assert_eq!(snap.get_index("a"), Some(100));
    assert_eq!(snap.get_term(101).as_deref(), Some("b"));
}

// =============================================================================
// fork_to() — independent, writable, persistable on-disk copy
// =============================================================================

#[test]
fn fork_to_is_lossless() {
    let dir = tempdir().unwrap();
    let src = PersistentVocabARTrie::create(dir.path().join("src.vocab")).unwrap();
    seed(&src, TERMS);

    let fork = src.fork_to(dir.path().join("fork.vocab")).unwrap();
    assert_lossless(&fork, &src);
}

#[test]
fn fork_to_is_independent() {
    let dir = tempdir().unwrap();
    let src = PersistentVocabARTrie::create(dir.path().join("src.vocab")).unwrap();
    seed(&src, &["shared1", "shared2"]);
    let fork = src.fork_to(dir.path().join("fork.vocab")).unwrap();

    // Mutating either side must not affect the other (own storage/WAL/overlay/maps).
    src.insert("src-only").unwrap();
    fork.insert("fork-only").unwrap();
    assert_eq!(fork.get_index("src-only"), None);
    assert_eq!(src.get_index("fork-only"), None);
    assert!(src.contains("shared1") && fork.contains("shared1"));
}

#[test]
fn fork_to_persists_and_reopens_independently() {
    let dir = tempdir().unwrap();
    let src = PersistentVocabARTrie::create(dir.path().join("src.vocab")).unwrap();
    seed(&src, TERMS);

    let fork_path = dir.path().join("fork.vocab");
    let fork = src.fork_to(&fork_path).unwrap(); // fork_to checkpoints its own image
    drop(fork);

    // Reopen the fork's file — its persisted image round-trips every mapping.
    let (reopened, _report) = PersistentVocabARTrie::open_with_recovery(&fork_path).unwrap();
    assert_lossless(&reopened, &src);
}

#[test]
fn fork_to_preserves_id_frontier_with_gaps() {
    let dir = tempdir().unwrap();
    let src = PersistentVocabARTrie::create(dir.path().join("src.vocab")).unwrap();
    src.insert("a").unwrap(); // id 0
    src.insert_with_index("z", 100).unwrap(); // id 100 — burns ids 1..=99
    assert_eq!(src.next_index(), 101);

    let fork = src.fork_to(dir.path().join("fork.vocab")).unwrap();
    // The exact next-id frontier is preserved (order-independent), so a fresh insert on the fork
    // continues the source's sequence rather than reusing a burned id.
    assert_eq!(fork.next_index(), 101);
    assert_eq!(fork.get_index("z"), Some(100));
    assert_eq!(fork.insert("new").unwrap(), 101);
}

#[test]
fn fork_to_preserves_start_index() {
    let dir = tempdir().unwrap();
    let src =
        PersistentVocabARTrie::create_with_start_index(dir.path().join("src.vocab"), 50).unwrap();
    src.insert("a").unwrap(); // id 50
    let fork = src.fork_to(dir.path().join("fork.vocab")).unwrap();
    assert_eq!(fork.start_index(), 50);
    assert_eq!(fork.get_index("a"), Some(50));
    assert_eq!(fork.insert("b").unwrap(), 51);
}

#[test]
fn fork_to_rejects_existing_path() {
    let dir = tempdir().unwrap();
    let src = PersistentVocabARTrie::create(dir.path().join("src.vocab")).unwrap();
    src.insert("a").unwrap();

    // Fork once (succeeds), then fork again to the SAME path — must refuse to clobber.
    let taken = dir.path().join("fork.vocab");
    let _fork = src.fork_to(&taken).unwrap();
    assert!(
        src.fork_to(&taken).is_err(),
        "fork_to must not clobber an existing file"
    );
}

#[test]
fn fork_of_empty_trie() {
    let dir = tempdir().unwrap();
    let src = PersistentVocabARTrie::create(dir.path().join("src.vocab")).unwrap();
    let fork = src.fork_to(dir.path().join("fork.vocab")).unwrap();
    assert_eq!(fork.len(), 0);
    assert_eq!(fork.insert("first").unwrap(), 0);
}

#[test]
fn fork_from_a_storage_less_snapshot() {
    // The key reason fork_to replays (rather than image-copies) and lives on `impl<S>`: it can
    // fork a storage-less `Clone` snapshot into a real, writable, on-disk trie.
    let dir = tempdir().unwrap();
    let src = PersistentVocabARTrie::create(dir.path().join("src.vocab")).unwrap();
    seed(&src, TERMS);

    let snap = src.clone(); // detached, storage-less snapshot
    let fork = snap.fork_to(dir.path().join("fork.vocab")).unwrap();
    assert_lossless(&fork, &src);

    // The fork is genuinely writable + persistable (the snapshot is not).
    assert_eq!(fork.insert("added-to-fork").unwrap(), TERMS.len() as u64);
    fork.checkpoint().unwrap();
}
