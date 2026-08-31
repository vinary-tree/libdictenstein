//! **Byte overlay-eviction correspondence tests (Phase 5/6)** — the BYTE twins of
//! char's `overlay_eviction_driver_correspondence` OE3 / OE5 / OE8.
//!
//! These live in-crate (not in `tests/`) because they drive the lifted `pub(crate)`
//! compact generation-bound eviction, exact fault-in via `get_lockfree`, and the
//! `bench_*` eviction surface, and
//! they inspect overlay-internal state (an OnDisk overlay child after eviction; the
//! stamp guard). They are the byte witness for the shared
//! `formal-verification/tla+/OverlayEvictionStale.tla` (the 1c lost-update guard) and
//! the byte half of the read/write fault-in design (§3/§4).
//!
//! - **OE3-twin `byte_evict_then_reload_returns_exact_values`** (counter `V=u64`):
//!   checkpoint-with-eviction → evict cold → reopen → cold VALUES byte-identical.
//! - **OE5-twin `byte_overwrite_since_checkpoint_is_not_evicted_to_stale_image`**: the
//!   1c lost-update guard (the M-2a `serial_disk_ptr` stamp) — overwriting a registered
//!   cold node then evicting it to its STALE disk_ptr returns `NotEvictable`, and the
//!   NEW value survives; the positive control (un-overwritten) still evicts + faults
//!   back exactly.
//! - **OE8-twin `byte_evict_faultin_evict_thrash_terminates`** (liveness): a tight
//!   evict→read-faults-in→evict loop terminates (regression-guards the counter
//!   infinite-spin the write-path fault-in fixes) and every read stays exact.
//!
//! Scratch is real disk (`target/test-tmp`), never `/tmp` (tmpfs on this host).

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::persistent_artrie::core::durability::DurabilityPolicy;
use crate::persistent_artrie::core::eviction::CompactEvictionBatch;
use crate::persistent_artrie::eviction::EvictionConfig;
use crate::persistent_artrie::node_impl::PersistentARTrieNode;
use crate::persistent_artrie::overlay_fault::evict_overlay_compact_batch;
use crate::persistent_artrie::PersistentARTrie;
use crate::{Dictionary, DictionaryNode, MappedDictionary};

/// A scratch directory on real disk (`target/test-tmp`), never tmpfs `/tmp`.
fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

fn byte_root_resident_totals<V, S>(trie: &PersistentARTrie<V, S>) -> (usize, usize)
where
    V: crate::value::DictionaryValue,
    S: crate::persistent_artrie::block_storage::BlockStorage,
{
    let coordinator = trie
        .eviction_coordinator
        .lock()
        .expect("eviction_coordinator mutex poisoned")
        .as_ref()
        .cloned()
        .expect("eviction enabled");
    let root = trie.lockfree_root.as_ref().expect("overlay root");
    coordinator.root_resident_totals(root).unwrap_or_default()
}

/// COLD-prefix predicate: a registry byte-path is cold iff it starts with `b'c'` (the
/// `cold-*` term family). Only COLD subtrees are fed to the evictor.
fn is_cold(path: &[u8]) -> bool {
    path.first() == Some(&b'c')
}

fn retain_byte_candidates<F>(batch: &mut CompactEvictionBatch<u8>, predicate: F)
where
    F: Fn(&[u8]) -> bool,
{
    let mut retained = Vec::new();
    if retained.try_reserve_exact(batch.candidates.len()).is_err() {
        batch.candidates.clear();
        return;
    }
    for candidate in &batch.candidates {
        retained.push(
            batch
                .materialize_path(candidate.path_id)
                .is_some_and(|path| predicate(&path)),
        );
    }
    let mut retained_index = 0usize;
    batch.candidates.retain(|_| {
        let keep = retained[retained_index];
        retained_index += 1;
        keep
    });
}

/// Drive ONE round of cold-only overlay eviction via the coordinator's byte selection
/// (coldest-first, registry-gated) filtered to COLD paths, reclaimed via the lifted
/// compact batch driver. Returns the number of overlay nodes evicted.
fn evict_cold_overlay<V, S>(trie: &PersistentARTrie<V, S>, budget_bytes: usize) -> usize
where
    V: crate::value::DictionaryValue,
    S: crate::persistent_artrie::block_storage::BlockStorage,
{
    let coordinator = match trie
        .eviction_coordinator
        .lock()
        .expect("eviction_coordinator mutex poisoned")
        .as_ref()
    {
        Some(c) => Arc::clone(c),
        None => return 0,
    };
    let Some(root) = trie.lockfree_root.as_ref() else {
        return 0;
    };
    coordinator
        .force_eviction_compact_bytes_root(root, budget_bytes, |mut batch| {
            retain_byte_candidates(&mut batch, is_cold);
            evict_overlay_compact_batch(trie, batch, 4)
        })
        .0
}

/// Traverse a detached byte `DictionaryNode` snapshot without using the machine
/// stack for trie depth. Each frame owns its node and prefix; production traversal
/// cursors use denser state, while this explicit test worklist keeps the witness
/// simple and stack-safe at arbitrary depth.
fn walk_byte_terms<V>(root: &PersistentARTrieNode<V>) -> BTreeSet<Vec<u8>>
where
    V: crate::value::DictionaryValue,
{
    let mut terms = BTreeSet::new();
    let mut work = vec![(root.clone(), Vec::<u8>::new())];

    while let Some((node, prefix)) = work.pop() {
        if node.is_final() {
            terms.insert(prefix.clone());
        }
        for (unit, child) in node.edges() {
            let mut child_prefix = prefix.clone();
            child_prefix.push(unit);
            work.push((child, child_prefix));
        }
    }

    terms
}

// ───────────────────────── OE3-twin (evict → reload exact values) ─────────────────────────

/// OE3-twin (counter `V=u64`): each cold term carries a distinct value; after a
/// checkpoint-with-eviction + evict-cold, a reopen must read back the EXACT durable
/// values (membership AND value), the byte witness that byte serialize-time registration
/// + the durable image round-trips losslessly through eviction.
#[test]
fn byte_evict_then_reload_returns_exact_values() {
    let dir = scratch("byte-oe3-evict-reload");
    let path = dir.path().join("oe3.part");

    let cold: Vec<(String, u64)> = (0..40)
        .map(|i| (format!("cold-{i:04}"), 1000 + i as u64))
        .collect();
    let live: Vec<(String, u64)> = (0..20)
        .map(|i| (format!("warm-{i:04}"), 5000 + i as u64))
        .collect();

    {
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        trie.install_overlay();
        trie.bench_enable_eviction(EvictionConfig::without_memory_monitor())
            .expect("bench_enable_eviction");

        // Order-A durable increments establish each term's value in the overlay.
        for (t, v) in cold.iter().chain(live.iter()) {
            trie.try_increment_cas_durable(t.as_bytes(), *v)
                .expect("durable increment");
        }
        // Checkpoint-with-eviction REGISTERS + STAMPS every node (the byte registration).
        trie.bench_immutable_checkpoint_with_eviction()
            .expect("checkpoint with eviction");
        assert!(
            trie.evictable_node_count().unwrap_or(0) > 0,
            "byte registry must be published (evictable_node_count > 0) — registration gap"
        );

        let trie = Arc::new(trie);
        let mut evicted = 0usize;
        for _ in 0..16 {
            evicted += evict_cold_overlay(&*trie, 1 << 20);
        }
        assert!(
            evicted > 0,
            "OE3-twin: no cold byte nodes evicted (driver no-op / registration gap)"
        );
        drop(trie);
    }

    // Reopen and read back the VALUES — byte-identical to what was checkpointed/WAL'd.
    let reopened = PersistentARTrie::<u64>::open(&path).expect("reopen");
    for (t, v) in cold.iter().chain(live.iter()) {
        assert_eq!(
            MappedDictionary::get_value(&reopened, t),
            Some(*v),
            "byte term {t:?} value wrong after evict+reload (expected {v})"
        );
    }
}

// ───────────── OE5-twin (the 1c overwrite-guard witness — M-2a serial_disk_ptr) ─────────────

/// **OE5-twin — the round-3 1c lost-update guard (the M-2a `serial_disk_ptr` stamp) for
/// byte.** Deterministic witness that compact eviction refuses to evict a node
/// overwritten since the checkpoint that registered it, preventing the
/// evictor from unswizzling the NEWER in-memory value onto the OLDER on-disk image (the
/// lost update).
///
/// - **Positive control:** an UN-overwritten registered cold node still evicts
///   (`Evicted`) and faults back to its exact value → the guard does not over-reject.
/// - **The witness:** after overwriting a registered cold node (a counter increment
///   path-copies its leaf into a fresh `stamp == 0` node), evicting it to its STALE
///   registry `disk_ptr` returns `NotEvictable`, and the NEW value survives.
#[test]
fn byte_overwrite_since_checkpoint_is_not_evicted_to_stale_image() {
    let dir = scratch("byte-oe5-overwrite-guard");
    let path = dir.path().join("oe5.part");

    let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
    trie.set_durability_policy(DurabilityPolicy::Immediate);
    trie.install_overlay();
    trie.bench_enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("bench_enable_eviction");

    // Two cold counter terms; checkpoint-with-eviction STAMPS + registers every node.
    trie.try_increment_cas_durable(b"cold-stable", 10)
        .expect("inc stable");
    trie.try_increment_cas_durable(b"cold-rewritten", 20)
        .expect("inc rewritten");
    trie.bench_immutable_checkpoint_with_eviction()
        .expect("checkpoint with eviction");

    // Positive control: exact compact eviction of the unmodified registered
    // leaf succeeds and faults back to the durable value.
    let coordinator = trie
        .eviction_coordinator
        .lock()
        .expect("eviction_coordinator mutex poisoned")
        .as_ref()
        .map(Arc::clone)
        .expect("coordinator present");
    let root = trie.lockfree_root.as_ref().expect("published overlay root");
    let stable_evicted =
        coordinator.force_eviction_compact_bytes_root(root, 1 << 20, |mut batch| {
            retain_byte_candidates(&mut batch, |path| path == b"cold-stable");
            evict_overlay_compact_batch(&trie, batch, 4)
        });
    assert_eq!(
        stable_evicted.0, 1,
        "unmodified leaf must evict exactly once"
    );
    assert_eq!(trie.get_lockfree(b"cold-stable"), Some(10));

    // Capture the generation-bearing candidate for the other leaf without
    // publishing a root change. The later overwrite invalidates this batch.
    let captured = std::cell::RefCell::new(None);
    coordinator.force_eviction_compact_bytes_root(root, 1 << 20, |mut batch| {
        retain_byte_candidates(&mut batch, |path| path == b"cold-rewritten");
        *captured.borrow_mut() = Some(batch);
        (0, 0)
    });
    let stale_batch = captured
        .into_inner()
        .expect("cold-rewritten compact candidate registered");

    // OVERWRITE cold-rewritten (counter +5 ⇒ path-copy ⇒ fresh stamp-0 leaf at its path).
    trie.try_increment_cas_durable(b"cold-rewritten", 5)
        .expect("overwrite");
    assert_eq!(
        trie.get_lockfree(b"cold-rewritten"),
        Some(25),
        "overwrite stuck (20+5)"
    );

    // THE WITNESS: the stale generation cannot prepare an exact residency
    // transition and therefore publishes no root change.
    assert_eq!(
        evict_overlay_compact_batch(&trie, stale_batch, 4),
        (0, 0),
        "a node overwritten since selection must not be evicted to its stale image"
    );
    assert_eq!(
        trie.get_lockfree(b"cold-rewritten"),
        Some(25),
        "the NEW value survives (not lost to a stale-image eviction)"
    );
}

// ───────────── OE8-twin (liveness: evict→faultin→evict thrash terminates) ─────────────

/// OE8-twin: a tight evict-then-read loop must TERMINATE (within `DEFAULT_MAX_FAULTIN_RETRIES`),
/// regression-guarding the byte counter infinite-spin the write-path fault-in fixes. Each
/// iteration evicts the cold subtrees, then reads them back (faulting in via the read-path
/// fault-in), then evicts again. If `find_leaf_faulting` (or the counter read/write step)
/// ever spun, this would hang; the test asserts it completes and every read is exact.
#[test]
fn byte_evict_faultin_evict_thrash_terminates() {
    let dir = scratch("byte-oe8-thrash");
    let path = dir.path().join("oe8.part");

    let cold: Vec<(String, u64)> = (0..24)
        .map(|i| (format!("cold-{i:03}"), 700 + i as u64))
        .collect();

    let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
    trie.set_durability_policy(DurabilityPolicy::Immediate);
    trie.install_overlay();
    trie.bench_enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("bench_enable_eviction");
    for (t, v) in &cold {
        trie.try_increment_cas_durable(t.as_bytes(), *v)
            .expect("durable increment");
    }
    trie.bench_immutable_checkpoint_with_eviction()
        .expect("checkpoint with eviction");
    let trie = Arc::new(trie);

    // Thrash: evict → read-faults-in → evict → … Each read must observe the exact value
    // and the loop must terminate (no infinite spin).
    let mut total_evicted = 0usize;
    for round in 0..8 {
        let mut evicted = 0usize;
        for _ in 0..8 {
            evicted += evict_cold_overlay(&*trie, 1 << 20);
        }
        assert!(
            evicted > 0,
            "OE8-twin: round {round} did not re-evict any record faulted in by the prior round"
        );
        total_evicted += evicted;
        for (t, v) in &cold {
            assert_eq!(
                trie.get_lockfree(t.as_bytes()),
                Some(*v),
                "OE8-twin: round {round} term {t:?} wrong value after evict/faultin thrash"
            );
        }
    }
    assert!(
        total_evicted > 0,
        "OE8-twin: thrash never evicted anything (vacuous — re-faulted nodes must become \
         re-evictable for the thrash to be meaningful)"
    );
}

#[test]
fn winning_byte_faults_restore_exact_registry_residency() {
    let dir = scratch("byte-fault-residency");
    let path = dir.path().join("fault-residency.part");
    let terms: Vec<(Vec<u8>, u64)> = (0..48)
        .map(|index| {
            (
                format!("fault-residency-{index:03}").into_bytes(),
                10_000 + index,
            )
        })
        .collect();

    let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
    trie.set_durability_policy(DurabilityPolicy::Immediate);
    trie.install_overlay();
    trie.bench_enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable eviction");
    for (term, value) in &terms {
        trie.try_increment_cas_durable(term, *value)
            .expect("durable increment");
    }
    trie.bench_immutable_checkpoint_with_eviction()
        .expect("checkpoint with eviction registry");

    let coordinator = trie
        .eviction_coordinator
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map(Arc::clone)
        .expect("coordinator");
    let root = trie.lockfree_root.as_ref().expect("published overlay root");
    let (resident_before, serialized_before) = byte_root_resident_totals(&trie);
    assert!(resident_before > 0);
    assert!(serialized_before > 0);

    let evicted = coordinator.force_eviction_compact_bytes_root(root, usize::MAX, |batch| {
        evict_overlay_compact_batch(&trie, batch, 4)
    });
    assert!(
        evicted.0 > 0,
        "compact eviction must publish at least one disk edge"
    );
    let (resident_after_eviction, serialized_after_eviction) = byte_root_resident_totals(&trie);
    assert!(resident_after_eviction < resident_before);
    assert!(serialized_after_eviction < serialized_before);

    for (term, value) in &terms {
        assert_eq!(trie.get_lockfree(term), Some(*value));
    }
    assert_eq!(
        byte_root_resident_totals(&trie),
        (resident_before, serialized_before)
    );
}

#[test]
fn detached_byte_dictionary_node_faults_do_not_change_published_residency() {
    let dir = scratch("byte-detached-fault-residency");
    let path = dir.path().join("detached-fault-residency.part");
    let cold_terms: Vec<Vec<u8>> = (0..40)
        .map(|index| format!("cold-detached-{index:03}").into_bytes())
        .collect();
    let warm_terms: Vec<Vec<u8>> = (0..20)
        .map(|index| format!("warm-detached-{index:03}").into_bytes())
        .collect();
    let all_terms: BTreeSet<Vec<u8>> = cold_terms
        .iter()
        .chain(warm_terms.iter())
        .cloned()
        .collect();
    let terms_with_values: Vec<(Vec<u8>, u64)> = all_terms
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, term)| (term, 50_000 + index as u64))
        .collect();

    let mut owned = PersistentARTrie::<u64>::create(&path).expect("create");
    owned.set_durability_policy(DurabilityPolicy::Immediate);
    owned.install_overlay();
    owned
        .bench_enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("enable eviction");
    for (term, value) in &terms_with_values {
        owned
            .try_increment_cas_durable(term, *value)
            .expect("durable valued insert");
    }
    owned
        .bench_immutable_checkpoint_with_eviction()
        .expect("checkpoint with eviction registry");

    let (resident_before, serialized_before) = byte_root_resident_totals(&owned);

    let mut evicted = 0usize;
    for _ in 0..8 {
        evicted += evict_cold_overlay(&owned, usize::MAX);
    }
    assert!(evicted > 0, "cold byte subtrees must be evicted");
    let (resident_after_eviction, serialized_after_eviction) = byte_root_resident_totals(&owned);
    assert!(resident_after_eviction < resident_before);
    assert!(serialized_after_eviction < serialized_before);

    let trie = Arc::new(owned);
    let detached_root = Dictionary::root(&trie);
    assert_eq!(
        walk_byte_terms(&detached_root),
        all_terms,
        "the detached public byte DictionaryNode must fault every durable term"
    );
    assert_eq!(
        byte_root_resident_totals(&trie),
        (resident_after_eviction, serialized_after_eviction),
        "detached byte loads must not change current-root residency"
    );
    drop(detached_root);

    for (term, value) in &terms_with_values {
        assert_eq!(
            trie.get_lockfree(term),
            Some(*value),
            "faulting current-root value read lost {term:?}"
        );
    }
    assert_eq!(
        byte_root_resident_totals(&trie),
        (resident_before, serialized_before)
    );

    // A detached root owns its faulter, and the faulter owns the trie. Therefore
    // the backing mmap/arena outlives every lazy OnDisk load even after the caller
    // releases its last explicit shared-trie handle.
    let mut reevicted = 0usize;
    for _ in 0..8 {
        reevicted += evict_cold_overlay(&trie, usize::MAX);
    }
    assert!(
        reevicted > 0,
        "the lifetime witness must retain actual OnDisk byte children"
    );
    let lifetime_root = Dictionary::root(&trie);
    let trie_lifetime = Arc::downgrade(&trie);
    drop(trie);
    assert!(
        trie_lifetime.upgrade().is_some(),
        "the detached byte root must lease the backing trie address space"
    );
    assert_eq!(walk_byte_terms(&lifetime_root), all_terms);
    drop(lifetime_root);
    assert!(
        trie_lifetime.upgrade().is_none(),
        "dropping the final detached byte root must release its trie lease"
    );
}

// ───────────── OE9-byte (Phase A — prefix-fault twin of the char OE9) ─────────────

/// Byte twin of char OE9: the production prefix path (`iter_prefix`/`_with_values` →
/// shared `overlay_navigate` + `overlay_collect_*`) must fault OnDisk children
/// READ-ONLY, else it under-reports evicted subtrees. Evict the shared "abc" interior
/// + subtree; `iter_prefix(b"ab")` must still return all 4 terms (faulted).
#[test]
fn oe9_byte_iter_prefix_faults_evicted_subtree_no_under_report() {
    let dir = scratch("oe9-byte-prefix-fault");
    let path = dir.path().join("oe9b.artb");
    let mut owned = PersistentARTrie::<u64>::create(&path).expect("create");
    owned.set_durability_policy(DurabilityPolicy::Immediate);
    owned.install_overlay();
    owned
        .bench_enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("bench_enable_eviction");

    let under_ab: [(&[u8], u64); 4] = [(b"abcd", 1), (b"abce", 2), (b"abcfg", 3), (b"abxy", 4)];
    for (t, v) in under_ab.iter() {
        owned.try_increment_cas_durable(t, *v).expect("inc");
    }
    owned.try_increment_cas_durable(b"az", 99).expect("sibling");
    owned
        .bench_immutable_checkpoint_with_eviction()
        .expect("checkpoint with eviction");

    let coordinator = owned
        .eviction_coordinator
        .lock()
        .expect("eviction_coordinator mutex poisoned")
        .as_ref()
        .map(std::sync::Arc::clone)
        .expect("coordinator present");
    let root = owned
        .lockfree_root
        .as_ref()
        .expect("published overlay root");
    let evicted = coordinator
        .force_eviction_compact_bytes_root(root, 1 << 20, |mut batch| {
            retain_byte_candidates(&mut batch, |path| path.starts_with(b"abc"));
            evict_overlay_compact_batch(&owned, batch, 4)
        })
        .0;
    assert!(
        evicted > 0,
        "OE9 byte: expected to evict the 'abc' subtree (0 = driver no-op)"
    );

    let mut got: Vec<Vec<u8>> = owned
        .iter_prefix(b"ab")
        .expect("prefix 'ab' present")
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            b"abcd".to_vec(),
            b"abce".to_vec(),
            b"abcfg".to_vec(),
            b"abxy".to_vec()
        ],
        "byte iter_prefix MUST fault the evicted subtree (no under-report)"
    );
    assert!(
        !got.iter().any(|t| t.as_slice() == b"az"),
        "prefix scoping: 'az' is outside 'ab' and must be excluded"
    );

    let mut gv: Vec<(Vec<u8>, u64)> = owned
        .iter_prefix_with_values(b"ab")
        .expect("prefix present")
        .collect();
    gv.sort();
    assert_eq!(
        gv,
        vec![
            (b"abcd".to_vec(), 1),
            (b"abce".to_vec(), 2),
            (b"abcfg".to_vec(), 3),
            (b"abxy".to_vec(), 4)
        ],
        "byte iter_prefix_with_values MUST fault evicted finals with exact counters"
    );
}

// ───────────── Phase 7 byte twin (budget ACTIVATION — checkpoint tail evicts to budget) ─────────────

/// Byte twin of `phase7_resident_budget_checkpoint_tail_evicts_to_budget`: with
/// `resident_budget_bytes = Some(small)`, the byte checkpoint tail evicts cold
/// resident nodes down to budget losslessly while retaining complete durable
/// topology for exact fault-in.
#[test]
fn phase7_byte_resident_budget_checkpoint_tail_evicts_to_budget() {
    #[derive(Debug)]
    struct Observation {
        resident: usize,
        registered: usize,
        resident_bytes: usize,
    }

    #[derive(Debug)]
    struct Run {
        first: Observation,
        second: Observation,
        all_present: bool,
    }

    fn run(budget: Option<usize>) -> Run {
        let dir = scratch("phase7-byte-budget");
        let path = dir.path().join("p7b.artb");
        let mut owned = PersistentARTrie::<u64>::create(&path).expect("create");
        owned.set_durability_policy(DurabilityPolicy::Immediate);
        owned.install_overlay();
        let config = EvictionConfig {
            resident_budget_bytes: budget,
            ..EvictionConfig::without_memory_monitor()
        };
        owned
            .bench_enable_eviction(config)
            .expect("bench_enable_eviction");

        let terms: Vec<String> = (0..40).map(|i| format!("ngram-{i:03}")).collect();
        for (i, t) in terms.iter().enumerate() {
            owned
                .try_increment_cas_durable(t.as_bytes(), (i + 1) as u64)
                .expect("inc");
        }
        let observe = |trie: &PersistentARTrie<u64>| {
            let coordinator = trie
                .eviction_coordinator
                .lock()
                .expect("eviction_coordinator mutex poisoned");
            let coordinator = coordinator.as_ref().expect("eviction enabled");
            let root = trie.lockfree_root.as_ref().expect("overlay root");
            Observation {
                resident: coordinator.root_resident_totals(root).unwrap_or_default().0,
                registered: coordinator.disk_registry_len(),
                resident_bytes: coordinator
                    .byte_root_resident_estimate_bytes(root)
                    .unwrap_or(0),
            }
        };

        owned
            .bench_immutable_checkpoint_with_eviction()
            .expect("ckpt1");
        let first = observe(&owned);
        owned
            .bench_immutable_checkpoint_with_eviction()
            .expect("ckpt2");
        let second = observe(&owned);
        let all_present = terms
            .iter()
            .enumerate()
            .all(|(i, t)| MappedDictionary::get_value(&owned, t.as_str()) == Some((i + 1) as u64));
        Run {
            first,
            second,
            all_present,
        }
    }

    let budget = 2_000;
    let treatment = run(Some(budget));
    assert!(
        treatment.first.registered > 0,
        "checkpoint must publish topology"
    );
    assert!(
        treatment.first.resident < treatment.first.registered,
        "byte budget tail must make part of the topology nonresident: {treatment:?}"
    );
    assert!(
        treatment.first.resident_bytes <= budget && treatment.second.resident_bytes <= budget,
        "both byte checkpoint tails must enforce the {budget}-byte bound: {treatment:?}"
    );
    assert_eq!(treatment.first.registered, treatment.second.registered);
    assert!(treatment.second.resident <= treatment.first.resident);
    assert!(
        treatment.all_present,
        "byte budget eviction must be lossless"
    );

    let control = run(None);
    assert_eq!(control.first.resident, control.second.resident);
    assert_eq!(control.first.registered, control.second.registered);
    assert_eq!(control.first.resident, control.first.registered);
    assert!(control.all_present, "byte control must retain every term");
    assert!(
        treatment.first.resident < control.first.resident,
        "byte treatment must retain fewer nodes than control: treatment={treatment:?}, control={control:?}"
    );
}

// ─────── Sequential-sibling corruption regression — BYTE twin ───────
//
// Byte does NOT have char's arena-space off-by-one (it uses `as_arena_slot()`), but it
// shared the key-order-vs-slot-order assumption AND, unlike char, had no per-index
// contiguity re-check in `validate_v2_serialization_context` — so a mis-selected
// sequential encoding would SILENTLY corrupt rather than fail loud. These byte twins of
// char's regression drive the incremental-checkpoint + resident-budget-eviction workload
// (the cross-arena post-eviction re-serialization that exercises the sequential path) and
// assert full completeness after reopen.

#[test]
fn byte_interleaved_checkpoint_with_resident_budget_eviction_preserves_all_terms() {
    let dir = scratch("seqsib-byte-interleaved");
    let path = dir.path().join("interleaved.artb");
    let n: i64 = 3_000;
    let checkpoint_every: i64 = 300;

    {
        let mut owned = PersistentARTrie::<i64>::create(&path).expect("create");
        owned.set_durability_policy(DurabilityPolicy::Immediate);
        owned.install_overlay();
        owned
            .bench_enable_eviction(EvictionConfig {
                resident_budget_bytes: Some(64 * 1024),
                ..EvictionConfig::without_memory_monitor()
            })
            .expect("enable eviction");

        for i in 0..n {
            let term = format!("symbol_{i:08}");
            owned.insert_with_value(&term, i);
            if i % checkpoint_every == 0 {
                owned
                    .checkpoint()
                    .expect("interleaved checkpoint must not corrupt the sequential layout");
            }
        }
        owned.checkpoint().expect("final checkpoint");

        let resident = owned.evictable_node_count().unwrap_or(usize::MAX);
        let (registered, resident_bytes) = {
            let coordinator = owned
                .eviction_coordinator
                .lock()
                .expect("eviction_coordinator mutex poisoned");
            let coordinator = coordinator.as_ref().expect("eviction enabled");
            let root = owned.lockfree_root.as_ref().expect("overlay root");
            (
                coordinator.disk_registry_len(),
                coordinator
                    .byte_root_resident_estimate_bytes(root)
                    .unwrap_or(0),
            )
        };
        assert!(
            resident < registered,
            "byte eviction must leave nonresident topology ({resident} < {registered})"
        );
        assert!(
            resident_bytes <= 64 * 1024,
            "byte resident bytes exceed budget"
        );

        for i in 0..n {
            let term = format!("symbol_{i:08}");
            assert_eq!(
                MappedDictionary::get_value(&owned, term.as_str()),
                Some(i),
                "byte term {term} lost in-process"
            );
        }
    }

    {
        let reopened =
            PersistentARTrie::<i64>::open(&path).expect("reopen after interleaved eviction");
        for i in 0..n {
            let term = format!("symbol_{i:08}");
            assert_eq!(
                MappedDictionary::get_value(&reopened, term.as_str()),
                Some(i),
                "byte term {term} lost after reopen"
            );
        }
    }
}

mod byte_seqsib_property {
    use super::*;
    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    enum Op {
        InsertBatch,
        Checkpoint,
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![3 => Just(Op::InsertBatch), 1 => Just(Op::Checkpoint)]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 12, ..ProptestConfig::default() })]

        /// PROPERTY (byte twin): any interleaving of insert batches and checkpoints under
        /// a small resident budget must never corrupt and must preserve every inserted
        /// (term, value) across a drop + reopen.
        #[test]
        fn byte_interleaved_insert_checkpoint_reopen_loses_nothing(
            ops in prop::collection::vec(op_strategy(), 1..32)
        ) {
            let dir = scratch("seqsib-byte-prop");
            let path = dir.path().join("seqsib-byte-prop.artb");
            let mut inserted: Vec<(String, i64)> = Vec::new();
            {
                let mut owned = PersistentARTrie::<i64>::create(&path).expect("create");
                owned.set_durability_policy(DurabilityPolicy::Immediate);
                owned.install_overlay();
                owned
                    .bench_enable_eviction(EvictionConfig {
                        resident_budget_bytes: Some(4 * 1024),
                        ..EvictionConfig::without_memory_monitor()
                    })
                    .expect("enable eviction");

                let mut next: i64 = 0;
                for op in &ops {
                    match op {
                        Op::InsertBatch => {
                            for _ in 0..8 {
                                let term = format!("symbol_{next:08}");
                                owned.insert_with_value(&term, next);
                                inserted.push((term, next));
                                next += 1;
                            }
                        }
                        Op::Checkpoint => {
                            owned
                                .checkpoint()
                                .expect("checkpoint must not corrupt the sequential layout");
                        }
                    }
                }
                owned.checkpoint().expect("final checkpoint");
                for (term, val) in &inserted {
                    prop_assert_eq!(MappedDictionary::get_value(&owned, term.as_str()), Some(*val));
                }
            }
            let reopened = PersistentARTrie::<i64>::open(&path).expect("reopen");
            for (term, val) in &inserted {
                prop_assert_eq!(
                    MappedDictionary::get_value(&reopened, term.as_str()),
                    Some(*val)
                );
            }
        }
    }
}

/// Dirty-skip growth fix — BYTE twin of the headline: re-checkpointing an unchanged trie appends
/// ~nothing (every node durable-clean and its ptr reused), so the `.artb` data file barely grows
/// regardless of checkpoint count. Pre-dirty-skip each idempotent checkpoint re-appended the
/// whole resident set.
#[test]
fn byte_dirty_skip_bounds_growth_across_idempotent_checkpoints() {
    let dir = scratch("dirtyskip-byte-idem");
    let path = dir.path().join("idem.artb");
    let mut owned = PersistentARTrie::<i64>::create(&path).expect("create");
    owned.set_durability_policy(DurabilityPolicy::Immediate);
    owned.install_overlay();
    owned
        .bench_enable_eviction(EvictionConfig {
            resident_budget_bytes: Some(64 * 1024),
            ..EvictionConfig::without_memory_monitor()
        })
        .expect("enable eviction");

    for i in 0..3_000i64 {
        owned.insert_with_value(&format!("symbol_{i:08}"), i);
    }
    owned.checkpoint().expect("first checkpoint");
    let size_after_first = std::fs::metadata(&path).expect("stat").len();

    for _ in 0..20 {
        owned.checkpoint().expect("idempotent checkpoint");
    }
    let size_after_idempotent = std::fs::metadata(&path).expect("stat").len();
    let growth = size_after_idempotent - size_after_first;
    assert!(
        growth < 256 * 1024,
        "byte: 20 idempotent checkpoints grew the .artb data file by {growth} bytes (from \
         {size_after_first}); dirty-skip must make re-checkpoints append ~nothing"
    );

    for i in 0..3_000i64 {
        assert_eq!(
            MappedDictionary::get_value(&owned, format!("symbol_{i:08}").as_str()),
            Some(i),
            "byte term {i} lost"
        );
    }
}
