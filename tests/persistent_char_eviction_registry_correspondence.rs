#![cfg(feature = "persistent-artrie")]
//! Correspondence between the eviction-registry-publication model and the real
//! persistent char trie (HEAD commit f10c43e, feature G6).
//!
//! Models:
//! - TLA+: `formal-verification/tla+/EvictionRegistryPublication.tla`
//! - Rocq: `formal-verification/rocq/Spec/PersistentCharEvictionRegistrySpec.v`
//!
//! Each test names the model invariant / theorem it refines. The model proves
//! that the eviction `DiskLocationRegistry` is published only after a verified
//! checkpoint, references only durable on-disk data, is invalidated by writes,
//! and is not recovery state; these tests demonstrate the implementation obeys
//! those properties through its public API.

use libdictenstein::artrie_trait::{ARTrie, EvictableARTrie};
use libdictenstein::persistent_artrie::eviction::EvictionConfig;
use libdictenstein::persistent_artrie_char::SharedCharARTrie;
use libdictenstein::MutableMappedDictionary;
use std::path::Path;
use tempfile::tempdir;

fn put(shared: &SharedCharARTrie<i32>, term: &str, value: i32) -> bool {
    MutableMappedDictionary::insert_with_value(shared, term, value)
}

fn value_of(shared: &SharedCharARTrie<i32>, term: &str) -> Option<i32> {
    shared.read().get(term).copied()
}

const KEYS: [(&str, i32); 5] = [
    ("alpha", 1),
    ("alphabet", 2),
    ("alpine", 3),
    ("zenith", 4),
    ("zephyr", 5),
];

fn build(path: &Path) -> SharedCharARTrie<i32> {
    let shared: SharedCharARTrie<i32> = ARTrie::create(path).expect("create char trie");
    for (t, v) in KEYS {
        assert!(put(&shared, t, v));
    }
    shared
}

/// TLA `NoPublishWithoutVerify` + Rocq `publish_empty_unless_verified`:
/// `checkpoint()` calls `update_disk_registry` only AFTER `verify_checkpoint()`
/// succeeds, so the published registry is empty (eviction selects nothing) until
/// the first checkpoint.
#[test]
fn registry_empty_until_verified_checkpoint() {
    let dir = tempdir().expect("tempdir");
    let shared = build(&dir.path().join("publish.trie"));
    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable");

    // No checkpoint has verified+published anything yet.
    assert_eq!(shared.read().evictable_node_count(), Some(0));
    assert_eq!(shared.force_eviction(1 << 20).expect("force"), (0, 0));

    shared.write().checkpoint().expect("checkpoint");

    // After a verified checkpoint, the registry is published and non-empty.
    assert!(shared.read().evictable_node_count().unwrap() > 0);
    assert!(shared.force_eviction(1 << 20).expect("force").0 >= 1);

    shared.disable_eviction().expect("disable");
}

/// TLA `RegistryEntriesAreDurable` + `EvictedNodeRemainsResolvable`: every node
/// eviction reclaims references durable, verified on-disk data, so an evicted
/// key always reloads to its exact value -- and a fresh reopen (which reads only
/// the durable image) agrees, confirming the registry pointed at durable data.
#[test]
fn evicted_entries_reference_durable_data() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("durable.trie");
    {
        let shared = build(&path);
        shared
            .enable_eviction(EvictionConfig::without_memory_monitor())
            .expect("enable");
        shared.write().checkpoint().expect("checkpoint");

        let evicted = shared.force_eviction(1 << 20).expect("force").0;
        assert!(evicted >= 1, "expected real reclamation, got {evicted}");

        // Reload path: every key still resolves to its value after eviction.
        for (t, v) in KEYS {
            assert_eq!(value_of(&shared, t), Some(v));
        }
        shared.disable_eviction().expect("disable");
    }
    // A fresh reopen reads only the durable on-disk image and agrees.
    let reopened: SharedCharARTrie<i32> = ARTrie::open(&path).expect("reopen");
    for (t, v) in KEYS {
        assert_eq!(value_of(&reopened, t), Some(v));
    }
}

/// TLA `Mutate` action (the A1 fix): a write invalidates the published registry,
/// preserving `RegistryEntriesAreDurable` across mutations -- eviction selects
/// nothing until the next checkpoint republishes a fresh registry.
#[test]
fn write_invalidates_published_registry() {
    let dir = tempdir().expect("tempdir");
    let shared = build(&dir.path().join("invalidate.trie"));
    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable");

    shared.write().checkpoint().expect("checkpoint");
    assert!(shared.force_eviction(1 << 20).expect("force").0 >= 1);

    // Re-publish, then mutate: invalidation makes eviction a no-op.
    shared.write().checkpoint().expect("re-checkpoint");
    assert!(put(&shared, "newcomer", 99));
    assert_eq!(shared.force_eviction(1 << 20).expect("force"), (0, 0));

    // The next checkpoint republishes; eviction works again.
    shared.write().checkpoint().expect("checkpoint 3");
    assert!(shared.force_eviction(1 << 20).expect("force").0 >= 1);
    assert_eq!(value_of(&shared, "newcomer"), Some(99));

    shared.disable_eviction().expect("disable");
}

/// TLA `RecoveredAreDurable` + `JustRecoveredMatchesDurable`; Rocq
/// `recovery_independent_of_registry` + `registry_is_side_effect_free_on_disk_root`:
/// reopening recovers exactly the durable map, independent of whether the
/// registry was ever built or used for eviction.
#[test]
fn recovery_independent_of_registry() {
    let dir = tempdir().expect("tempdir");
    let with_path = dir.path().join("with.trie");
    let without_path = dir.path().join("without.trie");

    {
        let shared = build(&with_path);
        shared
            .enable_eviction(EvictionConfig::without_memory_monitor())
            .expect("enable");
        shared.write().checkpoint().expect("checkpoint with");
        assert!(shared.force_eviction(1 << 20).expect("force").0 >= 1);
        shared.disable_eviction().expect("disable");
    }
    {
        let shared = build(&without_path);
        shared.write().checkpoint().expect("checkpoint without");
    }

    let a: SharedCharARTrie<i32> = ARTrie::open(&with_path).expect("reopen with");
    let b: SharedCharARTrie<i32> = ARTrie::open(&without_path).expect("reopen without");
    for (t, v) in KEYS {
        assert_eq!(value_of(&a, t), Some(v));
        assert_eq!(value_of(&a, t), value_of(&b, t));
    }
}
