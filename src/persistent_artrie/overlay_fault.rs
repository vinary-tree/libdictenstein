//! Byte overlay fault-in primitive + [`OverlayFaulter`] impl.
//!
//! The byte twin of char's `load_overlay_node_from_disk`
//! (`persistent_artrie::char::disk_io`). Byte has **no** overlay eviction and **no** other overlay
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

use crate::persistent_artrie::core::eviction::{DurableRecordRef, DurableRegistryRecord};
use crate::persistent_artrie::core::key_encoding::ByteKey;
use crate::persistent_artrie::core::overlay::evict::OverlayEvictable;
use crate::persistent_artrie::core::overlay::{AtomicNodePtr, Child, OverlayFaulter, OverlayNode};
use crate::value::DictionaryValue;

use super::arena_manager::ArenaSlot;
use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;
use super::error::{PersistentARTrieError, Result};
use super::serialization;
use super::serialization::v2::DeserializationContext;
use super::swizzled_ptr::SwizzledPtr;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Read one exact durable byte-node record without deserializing `V`.
    ///
    /// The arena lock is held only while copying bounded structural metadata.
    /// Child values remain opaque, making this suitable for iterative registry
    /// reconstruction after a restart or an unavailable carry source.
    pub(crate) fn read_byte_registry_record(
        &self,
        record_ref: DurableRecordRef,
    ) -> Result<DurableRegistryRecord<u8>> {
        let arena_id = record_ref.address.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::corrupted(
                "byte registry metadata record uses reserved block zero",
            )
        })?;
        let slot = ArenaSlot::new(arena_id, record_ref.address.slot_id);
        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for registry metadata read")
        })?;
        let arena = arena_manager.read();
        let record_bytes = arena.read(slot)?;
        let metadata = serialization::v2::decode_node_metadata(
            record_bytes,
            &DeserializationContext::new(slot),
            record_ref.expected_type,
        )?;
        drop(arena);

        let mut children = Vec::new();
        children
            .try_reserve_exact(metadata.children.len())
            .map_err(|error| {
                PersistentARTrieError::allocation_failed(
                    "byte registry metadata child references",
                    metadata.children.len(),
                    error,
                )
            })?;
        for (edge, pointer) in metadata.children {
            children.push((edge, DurableRecordRef::from_typed_pointer(&pointer)?));
        }

        Ok(DurableRegistryRecord {
            canonical_ptr: record_ref.address.canonical_pointer(metadata.node_type)?,
            address: record_ref.address,
            node_type: metadata.node_type,
            serialized_bytes: metadata.serialized_bytes,
            prefix: metadata.prefix,
            children,
        })
    }

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
        let value_bytes = serialization::v2::try_read_node_value(node_data)?;
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
        // The top-of-span node is an exact in-memory representation of `disk_ptr`: for an
        // uncompressed record it is `real`, and for a compressed record it is the head of the
        // lossless expanded chain. Stamp both cases before the node can be published. The stamp is
        // necessary for fault -> re-evict progress, but is not sufficient authority: exact
        // eviction additionally revalidates the root's registry generation, path, disk address,
        // and residency under the coordinator lifecycle transaction. Detached loads may therefore
        // carry the same truthful content stamp without changing published residency. Every
        // structural path copy still clears its stamp, so a modified ancestor cannot be evicted to
        // this older image.
        cur.set_durable_stamp(disk_ptr.to_raw());
        Ok(Arc::new(cur))
    }
}

/// Byte impl of the SAFE overlay fault-in capability (resolves `Child::OnDisk`
/// overlay children during an overlay-backed `DictionaryNode` walk). Delegates to
/// the inherent `PersistentARTrie::load_overlay_node_from_disk` while preserving
/// its exact I/O / decode error for durable callers.
impl<V: DictionaryValue, S: BlockStorage> OverlayFaulter<ByteKey, V> for PersistentARTrie<V, S> {
    #[inline]
    fn try_fault_overlay_slot(
        &self,
        slot: &SwizzledPtr,
    ) -> crate::persistent_artrie::core::error::Result<Arc<OverlayNode<ByteKey, V>>> {
        self.load_overlay_node_from_disk(slot)
    }
}

/// Byte implementation of the shared generic [`OverlayEvictable`] compact
/// eviction and exact fault-in machines over `OverlayNode<ByteKey, V>`.
/// Supplies the three variant-specific accessors (`lockfree_root` / `epoch_manager` /
/// `eviction_coordinator`); the machines themselves are trait defaults. The
/// `OverlayFaulter<ByteKey, V>` super-trait requirement
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
    fn overlay_epoch_manager(&self) -> &crate::persistent_artrie::core::concurrency::EpochManager {
        &self.epoch_manager
    }

    #[inline]
    fn overlay_eviction_coordinator(
        &self,
    ) -> Option<Arc<crate::persistent_artrie::eviction::EvictionCoordinator>> {
        self.eviction_coordinator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(Arc::clone)
    }

    #[inline]
    fn prepare_overlay_eviction_commit(
        &self,
        coordinator: &crate::persistent_artrie::eviction::EvictionCoordinator,
        root_revision: &crate::persistent_artrie::core::overlay::RootRevision<ByteKey, V>,
        batch: &crate::persistent_artrie::core::eviction::CompactEvictionBatch<u8>,
        successful: &mut [usize],
    ) -> Option<crate::persistent_artrie::core::eviction::PreparedPackedResidency> {
        coordinator.prepare_byte_eviction_commit(root_revision, batch, successful)
    }

    #[inline]
    fn commit_overlay_eviction(
        &self,
        coordinator: &crate::persistent_artrie::eviction::EvictionCoordinator,
        root: &AtomicNodePtr<ByteKey, V>,
        root_transition: crate::persistent_artrie::core::overlay::PreparedBoundRootTransition<
            ByteKey,
            V,
        >,
    ) -> crate::persistent_artrie::core::eviction::ExactEvictionOutcome {
        coordinator.commit_byte_eviction_transaction(root, root_transition)
    }

    #[inline]
    fn prepare_overlay_fault_commit(
        &self,
        coordinator: &crate::persistent_artrie::eviction::EvictionCoordinator,
        root_revision: &crate::persistent_artrie::core::overlay::RootRevision<ByteKey, V>,
        path: &[u8],
        disk_ptr: &SwizzledPtr,
    ) -> Option<crate::persistent_artrie::core::eviction::PreparedPackedResidency> {
        coordinator.prepare_byte_fault_commit(root_revision, path, disk_ptr)
    }

    #[inline]
    fn commit_overlay_fault(
        &self,
        coordinator: &crate::persistent_artrie::eviction::EvictionCoordinator,
        root: &AtomicNodePtr<ByteKey, V>,
        root_transition: crate::persistent_artrie::core::overlay::PreparedBoundRootTransition<
            ByteKey,
            V,
        >,
    ) -> crate::persistent_artrie::core::eviction::ExactFaultOutcome {
        coordinator.commit_byte_fault_transaction(root, root_transition)
    }
}

// ============================================================================
// Shared production byte eviction driver, followed by the byte twins of char's
// gated `bench_enable_eviction` / `bench_immutable_checkpoint_with_eviction` /
// `evictable_node_count` test and benchmark controls.
// ============================================================================

pub(crate) fn evict_overlay_compact_batch<V: DictionaryValue, S: BlockStorage>(
    trie: &PersistentARTrie<V, S>,
    batch: crate::persistent_artrie::core::eviction::CompactEvictionBatch<u8>,
    max_rebase_retries: usize,
) -> (usize, usize) {
    trie.evict_overlay_batch(batch, max_rebase_retries)
}

#[cfg(any(test, feature = "bench-internals"))]
impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// **REVERSIBLE BENCH/TEST ENABLER — EVICTION-ON** (byte twin of char's
    /// `bench_enable_eviction`, Phase 6). Install an [`EvictionCoordinator`] directly on
    /// this bare `PersistentARTrie` (sharing THIS trie's `epoch_manager`) so the in-crate
    /// byte OE tests can run eviction-ON checkpoints + drive the overlay evictor. The
    /// reclaim callback is a compact no-op `(0, 0)`; tests drive reclamation
    /// synchronously while the bench measures checkpoint registration.
    #[allow(dead_code)]
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
        let coordinator =
            crate::persistent_artrie::eviction::EvictionCoordinator::new_with_publication_gate(
                config.clone(),
                epoch_manager,
                Arc::clone(&self.registry_publication_gate),
            );

        // No-op compact callback: tests reclaim synchronously. The bench/test
        // only needs the registry-publication checkpoint path active.
        coordinator
            .start_compact(|_batch| (0usize, 0usize))
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
    ///   overlay capture/publish seams (NOT the production `checkpoint()` route-split). This
    ///   is what populates + publishes the byte disk-location registry the OE tests then
    ///   evict from (the M-2a stamps are written here).
    #[allow(dead_code)]
    pub(crate) fn bench_immutable_checkpoint_with_eviction(&self) -> Result<()> {
        let snapshot = self.capture_overlay_snapshot()?;
        self.publish_overlay_snapshot_retaining_with_eviction(snapshot)
    }

    /// Exact number of resident BYTE-node occurrences represented by the
    /// currently published eviction topology. Exact eviction and fault commits
    /// update it immediately; nonresident structural records are excluded.
    /// `None` when eviction is disabled; `Some(0)` before the first checkpoint.
    #[allow(dead_code)]
    pub(crate) fn evictable_node_count(&self) -> Option<usize> {
        let coordinator = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
            .cloned()?;
        let resident = self
            .lockfree_root
            .as_ref()
            .and_then(|root| coordinator.root_resident_totals(root))
            .map_or(0, |totals| totals.0);
        Some(resident)
    }

    /// Number of durable BYTE-node occurrences in the published topology,
    /// including nonresident records retained for exact fault-in.
    #[allow(dead_code)]
    pub(crate) fn registered_node_count(&self) -> Option<usize> {
        self.eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
            .map(|coordinator| coordinator.disk_registry_len())
    }
}
