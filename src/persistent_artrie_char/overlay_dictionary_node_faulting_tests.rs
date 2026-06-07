//! In-crate coverage for the **overlay-backed `DictionaryNode` OnDisk fault-in**
//! (F7 BLOCKER-1). The OnDisk path can only be reached after overlay **eviction**
//! (`evict_overlay_nodes`), whose driver is `pub(crate)` — so this lives in-crate,
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
use crate::persistent_artrie_char::overlay_fault::SharedOverlayFaulter;
use crate::persistent_artrie_char::{
    PersistentARTrieChar, PersistentARTrieCharNode, SharedCharARTrie,
};
use crate::persistent_artrie_core::durability::DurabilityPolicy;
use crate::persistent_artrie_core::overlay::OverlayFaulter;
use crate::persistent_artrie_core::shared_access::SharedTrieAccess;
use crate::{Dictionary, DictionaryNode};

fn scratch(prefix: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("target/test-tmp").ok();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp")
}

/// Cold predicate: `c-*` family is cold (the only family ever fed to the evictor).
fn is_cold(path: &[char]) -> bool {
    path.first() == Some(&'c')
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
    coordinator
        .force_eviction_char(budget_bytes, |cands| {
            let filtered: Vec<_> = cands.into_iter().filter(|(_, p, _)| is_cold(p)).collect();
            super::evict_overlay_nodes(trie, filtered, 4)
        })
        .0
}

/// DFS of an overlay `DictionaryNode` collecting every final term.
fn walk_terms(node: &PersistentARTrieCharNode<()>) -> BTreeSet<String> {
    fn go(node: &PersistentARTrieCharNode<()>, prefix: &mut String, out: &mut BTreeSet<String>) {
        if node.is_final() {
            out.insert(prefix.clone());
        }
        for (ch, child) in node.edges() {
            prefix.push(ch);
            go(&child, prefix, out);
            prefix.pop();
        }
    }
    let mut out = BTreeSet::new();
    go(node, &mut String::new(), &mut out);
    out
}

/// Build an overlay-backed root node directly from a trie's overlay root, with the
/// supplied faulter — the same node `SharedCharARTrie::root()` returns under the
/// flip, but constructed here so the test can choose faulter / no-faulter.
fn overlay_root_with_faulter<S: crate::persistent_artrie::block_storage::BlockStorage>(
    trie: &PersistentARTrieChar<(), S>,
    faulter: Option<
        Arc<dyn OverlayFaulter<crate::persistent_artrie_core::key_encoding::CharKey, ()>>,
    >,
) -> PersistentARTrieCharNode<()> {
    use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
    let root = <PersistentARTrieChar<(), S> as LockFreeOverlay<
        crate::persistent_artrie_core::key_encoding::CharKey,
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
    owned.enable_lockfree();
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

    // (1) WITH a faulter: the walk faults the evicted cold children back in and
    // recovers EVERY term — cold AND live. This is the no-drop guarantee.
    let trie_arc: SharedCharARTrie<()> = Arc::new(owned);
    let faulter: Arc<dyn OverlayFaulter<crate::persistent_artrie_core::key_encoding::CharKey, ()>> =
        Arc::new(SharedOverlayFaulter::new(Arc::clone(&trie_arc)));
    let faulted_walk = {
        let guard = trie_arc.read();
        walk_terms(&overlay_root_with_faulter(&guard, Some(faulter)))
    };
    assert_eq!(
        faulted_walk, all,
        "faulting overlay DictionaryNode walk must recover ALL terms (cold faulted \
         in + live resident) — an OnDisk child was dropped (terms lost)"
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
    let guard = trie_arc.read();
    let faulter2: Arc<
        dyn OverlayFaulter<crate::persistent_artrie_core::key_encoding::CharKey, ()>,
    > = Arc::new(SharedOverlayFaulter::new(Arc::clone(&trie_arc)));
    let root = overlay_root_with_faulter(&guard, Some(faulter2));
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
}
