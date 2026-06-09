//! On-disk serialization helpers for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (Phase-5 byte sub-module). The surviving
//! single-node serialize primitives the overlay checkpoint path drives:
//!
//! - `serialize_bucket_to_disk` — allocates an arena slot for a bucket
//! - `serialize_node_to_disk_with_value_len` — v2 single-node serialization with
//!   relative-offset (and sequential-sibling) encoding + optional value blob,
//!   returning the on-disk byte length for the eviction registry
//!
//! **L3.3c:** the owned-tree serializers (`persist_to_disk`, the recursive
//! `serialize_child_to_disk*`, the `serialize_node_to_disk` no-value wrappers) and the
//! dirty-tracking cache were deleted with the owned tree.

use crate::value::DictionaryValue;

use super::arena_manager::ArenaSlot;
use super::block_storage::BlockStorage;
use super::bucket::StringBucket;
use super::dict_impl::PersistentARTrie;
use super::error::{PersistentARTrieError, Result};
use super::nodes::Node;
use super::serialization::{self, v2::SerializationContext};
use super::swizzled_ptr::{NodeType, SwizzledPtr};

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Serialize a bucket to disk and return a SwizzledPtr to its location.
    ///
    /// Allocates a new arena slot via `ArenaManager`, writes the bucket bytes,
    /// and returns a SwizzledPtr pointing to the slot.
    pub(super) fn serialize_bucket_to_disk(&self, bucket: &StringBucket) -> Result<SwizzledPtr> {
        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk serialization")
        })?;

        let bucket_bytes = bucket.as_bytes();

        let slot = arena_manager.write().allocate(bucket_bytes)?;

        Ok(SwizzledPtr::from_arena_slot(slot, NodeType::Bucket))
    }

    // L3.3c: removed — `serialize_node_to_disk` (no-value wrapper) +
    // `serialize_node_to_disk_with_value` (length-dropping wrapper) were the owned
    // serialize entry points. The overlay checkpoint path calls the length-returning
    // `serialize_node_to_disk_with_value_len` below DIRECTLY.

    /// Serialize a node to disk, optionally appending a value blob (M4a / D-VAL), and
    /// ALSO return the on-disk serialized byte length of the node record — the byte twin
    /// of what char's `serialize_one_char_node_to_disk` measures as `data.len()` for its
    /// eviction registry `size_bytes`. `value_bytes = None` is byte-identical to the
    /// value-less path; `Some` sets the `HAS_VALUE` flag + appends the value (see
    /// [`serialization::v2::append_node_value`]), and its size is folded into the
    /// arena-slot estimate so the `slot == parent_slot` invariant below still holds.
    /// Phase 6: the overlay registration path (`serialize_overlay_node_to_disk`) uses the
    /// length so byte's registry entries carry the same on-disk-equivalent size char's do.
    pub(super) fn serialize_node_to_disk_with_value_len(
        &self,
        node: &Node,
        value_bytes: Option<&[u8]>,
    ) -> Result<(SwizzledPtr, usize)> {
        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk serialization")
        })?;

        let mut am = arena_manager.write();

        let temp_slot = am.next_slot();
        let temp_ctx = SerializationContext::new(temp_slot);
        let value_overhead = value_bytes.map_or(0, |vb| 4 + vb.len());
        let estimated_size =
            serialization::v2::estimate_serialized_size_v2(node, &temp_ctx) + value_overhead;

        let parent_slot = if am.can_fit(estimated_size) {
            am.next_slot()
        } else {
            ArenaSlot::new(am.arena_count() as u32, 0)
        };

        let ctx = if let Some(first_child) =
            Self::check_sequential_children(node, parent_slot.arena_id)
        {
            SerializationContext::sequential(parent_slot, first_child)
        } else {
            SerializationContext::new(parent_slot)
        };

        let node_bytes = serialization::v2::append_node_value(
            serialization::v2::serialize_node_v2(node, &ctx)?,
            value_bytes,
        );
        let data_len = node_bytes.len();

        let slot = am.allocate(&node_bytes)?;

        debug_assert_eq!(
            slot, parent_slot,
            "Slot mismatch: predicted {:?}, got {:?}",
            parent_slot, slot
        );

        let node_type = match node {
            Node::N4(_) => NodeType::Node4,
            Node::N16(_) => NodeType::Node16,
            Node::N48(_) => NodeType::Node48,
            Node::N256(_) => NodeType::Node256,
        };

        Ok((SwizzledPtr::from_arena_slot(slot, node_type), data_len))
    }

    // L3.3c: removed — `persist_to_disk` (owned-tree disk serializer) +
    // `serialize_child_to_disk` / `serialize_child_to_disk_with_path` (recursive owned
    // `ChildNode` serializers) walked the deleted owned `self.root` / `TrieRoot` /
    // `ChildNode` representation and used the deleted dirty-tracking cache. The overlay
    // checkpoint path (`overlay_checkpoint.rs`) serializes the immutable overlay
    // directly via `serialize_bucket_to_disk` + `serialize_node_to_disk_with_value_len`.
}
