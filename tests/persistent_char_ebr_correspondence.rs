//! Threaded eviction-vs-walk EBR correspondence (the Rust side of
//! `EvictionWalkEBR.tla` / `PersistentCharEpochReclamationSpec.v`).
//!
//! Exercises the REAL code: many concurrent `DictionaryNode` walks over a reopened
//! disk-backed char trie, running alongside the background eviction thread AND an
//! explicit `force_eviction` loop. The walk pins the (unified) epoch, so eviction
//! reclaims a node only after a quiescence drain proves no live walk holds it —
//! making the lock-free walk safe against concurrent eviction without blocking it.
//!
//! Two properties are checked simultaneously:
//!   * MEMORY SAFETY — no use-after-free / data race. This file is meant to be run
//!     under ThreadSanitizer / AddressSanitizer (`scripts/run-sanitizers.sh`);
//!     under the plain test profile it still validates the logical property below.
//!   * COMPLETENESS — every walk reaches the FULL snapshot. Because the walk faults
//!     swizzled (evicted/on-disk) children in and the pin defers reclamation, a
//!     walk concurrent with eviction observes exactly the inserted key/value set,
//!     never a truncated subtree.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::artrie_trait::EvictableARTrie;
use libdictenstein::persistent_artrie::eviction::EvictionConfig;
use libdictenstein::persistent_artrie_char::{
    PersistentARTrieChar, PersistentARTrieCharNode, SharedCharARTrie,
};
use libdictenstein::{Dictionary, DictionaryNode, MappedDictionaryNode};
use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use tempfile::tempdir;

/// Depth-first walk of the `DictionaryNode` graph, collecting `(term, value)` for
/// every final node — using only the trait surface a transducer drives.
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

#[test]
fn walk_concurrent_with_eviction_is_safe_and_complete() {
    // A reasonably deep, prefix-sharing, UTF-8 fixture so eviction has many
    // depth >= min_depth nodes to reclaim and the walk has structure to descend.
    let suffixes = ["a", "bb", "ccc", "café", "日本", "x😀"];
    let fixture: BTreeMap<String, i32> = (0..48)
        .map(|i| {
            (
                format!("term{:02}{}", i, suffixes[i % suffixes.len()]),
                i as i32,
            )
        })
        .collect();

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("ebr_concurrent.artc");

    // Build + checkpoint + DROP so the on-disk image is the source of truth.
    // F2-migrate: Bucket B — the reader threads walk the OWNED `DictionaryNode` graph
    // (`collect_walk(&shared.root())`), which faults swizzled OWNED children back in.
    // Under the lock-free overlay the owned tree is cleared on reopen (the walk would
    // see an empty subtree), so pin the source to the Owned regime (stamps an Owned WAL
    // ⇒ the reopen stays owned and the on-disk owned image is the walk's source of
    // truth). Feature-off (`i32` ineligible) this is a no-op.
    {
        let mut trie = PersistentARTrieChar::<i32>::create(&path).expect("create");
        trie.kill_switch_to_owned();
        for (term, value) in &fixture {
            trie.insert_with_value(term, *value).expect("insert");
        }
        trie.checkpoint().expect("checkpoint");
    }

    // Reopen (children swizzled), publish an eviction registry, enable eviction.
    let shared: SharedCharARTrie<i32> = Arc::new(RwLock::new(
        PersistentARTrieChar::<i32>::open(&path).expect("open"),
    ));
    shared.write().checkpoint().expect("post-reopen checkpoint");
    shared
        .enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable eviction");

    let stop = Arc::new(AtomicBool::new(false));

    // Reader threads: each performs a bounded number of full walks, every one of
    // which must reach the complete snapshot (no mutation occurs concurrently, so
    // the snapshot is constant; eviction must not truncate or dangle it).
    const READERS: usize = 4;
    const WALKS_PER_READER: usize = 80;
    let mut readers = Vec::with_capacity(READERS);
    for _ in 0..READERS {
        let shared = shared.clone();
        let fixture = fixture.clone();
        readers.push(thread::spawn(move || {
            for _ in 0..WALKS_PER_READER {
                let mut got = BTreeMap::new();
                let mut prefix = String::new();
                collect_walk(&shared.root(), &mut prefix, &mut got);
                assert_eq!(
                    got, fixture,
                    "a walk did not reach the full snapshot under concurrent eviction"
                );
            }
        }));
    }

    // Evictor thread: drive `force_eviction` continuously (the background eviction
    // thread started by `enable_eviction` also runs), so reclamation races the
    // walks for the whole test.
    let evictor = {
        let shared = shared.clone();
        let stop = stop.clone();
        thread::spawn(move || {
            let mut evictions = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let (evicted, _) = shared.force_eviction(1 << 20).unwrap_or((0, 0));
                evictions += evicted as u64;
                thread::yield_now();
            }
            evictions
        })
    };

    for reader in readers {
        reader.join().expect("reader thread");
    }
    stop.store(true, Ordering::Relaxed);
    evictor.join().expect("evictor thread");

    // Final walk after the storm: still the full snapshot.
    let mut got = BTreeMap::new();
    collect_walk(&shared.root(), &mut String::new(), &mut got);
    assert_eq!(got, fixture, "post-storm walk lost data");

    shared.disable_eviction().expect("disable eviction");
}
