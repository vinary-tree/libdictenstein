//! Disk I/O helpers for `PersistentVocabARTrie<S>`.
//!
//! Split out of vocab `dict_impl.rs` (lines ~224-694, ~471 LOC) as
//! the tenth Phase-6 vocab sub-module. These are the private +
//! `pub(super)` helpers that bridge between the in-memory
//! `VocabTrieNode` tree and disk-resident arena slots:
//!
//! - `load_trie_from_disk` — restore the trie from disk
//! - `load_vocab_node_structure` — recursive node deserialization
//! - `rebuild_reverse_index` — rebuild `u64 → NodeRef` map
//! - `update_reverse_index_recursive` — DFS that populates the map
//! - `serialize_vocab_node_to_disk` — bottom-up serialization with arena slots
//! - `build_disk_char_node_static` — static helper to construct the
//!   underlying `CharNode` from the vocab wrapper
//! - `persist_to_disk` — drive full serialization, returning root slot
//! - `replay_insert` — WAL recovery replay
//!
//! All methods are `pub(super)` so the constructors
//! (`mmap_ctor`/`io_uring_ctor`) and persistence/durability
//! (`persistence_api`) can call them.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use parking_lot::RwLock;
use xxhash_rust::xxh3::Xxh3DefaultBuilder;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::buffer_manager::BufferManager;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
use crate::persistent_artrie_char::arena_manager::{ArenaManager, ArenaSlot};
use crate::persistent_artrie_char::nodes::CharNode;
use crate::persistent_artrie_char::relative_encoding::SerializationContext;
use crate::persistent_artrie_char::serialization_char::{
    deserialize_char_node_v2, serialize_char_node_v2, DeserializationContext,
};
use crate::persistent_artrie_char::types::NodeRef;

use super::reverse_index::VocabReverseIndex;
use super::types::{VocabTrieNode, VocabTrieRoot};

impl<S: BlockStorage> super::dict_impl::PersistentVocabARTrie<S> {
    ///
    /// This uses a two-phase approach:
    /// 1. Load all nodes from disk into memory (children as in-memory pointers)
    /// 2. Rebuild node_map and parent pointers with fresh NodeRefs
    ///
    /// The two-phase approach is necessary because serialized nodes store parent NodeRefs
    /// from the original insertion order, which we can't reproduce during load.
    pub(super) fn load_trie_from_disk(
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        buffer_manager: &Arc<RwLock<BufferManager<S>>>,
        root_slot: ArenaSlot,
    ) -> Result<(
        VocabTrieRoot,
        HashMap<NodeRef, *const VocabTrieNode, Xxh3DefaultBuilder>,
        u64,
    )> {
        // Phase 1: Load all nodes from disk (parent fields will have stale NodeRefs)
        let root_node = Self::load_vocab_node_structure(arena_manager, buffer_manager, root_slot)?;

        // Phase 2: Rebuild node_map with fresh NodeRefs and update parent fields
        let mut node_map = HashMap::with_hasher(Xxh3DefaultBuilder);
        let mut next_slot: u64 = 1; // Start at 1, root gets 0

        let root_ref = NodeRef::new(0, 0);
        let root_ptr = Box::into_raw(Box::new(root_node));
        node_map.insert(root_ref, root_ptr as *const VocabTrieNode);

        // Recursively assign NodeRefs and update parent fields
        // Safety: root_ptr is valid, we just created it
        unsafe {
            Self::rebuild_node_map_and_parents(
                root_ptr as *mut VocabTrieNode,
                root_ref,
                &mut node_map,
                &mut next_slot,
            );
        }

        Ok((
            VocabTrieRoot::Node(unsafe { Box::from_raw(root_ptr) }),
            node_map,
            next_slot,
        ))
    }

    /// Load a VocabTrieNode and all its descendants from disk.
    ///
    /// This only loads the structure - parent NodeRefs will be stale and need to be
    /// fixed up by `rebuild_node_map_and_parents` afterward.
    fn load_vocab_node_structure(
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        buffer_manager: &Arc<RwLock<BufferManager<S>>>,
        slot: ArenaSlot,
    ) -> Result<VocabTrieNode> {
        // Read node data from arena
        let data = {
            let am = arena_manager.read();
            am.read(slot)?.to_vec()
        };

        // Deserialize CharNode using v2 format
        let ctx = DeserializationContext::new(slot);
        let mut cursor = Cursor::new(&data);
        let inner = deserialize_char_node_v2(&mut cursor, &ctx)?;

        // Read vocab-specific fields after the CharNode
        let offset = cursor.position() as usize;
        let remaining = &data[offset..];

        if remaining.len() < 13 {
            return Err(PersistentARTrieError::corrupted(
                "VocabTrieNode data too short for vocab fields",
            ));
        }

        // Read parent (8 bytes) - will be updated in phase 2
        let parent_bytes: [u8; 8] = remaining[0..8].try_into().expect("8 bytes for parent");
        let parent = NodeRef::from_bytes(&parent_bytes);

        // Read parent_edge (4 bytes)
        let parent_edge = u32::from_le_bytes(remaining[8..12].try_into().expect("4 bytes"));

        // Read has_value flag and value
        // Bug #4 fix: Error on corrupted data instead of silently dropping the value
        let has_value = remaining[12];
        let value = if has_value == 1 {
            if remaining.len() < 21 {
                return Err(PersistentARTrieError::corrupted(
                    "VocabTrieNode data too short for value (expected 21 bytes for vocab fields with value)"
                ));
            }
            Some(u64::from_le_bytes(
                remaining[13..21].try_into().expect("8 bytes"),
            ))
        } else {
            None
        };

        // Create VocabTrieNode
        let mut node = VocabTrieNode {
            inner,
            parent,
            parent_edge,
            value,
        };

        // Recursively load children that are on disk
        let mut child_nodes: Vec<(u32, Box<VocabTrieNode>)> = Vec::new();

        for (key, child_ptr) in node.inner.iter_children() {
            if let Some(disk_loc) = child_ptr.disk_location() {
                // Child is on disk - load it recursively
                let child_slot = ArenaSlot::new(
                    disk_loc.block_id.saturating_sub(1), // arena_id = block_id - 1
                    disk_loc.offset,
                );

                let child_node =
                    Self::load_vocab_node_structure(arena_manager, buffer_manager, child_slot)?;

                child_nodes.push((key, Box::new(child_node)));
            }
        }

        // Replace disk children with in-memory children
        if !child_nodes.is_empty() {
            // Clone the node structure without children
            let mut new_inner = CharNode::new_node4();
            {
                let new_header = new_inner.header_mut();
                let old_header = node.inner.header();
                new_header.prefix_len = old_header.prefix_len;
                new_header.flags = old_header.flags;
            }
            *new_inner.prefix_mut() = *node.inner.prefix();

            // Add loaded children
            for (key, child_box) in child_nodes {
                let child_ptr = Box::into_raw(child_box);
                let swizzled = SwizzledPtr::in_memory(child_ptr);

                // Bug #1 & #3 fix: Properly handle add_child_growing return value
                match new_inner.add_child_growing(key, swizzled) {
                    Ok(Some(grown)) => new_inner = grown,
                    Ok(None) => {} // Successfully added, no growth needed
                    Err(e) => {
                        // Reclaim the child to avoid leak before returning error
                        unsafe {
                            drop(Box::from_raw(child_ptr));
                        }
                        return Err(PersistentARTrieError::corrupted(format!(
                            "Failed to add child during trie load: {:?}",
                            e
                        )));
                    }
                }
            }

            node.inner = new_inner;
        }

        Ok(node)
    }

    /// Rebuild node_map and parent fields after loading from disk.
    ///
    /// This does a DFS traversal to:
    /// 1. Assign fresh NodeRefs to each node
    /// 2. Update each node's `parent` field to point to its actual parent's NodeRef
    /// 3. Build node_map with the fresh NodeRefs
    ///
    /// Safety: `node_ptr` must be a valid pointer to a VocabTrieNode.
    unsafe fn rebuild_node_map_and_parents(
        node_ptr: *mut VocabTrieNode,
        my_ref: NodeRef,
        node_map: &mut HashMap<NodeRef, *const VocabTrieNode, Xxh3DefaultBuilder>,
        next_slot: &mut u64,
    ) {
        let node = &mut *node_ptr;

        // Process all children
        for (_key, child_swizzled) in node.inner.iter_children() {
            if let Some(child_raw_ptr) = child_swizzled.as_ptr::<VocabTrieNode>() {
                let child_ptr = child_raw_ptr as *mut VocabTrieNode;
                let child = &mut *child_ptr;

                // Assign fresh NodeRef to this child
                let child_ref = NodeRef::new(0, *next_slot as u32);
                *next_slot += 1;

                // Update child's parent to point to us (the actual parent)
                child.parent = my_ref;

                // Add child to node_map
                node_map.insert(child_ref, child_ptr as *const VocabTrieNode);

                // Recursively process child's subtree
                Self::rebuild_node_map_and_parents(child_ptr, child_ref, node_map, next_slot);
            }
        }
    }

    /// Rebuild the reverse_index after loading from disk.
    ///
    /// When we load from disk, node_map gets fresh NodeRefs that don't match the
    /// old NodeRefs stored in the serialized reverse_index. This method traverses
    /// the trie in the same order as rebuild_node_map_and_parents and updates
    /// reverse_index entries for all final nodes (nodes with values).
    pub(super) fn rebuild_reverse_index(&mut self) -> Result<()> {
        let reverse_index = match self.reverse_index.as_mut() {
            Some(idx) => idx,
            None => return Ok(()), // No reverse index to rebuild
        };

        // Traverse the trie in the same order as rebuild_node_map_and_parents
        // to compute the same NodeRefs
        if let VocabTrieRoot::Node(ref root) = self.root {
            let root_ref = NodeRef::new(0, 0);
            let mut slot_counter: u64 = 1; // Start at 1, root is 0

            // Update reverse_index for root if it's final
            if let Some(vocab_index) = root.value {
                reverse_index.set(vocab_index, root_ref)?;
            }

            // Recursively process children
            Self::update_reverse_index_recursive(root.as_ref(), reverse_index, &mut slot_counter)?;
        }

        Ok(())
    }

    /// Recursively update reverse_index entries for final nodes.
    ///
    /// This mirrors the traversal order of rebuild_node_map_and_parents so that
    /// the NodeRefs we compute match those in node_map.
    fn update_reverse_index_recursive(
        node: &VocabTrieNode,
        reverse_index: &mut VocabReverseIndex,
        slot_counter: &mut u64,
    ) -> Result<()> {
        // Process children in the same order as rebuild_node_map_and_parents
        for (_key, child_swizzled) in node.inner.iter_children() {
            if let Some(child_ptr) = child_swizzled.as_ptr::<VocabTrieNode>() {
                let child = unsafe { &*child_ptr };

                // This child gets the current slot (same as rebuild_node_map_and_parents)
                let child_ref = NodeRef::new(0, *slot_counter as u32);
                *slot_counter += 1;

                // If child is final, update reverse_index
                if let Some(vocab_index) = child.value {
                    reverse_index.set(vocab_index, child_ref)?;
                }

                // Recurse into child's subtree
                Self::update_reverse_index_recursive(child, reverse_index, slot_counter)?;
            }
        }

        Ok(())
    }

    /// Serialize a VocabTrieNode to disk recursively (bottom-up).
    ///
    /// Children are serialized first to get their disk pointers, then the parent.
    fn serialize_vocab_node_to_disk(&mut self, node: &VocabTrieNode) -> Result<ArenaSlot> {
        // Verify arena manager exists first (don't keep reference across recursive calls)
        if self.arena_manager.is_none() {
            return Err(PersistentARTrieError::internal(
                "No arena manager for disk serialization",
            ));
        }

        // First, recursively serialize all children and collect their disk pointers
        let mut child_disk_ptrs: Vec<(u32, SwizzledPtr)> = Vec::new();
        for (key, child_ptr) in node.inner.iter_children() {
            if child_ptr.is_null() {
                continue;
            }

            // Check if child is already on disk
            if child_ptr.disk_location().is_some() {
                child_disk_ptrs.push((key, child_ptr.clone()));
            } else if let Some(child_raw) = child_ptr.as_ptr::<VocabTrieNode>() {
                // Child is in memory - serialize it recursively
                let child = unsafe { &*child_raw };
                let child_slot = self.serialize_vocab_node_to_disk(child)?;

                // Create SwizzledPtr pointing to disk location
                // block_id = arena_id + 1 (block 0 is header)
                let disk_ptr = SwizzledPtr::on_disk(
                    child_slot.arena_id + 1,
                    child_slot.slot_id,
                    crate::persistent_artrie::NodeType::CharNode4, // Type doesn't matter for disk ref
                );
                child_disk_ptrs.push((key, disk_ptr));
            }
        }

        // Now borrow arena_manager to get parent slot and allocate
        // (all recursive calls are complete at this point)
        let arena_manager = self.arena_manager.as_ref().expect("checked above");

        // Get the predicted parent slot for encoding
        let parent_slot = arena_manager.read().next_slot();

        // Build a CharNode with disk pointers for serialization
        let disk_node = Self::build_disk_char_node_static(&node.inner, &child_disk_ptrs);

        // Create serialization context
        let ctx = SerializationContext::new(parent_slot);

        // Serialize CharNode using v2 format
        let mut buffer = Vec::new();
        serialize_char_node_v2(&disk_node, &mut buffer, &ctx)?;

        // Append vocab-specific fields:
        // - parent: NodeRef (8 bytes)
        // - parent_edge: u32 (4 bytes)
        // - has_value: u8 (1 byte)
        // - value: u64 (8 bytes, if has_value)
        buffer.extend_from_slice(&node.parent.to_bytes());
        buffer.extend_from_slice(&node.parent_edge.to_le_bytes());
        if let Some(value) = node.value {
            buffer.push(1); // has_value = true
            buffer.extend_from_slice(&value.to_le_bytes());
        } else {
            buffer.push(0); // has_value = false
        }

        // Allocate in arena
        let slot = arena_manager.write().allocate(&buffer)?;

        Ok(slot)
    }

    /// Build a CharNode with disk SwizzledPtrs for serialization.
    fn build_disk_char_node_static(
        original: &CharNode,
        disk_children: &[(u32, SwizzledPtr)],
    ) -> CharNode {
        use crate::persistent_artrie_char::nodes::{CharBucket, CharNode16, CharNode4, CharNode48};

        // Create a new node of the same type
        let mut new_node = match original {
            CharNode::N4(_) => CharNode::N4(Box::new(CharNode4::new())),
            CharNode::N16(_) => CharNode::N16(Box::new(CharNode16::new())),
            CharNode::N48(_) => CharNode::N48(Box::new(CharNode48::new())),
            CharNode::Bucket(_) => CharNode::Bucket(Box::new(CharBucket::new())),
        };

        // Copy header properties
        {
            let new_header = new_node.header_mut();
            let orig_header = original.header();
            new_header.prefix_len = orig_header.prefix_len;
            new_header.flags = orig_header.flags;
        }

        // Copy prefix
        *new_node.prefix_mut() = *original.prefix();

        // Add disk children
        // Bug #3 fix: Properly handle add_child_growing return value for node growth
        for &(key, ref ptr) in disk_children {
            match new_node.add_child_growing(key, ptr.clone()) {
                Ok(Some(grown)) => new_node = grown,
                Ok(None) => {} // Successfully added, no growth needed
                Err(e) => {
                    // Log error but continue - this should rarely happen during serialization
                    eprintln!(
                        "Warning: failed to add child in build_disk_char_node_static: {:?}",
                        e
                    );
                }
            }
        }

        new_node
    }

    /// Persist the trie to disk (serializes nodes to arenas).
    pub(super) fn persist_to_disk(&mut self) -> Result<ArenaSlot> {
        // Check if root is empty first (without holding a borrow)
        let is_empty = matches!(self.root, VocabTrieRoot::Empty);
        if is_empty {
            return Err(PersistentARTrieError::internal("Cannot persist empty root"));
        }

        // Get pointer to root node for serialization
        // We need to extract a raw pointer to avoid borrow conflicts
        let root_node_ptr: *const VocabTrieNode = match &self.root {
            VocabTrieRoot::Node(node) => node.as_ref() as *const VocabTrieNode,
            VocabTrieRoot::Empty => unreachable!(), // Already checked above
        };

        // Serialize the root node (this recursively serializes all children)
        // Safety: root_node_ptr is valid because we own self.root
        let root_slot = unsafe { self.serialize_vocab_node_to_disk(&*root_node_ptr)? };

        // Flush arenas to disk
        if let Some(ref arena_manager) = self.arena_manager {
            arena_manager.write().flush()?;
        }

        Ok(root_slot)
    }

    /// Replay an insert during WAL recovery.
    pub(super) fn replay_insert(&mut self, term: &str, index: u64) -> Result<()> {
        let chars: Vec<char> = term.chars().collect();
        let root_ref = NodeRef::new(0, 0);

        match &mut self.root {
            VocabTrieRoot::Empty => {
                return Err(PersistentARTrieError::CorruptedFile {
                    reason: "Cannot replay insert into empty root".to_string(),
                });
            }
            VocabTrieRoot::Node(root) => {
                let mut current = root.as_mut();
                let mut current_ref = root_ref;

                for &c in chars.iter() {
                    let slot = self.next_slot;
                    self.next_slot += 1;
                    let child_ref = NodeRef::new(0, slot as u32);

                    let child = current.get_or_create_child(c, current_ref);

                    if !self.node_map.contains_key(&child_ref) {
                        self.node_map
                            .insert(child_ref, child as *const VocabTrieNode);
                    }

                    current_ref = child_ref;
                    current = child;
                }

                // Check if already final (idempotent replay)
                if !current.is_final() {
                    current.set_value(index);

                    // Update reverse index
                    if let Some(ref mut rev_idx) = self.reverse_index {
                        let _ = rev_idx.set(index, current_ref);
                    }

                    // Update bloom filter
                    if let Some(ref mut bloom) = self.bloom_filter {
                        bloom.insert(term);
                    }

                    // Update counts
                    self.entry_count.fetch_add(1, Ordering::AcqRel);
                }

                // Track next index atomically using CAS loop
                loop {
                    let current = self.next_index.load(Ordering::Acquire);
                    if index < current {
                        break; // Another thread already advanced it
                    }
                    let new_val = index + 1;
                    match self.next_index.compare_exchange(
                        current,
                        new_val,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(_) => continue, // Retry
                    }
                }
            }
        }

        Ok(())
    }
}
