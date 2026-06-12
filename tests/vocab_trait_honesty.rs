//! Trait-honesty tests for `PersistentVocabARTrie` and `SharedVocabARTrie`.
//!
//! These tests pin the documented no-op semantics of the trait methods that
//! cannot be honored faithfully by an append-only vocabulary trie. Each test
//! asserts that the stub returns its documented sentinel value (`false`,
//! `0`, or `Err(...)`) so any regression in the implementation surfaces here
//! rather than as a silent behavior drift.
//!
//! The companion `log::warn!` lines emitted by each stub are observable via
//! `RUST_LOG=warn cargo test --test vocab_trait_honesty -- --nocapture`.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::artrie_trait::ARTrie;
use libdictenstein::persistent_artrie::vocab::{PersistentVocabARTrie, SharedVocabARTrie};
use libdictenstein::{MappedDictionary, MutableMappedDictionary};
use parking_lot::RwLock;
use std::sync::Arc;
use tempfile::tempdir;

fn fresh_persistent(path: &std::path::Path) -> PersistentVocabARTrie {
    PersistentVocabARTrie::create(path).expect("create persistent vocab")
}

fn fresh_shared(path: &std::path::Path) -> SharedVocabARTrie {
    Arc::new(RwLock::new(fresh_persistent(path)))
}

// =============================================================================
// PersistentVocabARTrie — no interior mutability; trait methods are no-ops.
// =============================================================================

#[test]
fn persistent_vocab_insert_with_value_is_noop() {
    let dir = tempdir().unwrap();
    let vocab = fresh_persistent(&dir.path().join("vocab.dict"));

    // Returns false (sentinel = "no-op").
    assert!(!vocab.insert_with_value("apple", 999));
    // The term is NOT actually inserted (this type can't mutate via &self).
    assert!(vocab.get_value("apple").is_none());
}

#[test]
fn persistent_vocab_union_with_returns_zero() {
    let dir = tempdir().unwrap();
    let path_a = dir.path().join("a.dict");
    let path_b = dir.path().join("b.dict");
    let a = fresh_persistent(&path_a);
    let b = fresh_persistent(&path_b);

    let merge_was_called = std::cell::Cell::new(false);
    let count = a.union_with(&b, |_av, _bv| {
        merge_was_called.set(true);
        0u64
    });

    assert_eq!(count, 0, "no-op should report 0");
    assert!(!merge_was_called.get(), "merge_fn must be discarded");
}

#[test]
fn persistent_vocab_update_or_insert_is_noop() {
    let dir = tempdir().unwrap();
    let vocab = fresh_persistent(&dir.path().join("vocab.dict"));

    let update_was_called = std::cell::Cell::new(false);
    let added = vocab.update_or_insert("apple", 999, |_v| {
        update_was_called.set(true);
    });

    assert!(!added, "no-op should report no-change");
    assert!(!update_was_called.get(), "update_fn must be discarded");
    assert!(
        vocab.get_value("apple").is_none(),
        "term not actually inserted"
    );
}

// =============================================================================
// SharedVocabARTrie — actually mutates, but discards value/merge_fn/update_fn.
// =============================================================================

#[test]
fn shared_vocab_insert_with_value_ignores_value_but_inserts_term() {
    let dir = tempdir().unwrap();
    let shared = fresh_shared(&dir.path().join("vocab.dict"));

    // Caller asks for value=999. Vocab silently discards the value.
    let added = MutableMappedDictionary::insert_with_value(&shared, "apple", 999);
    assert!(added);

    // Term is inserted, but the assigned index is auto-allocated (the first
    // index in an empty trie is 0, not 999).
    let actual_index = MappedDictionary::get_value(&shared, "apple");
    assert_eq!(
        actual_index,
        Some(0),
        "vocab auto-assigns from 0, ignoring user-supplied value"
    );
}

#[test]
fn shared_vocab_union_with_ignores_merge_fn_but_unions_terms() {
    let dir = tempdir().unwrap();
    let path_a = dir.path().join("a.dict");
    let path_b = dir.path().join("b.dict");
    let a = fresh_shared(&path_a);
    let b = fresh_shared(&path_b);
    {
        let g = b.write();
        g.insert("foo").expect("insert term failed");
        g.insert("bar").expect("insert term failed");
    }

    let merge_was_called = std::cell::Cell::new(false);
    let count = MutableMappedDictionary::union_with(&a, &b, |_av: &u64, _bv: &u64| {
        merge_was_called.set(true);
        0u64
    });

    assert_eq!(count, 2);
    assert!(!merge_was_called.get(), "merge_fn must be discarded");
    assert!(a.read().contains("foo"));
    assert!(a.read().contains("bar"));
}

#[test]
fn shared_vocab_update_or_insert_ignores_callbacks_but_inserts_term() {
    let dir = tempdir().unwrap();
    let shared = fresh_shared(&dir.path().join("vocab.dict"));

    let update_was_called = std::cell::Cell::new(false);
    let added = MutableMappedDictionary::update_or_insert(&shared, "apple", 999, |_v: &mut u64| {
        update_was_called.set(true);
    });

    assert!(added, "should report new term inserted");
    assert!(!update_was_called.get(), "update_fn must be discarded");
    assert_eq!(shared.read().get_value("apple"), Some(0), "auto-assigned");

    // Calling again on the same term: update_fn would normally run, but here
    // it's still discarded and the method reports "no change".
    let added_again =
        MutableMappedDictionary::update_or_insert(&shared, "apple", 999, |_v: &mut u64| {
            update_was_called.set(true);
        });
    assert!(!added_again);
    assert!(
        !update_was_called.get(),
        "update_fn discarded even when term already exists"
    );
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
fn shared_vocab_artrie_insert_with_value_ignores_value() {
    let dir = tempdir().unwrap();
    let shared = fresh_shared(&dir.path().join("vocab.dict"));

    // ARTrie::insert_with_value ignores `value`.
    let added = ARTrie::insert_with_value(&shared, "apple", 999);
    assert!(added);
    assert_eq!(shared.read().get_value("apple"), Some(0));
}

#[test]
fn shared_vocab_artrie_upsert_ignores_value() {
    let dir = tempdir().unwrap();
    let shared = fresh_shared(&dir.path().join("vocab.dict"));

    let r = ARTrie::upsert(&shared, "apple", 999);
    assert!(r.expect("upsert succeeds for vocab"));
    assert_eq!(shared.read().get_value("apple"), Some(0));
}
