//! Trait-honesty tests for `PersistentVocabARTrie` and `SharedVocabARTrie`.
//!
//! The mapped value for a vocab trie is the assigned u64 index. New-term
//! value-shaped mutation APIs therefore honor the supplied value as an explicit
//! requested index. Existing-term updates cannot remap the index without
//! breaking the term <-> index bijection, so they report no insertion and leave
//! the assigned index unchanged.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::artrie_trait::ARTrie;
use libdictenstein::persistent_artrie::core::shared_access::SharedTrieAccess;
use libdictenstein::persistent_artrie::vocab::{PersistentVocabARTrie, SharedVocabARTrie};
use libdictenstein::{MappedDictionary, MutableMappedDictionary};
use std::sync::Arc;
use tempfile::tempdir;

fn fresh_persistent(path: &std::path::Path) -> PersistentVocabARTrie {
    PersistentVocabARTrie::create(path).expect("create persistent vocab")
}

fn fresh_shared(path: &std::path::Path) -> SharedVocabARTrie {
    Arc::new(fresh_persistent(path))
}

// =============================================================================
// PersistentVocabARTrie — value-shaped traits map values to explicit indices.
// =============================================================================

#[test]
fn persistent_vocab_insert_with_value_uses_requested_index() {
    let dir = tempdir().unwrap();
    let vocab = fresh_persistent(&dir.path().join("vocab.dict"));

    assert!(vocab.insert_with_value("apple", 999));
    assert_eq!(vocab.get_value("apple"), Some(999));
    assert_eq!(vocab.get_term(999).as_deref(), Some("apple"));
    assert!(
        !vocab.insert_with_value("apple", 999),
        "same term/index is idempotent"
    );
}

#[test]
fn persistent_vocab_insert_with_value_rejects_index_conflicts() {
    let dir = tempdir().unwrap();
    let vocab = fresh_persistent(&dir.path().join("vocab.dict"));

    assert!(vocab.insert_with_value("apple", 7));
    assert!(
        !vocab.insert_with_value("banana", 7),
        "a second term cannot reuse an assigned index"
    );
    assert!(vocab.get_value("banana").is_none());
}

#[test]
fn persistent_vocab_union_with_preserves_source_indices_for_missing_terms() {
    let dir = tempdir().unwrap();
    let path_a = dir.path().join("a.dict");
    let path_b = dir.path().join("b.dict");
    let a = fresh_persistent(&path_a);
    let b = fresh_persistent(&path_b);

    b.insert_with_index("foo", 20).expect("insert foo");
    b.insert_with_index("bar", 21).expect("insert bar");

    let merge_was_called = std::cell::Cell::new(false);
    let count = a.union_with(&b, |_av, _bv| {
        merge_was_called.set(true);
        0u64
    });

    assert_eq!(count, 2);
    assert!(!merge_was_called.get(), "merge_fn is only for conflicts");
    assert_eq!(a.get_value("foo"), Some(20));
    assert_eq!(a.get_value("bar"), Some(21));
}

#[test]
fn persistent_vocab_union_with_calls_merge_for_conflict_but_keeps_existing_index() {
    let dir = tempdir().unwrap();
    let path_a = dir.path().join("a.dict");
    let path_b = dir.path().join("b.dict");
    let a = fresh_persistent(&path_a);
    let b = fresh_persistent(&path_b);

    a.insert_with_index("foo", 10).expect("insert into a");
    b.insert_with_index("foo", 20).expect("insert into b");

    let merge_was_called = std::cell::Cell::new(false);
    let count = a.union_with(&b, |av, bv| {
        merge_was_called.set(true);
        av.max(bv).to_owned()
    });

    assert_eq!(count, 0, "existing term is not reinserted");
    assert!(merge_was_called.get(), "merge_fn should observe conflicts");
    assert_eq!(a.get_value("foo"), Some(10), "indices are immutable");
}

#[test]
fn persistent_vocab_update_or_insert_inserts_default_index() {
    let dir = tempdir().unwrap();
    let vocab = fresh_persistent(&dir.path().join("vocab.dict"));

    let update_was_called = std::cell::Cell::new(false);
    let added = vocab.update_or_insert("apple", 999, |_v| {
        update_was_called.set(true);
    });

    assert!(added, "absent term should be inserted at default index");
    assert!(
        !update_was_called.get(),
        "update_fn is only for existing terms"
    );
    assert_eq!(vocab.get_value("apple"), Some(999));
}

#[test]
fn persistent_vocab_update_or_insert_calls_update_but_does_not_remap_existing_index() {
    let dir = tempdir().unwrap();
    let vocab = fresh_persistent(&dir.path().join("vocab.dict"));

    assert!(vocab.insert_with_value("apple", 7));
    let update_was_called = std::cell::Cell::new(false);
    let added = vocab.update_or_insert("apple", 999, |v| {
        update_was_called.set(true);
        *v = 11;
    });

    assert!(!added, "existing term was not newly inserted");
    assert!(update_was_called.get(), "existing term invokes update_fn");
    assert_eq!(vocab.get_value("apple"), Some(7), "index is immutable");
}

// =============================================================================
// SharedVocabARTrie — same index-aware contract behind a synchronization handle.
// =============================================================================

#[test]
fn shared_vocab_insert_with_value_uses_requested_index() {
    let dir = tempdir().unwrap();
    let shared = fresh_shared(&dir.path().join("vocab.dict"));

    let added = MutableMappedDictionary::insert_with_value(&shared, "apple", 999);
    assert!(added);

    let actual_index = MappedDictionary::get_value(&shared, "apple");
    assert_eq!(actual_index, Some(999));
    assert_eq!(shared.read().get_term(999).as_deref(), Some("apple"));
}

#[test]
fn shared_vocab_union_with_preserves_source_indices() {
    let dir = tempdir().unwrap();
    let path_a = dir.path().join("a.dict");
    let path_b = dir.path().join("b.dict");
    let a = fresh_shared(&path_a);
    let b = fresh_shared(&path_b);
    {
        let g = b.write();
        g.insert_with_index("foo", 40).expect("insert term failed");
        g.insert_with_index("bar", 41).expect("insert term failed");
    }

    let merge_was_called = std::cell::Cell::new(false);
    let count = MutableMappedDictionary::union_with(&a, &b, |_av: &u64, _bv: &u64| {
        merge_was_called.set(true);
        0u64
    });

    assert_eq!(count, 2);
    assert!(!merge_was_called.get(), "merge_fn is only for conflicts");
    assert_eq!(a.read().get_index("foo"), Some(40));
    assert_eq!(a.read().get_index("bar"), Some(41));
}

#[test]
fn shared_vocab_update_or_insert_inserts_default_index_and_preserves_existing() {
    let dir = tempdir().unwrap();
    let shared = fresh_shared(&dir.path().join("vocab.dict"));

    let update_was_called = std::cell::Cell::new(false);
    let added = MutableMappedDictionary::update_or_insert(&shared, "apple", 999, |_v: &mut u64| {
        update_was_called.set(true);
    });

    assert!(added, "should report new term inserted");
    assert!(
        !update_was_called.get(),
        "update_fn is only for existing terms"
    );
    assert_eq!(shared.read().get_value("apple"), Some(999));

    let added_again =
        MutableMappedDictionary::update_or_insert(&shared, "apple", 111, |v: &mut u64| {
            update_was_called.set(true);
            *v = 111;
        });
    assert!(!added_again);
    assert!(update_was_called.get(), "existing term invokes update_fn");
    assert_eq!(shared.read().get_value("apple"), Some(999));
}

// =============================================================================
// SharedVocabARTrie ARTrie trait — remove/remove_prefix/increment unsupported.
// =============================================================================

#[test]
fn shared_vocab_artrie_remove_unconditionally_false() {
    let dir = tempdir().unwrap();
    let shared = fresh_shared(&dir.path().join("vocab.dict"));
    {
        let g = shared.write();
        g.insert("apple").expect("insert term failed");
    }
    assert!(shared.read().contains("apple"));

    // ARTrie::remove returns false unconditionally on vocab.
    assert!(!ARTrie::remove(&shared, "apple"));
    // Term is NOT removed.
    assert!(shared.read().contains("apple"));

    // Even for absent terms — still false (no-op, not "wasn't there").
    assert!(!ARTrie::remove(&shared, "missing"));
}

#[test]
fn shared_vocab_artrie_remove_prefix_unconditionally_zero() {
    let dir = tempdir().unwrap();
    let shared = fresh_shared(&dir.path().join("vocab.dict"));
    {
        let g = shared.write();
        g.insert("apple").expect("insert term failed");
        g.insert("application").expect("insert term failed");
    }

    assert_eq!(ARTrie::remove_prefix(&shared, "app"), 0);
    // Neither term removed.
    assert!(shared.read().contains("apple"));
    assert!(shared.read().contains("application"));
}

#[test]
fn shared_vocab_artrie_has_no_increment() {
    // C1: `increment` was removed from the `ARTrie` trait and re-homed as an inherent
    // `V: Counter` ({i64, u64}) method on the persistent COUNTER tries. A vocab value
    // is an auto-assigned index, not a counter, so vocab has NO `increment` at all —
    // `ARTrie::increment(&shared, ..)` / `shared.write().increment(..)` is now a COMPILE
    // error (more honest than the old runtime `InvalidOperation` reject; the
    // compile-time absence is pinned by the `compile_fail` doc-test on
    // `libdictenstein::value::Counter`). Here we assert the supported index path works.
    let dir = tempdir().unwrap();
    let shared = fresh_shared(&dir.path().join("vocab.dict"));
    {
        let g = shared.write();
        g.insert("counter").expect("insert term failed");
    }
    assert!(
        shared.read().contains("counter"),
        "vocab supports its auto-assigned-index ops (increment is compile-time-absent)"
    );
}

#[test]
fn shared_vocab_artrie_insert_with_value_uses_requested_index() {
    let dir = tempdir().unwrap();
    let shared = fresh_shared(&dir.path().join("vocab.dict"));

    let added = ARTrie::insert_with_value(&shared, "apple", 999);
    assert!(added);
    assert_eq!(shared.read().get_value("apple"), Some(999));
}

#[test]
fn shared_vocab_artrie_upsert_uses_requested_index() {
    let dir = tempdir().unwrap();
    let shared = fresh_shared(&dir.path().join("vocab.dict"));

    let r = ARTrie::upsert(&shared, "apple", 999);
    assert!(r.expect("upsert succeeds for vocab"));
    assert_eq!(shared.read().get_value("apple"), Some(999));
}
