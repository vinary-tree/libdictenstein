//! **Byte overlay-eviction correspondence tests (Phase 5/6)** — the BYTE twins of
//! char's `overlay_eviction_driver_correspondence` OE3 / OE5 / OE8.
//!
//! These live in-crate (not in `tests/`) because they drive the lifted `pub(crate)`
//! `OverlayEvictable` primitives (`evict_overlay_node_at_path` — the M-2a 1c-guarded
//! per-node evict — and `find_leaf_faulting` via `get_lockfree`), the `pub(crate)`
//! `evict_overlay_nodes` byte batch driver, and the `bench_*` eviction surface, and
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

use std::sync::Arc;

use crate::persistent_artrie::eviction::EvictionConfig;
use crate::persistent_artrie::overlay_fault::evict_overlay_nodes;
use crate::persistent_artrie::PersistentARTrie;
use crate::persistent_artrie_core::durability::DurabilityPolicy;
use crate::persistent_artrie_core::overlay::evict::OverlayEvictable;
use crate::MappedDictionary;

/// A scratch directory on real disk (`target/test-tmp`), never tmpfs `/tmp`.
fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

/// COLD-prefix predicate: a registry byte-path is cold iff it starts with `b'c'` (the
/// `cold-*` term family). Only COLD subtrees are fed to the evictor.
fn is_cold(path: &[u8]) -> bool {
    path.first() == Some(&b'c')
}

/// Drive ONE round of cold-only overlay eviction via the coordinator's byte selection
/// (coldest-first, registry-gated) filtered to COLD paths, reclaimed via the lifted
/// driver `evict_overlay_nodes`. Returns the number of overlay nodes evicted.
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
    coordinator
        .force_eviction_bytes(budget_bytes, |cands| {
            let filtered: Vec<_> = cands.into_iter().filter(|(_, p, _)| is_cold(p)).collect();
            evict_overlay_nodes(trie, filtered, 4)
        })
        .0
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
/// byte.** DETERMINISTIC witness that the lifted `evict_overlay_node_at_path` REFUSES to
/// evict a node OVERWRITTEN since the checkpoint that registered it — preventing the
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
    use crate::persistent_artrie_core::overlay::evict::OverlayEvictOutcome;

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

    // Capture each LEAF node's registry `disk_ptr` WITHOUT evicting (callback → (0,0)).
    let coordinator = trie
        .eviction_coordinator
        .lock()
        .expect("eviction_coordinator mutex poisoned")
        .as_ref()
        .map(Arc::clone)
        .expect("coordinator present");
    let captured: std::cell::RefCell<
        std::collections::HashMap<Vec<u8>, crate::persistent_artrie::swizzled_ptr::SwizzledPtr>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
    coordinator.force_eviction_bytes(1 << 20, |cands| {
        for (_, p, ptr) in cands {
            captured.borrow_mut().insert(p, ptr);
        }
        (0, 0)
    });
    let caps = captured.into_inner();
    let stable_ptr = caps
        .get(b"cold-stable".as_slice())
        .expect("cold-stable leaf registered")
        .clone();
    let rewritten_ptr = caps
        .get(b"cold-rewritten".as_slice())
        .expect("cold-rewritten leaf registered")
        .clone();

    // OVERWRITE cold-rewritten (counter +5 ⇒ path-copy ⇒ fresh stamp-0 leaf at its path).
    trie.try_increment_cas_durable(b"cold-rewritten", 5)
        .expect("overwrite");
    assert_eq!(
        trie.get_lockfree(b"cold-rewritten"),
        Some(25),
        "overwrite stuck (20+5)"
    );

    // THE WITNESS: evicting the overwritten node to its STALE disk_ptr is REFUSED.
    assert_eq!(
        trie.evict_overlay_node_at_path(b"cold-rewritten", rewritten_ptr),
        OverlayEvictOutcome::NotEvictable,
        "1c guard: a node overwritten since checkpoint must NOT be evicted to its stale image"
    );
    assert_eq!(
        trie.get_lockfree(b"cold-rewritten"),
        Some(25),
        "the NEW value survives (not lost to a stale-image eviction)"
    );

    // POSITIVE CONTROL: the UN-overwritten node still evicts and faults back exactly.
    assert_eq!(
        trie.evict_overlay_node_at_path(b"cold-stable", stable_ptr),
        OverlayEvictOutcome::Evicted,
        "an un-overwritten registered node still evicts (guard does not over-reject)"
    );
    assert_eq!(
        trie.get_lockfree(b"cold-stable"),
        Some(10),
        "the evicted node faults back to its exact durable value"
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
    let evicted = coordinator
        .force_eviction_bytes(1 << 20, |cands| {
            let filtered: Vec<_> = cands
                .into_iter()
                .filter(|(_, p, _)| p.starts_with(b"abc"))
                .collect();
            evict_overlay_nodes(&owned, filtered, 4)
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
/// `resident_budget_bytes = Some(small)`, the byte checkpoint tail evicts cold overlay
/// nodes down to budget LOSSLESSLY; a 2nd checkpoint re-registers fewer nodes.
#[test]
fn phase7_byte_resident_budget_checkpoint_tail_evicts_to_budget() {
    fn run(budget: Option<usize>) -> (usize, usize, bool) {
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
        owned
            .bench_immutable_checkpoint_with_eviction()
            .expect("ckpt1");
        let count1 = owned.evictable_node_count().unwrap_or(0);
        owned
            .bench_immutable_checkpoint_with_eviction()
            .expect("ckpt2");
        let count2 = owned.evictable_node_count().unwrap_or(0);
        let all_present = terms
            .iter()
            .enumerate()
            .all(|(i, t)| MappedDictionary::get_value(&owned, t.as_str()) == Some((i + 1) as u64));
        (count1, count2, all_present)
    }

    let (t1, t2, t_lossless) = run(Some(2000));
    assert!(t1 > 0, "byte checkpoint #1 must register the full overlay");
    assert!(
        t2 < t1,
        "byte budget tail must evict cold nodes ({t1} → {t2})"
    );
    assert!(t_lossless, "byte budget eviction must be LOSSLESS");

    let (c1, c2, c_lossless) = run(None);
    assert_eq!(c1, c2, "byte: no budget → no tail eviction");
    assert!(c_lossless, "byte control: all terms present");
    assert!(
        t2 < c2,
        "byte budgeted retains fewer than control ({t2} < {c2})"
    );
}
