//! In-crate coverage for the **overlay-backed `DictionaryNode` OnDisk fault-in**
//! (F7 BLOCKER-1). The OnDisk path can only be reached after overlay **eviction**
//! through compact generation-bound batches, whose driver is `pub(crate)` — so this lives in-crate,
//! not in `tests/` (which is a separate crate and cannot drive eviction).
//!
//! After cold overlay subtrees are evicted to `Child::OnDisk`, an overlay-backed
//! `DictionaryNode` walk MUST fault those children back in (via the SAFE
//! `SharedOverlayFaulter`) so the transducer / fuzzy walk still observes every term
//! — exactly as the production point-read fault-in (`find_leaf_faulting`) does. The
//! `DictionaryNode` walk must NEVER silently drop an OnDisk child (that would lose
//! terms). These tests are the Rust witness that the fault-in is wired and complete,
//! and that the no-faulter degrade is safe (absent, never a fabricated term).
//!
//! Scratch is real disk (`target/test-tmp`), never `/tmp` (tmpfs on this host).

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::persistent_artrie::eviction::EvictionConfig;
use crate::persistent_artrie::WalConfig;
// F4: the `.read()/.write()` compat shim on the collapsed handle.
use crate::persistent_artrie::char::overlay_fault::SharedOverlayFaulter;
use crate::persistent_artrie::char::{
    PersistentARTrieChar, PersistentARTrieCharNode, SharedCharARTrie,
};
use crate::persistent_artrie::core::durability::DurabilityPolicy;
use crate::persistent_artrie::core::eviction::CompactEvictionBatch;
use crate::persistent_artrie::core::overlay::evict::OverlayEvictable;
use crate::persistent_artrie::core::overlay::OverlayFaulter;
use crate::persistent_artrie::core::shared_access::SharedTrieAccess;
use crate::{Dictionary, DictionaryNode};

use super::lockfree_cas::DEFAULT_MAX_FAULTIN_RETRIES;

fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

fn char_root_resident_totals<V, S>(trie: &PersistentARTrieChar<V, S>) -> (usize, usize)
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

/// Cold predicate: `c-*` family is cold (the only family ever fed to the evictor).
fn is_cold(path: &[char]) -> bool {
    path.first() == Some(&'c')
}

fn retain_char_candidates<F>(batch: &mut CompactEvictionBatch<u32>, predicate: F)
where
    F: Fn(&[char]) -> bool,
{
    let mut retained = Vec::new();
    if retained.try_reserve_exact(batch.candidates.len()).is_err() {
        batch.candidates.clear();
        return;
    }
    for candidate in &batch.candidates {
        retained.push(
            batch
                .materialize_char_path(candidate.path_id)
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

/// One round of cold-only overlay eviction (coldest-first, registry-gated), exactly
/// as the OE1–OE4 correspondence tests drive it. Returns the count evicted.
fn evict_cold_overlay<V, S>(trie: &PersistentARTrieChar<V, S>, budget_bytes: usize) -> usize
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
        Some(c) => std::sync::Arc::clone(c),
        None => return 0,
    };
    let Some(root) = trie.lockfree_root.as_ref() else {
        return 0;
    };
    coordinator
        .force_eviction_compact_char_root(root, budget_bytes, |mut batch| {
            retain_char_candidates(&mut batch, is_cold);
            super::evict_overlay_compact_batch(trie, batch, 4)
        })
        .0
}

/// DFS of an overlay `DictionaryNode` collecting every final term.
fn walk_terms(node: &PersistentARTrieCharNode<()>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut work = vec![(node.clone(), String::new())];

    while let Some((node, prefix)) = work.pop() {
        if node.is_final() {
            out.insert(prefix.clone());
        }
        for (ch, child) in node.edges() {
            let mut child_prefix = prefix.clone();
            child_prefix.push(ch);
            work.push((child, child_prefix));
        }
    }

    out
}

/// Cursor equivalent of [`walk_terms`]. Capturing the root cursor must resolve
/// every durable child once into the revision-scoped arena; all subsequent DFS
/// work is direct index traversal with no repeated fault or owned-node clone.
fn walk_terms_cursor(node: &PersistentARTrieCharNode<()>) -> BTreeSet<String> {
    let root = node.snapshot_root_cursor().expect("overlay cursor root");
    let mut out = BTreeSet::new();
    let mut pending = vec![(String::new(), root)];
    while let Some((prefix, cursor)) = pending.pop() {
        // SAFETY: every pending cursor descends from `root` on this retained node.
        if unsafe { node.snapshot_cursor_is_final(cursor) }.expect("cursor finality") {
            out.insert(prefix.clone());
        }
        // SAFETY: same retained-revision cursor provenance.
        unsafe {
            node.filter_map_snapshot_cursor_edges_and_finality(
                cursor,
                Some,
                |label, child, projected| {
                    assert_eq!(label, projected);
                    let mut child_prefix = prefix.clone();
                    child_prefix.push(label);
                    pending.push((child_prefix, child));
                },
            )
        }
        .expect("cursor edges");
    }
    out
}

/// Build an overlay-backed root node directly from a trie's overlay root, with the
/// supplied faulter — the same node `SharedCharARTrie::root()` returns under the
/// flip, but constructed here so the test can choose faulter / no-faulter.
fn overlay_root_with_faulter<S: crate::persistent_artrie::block_storage::BlockStorage>(
    trie: &PersistentARTrieChar<(), S>,
    faulter: Option<
        Arc<dyn OverlayFaulter<crate::persistent_artrie::core::key_encoding::CharKey, ()>>,
    >,
) -> PersistentARTrieCharNode<()> {
    use crate::persistent_artrie::core::overlay::flip::LockFreeOverlay;
    let root = <PersistentARTrieChar<(), S> as LockFreeOverlay<
        crate::persistent_artrie::core::key_encoding::CharKey,
        (),
        S,
    >>::overlay_root_node(trie)
    .expect("overlay root present");
    PersistentARTrieCharNode::from_overlay_root(root, faulter)
}

/// HEADLINE: after cold overlay nodes are evicted to OnDisk, the overlay
/// `DictionaryNode` walk WITH a faulter recovers EVERY term (cold faulted in + live
/// resident); the SAME walk WITHOUT a faulter sees only the resident (live) terms —
/// the cold OnDisk children degrade to absent (never dropped-as-corruption, never a
/// fabricated term). This is the direct proof the OnDisk fault-in is wired and that
/// the no-faulter degrade is safe.
#[test]
fn overlay_dictionary_node_faults_evicted_children_in() {
    let dir = scratch("f7-overlay-fault");
    let path = dir.path().join("fault.artc");

    let cold_terms: Vec<String> = (0..30).map(|i| format!("cold-{i:04}")).collect();
    let live_terms: Vec<String> = (0..30).map(|i| format!("warm-{i:04}")).collect();
    let all: BTreeSet<String> = cold_terms
        .iter()
        .chain(live_terms.iter())
        .cloned()
        .collect();

    let mut owned: PersistentARTrieChar<()> =
        PersistentARTrieChar::create_with_config(&path, WalConfig::no_archive()).expect("create");
    owned.set_durability_policy(DurabilityPolicy::Immediate);
    owned.install_overlay();
    owned
        .bench_enable_eviction(EvictionConfig::without_memory_monitor())
        .expect("bench_enable_eviction");

    for t in cold_terms.iter().chain(live_terms.iter()) {
        assert!(
            owned.insert_cas_durable(t).expect("insert"),
            "term {t:?} should be newly inserted"
        );
    }
    owned
        .bench_immutable_checkpoint_with_eviction()
        .expect("checkpoint with eviction");
    assert!(
        owned.evictable_node_count().unwrap_or(0) > 0,
        "registry must publish evictable nodes"
    );
    let (resident_before, serialized_before) = char_root_resident_totals(&owned);

    // BEFORE eviction: the overlay is fully resident; a no-faulter walk already sees
    // everything (a baseline that proves the eviction below is what creates OnDisk).
    let pre_resident = walk_terms(&overlay_root_with_faulter(&owned, None));
    assert_eq!(
        pre_resident, all,
        "pre-eviction resident walk must see all terms"
    );

    // Evict the COLD subtrees to OnDisk.
    let mut evicted = 0usize;
    for _ in 0..8 {
        evicted += evict_cold_overlay(&owned, 1 << 20);
    }
    assert!(
        evicted > 0,
        "overlay eviction reclaimed ZERO cold nodes — cannot exercise the OnDisk \
         fault-in (the driver is a no-op)"
    );
    let (resident_after_eviction, serialized_after_eviction) = char_root_resident_totals(&owned);
    assert!(resident_after_eviction < resident_before);
    assert!(serialized_after_eviction < serialized_before);

    // (1) WITH a faulter: the walk faults the evicted cold children back in and
    // recovers EVERY term — cold AND live. This is the no-drop guarantee.
    let trie_arc: SharedCharARTrie<()> = Arc::new(owned);
    let faulter: Arc<
        dyn OverlayFaulter<crate::persistent_artrie::core::key_encoding::CharKey, ()>,
    > = Arc::new(SharedOverlayFaulter::new(Arc::clone(&trie_arc)));
    let (faulted_walk, faulted_cursor_walk) = {
        let guard = trie_arc.read();
        let root = overlay_root_with_faulter(&guard, Some(faulter));
        // Cursor walk first: this is the operation that must discover and retain
        // the still-OnDisk descendants while building its immutable arena.
        let cursor = walk_terms_cursor(&root);
        (walk_terms(&root), cursor)
    };
    assert_eq!(
        faulted_cursor_walk, all,
        "faulting overlay cursor walk must recover ALL durable terms"
    );
    assert_eq!(
        faulted_walk, all,
        "faulting overlay DictionaryNode walk must recover ALL terms (cold faulted \
         in + live resident) — an OnDisk child was dropped (terms lost)"
    );
    assert_eq!(
        char_root_resident_totals(&trie_arc),
        (resident_after_eviction, serialized_after_eviction),
        "detached char loads must not change current-root residency"
    );

    // (2) WITHOUT a faulter: the cold OnDisk children degrade to absent (no
    // transition), so only the resident LIVE terms appear — and crucially NO
    // fabricated term and NO panic. (Some cold prefix spine nodes may remain
    // resident, so the no-faulter walk is a SUBSET of all and a SUPERSET of nothing;
    // every live term must still be present.)
    let resident_only_walk = {
        let guard = trie_arc.read();
        walk_terms(&overlay_root_with_faulter(&guard, None))
    };
    assert!(
        resident_only_walk.is_subset(&all),
        "no-faulter walk must never fabricate a term not in the dictionary"
    );
    let live_set: BTreeSet<String> = live_terms.iter().cloned().collect();
    assert!(
        live_set.is_subset(&resident_only_walk),
        "no-faulter walk must still see every (resident) LIVE term"
    );
    // The faulter strictly recovers MORE than the resident-only walk (the evicted
    // cold finals) — proof the OnDisk arm is doing real work, not a no-op.
    assert!(
        faulted_walk.len() > resident_only_walk.len(),
        "faulting walk must recover strictly more terms than the resident-only walk \
         (the evicted cold finals) — OnDisk fault-in had no effect"
    );

    // `transition`-driven descent of a cold term also faults its spine in.
    let faulter2: Arc<
        dyn OverlayFaulter<crate::persistent_artrie::core::key_encoding::CharKey, ()>,
    > = Arc::new(SharedOverlayFaulter::new(Arc::clone(&trie_arc)));
    let root = overlay_root_with_faulter(trie_arc.as_ref(), Some(faulter2));
    let cold0: Vec<char> = cold_terms[0].chars().collect();
    let mut node = root;
    for ch in cold0 {
        node = node
            .transition(ch)
            .expect("transition must fault the evicted cold spine in (not drop it)");
    }
    assert!(
        node.is_final(),
        "the faulted cold-term terminal must be final"
    );
    assert_eq!(
        char_root_resident_totals(&trie_arc),
        (resident_after_eviction, serialized_after_eviction)
    );
    // `DictionaryNode` snapshots intentionally perform detached, non-publishing
    // loads, as the residency assertions above establish. Exercise the current
    // root's fault-and-CAS path explicitly here. The public membership fast path
    // may answer from its positive cache without traversing the overlay, while
    // `Dictionary::contains` creates another detached root snapshot; neither is
    // an authoritative residency-restoration operation.
    let root_slot = trie_arc
        .lockfree_root
        .as_ref()
        .expect("installed overlay root slot");
    for term in &all {
        let units: Vec<u32> = term.chars().map(u32::from).collect();
        assert!(
            trie_arc
                .find_leaf_faulting(root_slot, &units, DEFAULT_MAX_FAULTIN_RETRIES)
                .expect("current-root fault-in")
                .is_some(),
            "current-root read lost {term:?}"
        );
    }
    assert_eq!(
        char_root_resident_totals(&trie_arc),
        (resident_before, serialized_before)
    );

    // The detached node's owned faulter is the address-space lease: it keeps the
    // trie and its buffer/arena managers alive after the caller drops the last
    // explicit shared-trie handle, and releases them with the last node snapshot.
    drop(node);
    let mut reevicted = 0usize;
    for _ in 0..8 {
        reevicted += evict_cold_overlay(&trie_arc, usize::MAX);
    }
    assert!(
        reevicted > 0,
        "the lifetime witness must retain actual OnDisk char children"
    );
    let lifetime_root = Dictionary::root(&trie_arc);
    let trie_lifetime = Arc::downgrade(&trie_arc);
    drop(trie_arc);
    assert!(
        trie_lifetime.upgrade().is_some(),
        "the detached char root must lease the backing trie address space"
    );
    assert_eq!(walk_terms(&lifetime_root), all);
    drop(lifetime_root);
    assert!(
        trie_lifetime.upgrade().is_none(),
        "dropping the final detached char root must release its trie lease"
    );
}
