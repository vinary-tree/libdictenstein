//! On-disk persistence for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~506-953, ~448 LOC)
//! as the twentieth Phase-6 char sub-module. Methods covered:
//!
//! - `checkpoint` — full persist + WAL truncate sequence
//! - `verify_checkpoint` — header-checksum verification
//! - `persist_to_disk` — bottom-up serialization driver
//! - `check_sequential_char_children` — sequential-sibling
//!   encoding eligibility check
//! - `serialize_char_node_to_disk` — node serialization
//! - `build_disk_char_node` — construct on-disk node from in-memory
//! - `char_node_to_node_type` — node-type discriminant helper

use std::sync::atomic::Ordering as AtomicOrdering;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::eviction::DiskLocationRegistry;
use crate::persistent_artrie::swizzled_ptr::{NodeType, SwizzledPtr};
use crate::persistent_artrie::wal::WalRecord;
use crate::value::DictionaryValue;

use super::dict_impl_char::{ROOT_TYPE_EMPTY, ROOT_TYPE_NODE};
use super::nodes::CharNode;
use super::types::{CharTrieNodeInner, CharTrieRoot};

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// Checkpoint: persist trie to disk and truncate WAL
    ///
    /// This is the verified checkpoint sequence that ensures data integrity
    /// before truncating the WAL:
    ///
    /// 1. persist_to_disk() - serialize and sync data
    /// 2. verify_checkpoint() - read back and verify header checksum
    /// 3. WAL checkpoint record - mark checkpoint in WAL
    /// 4. WAL sync - ensure checkpoint record is durable
    /// 5. WAL truncate - only after verification passes
    ///
    /// If verification fails at step 2, the WAL is NOT truncated,
    /// allowing recovery from the existing WAL on next open.
    pub fn checkpoint(&mut self) -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Step 1: Persist trie to disk (collecting on-disk node locations for
        // eviction when eviction is enabled).
        let eviction_registry = self.persist_to_disk_tracked()?;

        // Step 2: Verify checkpoint - re-read header and verify checksum
        // This ensures the sync() actually succeeded and data is durable
        self.verify_checkpoint()?;

        // Step 2b: durability is now verified, so publish the freshly-built
        // disk-location registry to the eviction coordinator. Eviction can then
        // reclaim in-memory node boxes (unswizzling them to these on-disk
        // locations) under memory pressure or an explicit force_eviction. Built
        // only when eviction is enabled; a no-op otherwise.
        if let Some(registry) = eviction_registry {
            if let Some(ref coordinator) = self.eviction_coordinator {
                coordinator.update_disk_registry(registry);
            }
        }

        // Steps 3-5: WAL operations (only after verification passes)
        if let Some(ref wal_writer) = self.wal_writer {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let record = WalRecord::Checkpoint {
                checkpoint_lsn: self.next_lsn.load(AtomicOrdering::Acquire),
                timestamp,
            };
            // Step 3: Write checkpoint record
            wal_writer
                .append(record)
                .map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;
            // Step 4: Sync WAL
            wal_writer
                .sync()
                .map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;
            // Step 5: Archive or truncate WAL based on configuration
            // If archive mode is enabled, rotate to archive; otherwise truncate
            wal_writer
                .rotate_to_archive(&self.wal_config)
                .map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;
        }

        self.dirty.store(false, AtomicOrdering::Release);
        Ok(())
    }

    /// Verify checkpoint data integrity after persist_to_disk()
    ///
    /// Re-reads the file header from disk and verifies its checksum.
    /// This ensures the fsync() actually succeeded and data is durable.
    ///
    /// Returns an error if verification fails - the WAL should NOT be
    /// truncated in this case.
    fn verify_checkpoint(&self) -> Result<()> {
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for checkpoint verification")
        })?;

        // Re-read header from disk and verify checksum
        let bm = buffer_manager.read();

        let dm = bm.storage();

        // Read header and verify checksum
        let header = dm.read_header()?;
        if !header.verify_checksum() {
            return Err(PersistentARTrieError::CheckpointVerificationFailed {
                reason: format!(
                    "Header checksum mismatch after sync: stored={:#x}, computed={:#x}",
                    header.checksum,
                    header.compute_checksum()
                ),
            });
        }

        Ok(())
    }

    /// Persist the entire trie to disk
    ///
    /// This serializes the trie structure and writes it to the data file,
    /// updating the file header with the root pointer.
    pub fn persist_to_disk(&mut self) -> Result<()> {
        self.persist_to_disk_tracked().map(|_| ())
    }

    /// Like [`Self::persist_to_disk`], but when eviction is enabled it also
    /// builds a fresh [`DiskLocationRegistry`] mapping every serialized node's
    /// char-path to its on-disk location, returning it so the caller can publish
    /// it to the eviction coordinator once durability is verified. Returns `None`
    /// when eviction is disabled — zero overhead, no registry is built. The
    /// registry is a pure side-effect: the serialized bytes and the file header
    /// are identical whether or not it is collected, so recovery is unaffected.
    fn persist_to_disk_tracked(&mut self) -> Result<Option<DiskLocationRegistry>> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        use crate::persistent_artrie::NodeType;

        let mut eviction_registry = self
            .eviction_coordinator
            .as_ref()
            .map(|_| DiskLocationRegistry::new());

        // Get buffer manager
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for disk serialization")
        })?;

        // Serialize the trie root and get a descriptor
        let (root_type, root_ptr, is_final) = match &self.root {
            CharTrieRoot::Empty => (ROOT_TYPE_EMPTY, 0u64, false),
            CharTrieRoot::Node(node) => {
                // Recursively serialize the node and all children. `path` tracks
                // the char key-sequence from the root so each node registers its
                // disk location at its full path (eviction locates nodes by path).
                let mut path: Vec<char> = Vec::new();
                let ptr = self.serialize_char_node_to_disk(
                    node.as_ref(),
                    &mut path,
                    eviction_registry.as_mut(),
                )?;
                (ROOT_TYPE_NODE, ptr.to_raw(), node.is_final())
            }
        };

        // Flush arenas to disk FIRST to get their block_ids
        // (writes dirty arenas to buffer manager)
        // Uses slot-level incremental flush if configured, otherwise full arena flush
        if let Some(ref arena_manager) = self.arena_manager {
            let stats = arena_manager.write().flush_dirty_slots()?;
            if stats.partial_writes > 0 {
                log::debug!(
                    "Char incremental flush: {} full arenas, {} partial, {} slots, {} bytes written, {} bytes saved",
                    stats.full_arena_writes, stats.partial_writes, stats.slots_written,
                    stats.bytes_written, stats.bytes_saved
                );
            }
        }

        // Get arena count after flushing (block IDs are derived from sequential allocation)
        let arena_count: u32 = if let Some(ref arena_manager) = self.arena_manager {
            arena_manager.read().arena_count() as u32
        } else {
            0
        };

        // Create root descriptor (fixed 18 bytes)
        // Format:
        //   0: type (1 byte)
        //   1: is_final (1 byte)
        //   2-5: term_count (4 bytes, little endian)
        //   6-9: arena_count (4 bytes, little endian)
        //   10-17: root_ptr (8 bytes, little endian)
        //
        // Note: Arena block IDs are NOT stored - they are derived from sequential allocation:
        // Block 0 = file header + descriptor, Blocks 1..=arena_count = arenas
        let mut descriptor = [0u8; 18];
        descriptor[0] = root_type;
        descriptor[1] = if is_final { 1 } else { 0 };
        descriptor[2..6]
            .copy_from_slice(&(self.len.load(AtomicOrdering::Acquire) as u32).to_le_bytes());
        descriptor[6..10].copy_from_slice(&arena_count.to_le_bytes());
        descriptor[10..18].copy_from_slice(&root_ptr.to_le_bytes());

        // Write descriptor to fixed location in block 0 (offset 64, after file header)
        // This ensures arenas always occupy blocks 1, 2, 3, ... sequentially
        const DESCRIPTOR_OFFSET: usize = 64;
        let bm = buffer_manager.write();
        let dm = bm.storage();
        dm.write_bytes(0, DESCRIPTOR_OFFSET, &descriptor)?;

        // Update root_ptr to point to block 0, offset 64
        let root_descriptor_ptr =
            SwizzledPtr::on_disk(0, DESCRIPTOR_OFFSET as u32, NodeType::Bucket);
        dm.set_root_ptr(root_descriptor_ptr.to_raw())?;
        dm.set_entry_count(self.len.load(AtomicOrdering::Acquire) as u64)?;

        // Flush all pages to ensure durability. This publishes the root
        // descriptor, but checkpoint-level dirty state is cleared only after
        // the WAL checkpoint/rotation step succeeds in `checkpoint()`.
        bm.flush_all()?;
        dm.sync()?;
        Ok(eviction_registry)
    }

    /// Check if serialized children are consecutive in the same arena.
    ///
    /// For sequential sibling storage optimization: if all children are in the same arena
    /// and have consecutive slot IDs, we can store just `(first_slot, count)` instead of
    /// N separate pointers.
    ///
    /// # Arguments
    /// * `child_ptrs` - Child (key, SwizzledPtr) pairs from serialization
    /// * `parent_arena_id` - Arena ID where parent will be allocated
    ///
    /// # Returns
    /// `Some(first_child_slot)` if children are consecutive in same arena as parent,
    /// `None` otherwise.
    fn check_sequential_char_children(
        child_ptrs: &[(u32, SwizzledPtr)],
        parent_arena_id: u32,
        arena_node_count: u32,
    ) -> Option<super::arena_manager::ArenaSlot> {
        use super::arena_manager::ArenaSlot;

        if child_ptrs.len() < 2 {
            // Need at least 2 children for sequential optimization to be worthwhile
            return None;
        }

        // Collect arena slots from SwizzledPtrs
        let mut slots: Vec<ArenaSlot> = Vec::with_capacity(child_ptrs.len());
        for (_, ptr) in child_ptrs {
            // Get disk location from SwizzledPtr
            let loc = match ptr.disk_location() {
                Some(loc) => loc,
                None => return None, // All children must be on disk
            };
            let arena_id = loc.block_id;
            let slot_id = loc.offset;
            if arena_id != parent_arena_id {
                // All children must be in the same arena as parent
                return None;
            }
            slots.push(ArenaSlot::new(arena_id, slot_id));
        }

        // Sort by slot ID
        slots.sort_by_key(|s| s.slot_id);

        // Check if consecutive
        let first = slots[0];
        for (i, slot) in slots.iter().enumerate() {
            if slot.slot_id != first.slot_id + i as u32 {
                return None;
            }
        }

        // Verify first_slot + count won't overflow u32.
        // This prevents decode_sequential_siblings() from generating invalid slot IDs.
        // The last slot is first + (count - 1), so we check that doesn't overflow.
        let count = slots.len() as u32;
        if first.slot_id.checked_add(count.saturating_sub(1)).is_none() {
            return None; // Would overflow u32, use non-sequential encoding
        }

        // Verify last slot is within arena bounds.
        // This aligns with formal spec: first + count - 1 < arena_node_count
        // The overflow check above guarantees this subtraction is safe.
        let last_slot = first.slot_id + count - 1;
        if last_slot >= arena_node_count {
            return None; // Would exceed arena bounds, use non-sequential encoding
        }

        Some(first)
    }

    /// Serialize a CharTrieNodeInner to disk and return its SwizzledPtr
    ///
    /// Uses arena allocation for space-efficient storage. Multiple nodes are
    /// packed into each 256KB arena block instead of wasting one block per node.
    ///
    /// Node format on disk:
    /// ```text
    /// [CharNode serialized - 16-byte header + type-specific data]
    /// [value_len: u32]
    /// [value_bytes if value_len > 0]
    /// ```
    ///
    /// The SwizzledPtr uses:
    /// - arena_id as block_id (23 bits, up to 8M arenas)
    /// - slot_id as offset (22 bits, up to 4M slots per arena)
    fn serialize_char_node_to_disk(
        &self,
        node: &CharTrieNodeInner<V>,
        path: &mut Vec<char>,
        mut registry: Option<&mut DiskLocationRegistry>,
    ) -> Result<SwizzledPtr> {
        use super::relative_encoding::SerializationContext;
        use super::serialization_char::serialize_char_node_v2;

        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk serialization")
        })?;

        // Get the predicted parent slot for sequential sibling check
        let parent_arena_id = arena_manager.read().next_slot().arena_id;

        // First, recursively serialize all children and collect their disk pointers
        // Note: We handle both in-memory children (need serialization) and disk-backed
        // children (already have a disk pointer, just reuse it).
        let mut child_disk_ptrs: Vec<(u32, SwizzledPtr)> = Vec::with_capacity(node.num_children());
        for (key, child_ptr) in node.node.iter_children() {
            if child_ptr.is_null() {
                continue;
            }

            // Check if the child is already on disk (DiskRef) - just reuse its pointer
            if child_ptr.disk_location().is_some() {
                // Clone the SwizzledPtr to preserve its disk location
                child_disk_ptrs.push((key, child_ptr.clone()));
            } else if let Some(child_raw) = child_ptr.as_ptr::<CharTrieNodeInner<V>>() {
                // Child is in memory - serialize it recursively
                // Safety: ptr was created via Box::into_raw() from CharTrieNodeInner<V>
                let child = unsafe { &*child_raw };
                // Extend the path by this edge's char so the child registers its
                // own disk location at its full key path. Invalid codepoints
                // (should not occur in a char trie) skip path-tracking for that
                // subtree rather than corrupt the registry.
                let pushed = char::from_u32(key).map(|ch| path.push(ch)).is_some();
                let ptr = self.serialize_char_node_to_disk(child, path, registry.as_deref_mut())?;
                if pushed {
                    path.pop();
                }
                child_disk_ptrs.push((key, ptr));
            }
            // If neither disk_location nor as_ptr succeeds, skip this child
            // (should not happen in normal operation)
        }

        // Get the predicted parent slot and arena node count for encoding children
        let (parent_slot, arena_node_count) = {
            let mgr = arena_manager.read();
            let slot = mgr.next_slot();
            let node_count = mgr
                .get_arena(parent_arena_id)
                .map(|a| a.node_count())
                .unwrap_or(0);
            (slot, node_count)
        };

        // Check if children are consecutive (enables sequential sibling storage)
        // Create serialization context that determines encoding mode:
        // - Sequential: children stored as (first_slot, count) instead of N pointers
        // - Relative: child offsets encoded relative to parent (1-2 bytes vs 8 bytes)
        // - Full: absolute (arena_id, slot_id) for each child (9 bytes per child)
        //
        // IMPORTANT: If parent_slot.slot_id is small (especially 0), children serialized
        // in the previous arena(s) would have "negative" relative offsets, causing
        // decode underflow. Use full encoding to avoid this.
        let ctx = if parent_slot.slot_id < child_disk_ptrs.len() as u32 {
            // Parent slot is near the start of an arena - children likely in previous arena
            // Use full encoding to avoid relative offset underflow during decode
            SerializationContext::full_encoding(parent_slot)
        } else if let Some(first_child) = Self::check_sequential_char_children(
            &child_disk_ptrs,
            parent_arena_id,
            arena_node_count,
        ) {
            // Children are consecutive in same arena: use sequential sibling encoding
            SerializationContext::sequential(parent_slot, first_child)
        } else {
            // Children are not consecutive: use relative encoding only
            SerializationContext::new(parent_slot)
        };

        // Build a CharNode with disk pointers for serialization
        let disk_node = self.build_disk_char_node(&node.node, &child_disk_ptrs)?;

        // Serialize the value using bincode (needed regardless of encoding)
        let value_bytes: Vec<u8> = if let Some(ref value) = node.value {
            crate::serialization::bincode_compat::serialize(value).map_err(|e| {
                PersistentARTrieError::internal(&format!("Failed to serialize value: {}", e))
            })?
        } else {
            Vec::new()
        };

        // Serialize the CharNode to a buffer using v2 format with relative offsets
        let mut node_buffer = Vec::new();
        serialize_char_node_v2(&disk_node, &mut node_buffer, &ctx)?;

        // Build complete serialized data:
        // [node_buffer] + [value_len: u32] + [value_bytes]
        let build_data = |node_buf: &[u8], value_buf: &[u8]| -> Vec<u8> {
            let total_size = node_buf.len() + 4 + value_buf.len();
            let mut data = Vec::with_capacity(total_size);
            data.extend_from_slice(node_buf);
            data.extend_from_slice(&(value_buf.len() as u32).to_le_bytes());
            data.extend_from_slice(value_buf);
            data
        };

        let data = build_data(&node_buffer, &value_bytes);

        // Allocate in arena (space-efficient: packs many nodes per 256KB block)
        let slot = arena_manager.write().allocate(&data)?;

        // Check if arena overflow caused slot mismatch
        // If so, re-serialize using the actual slot to prevent relative encoding underflow
        let final_slot = if slot != ctx.parent_slot {
            // Arena overflow detected - need to re-serialize with correct parent slot
            // This happens when the predicted slot was in arena N, but allocation
            // went to arena N+1 due to arena being full
            //
            // Children are now likely in a different arena than the parent, requiring
            // cross-arena encoding (9 bytes per child) instead of relative encoding.
            let corrected_ctx = SerializationContext::new(slot);
            let mut corrected_buffer = Vec::new();
            serialize_char_node_v2(&disk_node, &mut corrected_buffer, &corrected_ctx)?;
            let corrected_data = build_data(&corrected_buffer, &value_bytes);

            if corrected_data.len() == data.len() {
                // Same size - can update in-place
                arena_manager.write().update(slot, &corrected_data)?;
                slot
            } else {
                // Different size (cross-arena encoding is larger) - allocate new slot
                // The original slot becomes wasted space (acceptable for rare overflow cases)
                arena_manager.write().allocate(&corrected_data)?
            }
        } else {
            slot
        };

        // Return pointer using arena addressing:
        // - block_id = arena_id + 1 (block 0 is file header, arena N is in block N+1)
        // - offset = slot_id
        let node_type = self.char_node_to_node_type(&disk_node);
        let result_ptr =
            SwizzledPtr::on_disk(final_slot.arena_id + 1, final_slot.slot_id, node_type);

        // Register this node's on-disk location so the eviction coordinator can
        // later reclaim its in-memory box (unswizzling it to this location).
        // Pure side-effect: `result_ptr` and the bytes written above are
        // identical whether or not the registry is present.
        if let Some(reg) = registry.as_deref_mut() {
            reg.register_char(
                path.clone(),
                result_ptr.clone(),
                data.len(),
                path.len(),
                node_type,
            );
        }

        Ok(result_ptr)
    }

    /// Build a CharNode with disk SwizzledPtrs for serialization.
    ///
    /// Creates a new CharNode of the same type as the original, but with
    /// children pointing to disk locations instead of in-memory nodes.
    ///
    /// Returns `Err` only if the rebuilt node's `add_child_growing` exceeds
    /// capacity — that indicates corruption (the original held that many
    /// children, so a same-type rebuild cannot fail to hold them) and the
    /// caller propagates the error up the serialization stack rather than
    /// crashing.
    fn build_disk_char_node(
        &self,
        original: &CharNode,
        disk_children: &[(u32, SwizzledPtr)],
    ) -> Result<CharNode> {
        use super::nodes::{CharBucket, CharNode16, CharNode4, CharNode48};

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
            new_header.version = orig_header.version;
        }

        // Copy prefix
        *new_node.prefix_mut() = *original.prefix();

        // Add disk children
        for &(key, ref ptr) in disk_children {
            new_node.add_child_growing(key, ptr.clone()).map_err(|e| {
                PersistentARTrieError::internal(&format!(
                    "build_disk_char_node: rebuilt node rejected child key {:#x} (Node type same \
                     as source): {:?} — indicates corruption in source node's child count",
                    key, e
                ))
            })?;
        }

        Ok(new_node)
    }

    /// Map CharNode type to NodeType for SwizzledPtr
    fn char_node_to_node_type(&self, node: &CharNode) -> NodeType {
        match node {
            CharNode::N4(_) => NodeType::CharNode4,
            CharNode::N16(_) => NodeType::CharNode16,
            CharNode::N48(_) => NodeType::CharNode48,
            CharNode::Bucket(_) => NodeType::CharBucket,
        }
    }
}
