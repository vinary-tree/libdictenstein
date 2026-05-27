#![cfg(feature = "persistent-artrie")]
//! Behavioral tests for HEAD commit f10c43e (persistent char trie).
//!
//! **G6 — eviction registry populated at checkpoint.** `checkpoint()` builds and
//! publishes a `DiskLocationRegistry` of char-node disk locations. These tests
//! drive real char-node reclamation through `force_eviction`, which now routes
//! to the char-aware `force_eviction_char` (it was previously a byte-only no-op
//! for char tries). They also cover the **A1 data-loss fix**: a mutation after a
//! checkpoint invalidates the published registry so eviction cannot unswizzle a
//! newer in-memory node onto a now-stale on-disk pointer.
//!
//! **G9 — `MappedDictionaryNode::value()`** on `PersistentARTrieCharNode<V>`.
//!
//! Two facts shape these tests:
//! 1. The node-walk API (`transition`) does NOT lazily reload evicted (on-disk)
//!    nodes — only the `get` read path does (via `get_child_lazy`). So G9
//!    `value()` is exercised over in-memory tries, and "value survives eviction"
//!    is checked through the reloading `get` path.
//! 2. The white-box proof that an evicted slot actually becomes a DiskRef — and
//!    the end-to-end *async* eviction path (`request_eviction` lives only on the
//!    private coordinator) — need internal access and live in the in-crate
//!    `#[cfg(test)]` module `persistent_artrie_char::eviction_registry_tests`.
//!    Here we use the public contract: `force_eviction` returns the count of
//!    nodes actually unswizzled (`evict_node_at_path` succeeded), and
//!    reload-from-disk preserves values.
//!
//! The "registry is a pure side-effect on serialization" claim is proven
//! formally in `formal-verification/rocq/Spec/PersistentCharEvictionRegistrySpec.v`
//! (`registry_is_side_effect_free_on_disk_root`) and observed here as
//! recovery-being-unaffected (`reopen_identical_with_and_without_eviction`) — a
//! raw byte-for-byte file compare is confounded by the checkpoint timestamp.

use libdictenstein::artrie_trait::{ARTrie, EvictableARTrie};
use libdictenstein::persistent_artrie::eviction::EvictionConfig;
use libdictenstein::persistent_artrie_char::{
    PersistentARTrieChar, PersistentARTrieCharNode, SharedCharARTrie,
};
use libdictenstein::{DictionaryNode, MappedDictionaryNode, MutableMappedDictionary};
use std::path::Path;
use tempfile::tempdir;

/// Read a value through the reloading `get` path (unambiguous inherent method;
/// both `MappedDictionary` and `ARTrie` also expose a `get_value`).
fn value_of(shared: &SharedCharARTrie<i32>, term: &str) -> Option<i32> {
    shared.read().get(term).copied()
}

/// Insert via the explicit `MutableMappedDictionary` method (both it and `ARTrie`
/// expose an `insert_with_value`, so the bare method call is ambiguous).
fn put(shared: &SharedCharARTrie<i32>, term: &str, value: i32) -> bool {
    MutableMappedDictionary::insert_with_value(shared, term, value)
}

/// Build a disk-backed shared char trie with multi-char, prefix-sharing keys so
/// many nodes sit at depth >= 1 (the default `min_eviction_depth`).
fn deep_shared_trie(path: &Path) -> SharedCharARTrie<i32> {
    let shared: SharedCharARTrie<i32> = ARTrie::create(path).expect("create char trie");
    assert!(put(&shared, "alpha", 1));
    assert!(put(&shared, "alphabet", 2));
    assert!(put(&shared, "alpine", 3));
    assert!(put(&shared, "zenith", 4));
    shared
}

const KEYS: [(&str, Option<i32>); 5] = [
    ("alpha", Some(1)),
    ("alphabet", Some(2)),
    ("alpine", Some(3)),
    ("zenith", Some(4)),
    ("missing", None),
];

fn snapshot(shared: &SharedCharARTrie<i32>) -> Vec<Option<i32>> {
    KEYS.iter().map(|(t, _)| value_of(shared, t)).collect()
}

fn expected() -> Vec<Option<i32>> {
    KEYS.iter().map(|(_, v)| *v).collect()
}

// ============================ G6: registry @ checkpoint ============================

#[test]
fn force_eviction_char_reclaims_and_key_reloads() {
    let dir = tempdir().expect("tempdir");
    let shared = deep_shared_trie(&dir.path().join("evict.trie"));

    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable eviction");

    // The registry is empty until the first checkpoint publishes it.
    assert_eq!(shared.read().evictable_node_count(), Some(0));

    shared.write().checkpoint().expect("checkpoint");

    // G6: checkpoint populated and published the char-location registry.
    let registered = shared
        .read()
        .evictable_node_count()
        .expect("eviction enabled");
    assert!(
        registered > 0,
        "checkpoint should register char nodes, got {registered}"
    );

    let before = snapshot(&shared);
    assert_eq!(before, expected());

    // `force_eviction` now routes to `force_eviction_char` (previously a no-op).
    // The returned count is the number of nodes actually unswizzled.
    let (evicted, _bytes) = shared.force_eviction(1 << 20).expect("force eviction");
    assert!(
        evicted >= 1,
        "expected >=1 char node reclaimed, got {evicted}"
    );

    // Behavioral oracle: every key still resolves to its value via reload-from-disk.
    assert_eq!(
        snapshot(&shared),
        before,
        "values must survive eviction via reload-from-disk"
    );

    shared.disable_eviction().expect("disable eviction");
}

#[test]
fn force_eviction_char_noop_when_registry_empty() {
    let dir = tempdir().expect("tempdir");
    let shared = deep_shared_trie(&dir.path().join("noreg.trie"));
    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable");
    // No checkpoint => no published registry => nothing to evict.
    assert_eq!(shared.force_eviction(1 << 20).expect("force"), (0, 0));
    shared.disable_eviction().expect("disable");
}

#[test]
fn force_eviction_is_zero_when_disabled() {
    let dir = tempdir().expect("tempdir");
    let shared = deep_shared_trie(&dir.path().join("disabled.trie"));
    assert!(!shared.eviction_enabled());
    assert_eq!(shared.force_eviction(1 << 20).expect("force"), (0, 0));
    assert_eq!(shared.read().evictable_node_count(), None);
}

#[test]
fn post_checkpoint_write_invalidates_registry() {
    // A1 data-loss fix: a mutation after checkpoint must invalidate the published
    // registry so eviction cannot unswizzle a newer in-memory node onto a stale
    // on-disk pointer (a lost update). Invalidation happens at the single
    // mutation chokepoint `append_to_wal`.
    let dir = tempdir().expect("tempdir");
    let shared = deep_shared_trie(&dir.path().join("invalidate.trie"));
    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable");

    shared.write().checkpoint().expect("checkpoint 1");
    assert!(
        shared.force_eviction(1 << 20).expect("force pre-write").0 >= 1,
        "fresh registry should yield evictions"
    );

    // Republish a fresh, valid registry, then mutate it out from under eviction.
    shared.write().checkpoint().expect("checkpoint 2");
    assert!(put(&shared, "newkey", 99)); // append_to_wal -> invalidate

    // Registry now invalid: eviction must select nothing until the next checkpoint.
    assert_eq!(
        shared.force_eviction(1 << 20).expect("force post-write"),
        (0, 0),
        "a post-checkpoint write must invalidate the eviction registry"
    );

    // A fresh checkpoint rebuilds + republishes; eviction works again, and the
    // newly inserted key is durable.
    shared.write().checkpoint().expect("checkpoint 3");
    assert!(
        shared
            .force_eviction(1 << 20)
            .expect("force post-recheckpoint")
            .0
            >= 1
    );
    assert_eq!(value_of(&shared, "newkey"), Some(99));

    shared.disable_eviction().expect("disable");
}

#[test]
fn force_eviction_respects_min_depth() {
    let dir = tempdir().expect("tempdir");
    let shared = deep_shared_trie(&dir.path().join("mindepth.trie"));
    let mut cfg = EvictionConfig::without_memory_monitor();
    cfg.min_eviction_depth = 50; // deeper than any key here
    shared.enable_eviction(cfg).expect("enable");
    shared.write().checkpoint().expect("checkpoint");

    // The registry is populated, but no node is deep enough to be selected.
    assert!(shared.read().evictable_node_count().unwrap() > 0);
    assert_eq!(shared.force_eviction(1 << 20).expect("force"), (0, 0));

    shared.disable_eviction().expect("disable");
}

#[test]
fn empty_trie_checkpoint_registers_nothing() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("empty.trie");
    let shared: SharedCharARTrie<i32> = ARTrie::create(&path).expect("create");
    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable");
    shared.write().checkpoint().expect("checkpoint empty");
    assert_eq!(shared.read().evictable_node_count(), Some(0));
    assert_eq!(shared.force_eviction(1 << 20).expect("force"), (0, 0));
    shared.disable_eviction().expect("disable");
}

// ============================ G6: recovery unaffected ============================

#[test]
fn reopen_identical_with_and_without_eviction() {
    // The registry is ephemeral and not recovery state: building it (and evicting
    // through it) must not change what a fresh reopen recovers. Write the same
    // data to two files — one with eviction enabled + checkpoint + force-evict,
    // one with eviction disabled + checkpoint — then reopen BOTH fresh (no
    // eviction) and assert identical mappings.
    let dir = tempdir().expect("tempdir");
    let with_path = dir.path().join("with_eviction.trie");
    let without_path = dir.path().join("without_eviction.trie");

    {
        let shared = deep_shared_trie(&with_path);
        shared
            .enable_eviction(EvictionConfig::without_memory_monitor())
            .expect("enable");
        shared
            .write()
            .checkpoint()
            .expect("checkpoint with eviction");
        assert!(shared.force_eviction(1 << 20).expect("force").0 >= 1);
        shared.disable_eviction().expect("disable");
    }
    {
        let shared = deep_shared_trie(&without_path);
        shared
            .write()
            .checkpoint()
            .expect("checkpoint without eviction");
    }

    let reopened_with: SharedCharARTrie<i32> = ARTrie::open(&with_path).expect("reopen with");
    let reopened_without: SharedCharARTrie<i32> =
        ARTrie::open(&without_path).expect("reopen without");

    for (term, value) in KEYS {
        assert_eq!(
            value_of(&reopened_with, term),
            value,
            "eviction must not corrupt recovery for {term:?}"
        );
        assert_eq!(
            value_of(&reopened_with, term),
            value_of(&reopened_without, term),
            "reopen must be identical with and without eviction for {term:?}"
        );
    }
}

// ============================ G9: MappedDictionaryNode::value ============================

/// Walk the node API from the root; returns `(is_final, value)` at the term, or
/// `(false, None)` if the term's path does not exist. In-memory only (the node
/// API does not reload evicted nodes — see the module docs).
fn value_at(trie: &PersistentARTrieChar<i32>, term: &str) -> (bool, Option<i32>) {
    let mut node = trie.root();
    for c in term.chars() {
        match node.transition(c) {
            Some(next) => node = next,
            None => return (false, None),
        }
    }
    (node.is_final(), node.value())
}

#[test]
fn value_at_terminal_and_nonterminal_nodes() {
    let mut trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
    trie.insert_with_value("test", 123).expect("insert test");
    trie.insert_with_value("tea", 7).expect("insert tea");

    // Terminal nodes carry their value.
    assert_eq!(value_at(&trie, "test"), (true, Some(123)));
    assert_eq!(value_at(&trie, "tea"), (true, Some(7)));

    // Interior, non-terminal nodes have no value.
    assert_eq!(value_at(&trie, "te"), (false, None));
    assert_eq!(value_at(&trie, "t"), (false, None));

    // A path that does not exist.
    assert_eq!(value_at(&trie, "zzz"), (false, None));
}

#[test]
fn value_on_empty_root_is_none() {
    // Covers the `self.node == None` arm of `value()`'s `and_then` — returns
    // `None` without dereferencing a pointer.
    let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
    let root = trie.root();
    assert_eq!(root.value(), None);
    assert!(!root.is_final());
}

#[test]
fn value_survives_eviction_via_get() {
    // G6 x G9: after eviction, the value-returning READ path reloads the evicted
    // node from disk and returns the stored value. (The node-walk `value_at`
    // would not reload — that is the in-memory-only node API.)
    let dir = tempdir().expect("tempdir");
    let shared = deep_shared_trie(&dir.path().join("g9evict.trie"));
    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable");
    shared.write().checkpoint().expect("checkpoint");
    assert!(shared.force_eviction(1 << 20).expect("force").0 >= 1);

    assert_eq!(value_of(&shared, "alphabet"), Some(2));
    assert_eq!(value_of(&shared, "zenith"), Some(4));
    assert_eq!(value_of(&shared, "missing"), None);

    shared.disable_eviction().expect("disable");
}

/// Compile-time proof that G9 wired the trait up: the value-aware transducer API
/// in the downstream `liblevenshtein` crate requires `D::Node:
/// MappedDictionaryNode`, and `<PersistentARTrieChar<V> as Dictionary>::Node` is
/// `PersistentARTrieCharNode<V>`. This function compiles only if that bound holds
/// with `Value = i32`.
fn _assert_node_is_mapped()
where
    PersistentARTrieCharNode<i32>: MappedDictionaryNode<Value = i32>,
{
}

#[test]
fn mapped_dictionary_node_trait_is_satisfied() {
    // Force the static assertion above to be type-checked (and document intent).
    let _ = _assert_node_is_mapped;
}
