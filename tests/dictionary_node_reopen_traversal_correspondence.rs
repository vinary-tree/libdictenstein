//! Faulting-traversal correspondence checks for the persistent char trie.
//!
//! These tests exercise the Rust side of `PublicDictionaryNodeTraversal.tla` and
//! `DictionaryNodeReopenTraversalSpec.v`: after a trie is checkpointed and reopened
//! from disk, the root is resident but its children are *swizzled* (on-disk). The
//! `DictionaryNode` graph walk that external transducers (e.g. liblevenshtein's
//! `Transducer`) drive — `root()` → `transition(char)*` / `edges()`, checking
//! `is_final()` / `value()` — MUST fault those children in, so the walk observes
//! exactly the same `(term, value)` snapshot as the known-correct faulting
//! `iter_with_values()` path and the original inserted data.
//!
//! Before the swizzle-aware fix, `transition`/`edges` used the non-faulting
//! `get_child`/`iter_children`, which drop swizzled children via `as_ptr`, so the
//! post-reopen walk saw an empty subtree and every fuzzy query returned zero hits.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie_char::{PersistentARTrieChar, PersistentARTrieCharNode};
use libdictenstein::{DictionaryNode, MappedDictionaryNode};
use proptest::prelude::*;
use std::collections::BTreeMap;
use tempfile::tempdir;

/// Depth-first walk of the `DictionaryNode` graph from `node`, collecting
/// `(term, value)` for every final node reached. Uses ONLY the trait surface a
/// transducer drives (`is_final`/`value`/`edges`), so it fails exactly when the
/// faulting walk fails to descend into swizzled children.
fn collect_walk(
    node: &PersistentARTrieCharNode<i32>,
    prefix: &mut String,
    out: &mut BTreeMap<String, i32>,
) {
    if node.is_final() {
        if let Some(value) = node.value() {
            out.insert(prefix.clone(), value);
        }
    }
    for (ch, child) in node.edges() {
        prefix.push(ch);
        collect_walk(&child, prefix, out);
        prefix.pop();
    }
}

fn walk_map(trie: &PersistentARTrieChar<i32>) -> BTreeMap<String, i32> {
    let mut out = BTreeMap::new();
    let mut prefix = String::new();
    collect_walk(&trie.root(), &mut prefix, &mut out);
    out
}

/// Deterministic end-to-end: UTF-8 (multi-byte) terms + values survive a
/// checkpoint/drop/reopen and are reachable via the faulting `DictionaryNode` walk.
#[test]
fn walk_after_reopen_matches_inserted_utf8_values() {
    let fixture: BTreeMap<String, i32> = BTreeMap::from([
        ("receive".to_string(), 1),
        ("recipe".to_string(), 2),
        ("recital".to_string(), 3),
        ("café".to_string(), 4),
        ("caffeine".to_string(), 5),
        ("日本語".to_string(), 6),
        ("日本".to_string(), 7),
        ("emoji😀".to_string(), 8),
    ]);

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("reopen_walk.artc");

    // Build + checkpoint + DROP so only the on-disk image remains.
    {
        let mut trie = PersistentARTrieChar::<i32>::create(&path).expect("create");
        // F2-migrate: Bucket B — these tests walk the OWNED `DictionaryNode` graph
        // (`walk_map` over `self.root`), faulting swizzled owned children. Under the
        // lock-free overlay the owned tree is cleared on reopen (empty walk), so pin the
        // source to the Owned regime so the on-disk owned image drives the walk. No-op
        // feature-off (`i32` is ineligible and stays owned anyway).
        trie.kill_switch_to_owned();
        for (term, value) in &fixture {
            trie.insert_with_value(term, *value).expect("insert");
        }
        trie.checkpoint().expect("checkpoint");
    }

    // Reopen: root resident, children swizzled (on-disk).
    let trie = PersistentARTrieChar::<i32>::open(&path).expect("open");
    assert_eq!(trie.len(), fixture.len(), "len mismatch after reopen");

    // edges() must enumerate the swizzled children (before the fix this was 0).
    assert!(
        trie.root().edges().count() > 0,
        "root edges empty after reopen — swizzle-fault regression"
    );

    // The full faulting walk must reproduce the inserted snapshot exactly, and
    // must agree with the known-correct `iter_with_values()` oracle.
    assert_eq!(
        walk_map(&trie),
        fixture,
        "walk != inserted snapshot after reopen"
    );
    let oracle: BTreeMap<String, i32> = trie.iter_with_values().collect();
    assert_eq!(oracle, fixture, "iter_with_values != inserted snapshot");

    // Descend the full path of a multi-byte term; every step faults the next
    // on-disk child in, ending on a final node carrying its value.
    let mut node = trie.root();
    for ch in "café".chars() {
        node = node
            .transition(ch)
            .unwrap_or_else(|| panic!("transition '{ch}' lost after reopen"));
    }
    assert!(node.is_final(), "terminal node not final after reopen");
    assert_eq!(node.value(), Some(4), "value lost after reopen");

    // An absent first character still yields no transition (no fabricated edge).
    assert!(
        trie.root().transition('z').is_none(),
        "spurious transition for absent edge"
    );
}

/// The resident (never-checkpointed) walk and the reopened (swizzled) walk must
/// return the same snapshot — the in-memory `as_ptr` fast path is observationally
/// equal to the on-disk faulting path.
#[test]
fn resident_walk_equals_reopened_walk() {
    let fixture: BTreeMap<String, i32> = BTreeMap::from([
        ("alpha".into(), 1),
        ("alpine".into(), 2),
        ("beta".into(), 3),
    ]);

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("resident_vs_reopen.artc");

    let mut resident = PersistentARTrieChar::<i32>::create(&path).expect("create");
    // F2-migrate: Bucket B — owned `DictionaryNode` walk (resident + reopened). Pin the
    // Owned regime so both the resident walk and the reopened (swizzled) walk read the
    // owned tree. No-op feature-off.
    resident.kill_switch_to_owned();
    for (term, value) in &fixture {
        resident.insert_with_value(term, *value).expect("insert");
    }
    let resident_map = walk_map(&resident); // children resident: as_ptr fast path
    resident.checkpoint().expect("checkpoint");
    drop(resident);

    let reopened = PersistentARTrieChar::<i32>::open(&path).expect("open");
    let reopened_map = walk_map(&reopened); // children swizzled: faulting path

    assert_eq!(resident_map, fixture);
    assert_eq!(reopened_map, fixture);
    assert_eq!(resident_map, reopened_map, "resident walk != reopened walk");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// For any set of UTF-8 terms+values, after checkpoint+drop+reopen the full
    /// faulting `DictionaryNode` walk enumerates exactly the inserted snapshot —
    /// equal to both the source map and the `iter_with_values()` oracle.
    #[test]
    fn walk_after_reopen_equals_snapshot(
        pairs in prop::collection::vec(("[a-cé日X]{1,6}", any::<i32>()), 1..40)
    ) {
        // Dedup by key (last value wins) so there is a single ground-truth map and
        // the trie's duplicate-key semantics never confound the comparison.
        let mut expected: BTreeMap<String, i32> = BTreeMap::new();
        for (k, v) in pairs {
            expected.insert(k, v);
        }

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("prop_walk.artc");
        {
            let mut trie = PersistentARTrieChar::<i32>::create(&path).expect("create");
            // F2-migrate: Bucket B — owned `DictionaryNode` walk after reopen. Pin the
            // Owned regime so the on-disk owned image drives the faulting walk. No-op
            // feature-off.
            trie.kill_switch_to_owned();
            for (k, v) in &expected {
                trie.insert_with_value(k, *v).expect("insert");
            }
            trie.checkpoint().expect("checkpoint");
        }

        let trie = PersistentARTrieChar::<i32>::open(&path).expect("open");
        prop_assert_eq!(trie.len(), expected.len());

        // (a) faulting walk == inserted snapshot
        prop_assert_eq!(walk_map(&trie), expected.clone());
        // (b) faulting walk == iter_with_values() oracle
        let oracle: BTreeMap<String, i32> = trie.iter_with_values().collect();
        prop_assert_eq!(oracle, expected);
    }
}
