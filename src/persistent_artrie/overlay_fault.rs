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
use crate::persistent_artrie_core::overlay::{Child, OverlayFaulter, OverlayNode};
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

        // Build the overlay node: prefix is always empty for the overlay
        // representation (the overlay is un-path-compressed), finality + value from
        // the durable image, children kept `Child::OnDisk` (lazy).
        let mut overlay = OverlayNode::<ByteKey, V>::new();
        if is_final {
            overlay = overlay.as_final();
        }
        if let Some(v) = value {
            overlay = overlay.with_value(v);
        }
        for (edge, ptr) in child_ptrs {
            overlay = overlay.with_child(edge, Child::OnDisk(ptr));
        }
        Ok(Arc::new(overlay))
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
