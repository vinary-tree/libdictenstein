//! Disk-loading helpers for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~391-1108, ~715 LOC) as
//! the eighteenth Phase-5 byte sub-module. These are the private
//! methods that deserialize trie roots, ART nodes, and child
//! pointers from disk through the `BlockStorage` / `BufferManager`
//! / `ArenaManager` layers. They are called from the `mmap_ctor`
//! and `io_uring_ctor` sibling modules during `open*` flows.
//!
//! Methods covered:
//! - `load_root_from_disk` / `load_root_from_disk_with_arena`
//! - `load_art_node_with_children` / `load_child_from_disk`
//! - `load_art_node_with_children_from_arena` / `load_child_from_disk_with_arena`
//! - `load_single_art_node_data` / `load_single_child_data`
//! - `load_art_node_with_children_from_arena_iterative`

#![allow(dead_code)]

use std::sync::Arc;

use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;

use super::arena_manager::{ArenaManager, ArenaSlot};
use super::block_storage::BlockStorage;
use super::bucket::StringBucket;
use super::buffer_manager::BufferManager;
use super::dict_impl::{
    PersistentARTrie, SingleChildData, TrieRoot, ART_NODE_BUFFER_SIZE, ROOT_TYPE_ART_NODE,
    ROOT_TYPE_BUCKET, ROOT_TYPE_EMPTY,
};
use super::error::{PersistentARTrieError, Result};
use super::nodes::Node;
use super::serialization;
use super::serialization::v2::DeserializationContext;
use super::swizzled_ptr::{NodeType, SwizzledPtr};
use super::transitions::ChildNode;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Load the trie root from disk.
    ///
    /// Reads the root descriptor block and deserializes the trie structure.
    ///
    /// # Returns
    /// Tuple of (TrieRoot, term_count) on success.
    fn load_root_from_disk(
        disk_manager: &impl BlockStorage,
        root_ptr: u64,
    ) -> Result<(TrieRoot<V>, u64)> {
        use super::disk_manager::BLOCK_SIZE;
        use super::BUCKET_PAGE_SIZE;

        // Decode the SwizzledPtr to get block_id
        let ptr = SwizzledPtr::from_raw(root_ptr);
        if ptr.is_null() || ptr.is_swizzled() {
            return Err(PersistentARTrieError::corrupted(
                "Invalid root pointer: null or already swizzled",
            ));
        }

        let location = ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Could not decode disk location from root pointer")
        })?;

        // Read the descriptor block
        let mut descriptor_buf = [0u8; BLOCK_SIZE];
        disk_manager.read_block(location.block_id, &mut descriptor_buf)?;

        // Parse root descriptor
        // Format:
        //   0: type (1 byte)
        //   1: is_final (1 byte)
        //   2-5: term_count (4 bytes, little endian)
        //   6-9: arena_count (4 bytes, little endian)
        //   10-17: root_ptr (8 bytes, little endian)
        //   18+: value bytes (if any)
        let root_type = descriptor_buf[0];
        let is_final = descriptor_buf[1] != 0;
        let term_count = u32::from_le_bytes([
            descriptor_buf[2],
            descriptor_buf[3],
            descriptor_buf[4],
            descriptor_buf[5],
        ]);
        let arena_count = u32::from_le_bytes([
            descriptor_buf[6],
            descriptor_buf[7],
            descriptor_buf[8],
            descriptor_buf[9],
        ]);
        let data_ptr = u64::from_le_bytes([
            descriptor_buf[10],
            descriptor_buf[11],
            descriptor_buf[12],
            descriptor_buf[13],
            descriptor_buf[14],
            descriptor_buf[15],
            descriptor_buf[16],
            descriptor_buf[17],
        ]);

        let _ = arena_count; // Arena count stored for recovery
        let _ = is_final; // Used for ArtNode but we simplified

        match root_type {
            ROOT_TYPE_BUCKET => {
                // Load bucket from disk
                let bucket_ptr = SwizzledPtr::from_raw(data_ptr);
                let bucket_loc = bucket_ptr.disk_location().ok_or_else(|| {
                    PersistentARTrieError::corrupted("Invalid bucket pointer in root descriptor")
                })?;

                let mut bucket_data = [0u8; BUCKET_PAGE_SIZE];
                disk_manager.read_bytes(bucket_loc.block_id, 0, &mut bucket_data)?;

                let bucket = StringBucket::from_bytes(&bucket_data).map_err(|e| {
                    PersistentARTrieError::corrupted(format!("Failed to load bucket: {:?}", e))
                })?;

                Ok((TrieRoot::Bucket(bucket), term_count as u64))
            }
            ROOT_TYPE_ART_NODE => {
                // Load the ART node from disk
                let node_ptr = SwizzledPtr::from_raw(data_ptr);

                // Load the node and its children recursively
                let (node, children) = Self::load_art_node_with_children(disk_manager, &node_ptr)?;

                // Value deserialization not yet implemented with arena storage
                // (value_len no longer in descriptor - using arena_count instead)
                let root_value: Option<V> = None;

                Ok((
                    TrieRoot::ArtNode {
                        node,
                        children,
                        is_final,
                        value: root_value,
                    },
                    term_count as u64,
                ))
            }
            ROOT_TYPE_EMPTY | _ => {
                // Empty or unknown type
                Ok((TrieRoot::Bucket(StringBucket::with_values()), 0))
            }
        }
    }

    /// Load an ART node from disk and recursively load all its children.
    ///
    /// This method deserializes an ART node and builds the in-memory ChildNode
    /// structure by loading each child (which may be a bucket or another ART node).
    ///
    /// # Returns
    /// Tuple of (Node, Vec<(u8, ChildNode)>) representing the node and its children.
    fn load_art_node_with_children(
        disk_manager: &impl BlockStorage,
        node_ptr: &SwizzledPtr,
    ) -> Result<(Node, Vec<(u8, ChildNode)>)> {
        // Get disk location from SwizzledPtr
        let location = node_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid node pointer: cannot get disk location")
        })?;

        // Read the node data from disk
        let mut node_data = [0u8; ART_NODE_BUFFER_SIZE];
        disk_manager.read_bytes(location.block_id, 0, &mut node_data)?;

        // Deserialize the node
        let node = serialization::from_bytes(&node_data).map_err(|e| {
            PersistentARTrieError::corrupted(format!("Failed to deserialize ART node: {:?}", e))
        })?;

        // Load all children recursively
        let mut children = Vec::new();
        for (key, child_ptr) in node.iter_children() {
            if !child_ptr.is_null() {
                let child = Self::load_child_from_disk(disk_manager, child_ptr)?;
                children.push((key, child));
            }
        }

        Ok((node, children))
    }

    /// Load a child node (bucket or ART node) from disk.
    ///
    /// This method examines the SwizzledPtr's node type to determine whether
    /// the child is a bucket or an ART node, and loads it appropriately.
    fn load_child_from_disk(
        disk_manager: &impl BlockStorage,
        child_ptr: &SwizzledPtr,
    ) -> Result<ChildNode> {
        use super::BUCKET_PAGE_SIZE;

        let location = child_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid child pointer: cannot get disk location")
        })?;

        // Determine child type from the DiskLocation's node_type
        let node_type = location.node_type;

        match node_type {
            NodeType::Bucket => {
                // Load bucket from disk
                let mut bucket_data = [0u8; BUCKET_PAGE_SIZE];
                disk_manager.read_bytes(location.block_id, 0, &mut bucket_data)?;

                let bucket = StringBucket::from_bytes(&bucket_data).map_err(|e| {
                    PersistentARTrieError::corrupted(format!(
                        "Failed to load child bucket: {:?}",
                        e
                    ))
                })?;

                Ok(ChildNode::Bucket(bucket))
            }
            NodeType::Node4 | NodeType::Node16 | NodeType::Node48 | NodeType::Node256 => {
                // Read the node data from disk
                let mut node_data = [0u8; ART_NODE_BUFFER_SIZE];
                disk_manager.read_bytes(location.block_id, 0, &mut node_data)?;

                // Deserialize the node
                let node = serialization::from_bytes(&node_data).map_err(|e| {
                    PersistentARTrieError::corrupted(format!(
                        "Failed to deserialize child ART node: {:?}",
                        e
                    ))
                })?;

                // Check if node is final (has IS_FINAL flag set)
                let is_final = node.header().is_final();

                // Load children recursively
                let mut children = Vec::new();
                for (key, grandchild_ptr) in node.iter_children() {
                    if !grandchild_ptr.is_null() {
                        let grandchild = Self::load_child_from_disk(disk_manager, grandchild_ptr)?;
                        children.push((key, grandchild));
                    }
                }

                Ok(ChildNode::ArtNode {
                    node,
                    is_final,
                    value: None, // Value serialization for nested nodes is future work
                    children,
                })
            }
            // Char-level nodes should never appear in byte-level trie
            NodeType::CharNode4
            | NodeType::CharNode16
            | NodeType::CharNode48
            | NodeType::CharBucket => Err(PersistentARTrieError::corrupted(
                "Char-level node type encountered in byte-level PersistentARTrie",
            )),
        }
    }

    /// Load the root of the trie from disk using arena-based storage.
    ///
    /// This version uses ArenaManager to read data from arena slots instead
    /// of reading full blocks directly from disk. The SwizzledPtr encodes:
    /// - block_id = arena_id
    /// - offset = slot_id
    ///
    /// # Returns
    /// Tuple of (TrieRoot, term_count) on success.
    pub(super) fn load_root_from_disk_with_arena(
        buffer_manager: &Arc<RwLock<BufferManager<S>>>,
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        root_ptr: u64,
    ) -> Result<(TrieRoot<V>, u64)> {
        // Decode the SwizzledPtr to get block_id and offset
        let ptr = SwizzledPtr::from_raw(root_ptr);
        if ptr.is_null() || ptr.is_swizzled() {
            return Err(PersistentARTrieError::corrupted(
                "Invalid root pointer: null or already swizzled",
            ));
        }

        let location = ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Could not decode disk location from root pointer")
        })?;

        // Read the descriptor from block 0 at the encoded offset (64)
        // The SwizzledPtr now encodes (block_id=0, offset=64)
        let bm = buffer_manager.read();
        let page = bm.fetch_page(location.block_id)?;
        let page_data = page.data();

        // Read descriptor from the offset within block 0
        let offset = location.offset as usize;
        let descriptor_buf = &page_data[offset..offset + 18];

        // Parse root descriptor (fixed 18 bytes)
        // Format:
        //   0: type (1 byte)
        //   1: is_final (1 byte)
        //   2-5: term_count (4 bytes, little endian)
        //   6-9: arena_count (4 bytes, little endian)
        //   10-17: root_ptr (8 bytes, little endian)
        let root_type = descriptor_buf[0];
        let is_final = descriptor_buf[1] != 0;
        let term_count = u32::from_le_bytes([
            descriptor_buf[2],
            descriptor_buf[3],
            descriptor_buf[4],
            descriptor_buf[5],
        ]);
        let data_ptr = u64::from_le_bytes([
            descriptor_buf[10],
            descriptor_buf[11],
            descriptor_buf[12],
            descriptor_buf[13],
            descriptor_buf[14],
            descriptor_buf[15],
            descriptor_buf[16],
            descriptor_buf[17],
        ]);

        drop(page);
        drop(bm);

        match root_type {
            ROOT_TYPE_BUCKET => {
                // Load bucket from arena
                let bucket_ptr = SwizzledPtr::from_raw(data_ptr);
                let bucket_loc = bucket_ptr.disk_location().ok_or_else(|| {
                    PersistentARTrieError::corrupted("Invalid bucket pointer in root descriptor")
                })?;

                // Get arena slot from the disk location
                // block_id = arena_id + 1 (block 0 is file header)
                // offset = slot_id
                let arena_id = bucket_loc.block_id.checked_sub(1).ok_or_else(|| {
                    PersistentARTrieError::corrupted("Invalid block_id 0 for arena bucket")
                })?;
                let slot = ArenaSlot::new(arena_id, bucket_loc.offset);
                let am = arena_manager.read();
                let bucket_data = am.read(slot)?;

                let bucket = StringBucket::from_bytes(bucket_data).map_err(|e| {
                    PersistentARTrieError::corrupted(format!("Failed to load bucket: {:?}", e))
                })?;

                Ok((TrieRoot::Bucket(bucket), term_count as u64))
            }
            ROOT_TYPE_ART_NODE => {
                // Load the ART node from arena
                let node_ptr = SwizzledPtr::from_raw(data_ptr);

                // Load the node and its children using iterative loading
                // (avoids stack overflow for deep tries)
                let (node, children) = Self::load_art_node_with_children_from_arena_iterative(
                    arena_manager,
                    &node_ptr,
                )?;

                // Value deserialization not yet implemented with arena storage
                let root_value: Option<V> = None;

                Ok((
                    TrieRoot::ArtNode {
                        node,
                        children,
                        is_final,
                        value: root_value,
                    },
                    term_count as u64,
                ))
            }
            ROOT_TYPE_EMPTY | _ => {
                // Empty or unknown type
                Ok((TrieRoot::Bucket(StringBucket::with_values()), 0))
            }
        }
    }

    /// Load an ART node from arena and recursively load all its children.
    ///
    /// This version uses ArenaManager to read data from arena slots.
    ///
    /// # Returns
    /// Tuple of (Node, Vec<(u8, ChildNode)>) representing the node and its children.
    fn load_art_node_with_children_from_arena(
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        node_ptr: &SwizzledPtr,
    ) -> Result<(Node, Vec<(u8, ChildNode)>)> {
        // Get arena slot from the disk location
        // block_id = arena_id + 1 (block 0 is file header)
        // offset = slot_id
        let disk_loc = node_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid node pointer: cannot get disk location")
        })?;
        let arena_id = disk_loc
            .block_id
            .checked_sub(1)
            .ok_or_else(|| PersistentARTrieError::corrupted("Invalid block_id 0 for arena node"))?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);
        let am = arena_manager.read();
        let node_data = am.read(slot)?;

        // Deserialize the node using v2 format with relative offset support
        // The slot is the "parent slot" for decoding relative child offsets
        let ctx = DeserializationContext::new(slot);
        let node = serialization::v2::deserialize_node_v2(node_data, &ctx).map_err(|e| {
            PersistentARTrieError::corrupted(format!("Failed to deserialize ART node: {:?}", e))
        })?;

        // Collect child pointers before dropping the arena lock
        let child_data: Vec<(u8, SwizzledPtr)> = node
            .iter_children()
            .filter(|(_, ptr)| !ptr.is_null())
            .map(|(key, ptr)| (key, ptr.clone()))
            .collect();

        // Drop arena lock before recursive calls
        drop(am);

        // Load all children recursively
        let mut children = Vec::new();
        for (key, child_ptr) in child_data {
            let child = Self::load_child_from_disk_with_arena(arena_manager, &child_ptr)?;
            children.push((key, child));
        }

        Ok((node, children))
    }

    /// Load a child node (bucket or ART node) from arena.
    ///
    /// This version uses ArenaManager to read data from arena slots.
    fn load_child_from_disk_with_arena(
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        child_ptr: &SwizzledPtr,
    ) -> Result<ChildNode> {
        // Get arena slot from the disk location
        // block_id = arena_id + 1 (block 0 is file header)
        // offset = slot_id
        let disk_loc = child_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid child pointer: cannot get disk location")
        })?;
        let arena_id = disk_loc
            .block_id
            .checked_sub(1)
            .ok_or_else(|| PersistentARTrieError::corrupted("Invalid block_id 0 for arena node"))?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);

        // Determine child type from the DiskLocation's node_type
        let node_type = disk_loc.node_type;

        // Read data from arena
        let am = arena_manager.read();
        let data = am.read(slot)?;

        match node_type {
            NodeType::Bucket => {
                let bucket = StringBucket::from_bytes(data).map_err(|e| {
                    PersistentARTrieError::corrupted(format!(
                        "Failed to load child bucket: {:?}",
                        e
                    ))
                })?;

                Ok(ChildNode::Bucket(bucket))
            }
            NodeType::Node4 | NodeType::Node16 | NodeType::Node48 | NodeType::Node256 => {
                // Deserialize the node using v2 format with relative offset support
                // The slot is the "parent slot" for decoding relative child offsets
                let ctx = DeserializationContext::new(slot);
                let node = serialization::v2::deserialize_node_v2(data, &ctx).map_err(|e| {
                    PersistentARTrieError::corrupted(format!(
                        "Failed to deserialize child ART node: {:?}",
                        e
                    ))
                })?;

                // Check if node is final (has IS_FINAL flag set)
                let is_final = node.header().is_final();

                // Collect child pointers before dropping the arena lock
                let child_data: Vec<(u8, SwizzledPtr)> = node
                    .iter_children()
                    .filter(|(_, ptr)| !ptr.is_null())
                    .map(|(key, ptr)| (key, ptr.clone()))
                    .collect();

                // Drop arena lock before recursive calls
                drop(am);

                // Load children recursively
                let mut children = Vec::new();
                for (key, grandchild_ptr) in child_data {
                    let grandchild =
                        Self::load_child_from_disk_with_arena(arena_manager, &grandchild_ptr)?;
                    children.push((key, grandchild));
                }

                Ok(ChildNode::ArtNode {
                    node,
                    is_final,
                    value: None, // Value serialization for nested nodes is future work
                    children,
                })
            }
            // Char-level nodes should never appear in byte-level trie
            NodeType::CharNode4
            | NodeType::CharNode16
            | NodeType::CharNode48
            | NodeType::CharBucket => Err(PersistentARTrieError::corrupted(
                "Char-level node type encountered in byte-level PersistentARTrie",
            )),
        }
    }

    /// Load a single ART node's data from arena WITHOUT loading children.
    ///
    /// This is a helper for iterative loading. Returns the node info and
    /// the list of child pointers that need to be loaded.
    fn load_single_art_node_data(
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        node_ptr: &SwizzledPtr,
    ) -> Result<(Node, bool, Vec<(u8, SwizzledPtr)>)> {
        let disk_loc = node_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid node pointer: cannot get disk location")
        })?;
        let arena_id = disk_loc
            .block_id
            .checked_sub(1)
            .ok_or_else(|| PersistentARTrieError::corrupted("Invalid block_id 0 for arena node"))?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);
        let am = arena_manager.read();
        let node_data = am.read(slot)?;

        // Deserialize the node using v2 format with relative offset support
        let ctx = DeserializationContext::new(slot);
        let node = serialization::v2::deserialize_node_v2(node_data, &ctx).map_err(|e| {
            PersistentARTrieError::corrupted(format!("Failed to deserialize ART node: {:?}", e))
        })?;

        let is_final = node.header().is_final();

        // Collect child pointers before dropping the arena lock
        let child_data: Vec<(u8, SwizzledPtr)> = node
            .iter_children()
            .filter(|(_, ptr)| !ptr.is_null())
            .map(|(key, ptr)| (key, ptr.clone()))
            .collect();

        drop(am);

        Ok((node, is_final, child_data))
    }

    /// Load a single child node's data from arena WITHOUT loading its children.
    ///
    /// Returns either a complete Bucket (no children) or the components needed
    /// to build an ArtNode (node, is_final, child pointers).
    fn load_single_child_data(
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        child_ptr: &SwizzledPtr,
    ) -> Result<SingleChildData> {
        let disk_loc = child_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("Invalid child pointer: cannot get disk location")
        })?;
        let arena_id = disk_loc
            .block_id
            .checked_sub(1)
            .ok_or_else(|| PersistentARTrieError::corrupted("Invalid block_id 0 for arena node"))?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);
        let node_type = disk_loc.node_type;

        let am = arena_manager.read();
        let data = am.read(slot)?;

        match node_type {
            NodeType::Bucket => {
                let bucket = StringBucket::from_bytes(data).map_err(|e| {
                    PersistentARTrieError::corrupted(format!(
                        "Failed to load child bucket: {:?}",
                        e
                    ))
                })?;
                Ok(SingleChildData::Bucket(bucket))
            }
            NodeType::Node4 | NodeType::Node16 | NodeType::Node48 | NodeType::Node256 => {
                let ctx = DeserializationContext::new(slot);
                let node = serialization::v2::deserialize_node_v2(data, &ctx).map_err(|e| {
                    PersistentARTrieError::corrupted(format!(
                        "Failed to deserialize child ART node: {:?}",
                        e
                    ))
                })?;

                let is_final = node.header().is_final();

                let child_data: Vec<(u8, SwizzledPtr)> = node
                    .iter_children()
                    .filter(|(_, ptr)| !ptr.is_null())
                    .map(|(key, ptr)| (key, ptr.clone()))
                    .collect();

                drop(am);

                Ok(SingleChildData::ArtNodePartial {
                    node,
                    is_final,
                    child_ptrs: child_data,
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

    /// Load an ART node and all its children using iterative (non-recursive) traversal.
    ///
    /// This avoids stack overflow for deep tries by using an explicit work stack.
    /// Uses a two-phase algorithm:
    ///
    /// 1. **Phase 1**: Load all nodes into a vector (without connecting children)
    /// 2. **Phase 2**: Connect children to parents in reverse order (bottom-up)
    fn load_art_node_with_children_from_arena_iterative(
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        root_node_ptr: &SwizzledPtr,
    ) -> Result<(Node, Vec<(u8, ChildNode)>)> {
        use std::collections::HashMap;

        /// Work item for iterative loading
        enum WorkItem {
            /// Load from the root ART node
            RootNode(SwizzledPtr),
            /// Load a child node
            Child(SwizzledPtr),
        }

        /// Loaded node info before children are connected
        enum LoadedInfo {
            /// The root node
            RootNode {
                node: Node,
                is_final: bool,
                child_ptrs: Vec<(u8, SwizzledPtr)>,
            },
            /// A bucket child (complete, no children to connect)
            Bucket(StringBucket),
            /// An ART child node (needs children connected)
            ArtNodePartial {
                node: Node,
                is_final: bool,
                child_ptrs: Vec<(u8, SwizzledPtr)>,
            },
        }

        // Stack for DFS traversal
        let mut work_stack: Vec<WorkItem> = vec![WorkItem::RootNode(root_node_ptr.clone())];

        // Results vector - nodes stored in DFS pre-order
        let mut loaded_nodes: Vec<LoadedInfo> = Vec::new();

        // Map from disk pointer raw value to result index
        let mut ptr_to_idx: HashMap<u64, usize> = HashMap::new();

        // Phase 1: Load all nodes without connecting children
        while let Some(work_item) = work_stack.pop() {
            let (ptr_raw, loaded_info, child_ptrs_to_push) = match work_item {
                WorkItem::RootNode(ptr) => {
                    let ptr_raw = ptr.to_raw();
                    if ptr_to_idx.contains_key(&ptr_raw) {
                        continue;
                    }

                    let (node, is_final, child_ptrs) =
                        Self::load_single_art_node_data(arena_manager, &ptr)?;
                    let ptrs_to_push: Vec<SwizzledPtr> =
                        child_ptrs.iter().map(|(_, p)| p.clone()).collect();
                    (
                        ptr_raw,
                        LoadedInfo::RootNode {
                            node,
                            is_final,
                            child_ptrs,
                        },
                        ptrs_to_push,
                    )
                }
                WorkItem::Child(ptr) => {
                    let ptr_raw = ptr.to_raw();
                    if ptr_to_idx.contains_key(&ptr_raw) {
                        continue;
                    }

                    match Self::load_single_child_data(arena_manager, &ptr)? {
                        SingleChildData::Bucket(bucket) => {
                            (ptr_raw, LoadedInfo::Bucket(bucket), vec![])
                        }
                        SingleChildData::ArtNodePartial {
                            node,
                            is_final,
                            child_ptrs,
                        } => {
                            let ptrs_to_push: Vec<SwizzledPtr> =
                                child_ptrs.iter().map(|(_, p)| p.clone()).collect();
                            (
                                ptr_raw,
                                LoadedInfo::ArtNodePartial {
                                    node,
                                    is_final,
                                    child_ptrs,
                                },
                                ptrs_to_push,
                            )
                        }
                    }
                }
            };

            let result_idx = loaded_nodes.len();
            ptr_to_idx.insert(ptr_raw, result_idx);
            loaded_nodes.push(loaded_info);

            // Push children in reverse order for correct DFS ordering
            for child_ptr in child_ptrs_to_push.into_iter().rev() {
                work_stack.push(WorkItem::Child(child_ptr));
            }
        }

        if loaded_nodes.is_empty() {
            return Err(PersistentARTrieError::corrupted(
                "No nodes loaded from disk",
            ));
        }

        // Phase 2: Build ChildNode structures bottom-up
        // We need to convert LoadedInfo into final ChildNode structures
        // Process in reverse order so children are ready before parents need them

        // Store built ChildNode results (indexed same as loaded_nodes)
        let mut built_children: Vec<Option<ChildNode>> = vec![None; loaded_nodes.len()];

        for idx in (0..loaded_nodes.len()).rev() {
            let child_node = match &mut loaded_nodes[idx] {
                LoadedInfo::RootNode { .. } => {
                    // Root is handled separately
                    continue;
                }
                LoadedInfo::Bucket(bucket) => ChildNode::Bucket(std::mem::take(bucket)),
                LoadedInfo::ArtNodePartial {
                    node,
                    is_final,
                    child_ptrs,
                } => {
                    // Collect built children
                    let mut children: Vec<(u8, ChildNode)> = Vec::with_capacity(child_ptrs.len());
                    for (key, child_ptr) in child_ptrs.drain(..) {
                        let child_idx = *ptr_to_idx.get(&child_ptr.to_raw()).ok_or_else(|| {
                            PersistentARTrieError::corrupted(
                                "Child pointer not found in loaded nodes map",
                            )
                        })?;
                        let child = built_children[child_idx].take().ok_or_else(|| {
                            PersistentARTrieError::corrupted("Child not yet built (ordering error)")
                        })?;
                        children.push((key, child));
                    }

                    // Take ownership of node
                    let node_taken = std::mem::replace(node, Node::new_node4());

                    ChildNode::ArtNode {
                        node: node_taken,
                        is_final: *is_final,
                        value: None,
                        children,
                    }
                }
            };

            built_children[idx] = Some(child_node);
        }

        // Extract root node info and build its children
        match &mut loaded_nodes[0] {
            LoadedInfo::RootNode {
                node,
                is_final: _,
                child_ptrs,
            } => {
                let mut children: Vec<(u8, ChildNode)> = Vec::with_capacity(child_ptrs.len());
                for (key, child_ptr) in child_ptrs.drain(..) {
                    let child_idx = *ptr_to_idx.get(&child_ptr.to_raw()).ok_or_else(|| {
                        PersistentARTrieError::corrupted(
                            "Root child pointer not found in loaded nodes map",
                        )
                    })?;
                    let child = built_children[child_idx].take().ok_or_else(|| {
                        PersistentARTrieError::corrupted("Root child not yet built")
                    })?;
                    children.push((key, child));
                }

                let root_node = std::mem::replace(node, Node::new_node4());
                Ok((root_node, children))
            }
            _ => Err(PersistentARTrieError::corrupted(
                "First loaded node is not root",
            )),
        }
    }
}
