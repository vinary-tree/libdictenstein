//! Disk-ref resolution + prefetch helpers for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~1154-1345, ~192 LOC) as
//! the twentieth Phase-5 byte sub-module. These methods bridge
//! between the in-memory `ChildNode` enum and the disk-resident
//! `SwizzledPtr`s, used by the contains / get_value / iter paths
//! when they encounter a `ChildNode::DiskRef`:
//!
//! - `prefetch_disk_refs` / `prefetch_disk_refs_bounded`
//! - `resolve_disk_ref` (pub(super); called by sibling modules)
//! - `resolve_child_if_needed` / `resolve_child_for_mutation`
//! - `check_sequential_children`
//!
//! `resolve_child_for_mutation_with_bm` stays in `dict_impl.rs` as
//! a free function; this module just wraps it.

#![allow(dead_code)]

use super::arena_manager::ArenaSlot;
use super::block_storage::BlockStorage;
use super::bucket::StringBucket;
use super::dict_impl::{resolve_child_for_mutation_with_bm, PersistentARTrie};
use super::error::{PersistentARTrieError, Result};
use super::nodes::Node;
use super::serialization;
use super::swizzled_ptr::{DiskLocation, NodeType, SwizzledPtr};
use super::transitions::ChildNode;
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Prefetch all DiskRef children in a children list.
    ///
    /// This hints the prefetcher to start loading disk-resident children
    /// in the background while we process the current node.
    #[allow(dead_code)]
    pub(super) fn prefetch_disk_refs(&self, children: &[(u8, ChildNode)]) {
        self.prefetch_disk_refs_bounded(children, 0);
    }

    /// Prefetch DiskRef children with depth bounds for multi-level prefetching.
    pub(super) fn prefetch_disk_refs_bounded(&self, children: &[(u8, ChildNode)], depth: u16) {
        let disk_children: Vec<(u8, SwizzledPtr)> = children
            .iter()
            .filter_map(|(key, child)| {
                if let ChildNode::DiskRef { ptr } = child {
                    Some((*key, ptr.clone()))
                } else {
                    None
                }
            })
            .collect();

        if !disk_children.is_empty() {
            self.prefetcher
                .prefetch_children_bounded(&disk_children, depth);
        }
    }

    /// Resolve a DiskRef to its actual node data by loading from disk.
    ///
    /// This is the core lazy loading mechanism. When a child is stored as a
    /// DiskRef (pointing to disk), this method reads the page data via
    /// `BufferManager`, deserializes the node/bucket, and returns the
    /// resolved `ChildNode`.
    pub(super) fn resolve_disk_ref(&self, disk_location: &DiskLocation) -> Result<ChildNode> {
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager available for disk I/O")
        })?;

        let bm = buffer_manager.read();

        let page_guard = bm.fetch_page(disk_location.block_id)?;
        let page_data = page_guard.data();

        let offset = disk_location.offset as usize;
        let node_data = &page_data[offset..];

        match disk_location.node_type {
            NodeType::Bucket => {
                // Bucket deserialization stub: returns empty bucket pending
                // dedicated bucket serializer (see plan T1-2 follow-up).
                let bucket = StringBucket::new();
                Ok(ChildNode::Bucket(bucket))
            }
            NodeType::Node4 | NodeType::Node16 | NodeType::Node48 | NodeType::Node256 => {
                let node = serialization::from_bytes(node_data)?;
                let is_final = node.header().is_final();
                Ok(ChildNode::ArtNode {
                    node,
                    is_final,
                    value: None,
                    children: Vec::new(),
                })
            }
            NodeType::CharNode4
            | NodeType::CharNode16
            | NodeType::CharNode48
            | NodeType::CharBucket => Err(PersistentARTrieError::corrupted(
                "Char-level node type encountered in byte-level PersistentARTrie",
            )),
        }
    }

    /// Resolve a DiskRef child to its in-memory form (without consuming the input).
    ///
    /// Returns `Some(resolved_child)` if the child was a DiskRef that was
    /// successfully resolved, or `None` if no resolution was needed (already
    /// in memory) or the resolution failed.
    pub(super) fn resolve_child_if_needed(&self, child: &ChildNode) -> Option<ChildNode> {
        match child {
            ChildNode::DiskRef { ptr } => {
                if let Some(disk_location) = ptr.disk_location() {
                    self.resolve_disk_ref(&disk_location).ok()
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Resolve a DiskRef child in place, replacing it with the loaded node.
    pub(super) fn resolve_child_for_mutation(&self, child: &mut ChildNode) -> bool {
        resolve_child_for_mutation_with_bm(child, self.buffer_manager.as_ref())
    }

    /// Check if child slots are consecutive in the same arena.
    ///
    /// For sequential sibling storage to work, all children must:
    /// 1. Be in the same arena as the parent will be
    /// 2. Have consecutive slot IDs (first, first+1, first+2, ...)
    ///
    /// Returns `Some(first_child_slot)` if children are consecutive,
    /// `None` otherwise.
    pub(super) fn check_sequential_children(
        node: &Node,
        parent_arena_id: u32,
    ) -> Option<ArenaSlot> {
        let mut child_slots: Vec<ArenaSlot> = Vec::new();

        for (_key, child_ptr) in node.iter_children() {
            if let Some(slot) = child_ptr.as_arena_slot() {
                child_slots.push(slot);
            } else if !child_ptr.is_null() {
                return None;
            }
        }

        if child_slots.len() < 2 {
            return None;
        }

        if child_slots
            .iter()
            .any(|slot| slot.arena_id != parent_arena_id)
        {
            return None;
        }

        child_slots.sort_by_key(|slot| slot.slot_id);

        let first = child_slots[0];
        for (i, slot) in child_slots.iter().enumerate() {
            if slot.slot_id != first.slot_id + i as u32 {
                return None;
            }
        }

        Some(first)
    }
}
