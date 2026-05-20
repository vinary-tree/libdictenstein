//! Disk loading + child resolution helpers for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~159-988, ~830 LOC)
//! as a Phase-6 char sub-module. Methods covered:
//!
//! - `load_root_from_disk` (pub(super); called by mmap/io_uring ctors)
//! - `load_char_node_from_disk` (+ `_lazy`, `_iterative`, `_with_depth` variants)
//! - `load_single_node_data` — iterative-style single-node deserialization
//! - `get_child_lazy` / `get_child_lazy_u32` (read-side child resolution)
//! - `get_child_mut_lazy` / `get_child_mut_lazy_u32` (mutable variants)
//! - `get_or_create_child_lazy_ptr` / `get_or_create_child_lazy_u32_ptr`
//! - `resolve_swizzled_ptr` / `resolve_swizzled_ptr_mut`
//!
//! These all bridge between in-memory `CharTrieNodeInner<V>` (children
//! as `Box`/`SwizzledPtr`) and on-disk arena slots loaded via
//! `BufferManager` / `ArenaManager`.

use std::sync::Arc;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::buffer_manager::BufferManager;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;

use super::dict_impl_char::{ROOT_TYPE_EMPTY, ROOT_TYPE_NODE};
use super::types::{CharTrieNodeInner, CharTrieRoot};

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    pub(super) fn load_root_from_disk(
        &self,
        buffer_manager: &Arc<RwLock<BufferManager<S>>>,
        root_desc_ptr: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
        eager_depth: Option<usize>,
    ) -> Result<(CharTrieRoot<V>, usize)> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

        // Read the root descriptor from block 0 at the encoded offset (64)
        let bm = buffer_manager.read();

        let disk_loc = root_desc_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::internal("Root descriptor pointer is swizzled or null")
        })?;
        let page_guard = bm.fetch_page(disk_loc.block_id)?;
        let page_data = page_guard.data();

        // Read descriptor from the offset within block 0
        let offset = disk_loc.offset as usize;
        let descriptor_buf = &page_data[offset..offset + 18];

        // Parse root descriptor (fixed 18 bytes)
        // Format:
        //   0: type (1 byte)
        //   1: is_final (1 byte)
        //   2-5: term_count (4 bytes, little endian)
        //   6-9: arena_count (4 bytes, little endian)
        //   10-17: root_ptr (8 bytes, little endian)
        let root_type = descriptor_buf[0];
        let _is_final = descriptor_buf[1] != 0;
        let term_count = u32::from_le_bytes([descriptor_buf[2], descriptor_buf[3], descriptor_buf[4], descriptor_buf[5]]) as usize;
        let arena_count = u32::from_le_bytes([descriptor_buf[6], descriptor_buf[7], descriptor_buf[8], descriptor_buf[9]]);
        let root_ptr = u64::from_le_bytes([
            descriptor_buf[10], descriptor_buf[11], descriptor_buf[12], descriptor_buf[13],
            descriptor_buf[14], descriptor_buf[15], descriptor_buf[16], descriptor_buf[17],
        ]);

        // Derive arena block IDs from sequential allocation
        // Block 0 = file header + descriptor, Blocks 1..=arena_count = arenas
        let arena_block_ids: Vec<u32> = (1..=arena_count).collect();

        drop(page_guard);
        drop(bm);

        // Load arenas into the arena manager
        if arena_count > 0 {
            if let Some(ref arena_manager) = self.arena_manager {
                {
                    let mut am = arena_manager.write();
                    // Clear the initial empty arena
                    am.clear_for_loading();
                    // Load each arena from disk
                    for block_id in arena_block_ids {
                        am.load_arena(block_id)?;
                    }
                    // Set active arena to the last one for new allocations
                    let count = am.arena_count();
                    am.set_active_arena(count.saturating_sub(1));
                }
            }
        }

        match root_type {
            ROOT_TYPE_EMPTY => {
                Ok((CharTrieRoot::Empty, 0))
            }
            ROOT_TYPE_NODE => {
                let root_swizzled = SwizzledPtr::from_raw(root_ptr);
                // Choose loading strategy based on eager_depth
                let node = match eager_depth {
                    None | Some(0) => {
                        // Fully lazy: only load root node, children on-demand
                        self.load_char_node_from_disk_lazy(buffer_manager, &root_swizzled)?
                    }
                    Some(depth) if depth >= usize::MAX / 2 => {
                        // Fully eager: load all levels
                        self.load_char_node_from_disk_iterative(buffer_manager, &root_swizzled)?
                    }
                    Some(depth) => {
                        // Depth-limited: load `depth` levels, rest lazy
                        self.load_char_node_from_disk_with_depth(buffer_manager, &root_swizzled, Some(depth))?
                    }
                };
                Ok((CharTrieRoot::Node(Box::new(node)), term_count))
            }
            _ => {
                Err(PersistentARTrieError::internal(format!(
                    "Unknown root type: {}",
                    root_type
                )))
            }
        }
    }

    /// Load a CharTrieNodeInner from disk
    ///
    /// Uses arena allocation for space-efficient reading. Nodes are packed
    /// into 256KB arena blocks, with SwizzledPtr encoding:
    /// - block_id = arena_id
    /// - offset = slot_id
    ///
    /// Disk format:
    /// ```text
    /// [CharNode serialized - 16-byte header + type-specific data]
    /// [value_len: u32]
    /// [value_bytes if value_len > 0]
    /// ```
    pub(super) fn load_char_node_from_disk(
        &self,
        _buffer_manager: &Arc<RwLock<BufferManager<S>>>,
        node_ptr: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
    ) -> Result<CharTrieNodeInner<V>> {
        use super::arena_manager::ArenaSlot;
        use super::serialization_char::{deserialize_char_node_v2, DeserializationContext};
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        use std::io::Cursor;

        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk reading")
        })?;

        // Get arena slot from the disk location
        // block_id = arena_id + 1 (block 0 is file header)
        // offset = slot_id
        let disk_loc = node_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::internal("Node pointer is swizzled or null")
        })?;
        let arena_id = disk_loc.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::internal("Invalid block_id 0 for arena node")
        })?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);

        // Read from arena
        let am = arena_manager.read();

        let node_data = am.read(slot)?;

        // Deserialize the CharNode using v2 format with context
        let deser_ctx = DeserializationContext::new(slot);
        let mut cursor = Cursor::new(node_data);
        let char_node = deserialize_char_node_v2(&mut cursor, &deser_ctx)?;

        // Use cursor position to find where value data starts (v2 format is variable size)
        let offset = cursor.position() as usize;

        // Read value_len and value_bytes
        let value_len = u32::from_le_bytes([
            node_data[offset],
            node_data[offset + 1],
            node_data[offset + 2],
            node_data[offset + 3],
        ]) as usize;

        let value: Option<V> = if value_len > 0 {
            let value_start = offset + 4;
            let value_end = value_start + value_len;
            let value_bytes = &node_data[value_start..value_end];
            Some(bincode::deserialize(value_bytes).map_err(|e| {
                PersistentARTrieError::internal(&format!("Failed to deserialize value: {}", e))
            })?)
        } else {
            None
        };

        // Collect child pointers from the CharNode
        let child_data: Vec<(u32, SwizzledPtr)> = char_node
            .iter_children()
            .map(|(key, ptr)| (key, ptr.clone()))
            .collect();

        // Drop the arena lock before recursive calls
        drop(am);

        // Create the result node with proper node type from disk
        let is_final = char_node.is_final();
        let mut result = CharTrieNodeInner::new();
        result.set_final(is_final);
        result.value = value;

        // Recursively load children and add them
        for (char_val, child_ptr) in child_data {
            if let Some(c) = char::from_u32(char_val) {
                let child_node = self.load_char_node_from_disk(_buffer_manager, &child_ptr)?;
                result.insert_child(c, child_node);
            }
        }

        Ok(result)
    }

    /// Load a CharTrieNodeInner from disk with lazy child loading
    ///
    /// Unlike `load_char_node_from_disk`, this version does NOT recursively load
    /// children. Instead, it stores the on-disk SwizzledPtrs directly, allowing
    /// children to be loaded on-demand when accessed.
    ///
    /// Uses arena allocation for space-efficient reading. Nodes are packed
    /// into 256KB arena blocks, with SwizzledPtr encoding:
    /// - block_id = arena_id
    /// - offset = slot_id
    ///
    /// This is the preferred loading method for large tries where loading
    /// everything upfront would be too expensive.
    ///
    /// Disk format:
    /// ```text
    /// [CharNode serialized - 16-byte header + type-specific data]
    /// [value_len: u32]
    /// [value_bytes if value_len > 0]
    /// ```
    pub(super) fn load_char_node_from_disk_lazy(
        &self,
        _buffer_manager: &Arc<RwLock<BufferManager<S>>>,
        node_ptr: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
    ) -> Result<CharTrieNodeInner<V>> {
        use super::arena_manager::ArenaSlot;
        use super::serialization_char::{deserialize_char_node_v2, DeserializationContext};
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        use std::io::Cursor;

        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk reading")
        })?;

        // Get arena slot from the disk location
        // block_id = arena_id + 1 (block 0 is file header)
        // offset = slot_id
        let disk_loc = node_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::internal("Node pointer is swizzled or null")
        })?;
        let arena_id = disk_loc.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::internal("Invalid block_id 0 for arena node")
        })?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);

        // Read from arena
        let am = arena_manager.read();

        let node_data = am.read(slot)?;

        // Deserialize the CharNode using v2 format with context
        let deser_ctx = DeserializationContext::new(slot);
        let mut cursor = Cursor::new(node_data);
        let char_node = deserialize_char_node_v2(&mut cursor, &deser_ctx)?;

        // Use cursor position to find where value data starts (v2 format is variable size)
        let offset = cursor.position() as usize;

        // Read value_len and value_bytes
        let value_len = u32::from_le_bytes([
            node_data[offset],
            node_data[offset + 1],
            node_data[offset + 2],
            node_data[offset + 3],
        ]) as usize;

        let value: Option<V> = if value_len > 0 {
            let value_start = offset + 4;
            let value_end = value_start + value_len;
            let value_bytes = &node_data[value_start..value_end];
            Some(bincode::deserialize(value_bytes).map_err(|e| {
                PersistentARTrieError::internal(&format!("Failed to deserialize value: {}", e))
            })?)
        } else {
            None
        };

        // Collect child pointers from the CharNode (as-is, for lazy loading)
        let child_data: Vec<(char, SwizzledPtr)> = char_node
            .iter_children()
            .filter_map(|(key, ptr)| {
                char::from_u32(key).map(|c| (c, ptr.clone()))
            })
            .collect();

        drop(am);

        // Create the node
        let is_final = char_node.is_final();
        let mut result = CharTrieNodeInner::new();
        result.set_final(is_final);
        result.value = value;

        // Insert children using insert_child_ptr (stores raw SwizzledPtrs without loading)
        for (c, child_ptr) in child_data {
            // If there's an old in-memory pointer, we'd need to free it,
            // but for fresh loading there shouldn't be any
            let _old = result.insert_child_ptr(c, child_ptr);
        }

        Ok(result)
    }

    /// Load a single CharTrieNodeInner's data from disk WITHOUT loading children.
    ///
    /// This is a helper for iterative loading. Returns the node (without children
    /// connected) and the list of child pointers that need to be loaded.
    ///
    /// The returned node has `is_final`, `value`, and an empty child set.
    /// Children must be connected by the caller after loading.
    pub(super) fn load_single_node_data(
        &self,
        node_ptr: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
    ) -> Result<(CharTrieNodeInner<V>, Vec<(char, crate::persistent_artrie::swizzled_ptr::SwizzledPtr)>)> {
        use super::arena_manager::ArenaSlot;
        use super::serialization_char::{deserialize_char_node_v2, DeserializationContext};
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        use std::io::Cursor;

        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk reading")
        })?;

        // Get arena slot from the disk location
        // block_id = arena_id + 1 (block 0 is file header)
        // offset = slot_id
        let disk_loc = node_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::internal("Node pointer is swizzled or null")
        })?;
        let arena_id = disk_loc.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::internal("Invalid block_id 0 for arena node")
        })?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);

        // Read from arena
        let am = arena_manager.read();

        let node_data = am.read(slot)?;

        // Deserialize the CharNode using v2 format with context
        let deser_ctx = DeserializationContext::new(slot);
        let mut cursor = Cursor::new(node_data);
        let char_node = deserialize_char_node_v2(&mut cursor, &deser_ctx)?;

        // Use cursor position to find where value data starts (v2 format is variable size)
        let offset = cursor.position() as usize;

        // Read value_len and value_bytes
        let value_len = u32::from_le_bytes([
            node_data[offset],
            node_data[offset + 1],
            node_data[offset + 2],
            node_data[offset + 3],
        ]) as usize;

        let value: Option<V> = if value_len > 0 {
            let value_start = offset + 4;
            let value_end = value_start + value_len;
            let value_bytes = &node_data[value_start..value_end];
            Some(bincode::deserialize(value_bytes).map_err(|e| {
                PersistentARTrieError::internal(&format!("Failed to deserialize value: {}", e))
            })?)
        } else {
            None
        };

        // Collect child pointers from the CharNode
        let child_entries: Vec<(char, SwizzledPtr)> = char_node
            .iter_children()
            .filter_map(|(key, ptr)| {
                char::from_u32(key).map(|c| (c, ptr.clone()))
            })
            .collect();

        drop(am);

        // Create the result node with proper node type from disk (NO children connected)
        let is_final = char_node.is_final();
        let mut result = CharTrieNodeInner::new();
        result.set_final(is_final);
        result.value = value;

        Ok((result, child_entries))
    }

    /// Load a CharTrieNodeInner from disk using iterative (non-recursive) traversal.
    ///
    /// This avoids stack overflow for deep tries by using an explicit work stack
    /// instead of recursive function calls. Uses a two-phase algorithm:
    ///
    /// 1. **Phase 1**: Load all nodes into a vector (without connecting children)
    /// 2. **Phase 2**: Connect children to parents in reverse order (bottom-up)
    ///
    /// This maintains identical semantics to `load_char_node_from_disk` but can
    /// handle arbitrarily deep tries without stack overflow.
    pub(super) fn load_char_node_from_disk_iterative(
        &self,
        _buffer_manager: &Arc<RwLock<BufferManager<S>>>,
        root_ptr: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
    ) -> Result<CharTrieNodeInner<V>> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        use std::collections::HashMap;

        /// Information about a loaded node before children are connected
        struct LoadedNodeInfo<V: DictionaryValue> {
            /// The node with is_final and value set, but NO children
            node: CharTrieNodeInner<V>,
            /// Child entries that need to be loaded and connected
            child_entries: Vec<(char, SwizzledPtr)>,
        }

        // Stack for DFS traversal (avoids recursion)
        let mut work_stack: Vec<SwizzledPtr> = vec![root_ptr.clone()];

        // Results vector - nodes are stored in DFS pre-order
        let mut loaded_nodes: Vec<LoadedNodeInfo<V>> = Vec::new();

        // Map from disk pointer raw value to result index (for parent-child linking)
        let mut ptr_to_idx: HashMap<u64, usize> = HashMap::new();

        // Phase 1: Load all nodes without connecting children
        while let Some(node_ptr) = work_stack.pop() {
            // Skip if already loaded (handles potential shared subtrees)
            let ptr_raw = node_ptr.to_raw();
            if ptr_to_idx.contains_key(&ptr_raw) {
                continue;
            }

            // Load this node's data from disk (single I/O)
            let (node, child_entries) = self.load_single_node_data(&node_ptr)?;

            // Reserve result index
            let result_idx = loaded_nodes.len();
            ptr_to_idx.insert(ptr_raw, result_idx);

            // Store child entries for Phase 2
            let child_ptrs: Vec<SwizzledPtr> = child_entries.iter()
                .map(|(_, ptr)| ptr.clone())
                .collect();

            loaded_nodes.push(LoadedNodeInfo { node, child_entries });

            // Push children onto stack (reverse order for correct DFS ordering)
            // This ensures children are processed in the order they appear
            for child_ptr in child_ptrs.into_iter().rev() {
                work_stack.push(child_ptr);
            }
        }

        // Handle empty tree case
        if loaded_nodes.is_empty() {
            return Err(PersistentARTrieError::internal("No nodes loaded from disk"));
        }

        // Phase 2: Connect children to parents (bottom-up)
        // Process in reverse order so children are fully built before parents connect to them
        for idx in (0..loaded_nodes.len()).rev() {
            // Take child_entries out to avoid borrowing issues
            let child_entries = std::mem::take(&mut loaded_nodes[idx].child_entries);

            for (char_key, child_ptr) in child_entries {
                let child_idx = *ptr_to_idx.get(&child_ptr.to_raw())
                    .ok_or_else(|| PersistentARTrieError::internal(
                        "Child pointer not found in loaded nodes map"
                    ))?;

                // Take ownership of the child node (replace with empty placeholder)
                let child_node = std::mem::replace(
                    &mut loaded_nodes[child_idx].node,
                    CharTrieNodeInner::new()
                );

                // Connect child to parent
                loaded_nodes[idx].node.insert_child(char_key, child_node);
            }
        }

        // Root is at index 0 (first node pushed/processed)
        Ok(std::mem::replace(&mut loaded_nodes[0].node, CharTrieNodeInner::new()))
    }

    /// Load a CharTrieNodeInner with depth-limited eager loading.
    ///
    /// Loads the first `max_depth` levels of the trie eagerly (all at once),
    /// while keeping nodes beyond that depth as disk pointers for lazy loading.
    ///
    /// This provides a balance between:
    /// - Fully eager loading (fast lookups, slow open, high memory)
    /// - Fully lazy loading (fast open, slower first lookups)
    ///
    /// # Arguments
    /// * `buffer_manager` - The buffer manager for disk I/O
    /// * `root_ptr` - The root node's disk pointer
    /// * `max_depth` - Maximum depth to load eagerly. Nodes at this depth have
    ///   their children stored as disk pointers. `None` means fully eager.
    ///
    /// # Example Depths
    /// - `Some(0)`: Only root loaded, all children lazy (same as lazy loading)
    /// - `Some(3)`: Root + 2 levels loaded, 4th level and beyond lazy
    /// - `Some(10)`: First 10 levels loaded eagerly
    /// - `None`: All levels loaded (same as full iterative loading)
    pub(super) fn load_char_node_from_disk_with_depth(
        &self,
        _buffer_manager: &Arc<RwLock<BufferManager<S>>>,
        root_ptr: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
        max_depth: Option<usize>,
    ) -> Result<CharTrieNodeInner<V>> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        use std::collections::HashMap;

        // If max_depth is 0, just do lazy loading (only load root)
        if max_depth == Some(0) {
            return self.load_char_node_from_disk_lazy(_buffer_manager, root_ptr);
        }

        /// Work item with depth tracking
        struct WorkItem {
            ptr: SwizzledPtr,
            depth: usize,
        }

        /// Information about a loaded node before children are connected
        struct LoadedNodeInfo<V: DictionaryValue> {
            node: CharTrieNodeInner<V>,
            /// Children to load eagerly (within depth limit)
            eager_children: Vec<(char, SwizzledPtr)>,
            /// Children to keep as disk pointers (beyond depth limit)
            lazy_children: Vec<(char, SwizzledPtr)>,
        }

        // Stack for DFS traversal with depth tracking
        let mut work_stack: Vec<WorkItem> = vec![WorkItem {
            ptr: root_ptr.clone(),
            depth: 0,
        }];

        // Results vector - nodes are stored in DFS pre-order
        let mut loaded_nodes: Vec<LoadedNodeInfo<V>> = Vec::new();

        // Map from disk pointer raw value to result index
        let mut ptr_to_idx: HashMap<u64, usize> = HashMap::new();

        // Phase 1: Load nodes up to depth limit
        while let Some(work_item) = work_stack.pop() {
            let ptr_raw = work_item.ptr.to_raw();
            if ptr_to_idx.contains_key(&ptr_raw) {
                continue;
            }

            // Load this node's data from disk
            let (node, child_entries) = self.load_single_node_data(&work_item.ptr)?;

            // Reserve result index
            let result_idx = loaded_nodes.len();
            ptr_to_idx.insert(ptr_raw, result_idx);

            // Determine which children to load eagerly vs lazily
            let at_depth_limit = max_depth.map_or(false, |max| work_item.depth >= max.saturating_sub(1));

            let (eager_children, lazy_children): (Vec<_>, Vec<_>) = if at_depth_limit {
                // At depth limit: all children become lazy
                (Vec::new(), child_entries)
            } else {
                // Within limit: all children loaded eagerly
                (child_entries, Vec::new())
            };

            // Push eager children to work stack (reverse order for correct DFS)
            for (_, child_ptr) in eager_children.iter().rev() {
                work_stack.push(WorkItem {
                    ptr: child_ptr.clone(),
                    depth: work_item.depth + 1,
                });
            }

            loaded_nodes.push(LoadedNodeInfo {
                node,
                eager_children,
                lazy_children,
            });
        }

        // Handle empty tree case
        if loaded_nodes.is_empty() {
            return Err(PersistentARTrieError::internal("No nodes loaded from disk"));
        }

        // Phase 2: Connect children (bottom-up)
        for idx in (0..loaded_nodes.len()).rev() {
            // First, insert lazy children as disk pointers
            let lazy_children = std::mem::take(&mut loaded_nodes[idx].lazy_children);
            for (char_key, child_ptr) in lazy_children {
                loaded_nodes[idx].node.insert_child_ptr(char_key, child_ptr);
            }

            // Then, connect eager children (already loaded)
            let eager_children = std::mem::take(&mut loaded_nodes[idx].eager_children);
            for (char_key, child_ptr) in eager_children {
                let child_idx = *ptr_to_idx.get(&child_ptr.to_raw())
                    .ok_or_else(|| PersistentARTrieError::internal(
                        "Child pointer not found in loaded nodes map"
                    ))?;

                // Take ownership of the child node
                let child_node = std::mem::replace(
                    &mut loaded_nodes[child_idx].node,
                    CharTrieNodeInner::new()
                );

                // Connect child to parent
                loaded_nodes[idx].node.insert_child(char_key, child_node);
            }
        }

        // Root is at index 0
        Ok(std::mem::replace(&mut loaded_nodes[0].node, CharTrieNodeInner::new()))
    }

    /// Get a child of a node with lazy loading support.
    ///
    /// If the child pointer is already swizzled (in-memory), returns the node directly.
    /// If on disk, loads the node lazily and atomically swizzles the pointer.
    ///
    /// Returns `Ok(None)` if the child doesn't exist.
    /// Returns `Err` if an I/O error occurs during lazy loading.
    pub(super) fn get_child_lazy(&self, node: &CharTrieNodeInner<V>, c: char) -> Result<Option<&CharTrieNodeInner<V>>> {
        self.get_child_lazy_u32(node, c as u32)
    }

    /// Get a child reference of a node with lazy loading support, using a u32 key directly.
    pub(super) fn get_child_lazy_u32(&self, node: &CharTrieNodeInner<V>, key: u32) -> Result<Option<&CharTrieNodeInner<V>>> {
        match node.node.find_child(key) {
            Some(ptr) => {
                if ptr.is_null() {
                    Ok(None)
                } else {
                    Ok(Some(self.resolve_swizzled_ptr(ptr)?))
                }
            }
            None => Ok(None),
        }
    }

    /// Get a mutable child reference of a node with lazy loading support.
    ///
    /// If the child pointer is already swizzled (in-memory), returns the node directly.
    /// If on disk, loads the node lazily and atomically swizzles the pointer.
    ///
    /// Returns `Ok(None)` if the child doesn't exist.
    /// Returns `Err` if an I/O error occurs during lazy loading.
    pub(super) fn get_child_mut_lazy(&self, node: &CharTrieNodeInner<V>, c: char) -> Result<Option<&mut CharTrieNodeInner<V>>> {
        self.get_child_mut_lazy_u32(node, c as u32)
    }

    /// Get a mutable child reference of a node with lazy loading support, using a u32 key directly.
    pub(super) fn get_child_mut_lazy_u32(&self, node: &CharTrieNodeInner<V>, key: u32) -> Result<Option<&mut CharTrieNodeInner<V>>> {
        match node.node.find_child(key) {
            Some(ptr) => {
                if ptr.is_null() {
                    Ok(None)
                } else {
                    Ok(Some(self.resolve_swizzled_ptr_mut(ptr)?))
                }
            }
            None => Ok(None),
        }
    }

    /// Get or create a child with lazy loading support.
    ///
    /// If the child exists (in memory or on disk), returns a raw pointer to it.
    /// If on disk, loads the node lazily first.
    /// If the child doesn't exist, creates a new one.
    ///
    /// Returns `Err` if an I/O error occurs during lazy loading.
    ///
    /// # Safety
    ///
    /// The caller must ensure `node` is part of this trie's structure.
    /// The returned pointer is valid as long as the trie exists.
    pub(super) fn get_or_create_child_lazy_ptr(
        &self,
        node: &mut CharTrieNodeInner<V>,
        c: char,
    ) -> Result<*mut CharTrieNodeInner<V>> {
        self.get_or_create_child_lazy_u32_ptr(node, c as u32)
    }

    /// Get or create a child with lazy loading support, using a u32 key directly.
    ///
    /// Same as `get_or_create_child_lazy_ptr` but accepts a raw u32 character code,
    /// avoiding the need for callers to convert from char first.
    pub(super) fn get_or_create_child_lazy_u32_ptr(
        &self,
        node: &mut CharTrieNodeInner<V>,
        key: u32,
    ) -> Result<*mut CharTrieNodeInner<V>> {
        // Check if child already exists
        if let Some(ptr) = node.node.find_child(key) {
            if !ptr.is_null() {
                // Child exists - ensure it's swizzled (load if on disk)
                let child_ref = self.resolve_swizzled_ptr_mut(ptr)?;
                return Ok(child_ref as *mut CharTrieNodeInner<V>);
            }
        }

        // Child doesn't exist - create new one
        let new_child = Box::new(CharTrieNodeInner::new());
        let ptr = Box::into_raw(new_child);
        let swizzled = SwizzledPtr::in_memory(ptr);

        // Add to node, handling potential growth
        match node.node.add_child_growing(key, swizzled) {
            Ok(Some(grown)) => {
                node.node = grown;
            }
            Ok(None) => {
                // No growth needed
            }
            Err(_) => {
                // Key already exists (shouldn't happen, but handle gracefully)
                unsafe { drop(Box::from_raw(ptr)); }
                // Try to get the existing child
                if let Some(existing_ptr) = node.node.find_child(key) {
                    let child_ref = self.resolve_swizzled_ptr_mut(existing_ptr)?;
                    return Ok(child_ref as *mut CharTrieNodeInner<V>);
                }
                return Err(PersistentARTrieError::internal("Failed to add or find child"));
            }
        }

        Ok(ptr)
    }

    /// Resolve a SwizzledPtr to a reference to a CharTrieNodeInner
    ///
    /// If the pointer is already swizzled (in-memory), returns the existing node.
    /// If on disk, loads the node lazily and atomically swizzles the pointer.
    ///
    /// This method handles the race condition where multiple threads try to load
    /// the same node simultaneously - only one allocation will survive.
    ///
    /// # Safety
    ///
    /// The returned reference is valid as long as the node is not evicted from
    /// memory. In the current implementation, nodes are never evicted.
    pub(super) fn resolve_swizzled_ptr(&self, ptr: &SwizzledPtr) -> Result<&CharTrieNodeInner<V>> {
        use crate::persistent_artrie::error::SwizzleError;

        // Fast path: already in memory
        if let Some(p) = ptr.as_ptr::<CharTrieNodeInner<V>>() {
            // Safety: We control all SwizzledPtr creation; ptr is valid
            return Ok(unsafe { &*p });
        }

        // Null pointer check
        if ptr.is_null() {
            return Err(PersistentARTrieError::internal("Cannot resolve null SwizzledPtr"));
        }

        // Slow path: load from disk
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for disk access")
        })?;

        // Load the node data (lazy - children are not recursively loaded)
        let loaded = self.load_char_node_from_disk_lazy(buffer_manager, ptr)?;
        let boxed = Box::new(loaded);
        let raw_ptr = Box::into_raw(boxed);

        // Try to swizzle atomically
        match ptr.swizzle(raw_ptr) {
            Ok(()) => {
                // We won the race
                Ok(unsafe { &*raw_ptr })
            }
            Err(SwizzleError::RaceCondition) | Err(SwizzleError::AlreadySwizzled) => {
                // Another thread won the race - free our copy and use theirs
                unsafe { drop(Box::from_raw(raw_ptr)); }
                // Safety: The winner has swizzled the pointer
                Ok(unsafe { &*ptr.as_ptr_unchecked::<CharTrieNodeInner<V>>() })
            }
            Err(e) => {
                // Something else went wrong - free our allocation
                unsafe { drop(Box::from_raw(raw_ptr)); }
                Err(PersistentARTrieError::internal(&format!("Swizzle failed: {:?}", e)))
            }
        }
    }

    /// Resolve a SwizzledPtr to a mutable reference to a CharTrieNodeInner
    ///
    /// Similar to `resolve_swizzled_ptr` but returns a mutable reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure exclusive access to the node.
    pub(super) fn resolve_swizzled_ptr_mut(&self, ptr: &SwizzledPtr) -> Result<&mut CharTrieNodeInner<V>> {
        use crate::persistent_artrie::error::SwizzleError;

        // Fast path: already in memory
        if let Some(p) = ptr.as_ptr::<CharTrieNodeInner<V>>() {
            // Safety: We control all SwizzledPtr creation; caller ensures exclusive access
            return Ok(unsafe { &mut *(p as *mut CharTrieNodeInner<V>) });
        }

        // Null pointer check
        if ptr.is_null() {
            return Err(PersistentARTrieError::internal("Cannot resolve null SwizzledPtr"));
        }

        // Slow path: load from disk
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for disk access")
        })?;

        // Load the node data (lazy - children are not recursively loaded)
        let loaded = self.load_char_node_from_disk_lazy(buffer_manager, ptr)?;
        let boxed = Box::new(loaded);
        let raw_ptr = Box::into_raw(boxed);

        // Try to swizzle atomically
        match ptr.swizzle(raw_ptr) {
            Ok(()) => {
                // We won the race
                Ok(unsafe { &mut *raw_ptr })
            }
            Err(SwizzleError::RaceCondition) | Err(SwizzleError::AlreadySwizzled) => {
                // Another thread won the race - free our copy and use theirs
                unsafe { drop(Box::from_raw(raw_ptr)); }
                // Safety: The winner has swizzled the pointer
                Ok(unsafe { &mut *(ptr.as_ptr_unchecked::<CharTrieNodeInner<V>>() as *mut CharTrieNodeInner<V>) })
            }
            Err(e) => {
                // Something else went wrong - free our allocation
                unsafe { drop(Box::from_raw(raw_ptr)); }
                Err(PersistentARTrieError::internal(&format!("Swizzle failed: {:?}", e)))
            }
        }
    }
}
