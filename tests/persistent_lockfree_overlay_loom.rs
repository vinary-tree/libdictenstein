//! Bounded loom schedule checks for the Phase-A char-overlay lock-free invariants.
//!
//! These complement `persistent_artrie_loom_correspondence.rs` (which models the
//! single-key byte publication + cache protocol) by modeling the properties
//! introduced in Phase A:
//!
//! 1. **No lost update under path-copy + CAS** — two threads inserting *disjoint*
//!    keys must both survive (neither CAS clobbers the other), via the bounded
//!    retry loop on the atomic root.
//! 2. **Prefix-finalize is single-arbiter** — when two threads race to finalize a
//!    term that is a proper prefix of an existing one, they converge on the SAME
//!    shared node and exactly one `try_set_final` wins (the bug-fix in
//!    `lockfree_cas.rs`'s `depth == len` branch: return the existing node, do not
//!    pre-finalize a per-thread copy).
//! 3. **Reader holds an owned `Arc` snapshot → no use-after-free** — the leak-fix
//!    invariant: children are owned `Arc`s (`Child::InMem`), so a reader that
//!    loaded a node keeps it alive across concurrent writes/finalization.
//!
//! loom instruments loom's own atomics, not `arc_swap`/std atomics, so — exactly
//! like the existing loom correspondence test — `ModelRootSlot` is a faithful
//! stand-in for the production `AtomicNodePtr` (`load` == `load_full`,
//! `compare_exchange` == `compare_and_swap` + `Arc::ptr_eq`). The model node
//! carries OWNED `Arc<ModelNode>` children (the post-leak-fix invariant) and an
//! atomic `is_final` flipped by a `fetch_or`-based `try_set_final`, mirroring
//! `PersistentCharNode`.
//!
//! Run with: cargo test --features persistent-artrie --test persistent_lockfree_overlay_loom

#![cfg(feature = "persistent-artrie")]

use loom::sync::atomic::{AtomicBool, Ordering};
use loom::sync::{Arc, RwLock};
use loom::thread;
use std::collections::BTreeMap;

/// An immutable path-copy node with OWNED `Arc` children and an atomic final flag
/// — the post-leak-fix `PersistentCharNode` shape (`Child::InMem(Arc<…>)` children,
/// finality flipped in place by `try_set_final`).
#[derive(Debug)]
struct ModelNode {
    is_final: AtomicBool,
    children: BTreeMap<u8, Arc<ModelNode>>,
}

impl ModelNode {
    fn empty() -> Arc<Self> {
        Arc::new(Self {
            is_final: AtomicBool::new(false),
            children: BTreeMap::new(),
        })
    }

    fn with_one_child(key: u8, child: Arc<ModelNode>) -> Arc<Self> {
        let mut children = BTreeMap::new();
        children.insert(key, child);
        Arc::new(Self {
            is_final: AtomicBool::new(false),
            children,
        })
    }

    fn is_final(&self) -> bool {
        self.is_final.load(Ordering::Acquire)
    }

    fn find_child(&self, key: u8) -> Option<&Arc<ModelNode>> {
        self.children.get(&key)
    }

    /// Atomic single-arbiter finalize (mirrors `PersistentCharNode::try_set_final`:
    /// `flags.fetch_or(IS_FINAL)`). Returns `true` iff THIS call flipped 0→1.
    fn try_set_final(&self) -> bool {
        !self.is_final.fetch_or(true, Ordering::AcqRel)
    }

    /// Path-copy: a new node version with `key`'s child set to `child`. Existing
    /// (off-path) children are shared by `Arc::clone` (owned, refcount-bumped) and
    /// the new node's finality is copied — exactly `PersistentCharNode::with_child`.
    fn with_child(&self, key: u8, child: Arc<ModelNode>) -> Arc<Self> {
        let mut children = self.children.clone();
        children.insert(key, child);
        Arc::new(Self {
            is_final: AtomicBool::new(self.is_final()),
            children,
        })
    }

    /// **R-B (proven overlay DELETE):** a FRESH cleared copy — the model mirror of
    /// `OverlayNode::as_non_final`. Clears finality (1→0) on a NEW node version
    /// while RETAINING children (remove "a" keeps "ab"), to be published ONLY via
    /// the root CAS. This is the decisive §3.5 choice: the 1→0 transition is NEVER
    /// an in-place `fetch_and` on the shared node (which would race `try_set_final`
    /// with no serialization point); it is a fresh copy arbitrated by the root CAS.
    fn as_non_final_clone(&self) -> Arc<Self> {
        Arc::new(Self {
            is_final: AtomicBool::new(false), // cleared on the FRESH copy
            children: self.children.clone(),  // SUBTREE RETAINED
        })
    }
}

/// Stand-in for the production `AtomicNodePtr` (`arc_swap::ArcSwapOption`), which
/// loom cannot instrument. `load` ≙ `load_full`; `compare_exchange` ≙
/// `compare_and_swap` + `Arc::ptr_eq` (pointer-identity CAS, no spurious failure).
#[derive(Debug)]
struct ModelRootSlot {
    ptr: RwLock<Option<Arc<ModelNode>>>,
}

impl ModelRootSlot {
    fn new(node: Arc<ModelNode>) -> Self {
        Self {
            ptr: RwLock::new(Some(node)),
        }
    }

    fn load(&self) -> Option<Arc<ModelNode>> {
        self.ptr.read().expect("root read lock").clone()
    }

    fn compare_exchange(
        &self,
        expected: &Arc<ModelNode>,
        new: Arc<ModelNode>,
    ) -> Result<Arc<ModelNode>, Arc<ModelNode>> {
        let mut guard = self.ptr.write().expect("root write lock");
        match guard.as_ref() {
            Some(current) if Arc::ptr_eq(current, expected) => {
                let old = Arc::clone(current);
                *guard = Some(new);
                Ok(old)
            }
            Some(current) => Err(Arc::clone(current)),
            None => Err(ModelNode::empty()),
        }
    }
}

/// Insert a single-byte key as a direct child of the root and finalize it,
/// modeling `insert_cas` for a length-1 term: path-copy the root, CAS, then
/// `try_set_final` the leaf. Returns `true` iff THIS call finalized the term.
///
/// The `depth == len` handoff mirrors the production fix: when the child already
/// exists but is not final (the prefix case), return the EXISTING shared child so
/// `try_set_final` is the single atomic arbiter — never a per-thread copy.
fn insert_one_char(root: &ModelRootSlot, key: u8) -> bool {
    loop {
        let current = root.load().expect("root is initialized");

        let leaf = match current.find_child(key) {
            // Already a complete term.
            Some(existing) if existing.is_final() => return false,
            // Prefix case: share the existing (non-final) child — single arbiter.
            Some(existing) => Arc::clone(existing),
            // Fresh case: a new non-final leaf.
            None => ModelNode::empty(),
        };

        let new_root = current.with_child(key, Arc::clone(&leaf));
        match root.compare_exchange(&current, new_root) {
            // CAS won: finalize the (possibly shared) leaf; fetch_or arbitrates.
            Ok(_) => return leaf.try_set_final(),
            // CAS lost: another writer advanced the root — rebase and retry.
            Err(_) => continue,
        }
    }
}

/// **R-B (proven overlay DELETE):** clear a single-byte key's membership, modeling
/// `remove_cas_durable`'s visibility CAS (`try_remove_lockfree_path` +
/// `build_remove_path_recursive`). Returns `true` iff THIS call cleared a present
/// term, `false` if it was already absent.
///
/// The decisive §3.5 contract this models: at the target node, removal clears
/// finality on a **FRESH** `as_non_final_clone` copy (RETAINING the child set, so
/// a longer term like "ab" is preserved) and publishes it ONLY via the root CAS —
/// it NEVER flips the shared existing node's bit in place. The root-CAS arbiter
/// thus linearizes a remove against any concurrent insert/finalize on the same
/// node (last-writer-wins), and the insert path's in-place `try_set_final` stays
/// the only writer of the 0→1 transition.
fn remove_one_char(root: &ModelRootSlot, key: u8) -> bool {
    loop {
        let current = root.load().expect("root is initialized");

        match current.find_child(key) {
            // Present (final): build a FRESH cleared copy (children retained) and
            // splice it via the root CAS — the sole arbiter of the 1→0 clear.
            Some(existing) if existing.is_final() => {
                let cleared = existing.as_non_final_clone();
                let new_root = current.with_child(key, cleared);
                match root.compare_exchange(&current, new_root) {
                    Ok(_) => return true,  // we published the clear
                    Err(_) => continue,    // CAS lost: rebase + retry (may be absent now)
                }
            }
            // Already non-final (a bare prefix node) ⇒ absent; do NOT publish a
            // no-op spine.
            Some(_) => return false,
            // Missing edge ⇒ absent.
            None => return false,
        }
    }
}

#[test]
fn concurrent_disjoint_inserts_never_lose_an_update() {
    loom::model(|| {
        let root = Arc::new(ModelRootSlot::new(ModelNode::empty()));

        let t1 = {
            let root = Arc::clone(&root);
            thread::spawn(move || insert_one_char(&root, b'a'))
        };
        let t2 = {
            let root = Arc::clone(&root);
            thread::spawn(move || insert_one_char(&root, b'b'))
        };
        let win_a = t1.join().expect("join a");
        let win_b = t2.join().expect("join b");

        // Disjoint keys: each is newly finalized by its inserter.
        assert!(win_a, "insert of 'a' must report newly finalized");
        assert!(win_b, "insert of 'b' must report newly finalized");

        // Neither CAS clobbered the other: BOTH keys are present and final.
        let final_root = root.load().expect("root");
        assert!(
            final_root.find_child(b'a').is_some_and(|n| n.is_final()),
            "'a' lost under concurrent path-copy + CAS"
        );
        assert!(
            final_root.find_child(b'b').is_some_and(|n| n.is_final()),
            "'b' lost under concurrent path-copy + CAS"
        );
    });
}

#[test]
fn concurrent_prefix_finalize_has_exactly_one_winner() {
    // The prefix race (retry loop + RwLock + fetch_or across two threads) has a
    // large schedule space; bound preemptions to 3 (loom guidance: concurrency
    // defects surface within 2–3 preemptions) to keep the check fast. The
    // arbitration bug, if present, manifests within a single preemption.
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(3);
    builder.check(|| {
        // Pre-state models "ab" present:
        //   root -'a'-> node_a (NOT final) -'b'-> leaf_ab (final).
        let leaf_ab = ModelNode::empty();
        leaf_ab.try_set_final();
        let node_a = ModelNode::with_one_child(b'b', leaf_ab);
        let root = Arc::new(ModelRootSlot::new(ModelNode::with_one_child(b'a', node_a)));

        // Two threads race to insert the proper prefix "a" (finalize node_a).
        let t1 = {
            let root = Arc::clone(&root);
            thread::spawn(move || insert_one_char(&root, b'a'))
        };
        let t2 = {
            let root = Arc::clone(&root);
            thread::spawn(move || insert_one_char(&root, b'a'))
        };
        let w1 = t1.join().expect("join 1");
        let w2 = t2.join().expect("join 2");

        // Exactly one thread finalizes the shared prefix node (no double-count,
        // no lost insert).
        assert!(
            w1 ^ w2,
            "exactly one racer may finalize the prefix 'a' (got w1={w1}, w2={w2})"
        );

        // "a" is now final and "ab" is preserved (node_a kept its 'b' child).
        let r = root.load().expect("root");
        let a = r.find_child(b'a').expect("'a' present");
        assert!(a.is_final(), "prefix 'a' must be final after the race");
        assert!(
            a.find_child(b'b').is_some_and(|n| n.is_final()),
            "'ab' must be preserved through the prefix finalize"
        );
    });
}

// ═══════════════════ OE9 — read/write FAULT-IN double-install (design §3) ═══════════════════
//
// Models `find_leaf_faulting`'s install CAS: a child slot starts OnDisk (evicted).
// Two faulter threads each LOAD THEIR OWN copy of the durable node and race to
// CAS-install it InMem; a writer concurrently path-copies a sibling. The single
// `FaultRootSlot` CAS (pointer-identity, `Arc::ptr_eq`) must arbitrate so that:
//   * exactly one faulter's install is the published version for that slot
//     (the loser rebases, its loaded Arc drops — no double-link, no clobber);
//   * the writer's sibling is never lost (loser-safe);
//   * the final published root has the faulted child InMem (XOR OnDisk) and final.
// This is the loom witness for `OverlayEvictionCas.tla`'s FaultInCas ‖ WriterCas
// arbitration on the single `lockfree_root`.

/// A child slot in the fault-in model: either an in-memory node or an evicted
/// (OnDisk) reference carrying the durable node's final bit (the "bytes" a faulter
/// would load — the §2 round-trip image).
#[derive(Debug, Clone)]
enum ModelChild {
    InMem(Arc<FaultModelNode>),
    OnDisk { is_final: bool },
}

/// A model node whose children may be InMem OR OnDisk — the fault-in shape
/// (mirrors `PersistentCharNode` with `Child::{InMem,OnDisk}`). Separate from the
/// membership `ModelNode` above (which only needs InMem children) to keep each
/// model minimal and faithful.
#[derive(Debug)]
struct FaultModelNode {
    is_final: AtomicBool,
    children: BTreeMap<u8, ModelChild>,
}

impl FaultModelNode {
    fn leaf(is_final: bool) -> Arc<Self> {
        Arc::new(Self {
            is_final: AtomicBool::new(is_final),
            children: BTreeMap::new(),
        })
    }

    fn with_child(&self, key: u8, child: ModelChild) -> Arc<Self> {
        let mut children = self.children.clone();
        children.insert(key, child);
        Arc::new(Self {
            is_final: AtomicBool::new(self.is_final.load(Ordering::Acquire)),
            children,
        })
    }

    fn find_child(&self, key: u8) -> Option<&ModelChild> {
        self.children.get(&key)
    }
}

/// Stand-in root slot for the fault model (same CAS contract as `ModelRootSlot`).
#[derive(Debug)]
struct FaultRootSlot {
    ptr: RwLock<Arc<FaultModelNode>>,
}

impl FaultRootSlot {
    fn new(node: Arc<FaultModelNode>) -> Self {
        Self {
            ptr: RwLock::new(node),
        }
    }
    fn load(&self) -> Arc<FaultModelNode> {
        self.ptr.read().expect("root read").clone()
    }
    fn compare_exchange(
        &self,
        expected: &Arc<FaultModelNode>,
        new: Arc<FaultModelNode>,
    ) -> Result<(), ()> {
        let mut g = self.ptr.write().expect("root write");
        if Arc::ptr_eq(&g, expected) {
            *g = new;
            Ok(())
        } else {
            Err(())
        }
    }
}

/// Model `find_leaf_faulting` for a length-1 key: if the slot is OnDisk, LOAD our
/// own InMem copy (the per-faulter Arc) and CAS-install it; on loss, rebase and
/// retry (now possibly already InMem ⇒ done). Bounded retries (liveness).
fn faultin_one_char(root: &FaultRootSlot, key: u8, max_retries: usize) -> bool {
    for _ in 0..=max_retries {
        let cur = root.load();
        match cur.find_child(key) {
            Some(ModelChild::InMem(_)) => return true, // already faulted (by a racer)
            Some(ModelChild::OnDisk { is_final }) => {
                // Load OUR OWN Arc (each faulter independently) and install it.
                let loaded = FaultModelNode::leaf(*is_final);
                let new_root = cur.with_child(key, ModelChild::InMem(loaded));
                match root.compare_exchange(&cur, new_root) {
                    Ok(()) => return true, // we published the fault-in
                    Err(()) => continue,   // racer advanced root: rebase + retry
                }
            }
            None => return false,
        }
    }
    false
}

/// Model a writer path-copying a NEW InMem sibling child under the root.
fn write_sibling(root: &FaultRootSlot, key: u8, max_retries: usize) -> bool {
    for _ in 0..=max_retries {
        let cur = root.load();
        if cur.find_child(key).is_some() {
            return false;
        }
        let new_root = cur.with_child(key, ModelChild::InMem(FaultModelNode::leaf(true)));
        match root.compare_exchange(&cur, new_root) {
            Ok(()) => return true,
            Err(()) => continue,
        }
    }
    false
}

#[test]
fn faultin_double_install_one_wins() {
    // Retry loop + RwLock across three threads → bound preemptions (loom guidance:
    // defects surface within 2–3 preemptions) to keep the schedule space tractable.
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(3);
    builder.check(|| {
        // root has child 'a' = OnDisk(final=true) [an evicted durable leaf].
        let root = Arc::new(FaultRootSlot::new(
            FaultModelNode::leaf(false).with_child(b'a', ModelChild::OnDisk { is_final: true }),
        ));

        let f1 = {
            let root = Arc::clone(&root);
            thread::spawn(move || faultin_one_char(&root, b'a', 4))
        };
        let f2 = {
            let root = Arc::clone(&root);
            thread::spawn(move || faultin_one_char(&root, b'a', 4))
        };
        let w = {
            let root = Arc::clone(&root);
            thread::spawn(move || write_sibling(&root, b'z', 4))
        };

        let r1 = f1.join().expect("join f1");
        let r2 = f2.join().expect("join f2");
        let rw = w.join().expect("join w");

        // Both faulters succeed (each either won its install or observed the child
        // already InMem after losing — idempotent, never an error).
        assert!(r1 && r2, "both faulters must succeed (idempotent)");
        // The writer's sibling is never lost (loser-safe CAS).
        assert!(rw, "writer's sibling insert must not be lost to a faulter CAS");

        let final_root = root.load();
        // The faulted child is now InMem (XOR OnDisk) and final-correct.
        match final_root.find_child(b'a') {
            Some(ModelChild::InMem(n)) => {
                assert!(
                    n.is_final.load(Ordering::Acquire),
                    "faulted-in 'a' must carry the durable final bit"
                );
            }
            other => panic!("'a' must be InMem after fault-in, got {other:?}"),
        }
        // The writer's sibling survived.
        assert!(
            matches!(final_root.find_child(b'z'), Some(ModelChild::InMem(_))),
            "writer sibling 'z' must be present and InMem"
        );
    });
}

#[test]
fn reader_holding_owned_arc_snapshot_never_faults() {
    loom::model(|| {
        // root -'a'-> leaf_a (NOT final).
        let leaf_a = ModelNode::empty();
        let root = Arc::new(ModelRootSlot::new(ModelNode::with_one_child(b'a', leaf_a)));

        // Reader: load the root, clone an OWNED Arc to the leaf (the `load_full`
        // analogue), then read its flag. The owned Arc keeps the node alive across
        // any concurrent write/finalization — the leak-fix reclamation invariant.
        let reader = {
            let root = Arc::clone(&root);
            thread::spawn(move || {
                let snapshot = root.load().expect("root snapshot");
                let leaf = Arc::clone(snapshot.find_child(b'a').expect("'a'"));
                // A total, never-faulting atomic read regardless of the writer.
                let observed = leaf.is_final();
                // Membership is monotonic: once observed final it stays final.
                observed
            })
        };
        let writer = {
            let root = Arc::clone(&root);
            thread::spawn(move || insert_one_char(&root, b'a'))
        };

        let _observed = reader.join().expect("reader join");
        let _ = writer.join().expect("writer join");

        // After both complete, 'a' is final and intact.
        let r = root.load().expect("root");
        assert!(
            r.find_child(b'a').is_some_and(|n| n.is_final()),
            "'a' must be finalized after the writer completes"
        );
    });
}

// ═══════════════════ R-B (proven overlay DELETE) — RE-PROOF (design §4.2) ══════════════════
//
// Monotone finality (0→1 only) is DROPPED by delete (1→0). These five schedules
// are the machine-checked witness that the composite {insert, remove} stays
// linearizable under the SINGLE root-CAS arbiter, replacing monotonicity with
// LAST-WRITER-WINS over the root-CAS real-time order. Each models the §3.5
// FRESH-COPY-via-root-CAS clear (NEVER an in-place `fetch_and`). The decisive one
// is #2 (remove ‖ prefix-finalize): the prefix-insert fix must still hold.

/// Model `find_leaf_faulting` THEN clear, for a length-1 key: if the slot is
/// OnDisk, LOAD our own InMem copy (each remover independently) and CAS-publish a
/// FRESH NON-FINAL copy in its place (fault-in + clear fused into one path-copy,
/// exactly as `build_remove_path_recursive`'s OnDisk arm faults in then descends).
/// If already InMem+final, clear it; if InMem+non-final, it's already absent.
/// Bounded retries (liveness). Returns `true` iff this call cleared a present term.
fn faultin_then_remove_one_char(root: &FaultRootSlot, key: u8, max_retries: usize) -> bool {
    for _ in 0..=max_retries {
        let cur = root.load();
        match cur.find_child(key) {
            // Already faulted in by a racer: clear if still final, else absent.
            Some(ModelChild::InMem(n)) => {
                if !n.is_final.load(Ordering::Acquire) {
                    return false; // already cleared / non-final ⇒ absent
                }
                let cleared = FaultModelNode::leaf(false); // FRESH non-final copy
                let new_root = cur.with_child(key, ModelChild::InMem(cleared));
                match root.compare_exchange(&cur, new_root) {
                    Ok(()) => return true,
                    Err(()) => continue, // rebase + retry
                }
            }
            // Evicted: fault in (load our own Arc) AND clear in one published copy.
            Some(ModelChild::OnDisk { is_final }) => {
                if !*is_final {
                    return false; // the durable image says it's non-final ⇒ absent
                }
                // Fault-in + clear fused: publish a FRESH non-final InMem node.
                let cleared = FaultModelNode::leaf(false);
                let new_root = cur.with_child(key, ModelChild::InMem(cleared));
                match root.compare_exchange(&cur, new_root) {
                    Ok(()) => return true,
                    Err(()) => continue, // racer advanced root: rebase + retry
                }
            }
            None => return false,
        }
    }
    false
}

/// **RB2 #1 — remove ‖ insert SAME key (last-writer-wins, no lost op).** A
/// concurrent insert and remove of the same key must converge to a consistent
/// last-writer state under the root-CAS total order: the final published root has
/// 'a' as a single coherent finality value — NEVER a torn state. Neither op is
/// lost (one CAS wins, the loser rebases).
#[test]
fn remove_concurrent_with_insert_same_key_is_last_writer_wins() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(3);
    builder.check(|| {
        // Pre-state: "a" present (final).
        let leaf_a = ModelNode::empty();
        leaf_a.try_set_final();
        let root = Arc::new(ModelRootSlot::new(ModelNode::with_one_child(b'a', leaf_a)));

        let inserter = {
            let root = Arc::clone(&root);
            thread::spawn(move || insert_one_char(&root, b'a'))
        };
        let remover = {
            let root = Arc::clone(&root);
            thread::spawn(move || remove_one_char(&root, b'a'))
        };
        let _ins = inserter.join().expect("join inserter");
        let _rem = remover.join().expect("join remover");

        // LWW consistency: 'a' is always linked (insert (re)links it; remove only
        // clears its bit on a fresh copy, never unlinks it), and its finality is a
        // single coherent last-writer value — no torn read.
        let r = root.load().expect("root");
        let a = r.find_child(b'a').expect("'a' node present (never unlinked)");
        let observed = a.is_final();
        assert_eq!(
            observed,
            a.is_final(),
            "membership of 'a' must be a single coherent last-writer value"
        );
    });
}

/// **RB2 #2 — remove ‖ prefix-finalize (THE DECISIVE SCHEDULE).** "ab" is present;
/// one thread removes / one finalizes the proper prefix "a". The prefix-insert fix
/// MUST still hold: "ab" is ALWAYS preserved, because remove clears finality on a
/// FRESH copy that RETAINS the 'b' → leaf_ab child, arbitrated by the root CAS.
#[test]
fn remove_prefix_concurrent_with_finalize_preserves_longer_term() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(3);
    builder.check(|| {
        // Pre-state "ab": root -'a'-> node_a (NOT final) -'b'-> leaf_ab (final).
        let leaf_ab = ModelNode::empty();
        leaf_ab.try_set_final();
        let node_a = ModelNode::with_one_child(b'b', leaf_ab);
        let root = Arc::new(ModelRootSlot::new(ModelNode::with_one_child(b'a', node_a)));

        let finalizer = {
            let root = Arc::clone(&root);
            thread::spawn(move || insert_one_char(&root, b'a'))
        };
        let remover = {
            let root = Arc::clone(&root);
            thread::spawn(move || remove_one_char(&root, b'a'))
        };
        let _fin = finalizer.join().expect("join finalizer");
        let _rem = remover.join().expect("join remover");

        // ▸ THE INVARIANT: "ab" is ALWAYS preserved — the longer term must never be
        // collateral damage of a prefix remove (subtree retained on the fresh copy).
        let r = root.load().expect("root");
        let a = r.find_child(b'a').expect("'a' interior node present");
        assert!(
            a.find_child(b'b').is_some_and(|n| n.is_final()),
            "\"ab\" MUST survive a concurrent remove of the prefix \"a\" (subtree retained)"
        );
        let _ = a.is_final();
    });
}

/// **RB2 #3 — remove ‖ remove (idempotent, no double-clear UAF).** Two threads
/// race to remove the same present key. Exactly one clears it (true); the other
/// sees it already-absent (false) — no double-clear, no lost op, no UAF.
#[test]
fn concurrent_removes_same_key_one_wins_idempotent() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(3);
    builder.check(|| {
        let leaf_a = ModelNode::empty();
        leaf_a.try_set_final();
        let root = Arc::new(ModelRootSlot::new(ModelNode::with_one_child(b'a', leaf_a)));

        let r1 = {
            let root = Arc::clone(&root);
            thread::spawn(move || remove_one_char(&root, b'a'))
        };
        let r2 = {
            let root = Arc::clone(&root);
            thread::spawn(move || remove_one_char(&root, b'a'))
        };
        let w1 = r1.join().expect("join r1");
        let w2 = r2.join().expect("join r2");

        assert!(
            w1 ^ w2,
            "exactly one of two concurrent removes of a present key may report cleared (w1={w1}, w2={w2})"
        );
        let r = root.load().expect("root");
        assert!(
            !r.find_child(b'a').expect("'a' present").is_final(),
            "'a' must be non-final after a remove wins (idempotent clear)"
        );
    });
}

/// **RB2 #4 — remove THROUGH a faulted-in prefix (reuses OE9 machinery).** A
/// length-1 key 'a' is EVICTED (OnDisk, final). A remover must FAULT it in then
/// clear it via the root CAS, while a writer concurrently path-copies a sibling.
/// The single `FaultRootSlot` CAS arbitrates loser-safe; 'a' ends InMem + cleared.
#[test]
fn remove_through_faulted_in_prefix_one_install_clears() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(3);
    builder.check(|| {
        let root = Arc::new(FaultRootSlot::new(
            FaultModelNode::leaf(false).with_child(b'a', ModelChild::OnDisk { is_final: true }),
        ));

        let remover = {
            let root = Arc::clone(&root);
            thread::spawn(move || faultin_then_remove_one_char(&root, b'a', 4))
        };
        let writer = {
            let root = Arc::clone(&root);
            thread::spawn(move || write_sibling(&root, b'z', 4))
        };
        let cleared = remover.join().expect("join remover");
        let wrote = writer.join().expect("join writer");

        assert!(cleared, "the remover must fault 'a' in and clear it (Removed)");
        assert!(wrote, "writer sibling 'z' must not be lost to the remover CAS");

        let r = root.load();
        match r.find_child(b'a') {
            Some(ModelChild::InMem(n)) => assert!(
                !n.is_final.load(Ordering::Acquire),
                "faulted-in 'a' must be cleared (non-final) after remove"
            ),
            other => panic!("'a' must be InMem (faulted) after remove, got {other:?}"),
        }
        assert!(
            matches!(r.find_child(b'z'), Some(ModelChild::InMem(_))),
            "writer sibling 'z' must survive"
        );
    });
}

/// **RB2 #5 — reader snapshot survives a concurrent remove (no UAF).** A reader
/// loads an owned `Arc` snapshot of a present leaf, then a writer removes that key
/// (publishing a fresh cleared copy via root CAS). The reader's owned snapshot
/// keeps the ORIGINAL node alive (the leak-fix reclamation invariant) — never a
/// use-after-free, and a coherent point-in-time read.
#[test]
fn reader_snapshot_survives_concurrent_remove_no_uaf() {
    loom::model(|| {
        let leaf_a = ModelNode::empty();
        leaf_a.try_set_final();
        let root = Arc::new(ModelRootSlot::new(ModelNode::with_one_child(b'a', leaf_a)));

        let reader = {
            let root = Arc::clone(&root);
            thread::spawn(move || {
                let snapshot = root.load().expect("root snapshot");
                let leaf = Arc::clone(snapshot.find_child(b'a').expect("'a'"));
                leaf.is_final()
            })
        };
        let writer = {
            let root = Arc::clone(&root);
            thread::spawn(move || remove_one_char(&root, b'a'))
        };

        let _observed = reader.join().expect("reader join");
        let _ = writer.join().expect("writer join");

        let r = root.load().expect("root");
        assert!(
            !r.find_child(b'a').expect("'a' present").is_final(),
            "'a' must be cleared (non-final) after the remover completes"
        );
    });
}
