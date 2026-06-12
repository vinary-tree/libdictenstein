//! REGRESSION test for BUG #46 (FIXED): arbitrary-V (non-`u64`) char overlay reads must
//! FAULT evicted nodes back in. Surfaced by the L0.1 owned-eviction-deletion test
//! migration; fixed in `overlay_write_mode.rs::overlay_value_get`.
//!
//! ## The bug (now fixed)
//! After an in-process overlay eviction (`force_eviction` → `evict_overlay_nodes` flips
//! resident overlay children to `OnDisk`), reading an evicted node of an ARBITRARY value
//! type (e.g. `i32`) dropped the node's children: deeper terms read back as `None` until
//! the trie was reopened. The arbitrary-V value-route arm (`overlay_value_get`) used a
//! NON-faulting walk (`find_leaf_lockfree`) on the false premise "overlay finals are never
//! evicted", while the `u64`-counter (`overlay_counter_get`) and `()`-membership
//! (`overlay_contains`) arms already faulted. The fix routes the arbitrary-V read through
//! `find_leaf_faulting` like the other two arms. (`u64`/libgrammstein were never affected;
//! the on-disk image was always intact, so a reopen always recovered the data.)
//!
//! Scratch is REAL DISK (`target/test-tmp`), never tmpfs `/tmp`.
#![cfg(feature = "persistent-artrie")]

use std::sync::Arc;

use libdictenstein::artrie_trait::{ARTrie, EvictableARTrie};
use libdictenstein::persistent_artrie::char::{PersistentARTrieChar, SharedCharARTrie};
use libdictenstein::persistent_artrie::core::durability::DurabilityPolicy;
use libdictenstein::persistent_artrie::core::shared_access::SharedTrieAccess;
use libdictenstein::persistent_artrie::eviction::EvictionConfig;
use libdictenstein::persistent_artrie::WalConfig;
use libdictenstein::{
    Dictionary, DictionaryNode, MappedDictionary, MappedDictionaryNode, MutableMappedDictionary,
};

fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch")
}

const KEYS: [&str; 4] = ["alpha", "alphabet", "alpine", "zenith"];
fn expected_i32() -> Vec<Option<i32>> {
    (1..=4).map(Some).collect()
}

fn build_evicted_i32(prefix: &str) -> (tempfile::TempDir, SharedCharARTrie<i32>) {
    let dir = scratch(prefix);
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
    assert_eq!(before, expected_i32(), "values present before eviction");
    let (evicted, _) = shared.force_eviction(1 << 20).expect("force");
    assert!(evicted >= 1, "expected >=1 node evicted, got {evicted}");
    shared.disable_eviction().ok();
    (dir, shared)
}

/// #46 — the value-read face: `get()` (→ `overlay_value_get`) must FAULT evicted
/// arbitrary-V nodes back in and yield every value in-process (no reopen).
#[test]
fn bug46_get_faults_evicted_arbitrary_v_value() {
    let (_dir, shared) = build_evicted_i32("bug46-i32-get");
    let after_fault: Vec<Option<i32>> = KEYS.iter().map(|t| shared.read().get(t)).collect();
    assert_eq!(
        after_fault,
        expected_i32(),
        "#46: arbitrary-V get() must fault evicted nodes and yield every value in-process"
    );
}

/// #46 — the node-walk face: the `DictionaryNode` walk (`root()`/`transition`/`value()`,
/// the value-aware transducer API) must ALSO fault evicted arbitrary-V nodes. Exercised
/// on a FRESH evicted trie (no prior `get` to fault things back first).
#[test]
fn bug46_node_walk_faults_evicted_arbitrary_v_value() {
    let (_dir, shared) = build_evicted_i32("bug46-i32-walk");
    // PRODUCTION transducer path: the `Dictionary` trait root on the Arc'd trie carries
    // the SAFE overlay faulter. (The inherent `PersistentARTrieChar::root()` is non-faulting
    // by design — it has only `&self`, no `Arc` to keep the trie + buffers alive across the
    // lazy fault loads; the canonical `DictionaryNode` walk goes through `Dictionary::root`.)
    for (i, t) in KEYS.iter().enumerate() {
        let mut node = Dictionary::root(&shared);
        let mut reached = true;
        for c in t.chars() {
            match node.transition(c) {
                Some(next) => node = next,
                None => {
                    reached = false;
                    break;
                }
            }
        }
        assert!(
            reached && node.is_final() && node.value() == Some((i + 1) as i32),
            "#46: node-walk must fault the evicted arbitrary-V node for {t:?} \
             (reached={reached} value={:?})",
            if reached { node.value() } else { None }
        );
    }
}

/// #46 — never permanent loss: a reopen always recovers every arbitrary-V value (the
/// on-disk checkpoint image is intact; only the in-process fault path was at fault).
#[test]
fn bug46_reopen_recovers_arbitrary_v_value() {
    let dir = scratch("bug46-i32-reopen");
    let path = dir.path().join("b46.artc");
    {
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
        assert!(shared.force_eviction(1 << 20).expect("force").0 >= 1);
        shared.disable_eviction().ok();
    }
    let reopened: SharedCharARTrie<i32> = ARTrie::open(&path).expect("reopen");
    let after_reopen: Vec<Option<i32>> = KEYS.iter().map(|t| reopened.read().get(t)).collect();
    assert_eq!(
        after_reopen,
        expected_i32(),
        "reopen must recover every value"
    );
}

/// BASELINE: the same eviction + in-process fault-in path on a u64 counter trie preserves
/// every value — the counter read arm always faulted (this is the parity the #46 fix
/// brings to the arbitrary-V arm).
#[test]
fn bug46_baseline_u64_faultin_preserves_values() {
    let dir = scratch("bug46-u64");
    let path = dir.path().join("b46.artc");
    let trie = PersistentARTrieChar::<u64>::create_with_config(&path, WalConfig::no_archive())
        .expect("create");
    trie.set_durability_policy(DurabilityPolicy::Immediate);
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
        "u64 baseline: fault-in preserves all values"
    );
}
