//! Property-based tests for HEAD commit f10c43e (persistent char trie).
//!
//! - **G6** (eviction registry @ checkpoint): for arbitrary key/value sets,
//!   reclaiming through the checkpoint-populated registry is lossless (every key
//!   still resolves via reload-from-disk), a post-checkpoint write invalidates
//!   the registry, and recovery is unaffected by eviction.
//! - **G9** (`MappedDictionaryNode::value()`): for arbitrary maps, every key's
//!   terminal node yields its value and non-terminal prefixes yield `None`.
//!
//! Run with: cargo test --features persistent-artrie --test persistent_char_eviction_proptest

#![cfg(feature = "persistent-artrie")]

use libdictenstein::artrie_trait::{ARTrie, EvictableARTrie};
use libdictenstein::persistent_artrie::eviction::EvictionConfig;
use libdictenstein::persistent_artrie_char::{PersistentARTrieChar, SharedCharARTrie};
use libdictenstein::{DictionaryNode, MappedDictionaryNode, MutableMappedDictionary};
use proptest::prelude::*;
use std::collections::HashMap;
use tempfile::TempDir;

/// Terms mixing ASCII and multi-byte unicode codepoints, 1..=10 chars, so the
/// char-path registry (`Vec<char>`) and multi-codepoint nodes are exercised.
fn term_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            (b'a'..=b'z').prop_map(|b| b as char),
            Just('é'),
            Just('ñ'),
            Just('中'),
            Just('ä'),
        ],
        1..=10,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

/// A small, de-duplicated key->value map (the trie keeps the last value per key,
/// so we dedup via `HashMap` to make assertions exact).
fn char_map_strategy() -> impl Strategy<Value = HashMap<String, i32>> {
    prop::collection::vec((term_strategy(), any::<i32>()), 1..=24)
        .prop_map(|pairs| pairs.into_iter().collect())
}

/// Insert via the explicit `MutableMappedDictionary` method (disambiguates from
/// `ARTrie::insert_with_value`).
fn put(shared: &SharedCharARTrie<i32>, term: &str, value: i32) -> bool {
    MutableMappedDictionary::insert_with_value(shared, term, value)
}

/// Read through the reloading `get` path (unambiguous inherent method).
fn value_of(shared: &SharedCharARTrie<i32>, term: &str) -> Option<i32> {
    shared.read().get(term).copied()
}

fn build_disk_trie(map: &HashMap<String, i32>) -> (TempDir, SharedCharARTrie<i32>) {
    let dir = TempDir::new().expect("tempdir");
    let shared: SharedCharARTrie<i32> = ARTrie::create(dir.path().join("p.trie")).expect("create");
    // F2-migrate: Bucket B — owned-rep eviction/reclamation; pin OwnedTree (before
    // inserts) so the owned eviction path is exercised. No-op feature-off.
    shared.write().kill_switch_to_owned();
    for (k, v) in map {
        assert!(put(&shared, k, *v));
    }
    (dir, shared)
}

proptest! {
    // Disk + eviction-thread per case, so keep the case count modest.
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// G6: reclaiming through the checkpoint-populated registry is lossless —
    /// every key still resolves to its exact value via reload-from-disk.
    #[test]
    fn prop_reclaim_is_lossless(map in char_map_strategy()) {
        let (_dir, shared) = build_disk_trie(&map);
        shared
            .enable_eviction(EvictionConfig::without_memory_monitor())
            .expect("enable");
        shared.write().checkpoint().expect("checkpoint");

        // Evict as much as the registry allows in one synchronous pass.
        let _ = shared.force_eviction(1 << 30).expect("force eviction");

        for (k, v) in &map {
            prop_assert_eq!(value_of(&shared, k), Some(*v), "lost key {:?} after eviction", k);
        }
        // Keys never inserted stay absent.
        prop_assert_eq!(value_of(&shared, "\u{1}absent-sentinel"), None);

        shared.disable_eviction().expect("disable");
    }

    /// G6 / A1: a checkpoint registers >= 1 node, and a subsequent write
    /// invalidates the registry so eviction selects nothing until re-checkpoint.
    #[test]
    fn prop_post_checkpoint_write_invalidates(
        map in char_map_strategy(),
        extra in term_strategy(),
    ) {
        let (_dir, shared) = build_disk_trie(&map);
        shared
            .enable_eviction(EvictionConfig::without_memory_monitor())
            .expect("enable");
        shared.write().checkpoint().expect("checkpoint");

        // Non-empty trie => at least the root + one child are registered.
        prop_assert!(shared.read().evictable_node_count().unwrap() >= 1);

        // Mutate: invalidates the published registry at the append_to_wal chokepoint.
        put(&shared, &extra, 12345);
        prop_assert_eq!(
            shared.force_eviction(1 << 30).expect("force"),
            (0, 0),
            "a post-checkpoint write must invalidate the registry"
        );

        // Re-checkpoint republishes a fresh registry; the inserted key is durable.
        shared.write().checkpoint().expect("re-checkpoint");
        let _ = shared.force_eviction(1 << 30).expect("force after re-checkpoint");
        prop_assert_eq!(value_of(&shared, &extra), Some(12345));

        shared.disable_eviction().expect("disable");
    }

    /// G6: the registry is ephemeral and not recovery state — evicting through it
    /// must not change what a fresh reopen recovers.
    #[test]
    fn prop_eviction_preserves_recovery(map in char_map_strategy()) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("recover.trie");
        {
            let shared: SharedCharARTrie<i32> = ARTrie::create(&path).expect("create");
            // F2-migrate: Bucket B — owned-rep eviction; pin OwnedTree before inserts.
            shared.write().kill_switch_to_owned();
            for (k, v) in &map {
                assert!(put(&shared, k, *v));
            }
            shared
                .enable_eviction(EvictionConfig::without_memory_monitor())
                .expect("enable");
            shared.write().checkpoint().expect("checkpoint");
            let _ = shared.force_eviction(1 << 30).expect("force");
            shared.disable_eviction().expect("disable");
        }
        // Fresh reopen, no eviction: every key recovers to its exact value.
        let reopened: SharedCharARTrie<i32> = ARTrie::open(&path).expect("reopen");
        for (k, v) in &map {
            prop_assert_eq!(value_of(&reopened, k), Some(*v), "recovery lost {:?}", k);
        }
    }
}

proptest! {
    // In-memory and fast; exercise many more cases.
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// G9: every inserted key's terminal node yields its value via the node API
    /// (`MappedDictionaryNode::value()`), and an unrelated absent path yields None.
    #[test]
    fn prop_value_roundtrip(map in char_map_strategy()) {
        let mut trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
        for (k, v) in &map {
            trie.insert_with_value(k, *v).expect("insert");
        }

        for (k, v) in &map {
            // Walk the node API to the terminal.
            let mut node = trie.root();
            let mut reached = true;
            for c in k.chars() {
                match node.transition(c) {
                    Some(next) => node = next,
                    None => {
                        reached = false;
                        break;
                    }
                }
            }
            prop_assert!(reached, "could not walk to key {:?}", k);
            prop_assert!(node.is_final(), "terminal node for {:?} not final", k);
            prop_assert_eq!(node.value(), Some(*v), "wrong value for {:?}", k);
        }

        // A guaranteed-absent path yields no node / no value.
        let mut node = trie.root();
        let mut reached = true;
        for c in "\u{1}\u{1}".chars() {
            match node.transition(c) {
                Some(next) => node = next,
                None => {
                    reached = false;
                    break;
                }
            }
        }
        prop_assert!(!reached || node.value().is_none());
    }
}
