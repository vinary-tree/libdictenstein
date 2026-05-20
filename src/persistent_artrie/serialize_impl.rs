//! On-disk serialization helpers for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~1155-1501, ~347 LOC) as
//! the twenty-first Phase-5 byte sub-module. Methods covered:
//!
//! - `serialize_bucket_to_disk` — allocates an arena slot for a bucket
//! - `serialize_node_to_disk` — v2 serialization with relative-offset
//!   encoding (and sequential-sibling encoding when applicable)
//! - `persist_to_disk` — walks the trie, serializes root + children,
//!   writes the root descriptor, flushes arenas + buffer pool, syncs
//!   the disk manager
//! - `serialize_child_to_disk` / `serialize_child_to_disk_with_path` —
//!   per-`ChildNode` serialization with selective dirty-subtree
//!   traversal
//!
//! Dirty-tracking helpers (`cache_disk_location`,
//! `path_needs_persistence`, `get_cached_disk_location`,
//! `clear_dirty_tracking_state`, `record_dirty_path`,
//! `propagate_dirty_to_root`) are widened to `pub(super)` in
//! `dict_impl.rs` so this module can call them.

use std::sync::atomic::Ordering as AtomicOrdering;

use crate::value::DictionaryValue;

use super::arena_manager::ArenaSlot;
use super::block_storage::BlockStorage;
use super::bucket::StringBucket;
use super::dict_impl::{PersistentARTrie, TrieRoot, ROOT_TYPE_ART_NODE, ROOT_TYPE_BUCKET};
use super::error::{PersistentARTrieError, Result};
use super::nodes::Node;
use super::serialization::{self, v2::SerializationContext};
use super::swizzled_ptr::{NodeType, SwizzledPtr};
use super::transitions::ChildNode;

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

    /// Serialize an ART node to disk and return a SwizzledPtr to its location.
    ///
    /// Uses v2 serialization with relative offset encoding for child pointers
    /// (and the sequential-sibling encoding when applicable).
    pub(super) fn serialize_node_to_disk(&self, node: &Node) -> Result<SwizzledPtr> {
        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk serialization")
        })?;

        let mut am = arena_manager.write();

        let temp_slot = am.next_slot();
        let temp_ctx = SerializationContext::new(temp_slot);
        let estimated_size = serialization::v2::estimate_serialized_size_v2(node, &temp_ctx);

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

        let node_bytes = serialization::v2::serialize_node_v2(node, &ctx)?;

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

        Ok(SwizzledPtr::from_arena_slot(slot, node_type))
    }

    /// Persist all modified nodes in the trie to disk.
    ///
    /// This method walks through the trie structure and serializes all
    /// in-memory nodes to disk, then updates the file header with the
    /// root pointer.
    pub fn persist_to_disk(&mut self) -> Result<()> {
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for disk serialization")
        })?;

        let (root_type, root_ptr, is_final, term_count) = match &self.root {
            TrieRoot::Bucket(bucket) => {
                let ptr = self.serialize_bucket_to_disk(bucket)?;
                (
                    ROOT_TYPE_BUCKET,
                    ptr.to_raw(),
                    false,
                    self.term_count.load(AtomicOrdering::Acquire),
                )
            }
            TrieRoot::ArtNode {
                node,
                children,
                is_final,
                value,
            } => {
                let mut child_ptrs: Vec<(u8, u64)> = Vec::with_capacity(children.len());
                for (edge, child) in children {
                    let child_path = [*edge];
                    let ptr = self.serialize_child_to_disk_with_path(child, &child_path)?;
                    child_ptrs.push((*edge, ptr.to_raw()));
                }

                let mut node_copy = node.clone();
                for (edge, ptr_raw) in &child_ptrs {
                    if let Some(child_ptr) = node_copy.find_child_mut(*edge) {
                        *child_ptr = SwizzledPtr::from_raw(*ptr_raw);
                    }
                }

                let node_ptr = self.serialize_node_to_disk(&node_copy)?;

                let _ = value;

                (
                    ROOT_TYPE_ART_NODE,
                    node_ptr.to_raw(),
                    *is_final,
                    self.term_count.load(AtomicOrdering::Acquire),
                )
            }
        };

        if let Some(ref arena_manager) = self.arena_manager {
            let stats = arena_manager.write().flush_dirty_slots()?;
            if stats.partial_writes > 0 {
                log::debug!(
                    "Incremental flush: {} full arenas, {} partial, {} slots, {} bytes written, {} bytes saved",
                    stats.full_arena_writes,
                    stats.partial_writes,
                    stats.slots_written,
                    stats.bytes_written,
                    stats.bytes_saved
                );
            }
        }

        let arena_count: u32 = if let Some(ref arena_manager) = self.arena_manager {
            arena_manager.read().arena_count() as u32
        } else {
            0
        };

        let mut descriptor = [0u8; 18];
        descriptor[0] = root_type;
        descriptor[1] = if is_final { 1 } else { 0 };
        descriptor[2..6].copy_from_slice(&(term_count as u32).to_le_bytes());
        descriptor[6..10].copy_from_slice(&arena_count.to_le_bytes());
        descriptor[10..18].copy_from_slice(&root_ptr.to_le_bytes());

        const DESCRIPTOR_OFFSET: usize = 64;
        let bm = buffer_manager.write();
        let dm = bm.storage();
        dm.write_bytes(0, DESCRIPTOR_OFFSET, &descriptor)?;

        let root_descriptor_ptr =
            SwizzledPtr::on_disk(0, DESCRIPTOR_OFFSET as u32, NodeType::Bucket);
        dm.set_root_ptr(root_descriptor_ptr.to_raw())?;
        dm.set_entry_count(term_count as u64)?;

        bm.flush_all()?;
        dm.sync()?;

        self.dirty.store(false, AtomicOrdering::Release);

        drop(bm);
        self.clear_dirty_tracking_state();

        Ok(())
    }

    /// Serialize a ChildNode to disk and return its SwizzledPtr.
    ///
    /// This is a convenience wrapper around `serialize_child_to_disk_with_path`
    /// that uses an empty path (legacy behavior).
    #[allow(dead_code)]
    pub(super) fn serialize_child_to_disk(&self, child: &ChildNode) -> Result<SwizzledPtr> {
        self.serialize_child_to_disk_with_path(child, &[])
    }

    /// Serialize a ChildNode to disk with path tracking for selective persistence.
    pub(super) fn serialize_child_to_disk_with_path(
        &self,
        child: &ChildNode,
        path: &[u8],
    ) -> Result<SwizzledPtr> {
        match child {
            ChildNode::Bucket(bucket) => {
                let ptr = self.serialize_bucket_to_disk(bucket)?;
                self.cache_disk_location(path, ptr.clone());
                Ok(ptr)
            }
            ChildNode::ArtNode {
                node,
                is_final,
                value,
                children,
            } => {
                let needs_persist =
                    node.header().needs_persistence() || self.path_needs_persistence(path);

                if !needs_persist {
                    if let Some(cached_ptr) = self.get_cached_disk_location(path) {
                        log::trace!(
                            "Skipping clean subtree at path {:?} (using cached disk location)",
                            String::from_utf8_lossy(path)
                        );
                        return Ok(cached_ptr);
                    }
                }

                let mut child_ptrs: Vec<(u8, u64)> = Vec::with_capacity(children.len());
                for (edge, child) in children {
                    let mut child_path = path.to_vec();
                    child_path.push(*edge);

                    let ptr = self.serialize_child_to_disk_with_path(child, &child_path)?;
                    child_ptrs.push((*edge, ptr.to_raw()));
                }

                let mut node_copy = node.clone();
                for (edge, ptr_raw) in &child_ptrs {
                    if let Some(child_ptr) = node_copy.find_child_mut(*edge) {
                        *child_ptr = SwizzledPtr::from_raw(*ptr_raw);
                    }
                }

                node_copy.header_mut().set_final(*is_final);

                let node_ptr = self.serialize_node_to_disk(&node_copy)?;

                self.cache_disk_location(path, node_ptr.clone());

                let _ = value;

                Ok(node_ptr)
            }
            ChildNode::DiskRef { ptr } => {
                self.cache_disk_location(path, ptr.clone());
                Ok(ptr.clone())
            }
        }
    }
}
