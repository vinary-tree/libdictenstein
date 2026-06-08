//! CHARACTERIZATION test for BUG #46 (pre-existing, data-loss-ADJACENT, surfaced by
//! the L0.1 owned-eviction-deletion test migration). It PINS the current behavior so
//! the suite stays green and the bug stays VISIBLE; it FAILS LOUDLY the moment #46 is
//! fixed (forcing this file to be flipped to the correctness assertions), so it can
//! never silently mask the bug.
//!
//! ## The bug
//! After an in-process overlay eviction (`force_eviction` → `evict_overlay_nodes` flips
//! resident overlay children to `OnDisk`), reading an evicted node of an ARBITRARY value
//! type (e.g. `i32`) via the lazy single-node fault-in (`load_char_node_from_disk_lazy`)
//! restores the node's own value+finality but DROPS ITS CHILDREN — so deeper terms under
//! it read back as `None` until the trie is reopened:
//!   insert {alpha:1, alphabet:2, alpine:3, zenith:4} → checkpoint → force_eviction → get
//!     i32 : [Some(1), None, None, None]      ← children of the evicted node are lost
//!     u64 : [Some(1), Some(2), Some(3), Some(4)]  ← counter value type is UNAFFECTED
//!
//! ## Why it is NOT permanent loss
//! A full REOPEN recovers every i32 value — the on-disk checkpoint image is correct; only
//! the in-process single-node fault path mis-handles arbitrary-V records. So the data is
//! safe across restarts; the defect is wrong in-process reads after eviction for non-u64 V.
//!
//! ## Scope / fix
//! u64 (the libgrammstein counter use case) is unaffected. MUST be fixed before L3.1, which
//! removes the owned reopen scratch — the only recovery path. Production read path +
//! data-loss → plan→red-team→implement rigor. See task #46.
//!
//! Scratch is REAL DISK (`target/test-tmp`), never tmpfs `/tmp`.
#![cfg(feature = "persistent-artrie")]

use std::sync::Arc;

use libdictenstein::artrie_trait::{ARTrie, EvictableARTrie};
use libdictenstein::persistent_artrie::eviction::EvictionConfig;
use libdictenstein::persistent_artrie::WalConfig;
use libdictenstein::persistent_artrie_char::{PersistentARTrieChar, SharedCharARTrie};
use libdictenstein::persistent_artrie_core::durability::DurabilityPolicy;
use libdictenstein::persistent_artrie_core::shared_access::SharedTrieAccess;
use libdictenstein::{MappedDictionary, MutableMappedDictionary};

fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch")
}

const KEYS: [&str; 4] = ["alpha", "alphabet", "alpine", "zenith"];
fn expected_i32() -> Vec<Option<i32>> {
    (1..=4).map(|v| Some(v)).collect()
}

/// PIN: arbitrary-V (i32) fault-in currently LOSES the evicted node's children in-process,
/// but a REOPEN recovers them (disk image intact). When #46 is fixed, `after_fault` will
/// equal `before` and the first assertion below will fail — flip this file to assert
/// full in-process survival at that point.
#[test]
fn bug46_arbitrary_v_faultin_loses_children_but_reopen_recovers() {
    let dir = scratch("bug46-i32");
    let path = dir.path().join("b46.artc");
    let shared: SharedCharARTrie<i32> = ARTrie::create(&path).expect("create");
    for (i, t) in KEYS.iter().enumerate() {
        assert!(MutableMappedDictionary::insert_with_value(
            &shared,
            t,
            (i + 1) as i32
        ));
    }
    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable");
    shared.write().checkpoint().expect("checkpoint");
    let before: Vec<Option<i32>> = KEYS.iter().map(|t| shared.read().get(t)).collect();
    assert_eq!(
        before,
        expected_i32(),
        "values must be present before eviction"
    );

    let (evicted, _) = shared.force_eviction(1 << 20).expect("force");
    assert!(evicted >= 1, "expected >=1 node evicted, got {evicted}");
    let after_fault: Vec<Option<i32>> = KEYS.iter().map(|t| shared.read().get(t)).collect();
    shared.disable_eviction().ok();
    drop(shared);

    let reopened: SharedCharARTrie<i32> = ARTrie::open(&path).expect("reopen");
    let after_reopen: Vec<Option<i32>> = KEYS.iter().map(|t| reopened.read().get(t)).collect();

    // PIN the bug. This MUST be flipped to `assert_eq!(after_fault, before)` when #46 lands.
    assert_ne!(
        after_fault, before,
        "BUG #46 appears FIXED (arbitrary-V fault-in preserved children) — flip this \
         characterization test to assert full in-process survival (see task #46)."
    );
    // The data is never permanently lost: reopen recovers it from the disk image.
    assert_eq!(
        after_reopen, before,
        "reopen MUST recover every value (on-disk checkpoint image is intact)"
    );
}

/// BASELINE: the same eviction + in-process fault-in path on a u64 counter trie preserves
/// every value — proving #46 is specific to non-u64 value types (not a general fault bug).
#[test]
fn bug46_baseline_u64_faultin_preserves_values() {
    let dir = scratch("bug46-u64");
    let path = dir.path().join("b46.artc");
    let mut trie = PersistentARTrieChar::<u64>::create_with_config(&path, WalConfig::no_archive())
        .expect("create");
    trie.set_durability_policy(DurabilityPolicy::Immediate);
    trie.enable_lockfree();
    let trie = Arc::new(trie);
    trie.enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable");
    for (i, t) in KEYS.iter().enumerate() {
        trie.try_increment_cas_durable(t, (i + 1) as u64)
            .expect("inc");
    }
    trie.checkpoint().expect("checkpoint");
    let before: Vec<Option<u64>> = KEYS
        .iter()
        .map(|t| MappedDictionary::get_value(&*trie, t))
        .collect();
    let (evicted, _) = trie.force_eviction(1 << 20).expect("force");
    assert!(evicted >= 1, "expected >=1 node evicted, got {evicted}");
    let after_fault: Vec<Option<u64>> = KEYS
        .iter()
        .map(|t| MappedDictionary::get_value(&*trie, t))
        .collect();
    trie.disable_eviction().ok();
    assert_eq!(
        after_fault, before,
        "u64 baseline: in-process fault-in must preserve all values"
    );
}
