//! Integration tests for the libgrammstein support surface (task #45), exercised
//! through the PUBLIC API an external embedder actually uses:
//!
//! 1. `commit_document(&self)` — an `Arc`'d trie commits chunked document transactions
//!    without `&mut`, including from another thread. This is the shape a lock-free
//!    embedder needs when it also arms `enable_eviction` (which requires a bare
//!    `Arc<PersistentARTrie>`, not an `Arc<RwLock<…>>` that can hand out `&mut`).
//! 2. `eviction_stats().resident_bytes` — a public resident-overlay-heap gauge (the
//!    estimate was previously only reachable via `pub(crate)` accessors). It is folded
//!    into the snapshot the existing `eviction_stats()` trait method returns, so byte,
//!    char, and vocab all carry it with no per-variant code.
//! 3. `eviction_stats().nodes_evicted` fed by the synchronous checkpoint-tail
//!    resident-budget eviction. Previously only the async memory-pressure loop recorded
//!    it; the `checkpoint()` resident-budget path under-reported (always 0), so an
//!    embedder could not confirm the budget was reclaiming.
//!
//! Scratch is REAL DISK (`target/test-tmp`), never `/tmp` (tmpfs on this host) — the
//! eviction faults from the disk-backed image, which tmpfs would not exercise.

#![cfg(feature = "persistent-artrie")]

use std::sync::Arc;

use libdictenstein::artrie_trait::EvictableARTrie;
use libdictenstein::persistent_artrie::eviction::EvictionConfig;
use libdictenstein::persistent_artrie::{PersistentARTrie, WalConfig};
use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
use libdictenstein::persistent_artrie_core::durability::DurabilityPolicy;
use libdictenstein::MappedDictionary;

/// A scratch directory on real disk (`target/test-tmp`), never tmpfs `/tmp`. Each call
/// returns a fresh unique dir, so parallel tests never collide on the WAL sidecar.
fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

/// (1) byte: an `Arc<PersistentARTrie>` commits a chunked document transaction with no
/// `&mut` — including from a spawned thread (compiles only because `commit_document`
/// takes `&self`; runs only because the trie is `Send + Sync`).
#[test]
fn arc_commit_document_byte_needs_no_mut() {
    let dir = scratch("libg-arc-commit-byte");
    let path = dir.path().join("c.artb");
    let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
    trie.set_durability_policy(DurabilityPolicy::Immediate);
    let trie = Arc::new(trie);

    // Commit through the Arc on ANOTHER thread — the exact libgrammstein shape.
    let worker = Arc::clone(&trie);
    let committed = std::thread::spawn(move || {
        let mut tx = worker.begin_document("doc-A").expect("begin");
        worker.tx_insert(&mut tx, "alpha", Some(1));
        worker.tx_insert(&mut tx, "beta", Some(2));
        worker.commit_document(tx).expect("commit on Arc")
    })
    .join()
    .expect("worker thread");
    assert_eq!(committed, 2, "two new terms committed via the Arc");

    // A second commit through the original Arc handle (still no `&mut` anywhere).
    let mut tx2 = trie.begin_document("doc-B").expect("begin2");
    trie.tx_insert(&mut tx2, "gamma", Some(3));
    assert_eq!(trie.commit_document(tx2).expect("commit2"), 1);

    assert_eq!(MappedDictionary::get_value(&*trie, "alpha"), Some(1));
    assert_eq!(MappedDictionary::get_value(&*trie, "beta"), Some(2));
    assert_eq!(MappedDictionary::get_value(&*trie, "gamma"), Some(3));
}

/// (1) char twin — same `&self`-on-`Arc` capability for the UTF-8 trie.
#[test]
fn arc_commit_document_char_needs_no_mut() {
    let dir = scratch("libg-arc-commit-char");
    let path = dir.path().join("c.artc");
    let mut trie = PersistentARTrieChar::<u64>::create_with_config(&path, WalConfig::no_archive())
        .expect("create");
    trie.set_durability_policy(DurabilityPolicy::Immediate);
    let trie = Arc::new(trie);

    let worker = Arc::clone(&trie);
    let committed = std::thread::spawn(move || {
        let mut tx = worker.begin_document("doc-A").expect("begin");
        worker.tx_insert(&mut tx, "αlpha", Some(1));
        worker.tx_insert(&mut tx, "βeta", Some(2));
        worker.commit_document(tx).expect("commit on Arc")
    })
    .join()
    .expect("worker thread");
    assert_eq!(committed, 2);

    assert_eq!(MappedDictionary::get_value(&*trie, "αlpha"), Some(1));
    assert_eq!(MappedDictionary::get_value(&*trie, "βeta"), Some(2));
}

/// (2) + (3): via the PUBLIC API only (`enable_eviction` + `checkpoint()` +
/// `eviction_stats()`), the resident-budget checkpoint tail feeds `nodes_evicted`, and
/// `resident_bytes` reports the live overlay heap (bounded by the budget).
#[test]
fn public_eviction_stats_resident_bytes_and_checkpoint_tail_nodes_evicted() {
    /// Build an eviction-enabled char counter trie, insert N keys, checkpoint once, and
    /// return `(resident_bytes, nodes_evicted)` from the public stats snapshot.
    fn run(budget: Option<usize>) -> (u64, u64) {
        let dir = scratch("libg-evict-stats");
        let path = dir.path().join("e.artc");
        let mut trie =
            PersistentARTrieChar::<u64>::create_with_config(&path, WalConfig::no_archive())
                .expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        let trie = Arc::new(trie);
        let config = EvictionConfig {
            resident_budget_bytes: budget,
            ..EvictionConfig::without_memory_monitor()
        };
        trie.enable_eviction(config).expect("enable_eviction");

        for i in 0..2_000u32 {
            let term = format!("ngram-{i:06}");
            trie.try_increment_cas_durable(&term, 1).expect("increment");
        }
        // The PRODUCTION checkpoint route-splits to the resident-budget tail.
        trie.checkpoint().expect("checkpoint");
        let stats = trie.eviction_stats();
        trie.disable_eviction().ok();
        (stats.resident_bytes, stats.nodes_evicted)
    }

    // A small budget: the checkpoint tail evicts the cold overlay nodes down to it.
    let (budg_resident, budg_evicted) = run(Some(4_000));
    // No budget (the default): the tail evicts nothing; the heap is unbounded.
    let (ctrl_resident, ctrl_evicted) = run(None);

    assert!(
        budg_evicted > 0,
        "the checkpoint-tail resident-budget eviction must feed nodes_evicted (the #45 fix); got {budg_evicted}"
    );
    assert_eq!(
        ctrl_evicted, 0,
        "no resident budget ⇒ no checkpoint-tail eviction; got {ctrl_evicted}"
    );
    assert!(
        ctrl_resident > 0,
        "resident_bytes must report the live overlay heap; got 0 with 2000 resident nodes"
    );
    assert!(
        budg_resident <= ctrl_resident,
        "the resident budget must not grow the resident heap ({budg_resident} <= {ctrl_resident})"
    );
}
