//! White-box test for exact character-registry eviction wiring. It lives in-crate
//! because the registry and coordinator are private implementation details.
//!
//! (Two owned-tree white-box tests — the "state oracle" unswizzle probe and the async
//! reclaim end-to-end — were retired at L0.1 with the owned eviction path. The overlay
//! evict-to-disk primitive is covered by `lockfree_cas::eviction_primitive_tests`; the
//! async path's arbitrary-V value-fault invariant is pinned by
//! `tests/overlay_eviction_arbitrary_v_bug46.rs`, BUG #46.)

use crate::artrie_trait::{ARTrie, EvictableARTrie};
use crate::persistent_artrie::char::SharedCharARTrie;
use crate::persistent_artrie::core::shared_access::SharedTrieAccess;
use crate::persistent_artrie::eviction::EvictionConfig;
use crate::{MappedDictionary, MutableMappedDictionary};

/// Insert via the explicit `MutableMappedDictionary` method (both it and `ARTrie`
/// expose an `insert_with_value`, so the bare call is ambiguous).
fn put(shared: &SharedCharARTrie<i32>, term: &str, value: i32) -> bool {
    MutableMappedDictionary::insert_with_value(shared, term, value)
}

#[test]
fn exact_char_registry_drives_trie_level_eviction_and_fault_recovery() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("direct.trie");

    let shared: SharedCharARTrie<i32> = ARTrie::create(&path).expect("create");
    assert!(put(&shared, "hello", 1));
    assert!(put(&shared, "help", 2));
    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable");
    shared.write().checkpoint().expect("checkpoint");

    let (count, _bytes) = shared.force_eviction(1 << 20).expect("exact eviction");

    assert!(
        count >= 1,
        "exact char registry should reclaim at least one node"
    );
    assert_eq!(MappedDictionary::get_value(&shared, "hello"), Some(1));
    assert_eq!(MappedDictionary::get_value(&shared, "help"), Some(2));

    shared.disable_eviction().expect("disable");
}
