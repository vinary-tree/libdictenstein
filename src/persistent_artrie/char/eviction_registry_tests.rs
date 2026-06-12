//! White-box test for the eviction `DiskLocationRegistry` wiring (commit f10c43e). Lives
//! in-crate (not in `tests/`) because it drives the private `EvictionCoordinator`
//! directly: `force_eviction_char` selects from `char_locations` and invokes the callback
//! inline (unlike the byte `force_eviction`, which never calls back).
//!
//! (Two owned-tree white-box tests — the "state oracle" unswizzle probe and the async
//! reclaim end-to-end — were retired at L0.1 with the owned eviction path. The overlay
//! evict-to-disk primitive is covered by `lockfree_cas::eviction_primitive_tests`; the
//! async path's arbitrary-V value-fault invariant is pinned by
//! `tests/overlay_eviction_arbitrary_v_bug46.rs`, BUG #46.)

use std::sync::Arc;

use parking_lot::RwLock;

use crate::artrie_trait::{ARTrie, EvictableARTrie};
use crate::persistent_artrie::char::SharedCharARTrie;
use crate::persistent_artrie::core::shared_access::SharedTrieAccess;
use crate::persistent_artrie::eviction::EvictionConfig;
use crate::MutableMappedDictionary;

/// Insert via the explicit `MutableMappedDictionary` method (both it and `ARTrie`
/// expose an `insert_with_value`, so the bare call is ambiguous).
fn put(shared: &SharedCharARTrie<i32>, term: &str, value: i32) -> bool {
    MutableMappedDictionary::insert_with_value(shared, term, value)
}

#[test]
fn force_eviction_char_invoked_directly_on_coordinator() {
    // Exercise EvictionCoordinator::force_eviction_char in isolation: it selects
    // from char_locations and invokes the callback inline, unlike the byte
    // force_eviction which never calls back.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("direct.trie");

    let shared: SharedCharARTrie<i32> = ARTrie::create(&path).expect("create");
    assert!(put(&shared, "hello", 1));
    assert!(put(&shared, "help", 2));
    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable");
    shared.write().checkpoint().expect("checkpoint");

    let coordinator = shared
        .read()
        .eviction_coordinator
        .lock()
        .expect("eviction_coordinator mutex poisoned")
        .clone()
        .expect("eviction enabled");

    // The callback observes the selected char paths and reports them as "evicted".
    let observed = Arc::new(RwLock::new(Vec::<Vec<char>>::new()));
    let observed_cb = Arc::clone(&observed);
    let (count, _bytes) = coordinator.force_eviction_char(1 << 20, move |nodes| {
        let n = nodes.len();
        for (_, path, _) in &nodes {
            observed_cb.write().push(path.clone());
        }
        (n, n * 256)
    });

    assert!(
        count >= 1,
        "force_eviction_char should select >=1 char node"
    );
    assert_eq!(
        observed.read().len(),
        count,
        "callback must receive exactly the selected nodes"
    );
    // Every reported path is non-empty (the root, depth 0, is never selected).
    assert!(observed.read().iter().all(|p| !p.is_empty()));

    shared.disable_eviction().expect("disable");
}
