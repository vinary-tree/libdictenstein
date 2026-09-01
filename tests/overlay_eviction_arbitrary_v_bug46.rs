//! REGRESSION test for BUG #46 (FIXED): arbitrary-V (non-`u64`) char overlay reads must
//! FAULT evicted nodes back in. Surfaced by the L0.1 owned-eviction-deletion test
//! migration; fixed in `overlay_write_mode.rs::overlay_value_get`.
//!
//! ## The bug (now fixed)
//! After an in-process overlay eviction (`force_eviction` drives a generation-bound
//! compact batch that flips resident overlay children to `OnDisk`), reading an evicted node of an ARBITRARY value
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
use libdictenstein::persistent_artrie::{SharedARTrie, WalConfig};
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
    (dir, shared)
}

fn build_evicted_byte_i32(prefix: &str) -> (tempfile::TempDir, SharedARTrie<i32>) {
    let dir = scratch(prefix);
    let path = dir.path().join("b46.part");
    let shared: SharedARTrie<i32> = ARTrie::create(&path).expect("create byte trie");
    for (i, term) in KEYS.iter().enumerate() {
        assert!(shared.write().insert_with_value(term, (i + 1) as i32));
    }
    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable byte eviction");
    shared.write().checkpoint().expect("checkpoint byte trie");
    let before: Vec<Option<i32>> = KEYS
        .iter()
        .map(|term| MappedDictionary::get_value(&shared, term))
        .collect();
    assert_eq!(
        before,
        expected_i32(),
        "byte values present before eviction"
    );
    let (evicted, _) = shared.force_eviction(1 << 20).expect("force byte eviction");
    assert!(
        evicted >= 1,
        "expected >=1 byte node evicted, got {evicted}"
    );
    (dir, shared)
}

/// #46 — the value-read face: `get()` (→ `overlay_value_get`) must FAULT evicted
/// arbitrary-V nodes back in and yield every value in-process (no reopen).
#[test]
fn bug46_get_faults_evicted_arbitrary_v_value() {
    let (_dir, shared) = build_evicted_i32("bug46-i32-get");
    shared.disable_eviction().expect("disable char eviction");
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
    shared.disable_eviction().expect("disable char eviction");
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

#[test]
fn byte_disable_unbinds_root_and_preserves_evicted_arbitrary_values() {
    let (_dir, shared) = build_evicted_byte_i32("bug46-byte-disable");
    shared.disable_eviction().expect("disable byte eviction");
    let after: Vec<Option<i32>> = KEYS
        .iter()
        .map(|term| MappedDictionary::get_value(&shared, term))
        .collect();
    assert_eq!(after, expected_i32());
}

#[test]
fn char_close_unbinds_root_and_preserves_evicted_arbitrary_values() {
    let (_dir, shared) = build_evicted_i32("bug46-char-close");
    shared.read().close();
    let after: Vec<Option<i32>> = KEYS
        .iter()
        .map(|term| MappedDictionary::get_value(&shared, term))
        .collect();
    assert_eq!(after, expected_i32());
}

#[test]
fn byte_close_unbinds_root_and_preserves_evicted_arbitrary_values() {
    let (_dir, shared) = build_evicted_byte_i32("bug46-byte-close");
    shared.read().close();
    let after: Vec<Option<i32>> = KEYS
        .iter()
        .map(|term| MappedDictionary::get_value(&shared, term))
        .collect();
    assert_eq!(after, expected_i32());
}

#[test]
fn byte_durable_remove_faults_evicted_terms_and_survives_reopen() {
    let (dir, shared) = build_evicted_byte_i32("byte-remove-under-evicted-prefix");
    let path = dir.path().join("b46.part");
    shared
        .read()
        .set_durability_policy(DurabilityPolicy::Immediate);

    for term in KEYS {
        assert!(
            shared
                .read()
                .remove_cas_durable(term.as_bytes())
                .expect("durable byte removal under an evicted prefix"),
            "{term:?} must have been present"
        );
        assert_eq!(MappedDictionary::get_value(&shared, term), None);
    }
    shared.disable_eviction().expect("disable byte eviction");
    drop(shared);

    let reopened: SharedARTrie<i32> = ARTrie::open(&path).expect("reopen byte trie");
    for term in KEYS {
        assert_eq!(MappedDictionary::get_value(&reopened, term), None);
    }
}

#[test]
fn char_durable_remove_faults_evicted_terms_and_survives_reopen() {
    let (dir, shared) = build_evicted_i32("char-remove-under-evicted-prefix");
    let path = dir.path().join("b46.artc");
    shared
        .read()
        .set_durability_policy(DurabilityPolicy::Immediate);

    for term in KEYS {
        assert!(
            shared
                .read()
                .remove_cas_durable(term)
                .expect("durable character removal under an evicted prefix"),
            "{term:?} must have been present"
        );
        assert_eq!(shared.read().get(term), None);
    }
    shared.disable_eviction().expect("disable char eviction");
    drop(shared);

    let reopened: SharedCharARTrie<i32> = ARTrie::open(&path).expect("reopen character trie");
    for term in KEYS {
        assert_eq!(reopened.read().get(term), None);
    }
}

#[test]
fn byte_durable_insert_extends_an_evicted_prefix_and_survives_reopen() {
    let dir = scratch("byte-insert-under-evicted-prefix");
    let path = dir.path().join("insert-under-evicted.part");
    let shared: SharedARTrie<()> = ARTrie::create(&path).expect("create byte trie");
    shared
        .read()
        .set_durability_policy(DurabilityPolicy::Immediate);
    for prefix in [b"cold-prefix-a".as_slice(), b"cold-prefix-b".as_slice()] {
        assert!(shared
            .read()
            .insert_cas_durable(prefix)
            .expect("seed durable prefix"));
    }
    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable byte eviction");
    shared.read().checkpoint().expect("checkpoint prefixes");
    assert!(
        shared.force_eviction(1 << 20).expect("force eviction").0 >= 1,
        "at least one checkpointed prefix must be evicted"
    );

    for extension in [
        b"cold-prefix-a-extension".as_slice(),
        b"cold-prefix-b-extension".as_slice(),
    ] {
        assert!(shared
            .read()
            .insert_cas_durable(extension)
            .expect("durable insert below an evicted prefix"));
        assert!(shared.read().contains_lockfree(extension));
    }
    shared.disable_eviction().expect("disable byte eviction");
    drop(shared);

    let reopened: SharedARTrie<()> = ARTrie::open(&path).expect("reopen byte trie");
    for extension in [
        b"cold-prefix-a-extension".as_slice(),
        b"cold-prefix-b-extension".as_slice(),
    ] {
        assert!(reopened.read().contains_lockfree(extension));
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
        shared.disable_eviction().expect("disable char eviction");
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
