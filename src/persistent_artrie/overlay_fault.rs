//! Byte overlay fault-in primitive + [`OverlayFaulter`] impl.
//!
//! The byte twin of char's `load_overlay_node_from_disk` (`persistent_artrie_char/
//! disk_io.rs`). Byte has **no** overlay eviction and **no** other overlay
//! fault-in (its routed overlay is always fully `Child::InMem`, since the
//! reestablish folds publish in-memory and nothing serializes overlay children
//! back into the live in-memory tree). This module exists so the overlay-backed
//! `DictionaryNode` (`node_impl::NodeInner::Overlay`) can resolve a
//! `Child::OnDisk` overlay child **if one is ever encountered**, rather than
//! silently dropping it (which would lose terms from a transducer / fuzzy walk) —
//! keeping byte symmetric with char and future-proof against a later byte overlay
//! eviction path.
//!
//! ZERO new `unsafe`: this reuses the existing safe byte v2 node decoder
//! (`serialization::v2::deserialize_node_v2` + `read_node_value`) through a safe
//! `&self` boundary; the conversion is pure node copies + `Arc` allocation. The
//! returned node's children stay `Child::OnDisk` (single-level / lazy — the overlay
//! fault granularity), exactly as char's `inner_to_overlay` keeps them.

use std::sync::Arc;

use crate::persistent_artrie_core::key_encoding::ByteKey;
use crate::persistent_artrie_core::overlay::evict::OverlayEvictable;
use crate::persistent_artrie_core::overlay::{AtomicNodePtr, Child, OverlayFaulter, OverlayNode};
use crate::value::DictionaryValue;

use super::arena_manager::ArenaSlot;
use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;
use super::error::{PersistentARTrieError, Result};
use super::serialization;
use super::serialization::v2::DeserializationContext;
use super::swizzled_ptr::SwizzledPtr;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Load an `OnDisk` overlay child back into an immutable overlay node
    /// (`Arc<OverlayNode<ByteKey, V>>`) — the byte **fault-in load+deserialize
    /// primitive**. Reuses the production/recovery-tested byte v2 single-node
    /// decoder (`deserialize_node_v2` + `read_node_value`); the decoded node's
    /// children are kept `Child::OnDisk` (the fault is single-level / lazy —
    /// exactly the overlay granularity, matching char's `load_overlay_node_from_disk`
    /// → `inner_to_overlay`).
    ///
    /// The returned node's finality / value / child-set equal the durable image's,
    /// so a faulted node can never manufacture or drop a term. Fault-in writes
    /// nothing to disk and advances no watermark.
    ///
    /// ZERO new `unsafe` — see the module doc.
    pub(crate) fn load_overlay_node_from_disk(
        &self,
        disk_ptr: &SwizzledPtr,
    ) -> Result<Arc<OverlayNode<ByteKey, V>>> {
        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for overlay fault-in load")
        })?;

        let disk_loc = disk_ptr
            .disk_location()
            .ok_or_else(|| PersistentARTrieError::internal("Node pointer is swizzled or null"))?;
        let arena_id = disk_loc
            .block_id
            .checked_sub(1)
            .ok_or_else(|| PersistentARTrieError::internal("Invalid block_id 0 for arena node"))?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);

        let am = arena_manager.read();
        let node_data = am.read(slot)?;

        // Deserialize the byte node (v2, relative-offset aware).
        let ctx = DeserializationContext::new(slot);
        let node = serialization::v2::deserialize_node_v2(node_data, &ctx).map_err(|e| {
            PersistentARTrieError::corrupted(format!(
                "Failed to deserialize overlay ART node: {:?}",
                e
            ))
        })?;
        let is_final = node.header().is_final();
        // Capture the value blob BEFORE dropping the arena lock (it borrows
        // `node_data`, which borrows `am`).
        let value_bytes = serialization::v2::read_node_value(node_data);
        // Collect child pointers (non-null) BEFORE dropping the arena lock.
        let child_ptrs: Vec<(u8, SwizzledPtr)> = node
            .iter_children()
            .filter(|(_, ptr)| !ptr.is_null())
            .map(|(key, ptr)| (key, ptr.clone()))
            .collect();
        // CX/#43 (4A): capture the path-compression prefix BEFORE dropping the arena lock (`node`
        // borrows `node_data` borrows `am`). The prior code built `OverlayNode::new()` and DROPPED
        // the prefix, so a compressed node lost its prefix on fault-in (silent key-data loss). No-op
        // for `prefix_len == 0` (every current production image), so #39 eviction / reopen unchanged.
        let prefix_len = node.header().prefix_len as usize;
        let prefix_bytes: Vec<u8> = if prefix_len > 0 {
            node.prefix().bytes[..prefix_len].to_vec()
        } else {
            Vec::new()
        };
        drop(am);

        // Deserialize the value blob into `V` (propagate errors — data-loss path).
        let value: Option<V> = match value_bytes {
            Some(vb) => Some(
                crate::serialization::bincode_compat::deserialize(&vb).map_err(|e| {
                    PersistentARTrieError::corrupted(format!("deserialize overlay value: {e}"))
                })?,
            ),
            None => None,
        };

        // Build the REAL (terminus) node: finality + value from the durable image, children kept
        // `Child::OnDisk` (lazy). It carries NO prefix (prefix_len = 0).
        let mut real = OverlayNode::<ByteKey, V>::new();
        if is_final {
            real = real.as_final();
        }
        if let Some(v) = value {
            real = real.with_value(v);
        }
        for (edge, ptr) in child_ptrs {
            real = real.with_child(edge, Child::OnDisk(ptr));
        }

        // CX/#43 (4A): EXPAND `prefix_len = p` into a chain of `p` single-child prefix_len=0
        // intermediates ABOVE `real` — the uncompressed shape the write path builds, since the
        // overlay traversal is prefix-UNAWARE. The prefix bytes are the intermediates' child-edges
        // (parent reaches intermediate_0 by the dense node's incoming edge; intermediate_i reaches
        // intermediate_{i+1} by prefix[i]; the last reaches `real` by prefix[p-1]). p == 0 ⇒ no-op
        // (real only — the prior behavior for every uncompressed image). Mirrors char `inner_to_overlay`.
        let mut cur = real;
        for i in (0..prefix_len).rev() {
            cur = OverlayNode::<ByteKey, V>::new()
                .with_child(prefix_bytes[i], Child::InMem(Arc::new(cur)));
            debug_assert!(
                cur.prefix_len() == 0 && !cur.is_final() && cur.num_children() == 1,
                "CX #43 (4A): an expanded prefix intermediate must be prefix_len=0, non-final, single-child"
            );
        }
        // CX/#43 (#6 eviction-ON): stamp the TOP-of-span node (`cur` = the head of the expanded
        // chain, or `real` itself when p==0) with `disk_ptr` IFF this was a COMPRESSED node
        // (`prefix_len > 0`), so a fault-then-evict re-installs `Child::OnDisk` for the WHOLE
        // re-expanded span (the evictor walks to this top node + checks `durable_stamp == disk_ptr`).
        // NO-OP for `prefix_len == 0` (every current production image), so the production fault path
        // + #39 eviction stay byte-for-byte unchanged. The byte twin of char's `disk_io.rs` stamp.
        if prefix_len > 0 {
            cur.set_durable_stamp(disk_ptr.to_raw());
        }
        Ok(Arc::new(cur))
    }
}

/// Byte impl of the SAFE overlay fault-in capability (resolves `Child::OnDisk`
/// overlay children during an overlay-backed `DictionaryNode` walk). Delegates to
/// the inherent [`PersistentARTrie::load_overlay_node_from_disk`]; an I/O / decode
/// error degrades to `None` (no child) — never UB, never a fabricated term.
impl<V: DictionaryValue, S: BlockStorage> OverlayFaulter<ByteKey, V> for PersistentARTrie<V, S> {
    #[inline]
    fn fault_overlay_slot(&self, slot: &SwizzledPtr) -> Option<Arc<OverlayNode<ByteKey, V>>> {
        self.load_overlay_node_from_disk(slot).ok()
    }
}

/// Byte impl of the SHARED GENERIC [`OverlayEvictable`] (Phase 5) — the per-attempt
/// overlay evict + read-fault primitives, K-generic over `OverlayNode<ByteKey, V>`.
/// Supplies the three variant-specific accessors (`lockfree_root` / `epoch_manager` /
/// `eviction_coordinator`); the primitives themselves are the trait defaults
/// (`find_leaf_faulting` for the byte read/counter fault-in, `evict_overlay_node_at_path`
/// for the byte evict driver). The `OverlayFaulter<ByteKey, V>` super-trait requirement
/// is satisfied by the impl above (the `load_overlay_node_from_disk` loader — byte's
/// arena+`deserialize_node_v2` body, NOT unified with char's buffer-manager loader).
///
/// `note_faultin_cas` keeps the trait default (no-op): byte's pre-Phase-5 hot paths
/// had NO fault-in, so they never bumped `cas_retries` on a fault — keeping the no-op
/// preserves byte's observable `cas_retry_count()` (no behavioral delta). The byte
/// write-path fault-in (the build-path arms) splices `Child::InMem` into the fresh
/// path-copy and lets the writer's existing single root CAS arbitrate, bumping
/// `cas_retries` exactly where it already did (on a lost root CAS) — unchanged.
impl<V: DictionaryValue, S: BlockStorage> OverlayEvictable<ByteKey, V, S>
    for PersistentARTrie<V, S>
{
    #[inline]
    fn overlay_root_slot(&self) -> Option<&AtomicNodePtr<ByteKey, V>> {
        self.lockfree_root.as_ref()
    }

    #[inline]
    fn overlay_epoch_manager(&self) -> &crate::persistent_artrie_core::concurrency::EpochManager {
        &self.epoch_manager
    }

    #[inline]
    fn overlay_eviction_coordinator(
        &self,
    ) -> Option<Arc<crate::persistent_artrie::eviction::EvictionCoordinator>> {
        self.eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
            .map(Arc::clone)
    }
}

// ============================================================================
// Phase 6 byte bench/test eviction surface — the byte twins of char's
// `bench_enable_eviction` / `bench_immutable_checkpoint_with_eviction` /
// `evictable_node_count` + the `evict_overlay_nodes` batch driver. `#[cfg]`-gated to
// the test/bench surface; the production force-eviction wiring is a later phase.
// ============================================================================

/// Reclaim a batch of COLD OVERLAY nodes (the byte twin of char's `evict_overlay_nodes`,
/// Phase 6). Evicts LEAF-FIRST (descending depth) so a node is evicted before any
/// ancestor — keeping each victim's parent spine in memory at eviction time (a later
/// shallower candidate whose spine now passes through an already-on-disk slot is reported
/// `NotEvictable` and skipped). Each victim gets up to `max_rebase_retries` root-CAS
/// attempts via the lifted K-generic primitive
/// [`OverlayEvictable::evict_overlay_node_at_path`] (the 1c guard lives in it): a
/// `RootCasLost` (a concurrent writer won) rebases + retries; on exhaustion the victim is
/// SKIPPED (a missed eviction is liveness-only — loser-safe).
///
/// Returns `(evicted, bytes_freed)` (nominal ~256 B/node estimate; the peak-RSS pass is
/// the physical witness). Registry plumbing (`Vec<u8>` paths, byte `remove_hash`) is
/// variant-specific. Takes NO lock and uses NO `unsafe`.
///
/// Phase 7.4: UN-GATED to production (the byte checkpoint-tail resident-budget eviction
/// calls it). The `bench_*` enabler impl below stays gated; this driver does not.
pub(crate) fn evict_overlay_nodes<V: DictionaryValue, S: BlockStorage>(
    trie: &PersistentARTrie<V, S>,
    mut nodes: Vec<(u64, Vec<u8>, SwizzledPtr)>,
    max_rebase_retries: usize,
) -> (usize, usize) {
    use crate::persistent_artrie::eviction::lru_tracker::LruRegistry;
    use crate::persistent_artrie_core::overlay::evict::{OverlayEvictOutcome, OverlayEvictable};

    // LEAF-FIRST: sort by DESCENDING path length (depth).
    nodes.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    let mut evicted = 0usize;
    let mut bytes_freed = 0usize;
    for (_path_hash, path, disk_ptr) in nodes {
        // Byte overlay keys are `u8`; the registry path IS already `&[u8]` — no
        // conversion needed (unlike char's `Vec<char>` → `u32`).
        let mut attempt = 0;
        loop {
            match trie.evict_overlay_node_at_path(&path, disk_ptr.clone()) {
                OverlayEvictOutcome::Evicted => {
                    evicted += 1;
                    bytes_freed += 256;
                    // Drop the LRU entry so a later (re)insert of this cold path starts
                    // fresh (parity with char). Byte uses the `u8`-path hash.
                    if let Some(coordinator) = trie.overlay_eviction_coordinator() {
                        coordinator
                            .lru_registry()
                            .remove_hash(LruRegistry::path_hash(&path));
                    }
                    break;
                }
                OverlayEvictOutcome::RootCasLost => {
                    attempt += 1;
                    if attempt > max_rebase_retries {
                        break; // exhausted → SKIP (liveness-only miss)
                    }
                    // else: rebase (re-load the root) on the next iteration.
                }
                OverlayEvictOutcome::NotEvictable => break, // skip; never retried
            }
        }
    }
    (evicted, bytes_freed)
}

#[cfg(any(test, feature = "bench-internals"))]
impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// **REVERSIBLE BENCH/TEST ENABLER — EVICTION-ON** (byte twin of char's
    /// `bench_enable_eviction`, Phase 6). Install an [`EvictionCoordinator`] directly on
    /// this bare `PersistentARTrie` (sharing THIS trie's `epoch_manager`) so the in-crate
    /// byte OE tests can run eviction-ON checkpoints + drive the overlay evictor. The
    /// reclaim callback is a no-op `(0, 0)` (the test drives reclamation synchronously via
    /// `evict_overlay_nodes`); the bench measures the CHECKPOINT registration path.
    pub(crate) fn bench_enable_eviction(
        &self,
        config: crate::persistent_artrie::eviction::EvictionConfig,
    ) -> Result<()> {
        config
            .validate()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        if self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .is_some()
        {
            return Err(PersistentARTrieError::internal("Eviction already enabled"));
        }

        // Share THIS trie's epoch manager with the coordinator (Phase 6 epoch-share,
        // parity with char's `bench_enable_eviction`).
        let epoch_manager = Arc::clone(&self.epoch_manager);
        let coordinator = crate::persistent_artrie::eviction::EvictionCoordinator::new(
            config.clone(),
            epoch_manager,
        );

        // No-op reclaim callback: the byte OE tests reclaim synchronously via
        // `evict_overlay_nodes`. The bench/test only needs the registry-publication
        // CHECKPOINT path active.
        coordinator
            .start(|_nodes_to_evict| (0usize, 0usize))
            .map_err(|e| PersistentARTrieError::internal(&e))?;
        coordinator
            .start_memory_monitor()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        *self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned") = Some(coordinator);
        Ok(())
    }

    /// **REVERSIBLE BENCH/TEST CHECKPOINT — EVICTION-ON** (byte twin of char's
    /// `bench_immutable_checkpoint_with_eviction`, Phase 6). Capture the IMMUTABLE overlay
    /// + publish RETAINING the WAL with eviction-registry publication — directly via the
    /// overlay capture/publish seams (NOT the production `checkpoint()` route-split, which
    /// is INERT pre-flip). This is what populates + publishes the byte disk-location
    /// registry the OE tests then evict from (the M-2a stamps are written here).
    pub(crate) fn bench_immutable_checkpoint_with_eviction(&self) -> Result<()> {
        let snapshot = self.capture_overlay_snapshot()?;
        self.publish_overlay_snapshot_retaining_with_eviction(snapshot)
    }

    /// Number of BYTE nodes registered as evictable in the disk-location registry
    /// published at the last `bench_immutable_checkpoint_with_eviction` (byte twin of
    /// char's `evictable_node_count`, Phase 6). `None` when eviction is disabled;
    /// `Some(0)` before the first checkpoint.
    pub(crate) fn evictable_node_count(&self) -> Option<usize> {
        self.eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
            .map(|c| c.disk_registry_len())
    }
}
