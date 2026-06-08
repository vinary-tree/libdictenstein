//! `OverlayEvictable<K, V, S>` — the SHARED GENERIC overlay-eviction + read-fault
//! primitives, lifted K-generic over [`OverlayNode<K, V>`] from char's PROVEN
//! implementation (Phase 4 of the overlay-eviction-v4 design, `docs/design/
//! f7-overlay-eviction-v4-design.md` §4).
//!
//! # Why a trait (trait-first, char is the first/proven impl)
//!
//! The foundation [`OverlayNode<K, V>`] / [`AtomicNodePtr<K, V>`] is ALREADY
//! generic, so per the trait-first rule the shared eviction layer is built as a
//! trait from the START — never a concrete byte twin extracted later. The two
//! per-attempt primitives the design lifts —
//!
//! * `evict_overlay_node_at_path` (the 1c overwrite-race-safe single-node evict),
//! * `find_leaf_faulting` (the read-path single-level fault-in walk),
//!
//! are token-for-token identical between byte and char except for THREE accessors:
//!
//! 1. the `arc-swap` overlay root slot (`lockfree_root: AtomicNodePtr<K, V>`),
//! 2. the [`EpochManager`] (`enter_read` for active-reader accounting),
//! 3. the [`EvictionCoordinator`] (the LRU `remove_hash` after a successful evict —
//!    used by the variant-specific batch driver, exposed here for completeness).
//!
//! and ONE capability: loading an `OnDisk` overlay child back into memory, which
//! is routed through the [`OverlayFaulter<K, V>`] super-trait
//! (`fault_overlay_slot`). The LOADERS stay variant-specific (char
//! `buffer_manager` + `load_char_node_from_disk_lazy`; byte `arena_manager` +
//! `deserialize_node_v2`) — only the SPINE-WALK is shared. The registry plumbing
//! (`register`/`register_char`, the `Vec<u8>` vs `Vec<char>` path conversion in the
//! batch `evict_overlay_nodes`) ALSO stays variant-specific; this trait covers the
//! per-attempt primitives, not the batch driver or the registry.
//!
//! # The 1c overwrite guard (M-2a / M-3a) is preserved VERBATIM
//!
//! [`OverlayEvictable::evict_overlay_node_at_path`] keeps the
//! `current.durable_stamp() != disk_ptr.to_raw() ⇒ NotEvictable` guard exactly as
//! char proved it (char `mod.rs` ~1966): the guard reads the stamp on the
//! FRESHLY-walked victim from THIS `old_root` snapshot, INSIDE the per-attempt fn,
//! so every loser-safe rebase re-reads it. The subsequent root CAS closes the
//! "writer races AFTER the guard" window. See the v4 design §1.4.
//!
//! ZERO `unsafe`: only `AtomicNodePtr::{load,compare_exchange}` (hazard-protected),
//! pure node copies, `Arc` clone/drop, and the EXISTING per-variant lazy loader
//! (called through the safe `&self` `fault_overlay_slot` boundary).

use std::sync::Arc;

use crate::persistent_artrie_core::concurrency::EpochManager;
use crate::persistent_artrie_core::eviction::EvictionCoordinator;
use crate::persistent_artrie_core::key_encoding::KeyEncoding;
use crate::persistent_artrie_core::overlay::atomic_ptr::AtomicNodePtr;
use crate::persistent_artrie_core::overlay::faulter::OverlayFaulter;
use crate::persistent_artrie_core::overlay::node::{Child, OverlayNode};
use crate::persistent_artrie_core::swizzled_ptr::SwizzledPtr;
use crate::value::DictionaryValue;

/// Outcome of an attempt to evict ONE overlay node to an on-disk reference. The
/// SHARED GENERIC outcome — both variants re-export it so their `#[cfg]`-gated
/// drivers + tests name a single type (char keeps `pub(crate) use ... as
/// OverlayEvictOutcome`).
///
/// `#[allow(dead_code)]`: the per-node EVICT primitive (and thus this outcome) is
/// exercised only by the `#[cfg(any(test, bench-internals))]` batch drivers + the OE
/// tests until the production force-eviction caller is wired (a later phase); the
/// READ-fault default (`find_leaf_faulting`) IS used in non-test production builds, so
/// the trait itself is not dead — only the evict-only members are, pre-flip.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverlayEvictOutcome {
    /// The node's parent slot was atomically swapped to an `OnDisk` reference and
    /// the new root CAS-published. The superseded in-memory subtree reclaims by
    /// `Arc` refcount once the last referencing root version (incl. concurrent
    /// reader snapshots) drops.
    Evicted,
    /// A concurrent writer advanced the overlay root between our `load` and our
    /// CAS, so the CAS lost. Loser-safe: nothing was published; the caller should
    /// rebase (re-load the root) and retry.
    RootCasLost,
    /// The node could not be evicted from THIS root snapshot (path missing, a
    /// spine slot is already on-disk, the target child is already on-disk,
    /// `disk_ptr` is not a real disk location, or — the M-2a/1c guard — the live
    /// node was OVERWRITTEN since the checkpoint that registered `disk_ptr` so its
    /// `durable_stamp()` no longer matches). Skipped — never retried.
    NotEvictable,
}

/// The SHARED GENERIC overlay-eviction + read-fault capability — a subtrait of
/// [`OverlayFaulter<K, V>`] (the per-variant `OnDisk`-child loader).
///
/// `K`/`V` are the key encoding + value; `S` is the block-storage parameter the
/// variant's loader needs (it never appears in this trait's signatures — it is
/// carried so the impl can name the variant's `<V, S>` type). The three accessors
/// expose the per-attempt primitives' only variant-specific state; the two default
/// methods are the lifted primitives.
pub(crate) trait OverlayEvictable<K: KeyEncoding, V: DictionaryValue, S>:
    OverlayFaulter<K, V>
{
    /// The `arc-swap` overlay root slot (`lockfree_root`), or `None` when the
    /// lock-free overlay is not enabled.
    ///
    /// `#[allow(dead_code)]`: used only by the evict primitive (gated to the
    /// test/bench eviction surface until the production caller is wired — a later
    /// phase). The read-fault default takes its root slot as a parameter.
    #[allow(dead_code)]
    fn overlay_root_slot(&self) -> Option<&AtomicNodePtr<K, V>>;

    /// The trie's epoch manager (pinned `enter_read` for reader accounting parity;
    /// the overlay needs no EBR for correctness — reclamation is by `Arc` refcount).
    fn overlay_epoch_manager(&self) -> &EpochManager;

    /// Clone out the installed eviction coordinator (the LRU registry lives here;
    /// the variant-specific batch driver uses it to `remove_hash` an evicted path).
    /// `None` when eviction is not enabled.
    ///
    /// `#[allow(dead_code)]`: used only by the (gated) batch eviction drivers.
    #[allow(dead_code)]
    fn overlay_eviction_coordinator(&self) -> Option<Arc<EvictionCoordinator>>;

    /// Record a fault-in install-CAS attempt (won OR lost) in the variant's
    /// contention monitor. Default no-op; char overrides it to bump its
    /// `cas_retries` counter EXACTLY as its pre-lift `find_leaf_faulting` did
    /// (preserving the observable `cas_retry_count()`). Byte's pre-lift hot paths
    /// did not bump on fault-in (byte had no fault-in), so the byte impl keeps the
    /// default no-op — no behavioral delta on either side.
    #[inline]
    fn note_faultin_cas(&self) {}

    /// Evict a single OVERLAY node at `path` to the on-disk reference `disk_ptr`,
    /// by path-copying the overlay-root spine and CAS-publishing a new root whose
    /// `path` child is `Child::OnDisk(disk_ptr)`. The K-generic LIFT of char's
    /// proven `evict_overlay_node_at_path` (char `mod.rs`); behavior-identical.
    ///
    /// 1. pin the read epoch (parity with the write/read paths) and `load()` the
    ///    current published root (hazard-protected);
    /// 2. walk `path` cloning the in-memory child `Arc` per hop. Any on-disk or
    ///    missing slot along the spine (or at the target) ⇒ `NotEvictable`;
    /// 3. **the M-2a / 1c OVERWRITE GUARD** (preserved verbatim): evict ONLY IF the
    ///    freshly-walked victim's `durable_stamp() == disk_ptr.to_raw()`; a mismatch
    ///    (overwritten/stale since the registering checkpoint) ⇒ `NotEvictable`;
    /// 4. rebuild the spine bottom-up (victim's parent → `Child::OnDisk(disk_ptr)`,
    ///    each ancestor → `Child::InMem(new_child)`);
    /// 5. loser-safe `compare_exchange(&old_root, new_root)`: `Ok` ⇒ `Evicted`,
    ///    `Err` ⇒ `RootCasLost` (never clobbers a concurrent insert).
    ///
    /// **No UAF** (pure `Arc`/arc-swap; a pre-evict reader holds its snapshot Arc,
    /// freed only when the last version drops). **No lost write** (the 1c guard +
    /// the root CAS). `path` is the full edge sequence (`&[K::Unit]`) from the
    /// overlay root to the victim.
    ///
    /// `#[allow(dead_code)]`: the production force-eviction caller is a later phase;
    /// pre-flip this is exercised only by the gated batch drivers + the OE tests.
    #[allow(dead_code)]
    fn evict_overlay_node_at_path(
        &self,
        path: &[K::Unit],
        disk_ptr: SwizzledPtr,
    ) -> OverlayEvictOutcome {
        if path.is_empty() {
            // The root is never evicted via this path (it has no parent slot).
            return OverlayEvictOutcome::NotEvictable;
        }
        // The supplied pointer must encode a real on-disk location (a checkpointed
        // node's `SwizzledPtr`); a swizzled/null one is rejected.
        if disk_ptr.disk_location().is_none() {
            return OverlayEvictOutcome::NotEvictable;
        }

        let root_slot = match self.overlay_root_slot() {
            Some(r) => r,
            None => return OverlayEvictOutcome::NotEvictable,
        };

        // Pin the epoch for parity with the read/write paths (the overlay needs no
        // EBR for correctness — reclamation is by Arc refcount — but pinning keeps
        // the active-reader accounting honest under concurrent walks).
        let _epoch = self.overlay_epoch_manager().enter_read();

        // (1) Load the current published root snapshot.
        let old_root = match root_slot.load() {
            Some(r) => r,
            None => return OverlayEvictOutcome::NotEvictable,
        };

        // (2) Walk the spine top-down, collecting (node, edge) for the rebuild.
        // Preallocate to the known path length (no reallocation).
        let mut spine: Vec<(Arc<OverlayNode<K, V>>, K::Unit)> = Vec::with_capacity(path.len());
        let mut current = Arc::clone(&old_root);
        for &edge in path {
            let child = match current.find_child(edge) {
                Some(c) => c,
                None => return OverlayEvictOutcome::NotEvictable, // path missing
            };
            // We must descend through in-memory slots only; an already-on-disk
            // spine slot means a deeper node was evicted before its ancestor
            // (or this very node already is on disk) ⇒ skip.
            let child_arc = match child.as_in_mem() {
                Some(a) => Arc::clone(a),
                None => return OverlayEvictOutcome::NotEvictable,
            };
            spine.push((Arc::clone(&current), edge));
            current = child_arc;
        }
        // `current` is now the victim node (still in memory); `spine` holds its
        // ancestor chain root→parent with the edge taken at each step.

        // (2b) M-2a / 1c OVERWRITE GUARD (the round-3 lost-update fix). Evict the victim
        // to `disk_ptr` ONLY IF its durable stamp still equals `disk_ptr.to_raw()` — i.e.
        // the live node is STILL the exact content that the checkpoint serialized to
        // `disk_ptr`. A concurrent writer that overwrote this term since that checkpoint
        // path-copied the victim into a fresh `stamp == 0` node (and, by the immutable
        // path-copy invariant, every ancestor too — so we'd have walked to that fresh
        // node here). A mismatch ⇒ "overwritten/stale since durable" ⇒ `NotEvictable`
        // (skip): unswizzling it to `disk_ptr` would replace the NEWER in-memory value
        // with the OLDER on-disk image = the lost update. `current` is the FRESHLY-walked
        // victim from THIS `old_root` (not a selection-time node), and this guard lives
        // INSIDE the per-attempt fn, so every loser-safe rebase re-reads the stamp. The
        // subsequent root CAS (4) closes the "writer races AFTER this guard" window (the
        // overwrite advances the root ⇒ our CAS on `old_root` fails ⇒ rebase ⇒ re-walk
        // reaches the fresh stamp-0 node ⇒ `NotEvictable`).
        if current.durable_stamp() != disk_ptr.to_raw() {
            return OverlayEvictOutcome::NotEvictable;
        }

        // (3) Rebuild bottom-up. The deepest spine entry is the victim's PARENT;
        // its `edge` child becomes the OnDisk reference. Each shallower ancestor is
        // rebuilt InMem around the new child.
        let mut new_child: Option<Arc<OverlayNode<K, V>>> = None;
        for (ancestor, edge) in spine.into_iter().rev() {
            let rebuilt = match new_child.take() {
                // Higher ancestors: re-link the freshly rebuilt in-memory child.
                Some(c) => ancestor.with_child(edge, Child::InMem(c)),
                // The victim's parent (deepest): swap its child for the on-disk ref.
                None => ancestor.with_child(edge, Child::OnDisk(disk_ptr.clone())),
            };
            new_child = Some(Arc::new(rebuilt));
        }
        let new_root = match new_child {
            Some(r) => r,
            // Unreachable: `path` is non-empty so `spine` had ≥1 entry.
            None => return OverlayEvictOutcome::NotEvictable,
        };

        // (4) Loser-safe root CAS. Ok ⇒ published (Evicted). Err ⇒ a concurrent
        // writer advanced the root; we publish nothing (RootCasLost) and never
        // overwrite the concurrent insert.
        match root_slot.compare_exchange(&old_root, new_root) {
            Ok(_) => OverlayEvictOutcome::Evicted,
            Err(_actual) => OverlayEvictOutcome::RootCasLost,
        }
    }

    /// Find the leaf node for `key` in the overlay, FAULTING any `OnDisk` (evicted)
    /// child back in along the way. The K-generic LIFT of char's proven
    /// `find_leaf_faulting` (char `lockfree_cas.rs`); behavior-identical.
    ///
    /// Per attempt (bounded by `max_faultin_retries`): pin the epoch, `load()` the
    /// root, walk `key` top-down; `None` edge ⇒ absent (`Ok(None)`); `InMem` ⇒
    /// descend; **`OnDisk` ⇒ fault** (`fault_overlay_slot`, rebuild the spine
    /// bottom-up splicing `Child::InMem(loaded)`, then loser-safe install-CAS), then
    /// rebase to a fresh root load. On retry exhaustion ONE final read-only walk —
    /// a still-`OnDisk` slot reads absent (durable; a later read retries), never
    /// spins.
    ///
    /// **Idempotent / loser-safe:** two faulters each load their own `Arc`; exactly
    /// one install CAS wins, the loser drops + re-reads the now-`InMem` child.
    ///
    /// MAINTENANCE COUPLING: mirrors [`Self::evict_overlay_node_at_path`]; keep in
    /// lockstep (where eviction swaps InMem→OnDisk, fault-in swaps OnDisk→InMem).
    ///
    /// 🚫 NEVER call this from a read-BEFORE-WAL-append hot-insert present-hoist: a
    /// faulting read before the WAL append, racing a checkpoint/eviction that holds
    /// the buffer/arena lock, is a lock-ordering inversion (char's documented
    /// "75-minute hang"). Use the NON-faulting in-memory walk for any such hoist.
    fn find_leaf_faulting(
        &self,
        root_slot: &AtomicNodePtr<K, V>,
        key: &[K::Unit],
        max_faultin_retries: usize,
    ) -> crate::persistent_artrie_core::error::Result<Option<Arc<OverlayNode<K, V>>>> {
        // One read-only walk of `root` (no faulting): used for the empty-key leaf
        // and the post-exhaustion liveness fallback. A still-OnDisk slot reads
        // absent (durable; a later call retries) — never spins.
        fn walk_no_fault<K: KeyEncoding, V: DictionaryValue>(
            root: &Arc<OverlayNode<K, V>>,
            key: &[K::Unit],
        ) -> Option<Arc<OverlayNode<K, V>>> {
            let mut current = Arc::clone(root);
            for &edge in key {
                let child = current.find_child(edge)?;
                let child_arc = child.as_in_mem()?;
                let next = Arc::clone(child_arc);
                current = next;
            }
            if current.is_final() {
                Some(current)
            } else {
                None
            }
        }

        // +1 so we always get at least one fresh-root liveness walk even when
        // `max_faultin_retries == 0`.
        for _attempt in 0..=max_faultin_retries {
            let _epoch = self.overlay_epoch_manager().enter_read();

            let old_root = match root_slot.load() {
                Some(r) => r,
                None => return Ok(None), // empty overlay
            };

            // Walk top-down, collecting (node, edge) for a possible rebuild, until
            // we either reach the leaf (all InMem ⇒ answer directly), hit a missing
            // edge (absent), or hit an OnDisk edge (fault + CAS + rebase).
            let mut spine: Vec<(Arc<OverlayNode<K, V>>, K::Unit)> = Vec::with_capacity(key.len());
            let mut current = Arc::clone(&old_root);
            let mut faulted = false;

            let mut idx = 0usize;
            while idx < key.len() {
                let edge = key[idx];
                let child = match current.find_child(edge) {
                    Some(c) => c,
                    None => return Ok(None), // genuinely absent on this snapshot
                };
                match child {
                    Child::InMem(child_arc) => {
                        let next = Arc::clone(child_arc);
                        spine.push((Arc::clone(&current), edge));
                        current = next;
                        idx += 1;
                    }
                    Child::OnDisk(ptr) if !ptr.is_null() => {
                        // FAULT: load the OnDisk child back into memory (the
                        // per-variant loader, via the `OverlayFaulter` seam), then
                        // rebuild the spine bottom-up splicing it InMem at THIS edge.
                        // A loader error (`None`) degrades to "absent on this
                        // snapshot" — never UB; a later read retries.
                        let loaded = match self.fault_overlay_slot(ptr) {
                            Some(node) => node,
                            None => return Ok(None),
                        };

                        // The deepest rebuilt node is `current` with its `edge` child
                        // replaced by InMem(loaded); each shallower ancestor in
                        // `spine` is re-linked InMem around the rebuilt child.
                        let mut new_child =
                            Arc::new(current.with_child(edge, Child::InMem(loaded)));
                        for (ancestor, anc_edge) in spine.iter().rev() {
                            new_child =
                                Arc::new(ancestor.with_child(*anc_edge, Child::InMem(new_child)));
                        }

                        // Loser-safe install CAS against the snapshot root. Whether
                        // we won (published) or lost (a racer advanced the root,
                        // possibly already faulting this node), rebase. Record the
                        // attempt in the variant's contention monitor (char's
                        // pre-lift `find_leaf_faulting` bumped `cas_retries` on both
                        // the win and the loss arm).
                        let _ = root_slot.compare_exchange(&old_root, new_child);
                        self.note_faultin_cas();
                        faulted = true;
                        break;
                    }
                    // Null filler (never yielded as a real child) ⇒ absent.
                    Child::OnDisk(_) => return Ok(None),
                }
            }

            if faulted {
                // Re-walk from a freshly-published root on the next attempt.
                continue;
            }

            // Reached the terminal depth with an all-InMem spine: answer directly.
            return Ok(if current.is_final() {
                Some(current)
            } else {
                None
            });
        }

        // Retry budget exhausted: ONE final read-only walk of the freshest root.
        // A still-OnDisk slot reads absent (liveness-only; durable, a later read
        // faults it). Never spins.
        let final_root = match root_slot.load() {
            Some(r) => r,
            None => return Ok(None),
        };
        Ok(walk_no_fault(&final_root, key))
    }
}
