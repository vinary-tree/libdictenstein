//! Deep-key stack-safety and model-correspondence gates for byte `PersistentARTrie`.
//!
//! The byte overlay is deliberately uncompressed along a unary key: a key with
//! $`n`$ bytes may therefore have $`n`$ node transitions. Production point reads
//! must keep only the current immutable-node `Arc`; immutable writes must retain
//! exactly the traversed parent/unit spine and rebuild it bottom-up. Neither may
//! use one Rust call frame per byte.
//!
//! The deterministic witness exercises the production, WAL-backed value-upsert,
//! lookup, membership-insert, and remove routes on a 100,000-byte collision under
//! a 128-KiB worker stack. The property gate compares arbitrary byte-key upserts,
//! removes, membership, and values with a `BTreeMap` oracle. Scratch state lives on
//! real disk below `target/test-tmp`, never on tmpfs-backed `/tmp` or `/run`.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::core::durability::DurabilityPolicy;
use libdictenstein::persistent_artrie::PersistentARTrie;
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::TempDir;

const DEEP_KEY_LEN: usize = 100_000;
const CONSTRAINED_STACK_BYTES: usize = 128 * 1024;

fn scratch_dir(prefix: &str) -> TempDir {
    std::fs::create_dir_all("target/test-tmp").expect("create disk-backed test root");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("create disk-backed scratch directory")
}

fn durable_trie(prefix: &str) -> (TempDir, PersistentARTrie<u64>) {
    let dir = scratch_dir(prefix);
    let trie = PersistentARTrie::<u64>::create(dir.path().join("deep-key.part"))
        .expect("create persistent byte trie");
    trie.set_durability_policy(DurabilityPolicy::Immediate);
    (dir, trie)
}

#[test]
fn hundred_thousand_byte_value_collision_insert_lookup_and_remove_are_stack_safe() {
    let (_dir, trie) = durable_trie("byte-deep-stack");
    let trie = Arc::new(trie);
    let worker_trie = Arc::clone(&trie);

    std::thread::Builder::new()
        .name("persistent-artrie-deep-key".into())
        .stack_size(CONSTRAINED_STACK_BYTES)
        .spawn(move || {
            let key = vec![b'a'; DEEP_KEY_LEN];
            let mut adjacent = key.clone();
            adjacent[DEEP_KEY_LEN - 1] = b'b';

            assert!(worker_trie
                .try_insert_with_value_bytes(&key, 7)
                .expect("first deep-key value insert"));
            assert_eq!(worker_trie.get_value_bytes(&key), Some(7));

            // Identical-key collision: the precheck and value path-copy must both
            // traverse the full 100,000-byte path without recursive call growth.
            assert!(!worker_trie
                .try_insert_with_value_bytes(&key, 11)
                .expect("deep-key value overwrite"));
            assert_eq!(worker_trie.get_value_bytes(&key), Some(11));

            // A late branch forces a 99,999-edge existing-prefix path-copy.
            assert!(worker_trie
                .try_insert_with_value_bytes(&adjacent, 13)
                .expect("deep adjacent-key insert"));
            assert_eq!(worker_trie.get_value_bytes(&adjacent), Some(13));

            assert!(worker_trie
                .remove_cas_durable(&key)
                .expect("deep-key remove"));
            assert!(!worker_trie.contains_bytes(&key));
            assert_eq!(worker_trie.get_value_bytes(&key), None);
            assert_eq!(worker_trie.get_value_bytes(&adjacent), Some(13));

            // Re-finalizing the retained non-final leaf exercises the iterative
            // durable membership builder over the complete existing prefix.
            assert!(worker_trie
                .insert_cas_durable(&key)
                .expect("deep-key membership insert"));
            assert!(worker_trie.contains_bytes(&key));
            assert!(!worker_trie
                .insert_cas_durable(&key)
                .expect("identical membership insert is idempotent"));
        })
        .expect("spawn constrained-stack worker")
        .join()
        .expect("constrained-stack deep-key worker must not overflow");

    assert!(trie.contains_bytes(&vec![b'a'; DEEP_KEY_LEN]));
}

#[derive(Clone, Debug)]
enum Op {
    Upsert(Vec<u8>, u64),
    Remove(Vec<u8>),
    Observe(Vec<u8>),
}

fn key_strategy() -> impl Strategy<Value = Vec<u8>> {
    // Tiny alphabet + short keys force duplicate keys, proper prefixes, and long
    // shared spines far more often than unconstrained random bytes.
    prop::collection::vec(0u8..4, 1..16)
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (key_strategy(), any::<u64>()).prop_map(|(key, value)| Op::Upsert(key, value)),
        key_strategy().prop_map(Op::Remove),
        key_strategy().prop_map(Op::Observe),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// Every iterative byte-key mutation/read outcome agrees with the finite-map
    /// specification, including duplicate keys and shared prefixes.
    #[test]
    fn arbitrary_byte_mutations_match_btree_map(
        ops in prop::collection::vec(op_strategy(), 1..96)
    ) {
        let (_dir, trie) = durable_trie("byte-iterative-differential");
        let mut oracle = BTreeMap::<Vec<u8>, u64>::new();

        for op in ops {
            match op {
                Op::Upsert(key, value) => {
                    let expected_new = !oracle.contains_key(&key);
                    let actual_new = trie
                        .try_insert_with_value_bytes(&key, value)
                        .expect("durable value upsert");
                    prop_assert_eq!(actual_new, expected_new);
                    oracle.insert(key.clone(), value);
                    prop_assert_eq!(trie.get_value_bytes(&key), Some(value));
                    prop_assert!(trie.contains_bytes(&key));
                }
                Op::Remove(key) => {
                    let expected = oracle.remove(&key).is_some();
                    let actual = trie
                        .remove_cas_durable(&key)
                        .expect("durable membership remove");
                    prop_assert_eq!(actual, expected);
                    prop_assert_eq!(trie.contains_bytes(&key), oracle.contains_key(&key));
                    prop_assert_eq!(trie.get_value_bytes(&key), oracle.get(&key).copied());
                }
                Op::Observe(key) => {
                    prop_assert_eq!(trie.contains_bytes(&key), oracle.contains_key(&key));
                    prop_assert_eq!(trie.get_value_bytes(&key), oracle.get(&key).copied());
                }
            }
        }

        for (key, value) in oracle {
            prop_assert!(trie.contains_bytes(&key));
            prop_assert_eq!(trie.get_value_bytes(&key), Some(value));
        }
    }
}
