//! White-box tests for the eviction `DiskLocationRegistry` wiring added in HEAD
//! commit f10c43e. These live in-crate (not in `tests/`) because they inspect
//! private node internals and drive the private `EvictionCoordinator` directly:
//!
//! - **State oracle**: prove that a synchronous `force_eviction` actually turns a
//!   live (swizzled) child slot into an on-disk DiskRef — i.e. real reclamation,
//!   not a no-op callback. Integration tests cannot observe this because the
//!   `transition` node API does not reload evicted nodes and the `get` read path
//!   transparently re-swizzles them.
//! - **Async end-to-end**: drive the production async eviction path
//!   (`request_eviction` → background `eviction_loop_char` → `evict_char_nodes`)
//!   over a checkpoint-populated registry and confirm it reclaims nodes while
//!   keys remain readable. `request_eviction` is only on the private coordinator,
//!   so this cannot be expressed through the public trie API.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::artrie_trait::{ARTrie, EvictableARTrie};
use crate::persistent_artrie::eviction::{EvictionConfig, EvictionUrgency};
// F4: the `.read()/.write()` compat shim on the collapsed handle.
use crate::persistent_artrie_char::types::{CharTrieNodeInner, CharTrieRoot};
use crate::persistent_artrie_char::SharedCharARTrie;
use crate::persistent_artrie_core::shared_access::SharedTrieAccess;
use crate::MutableMappedDictionary;

/// Insert via the explicit `MutableMappedDictionary` method (both it and `ARTrie`
/// expose an `insert_with_value`, so the bare call is ambiguous).
fn put(shared: &SharedCharARTrie<i32>, term: &str, value: i32) -> bool {
    MutableMappedDictionary::insert_with_value(shared, term, value)
}

/// Follow a single-child chain from the root, returning `true` as soon as a child
/// slot along `chain` is on-disk (unswizzled). Walks only through in-memory
/// (swizzled) slots, since an on-disk parent cannot be descended without
/// reloading. With a linear-chain key, the first on-disk slot encountered is the
/// shallowest node that eviction unswizzled.
fn chain_has_on_disk_slot(root: &CharTrieRoot<i32>, chain: &str) -> bool {
    let mut current: &CharTrieNodeInner<i32> = match root {
        CharTrieRoot::Node(boxed) => boxed.as_ref(),
        CharTrieRoot::Empty => return false,
    };
    for c in chain.chars() {
        match current.node.find_child(c as u32) {
            Some(slot) if slot.is_on_disk() => return true,
            Some(slot) => match slot.as_ptr::<CharTrieNodeInner<i32>>() {
                // In-memory: descend.
                Some(ptr) => current = unsafe { &*ptr },
                None => return false,
            },
            None => return false,
        }
    }
    false
}

/// Count how many slots along `chain` are currently swizzled (in-memory),
/// walking only through in-memory slots.
fn chain_swizzled_slot_count(root: &CharTrieRoot<i32>, chain: &str) -> usize {
    let mut current: &CharTrieNodeInner<i32> = match root {
        CharTrieRoot::Node(boxed) => boxed.as_ref(),
        CharTrieRoot::Empty => return 0,
    };
    let mut count = 0;
    for c in chain.chars() {
        match current.node.find_child(c as u32) {
            Some(slot) if slot.is_swizzled() => {
                count += 1;
                match slot.as_ptr::<CharTrieNodeInner<i32>>() {
                    Some(ptr) => current = unsafe { &*ptr },
                    None => break,
                }
            }
            _ => break,
        }
    }
    count
}

#[test]
fn force_eviction_unswizzles_a_live_slot_to_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("whitebox.trie");

    // A single linear chain so eviction has unambiguous, navigable nodes.
    let chain = "abcdefgh";
    let shared: SharedCharARTrie<i32> = ARTrie::create(&path).expect("create");
    // F2-migrate: Bucket B — white-box over the OWNED tree (`chain_swizzled_slot_count`
    // walks `guard.root`; eviction unswizzles owned slots). Under the lock-free overlay
    // the owned tree is empty, so pin the Owned regime. No-op feature-off.
    shared.write().kill_switch_to_owned();
    assert!(put(&shared, chain, 42));

    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable");

    // Before checkpoint: the whole chain is in memory (swizzled).
    {
        let guard = shared.read();
        let swizzled_before = chain_swizzled_slot_count(&*guard.root.read(), chain);
        assert_eq!(
            swizzled_before,
            chain.len(),
            "every chain slot should be in-memory before eviction"
        );
        assert!(
            !chain_has_on_disk_slot(&*guard.root.read(), chain),
            "no slot should be on-disk before eviction"
        );
    }

    shared.write().checkpoint().expect("checkpoint");

    // Synchronous reclamation through the char-aware path.
    let (evicted, _bytes) = shared.force_eviction(1 << 20).expect("force eviction");
    assert!(evicted >= 1, "expected >=1 node reclaimed, got {evicted}");

    // State oracle: BEFORE any `get` re-swizzles anything, at least one chain slot
    // is now an on-disk DiskRef — proof that the box was actually reclaimed.
    {
        let guard = shared.read();
        assert!(
            chain_has_on_disk_slot(&*guard.root.read(), chain),
            "force_eviction must leave >=1 chain slot on-disk (real reclamation)"
        );
    }

    // And the key still resolves via reload-from-disk.
    assert_eq!(shared.read().get(chain), Some(42));

    shared.disable_eviction().expect("disable");
}

#[test]
fn async_request_eviction_reclaims_registered_nodes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("async.trie");

    let shared: SharedCharARTrie<i32> = ARTrie::create(&path).expect("create");
    // F2-migrate: Bucket B — exercises the async OWNED-node reclamation path. Under the
    // lock-free overlay the owned tree is empty (nothing to reclaim), so pin the Owned
    // regime. No-op feature-off.
    shared.write().kill_switch_to_owned();
    for (i, term) in ["alpha", "alphabet", "alpine", "zenith", "zephyr"]
        .iter()
        .enumerate()
    {
        assert!(put(&shared, term, i as i32));
    }

    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable");
    shared.write().checkpoint().expect("checkpoint");

    // Clone the coordinator Arc so we can drive the async path and poll stats
    // WITHOUT holding the trie lock (the background thread takes the write lock).
    let coordinator = shared
        .read()
        .eviction_coordinator
        .lock()
        .expect("eviction_coordinator mutex poisoned")
        .clone()
        .expect("eviction enabled");
    assert!(coordinator.disk_registry_char_len() > 0);

    coordinator.request_eviction(EvictionUrgency::Emergency);

    // Bounded wait for the background eviction loop to reclaim at least one node.
    // No concurrent readers => quiescence resolves immediately; this is a real
    // (non-ignored) test, with a generous ceiling only to tolerate a loaded CI.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if coordinator.stats().nodes_evicted >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "async eviction did not reclaim any node within the timeout"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // Every key still resolves to its value via reload-from-disk.
    for (i, term) in ["alpha", "alphabet", "alpine", "zenith", "zephyr"]
        .iter()
        .enumerate()
    {
        assert_eq!(shared.read().get(term), Some(i as i32));
    }

    shared.disable_eviction().expect("disable");
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
