//! Disk loading + child resolution helpers for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` as a Phase-6 char sub-module. After the
//! L3.3c owned-tree deletion, only the overlay / enumerate read path survives:
//!
//! - `read_char_record_fields` / `enumerate_char_terms_from_disk` — the L3.1
//!   dense→(term, value) enumerator (the sole reopen path, fed to the overlay
//!   builder; NO transient owned `CharTrieRoot`).
//! - `load_char_node_from_disk_lazy` — single-node lazy decode (children stay
//!   `SwizzledPtr`), reused by the overlay fault-in primitive.
//! - `load_overlay_node_from_disk` — the overlay fault-in load+deserialize
//!   primitive (decode via the lazy decoder, then `inner_to_overlay`).
//!
//! These bridge the on-disk arena image (`CharTrieNodeInner<V>` decode +
//! `SwizzledPtr`) to the lock-free overlay, loaded via `BufferManager` /
//! `ArenaManager`.

#![allow(dead_code)]

/// Everything a full dense-image scan yields: every `(term, value)` pair keyed by
/// its char units, the root's own value (outer `Option` = "root carried a value at
/// all", inner = the value itself), and the highest node id observed.
type DenseScan<V> = (
    std::collections::BTreeMap<Vec<u32>, Option<V>>,
    Option<Option<V>>,
    u64,
);

use std::sync::Arc;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::buffer_manager::BufferManager;
use crate::persistent_artrie::core::eviction::{
    DiskRecordAddress, DurableRecordRef, DurableRegistryRecord,
};
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;

use super::dict_impl_char::{ROOT_TYPE_EMPTY, ROOT_TYPE_NODE};
use super::types::{CharTrieNodeInner, NonResidentCharChild};

const ROOT_DESCRIPTOR_OFFSET: usize = 64;
const ROOT_DESCRIPTOR_LEN: usize = 18;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// Read one exact durable char-node record without deserializing `V` and
    /// without manufacturing a node type for relative child references.
    pub(crate) fn read_char_registry_record(
        &self,
        record_ref: DurableRecordRef,
    ) -> Result<DurableRegistryRecord<u32>> {
        use super::arena_manager::ArenaSlot;
        use super::serialization_char::{
            decode_char_node_metadata, DecodedCharMetadataChild, DeserializationContext,
        };

        let arena_id = record_ref.address.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::corrupted(
                "char registry metadata record uses reserved block zero",
            )
        })?;
        let slot = ArenaSlot::new(arena_id, record_ref.address.slot_id);
        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for char registry metadata read")
        })?;
        let arena = arena_manager.read();
        let record_bytes = arena.read(slot)?;
        let metadata = decode_char_node_metadata(
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
                    "char registry metadata child references",
                    metadata.children.len(),
                    error,
                )
            })?;
        for (edge, child) in metadata.children {
            let child_ref = match child {
                DecodedCharMetadataChild::Typed(pointer) => {
                    DurableRecordRef::from_typed_pointer(&pointer)?
                }
                DecodedCharMetadataChild::Untyped(slot) => {
                    let block_id = slot.arena_id.checked_add(1).ok_or_else(|| {
                        PersistentARTrieError::corrupted(
                            "char metadata child arena id exceeds persistent block range",
                        )
                    })?;
                    DurableRecordRef::untyped(DiskRecordAddress {
                        block_id,
                        slot_id: slot.slot_id,
                    })
                }
            };
            children.push((edge, child_ref));
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

    /// **L3.1 — read ONE char node RECORD's fields (no `CharTrieNodeInner`).**
    /// `load_char_node_from_disk_lazy` minus the owned-node construction: decode the dense
    /// `CharNode` record via the proven `deserialize_char_node_v2`, read its value blob, and
    /// extract `(is_final, value, prefix_units, child_edges+ptrs)`. `prefix_units` are the
    /// node's CX compressed-prefix code points (`prefix_len == 0` for every current char image —
    /// char has no `compact()` — so the fold is a defensive no-op, but it mirrors
    /// `load_char_node_from_disk_lazy`'s CX-#43 prefix preservation). Edges/prefix are `u32`
    /// (`CharKey::Unit`).
    #[allow(clippy::type_complexity)]
    fn read_char_record_fields(
        &self,
        node_ptr: &SwizzledPtr,
    ) -> Result<(bool, Option<V>, Vec<u32>, Vec<(u32, SwizzledPtr)>)> {
        use super::arena_manager::ArenaSlot;
        use super::serialization_char::{
            char_record_node_type, deserialize_char_node_record, resolve_legacy_v2_child_types,
            DeserializationContext,
        };
        use std::io::Cursor;

        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("L3.1 char enumerate: no arena manager")
        })?;
        let disk_loc = node_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("L3.1 char enumerate: swizzled/null node ptr")
        })?;
        let arena_id = disk_loc.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::corrupted(
                "L3.1 char enumerate: invalid block_id 0 for arena node",
            )
        })?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);

        let am = arena_manager.read();
        let node_data = am.read(slot)?;

        let deser_ctx = DeserializationContext::new(slot);
        let mut cursor = Cursor::new(node_data);
        let (mut char_node, wire_header) = deserialize_char_node_record(&mut cursor, &deser_ctx)?;
        resolve_legacy_v2_child_types(&wire_header, &mut char_node, |child_slot| {
            char_record_node_type(am.read(child_slot)?)
        })?;

        // Value blob follows the node bytes (variable-size v2): [value_len:u32][value_bytes].
        let offset = cursor.position() as usize;
        if offset + 4 > node_data.len() {
            return Err(PersistentARTrieError::corrupted(
                "L3.1 char enumerate: value_len extends past node record",
            ));
        }
        let value_len = u32::from_le_bytes([
            node_data[offset],
            node_data[offset + 1],
            node_data[offset + 2],
            node_data[offset + 3],
        ]) as usize;
        let value: Option<V> = if value_len > 0 {
            let value_start = offset + 4;
            let value_end = value_start.checked_add(value_len).ok_or_else(|| {
                PersistentARTrieError::corrupted("L3.1 char enumerate: value length overflow")
            })?;
            if value_end > node_data.len() {
                return Err(PersistentARTrieError::corrupted(
                    "L3.1 char enumerate: value bytes extend past node record",
                ));
            }
            Some(
                crate::serialization::bincode_compat::deserialize(
                    &node_data[value_start..value_end],
                )
                .map_err(|e| {
                    PersistentARTrieError::corrupted(format!(
                        "L3.1 char enumerate: value deserialize failed: {}",
                        e
                    ))
                })?,
            )
        } else {
            None
        };

        let is_final = char_node.is_final();
        let plen = char_node.header().prefix_len as usize;
        let prefix_units: Vec<u32> = char_node.prefix().chars[..plen].to_vec();
        let children: Vec<(u32, SwizzledPtr)> = char_node
            .iter_children()
            .filter(|(_, ptr)| !ptr.is_null())
            .map(|(key, ptr)| (key, ptr.clone()))
            .collect();

        drop(am);
        Ok((is_final, value, prefix_units, children))
    }

    /// **L3.1 — direct dense→(term, value) enumerator (NO `CharTrieRoot`/`CharTrieNodeInner`).**
    ///
    /// The char twin of byte `enumerate_terms_from_disk`. Reads the dense char image (root
    /// descriptor + `CharNode` records — char has NO `compact()`/node-prefix format-1 and NO
    /// suffix-bucket: `CharBucket` is a single-char-edge FAN-OUT node decoded natively by
    /// `deserialize_char_node_v2`) DIRECTLY into a `(Vec<u32> → Option<V>)` map + the empty term
    /// "", WITHOUT materializing a `CharTrieRoot`. Fed to `build_overlay_root_from_terms` it
    /// yields the resident overlay root; at L3.3 it lets `load_root_from_disk` + `CharTrieRoot` +
    /// `CharTrieNodeInner` + the char owned readers be deleted.
    ///
    /// ONE eager DFS (explicit work-stack). UNIFORM root+child handling: every node reads its
    /// `(is_final, value, prefix, children)` from its RECORD (char root finality/value live on
    /// the record, NOT the descriptor — matching `load_root_from_disk`'s `CharTrieRoot::Node`),
    /// folds its prefix into the path, yields its value-or-`None` ONCE (membership∪value
    /// intrinsic), and pushes children at `+edge`. Loads the arenas first (the char ctor does
    /// NOT — only `load_root_from_disk` did, which this replaces).
    pub(super) fn enumerate_char_terms_from_disk(
        &self,
        buffer_manager: &Arc<RwLock<BufferManager<S>>>,
        root_ptr: u64,
    ) -> Result<DenseScan<V>> {
        use std::collections::BTreeMap;

        // (1) Read + parse the 18-byte root descriptor (block 0 offset 64) — mirror
        // `load_root_from_disk`'s prologue.
        let root_desc = SwizzledPtr::from_raw(root_ptr);
        let disk_loc = root_desc.disk_location().ok_or_else(|| {
            PersistentARTrieError::internal("L3.1 char enumerate: swizzled/null root ptr")
        })?;
        if disk_loc.block_id != 0 || disk_loc.offset as usize != ROOT_DESCRIPTOR_OFFSET {
            return Err(PersistentARTrieError::corrupted(format!(
                "L3.1 char enumerate: root descriptor must target block 0 offset {}, got block {} offset {}",
                ROOT_DESCRIPTOR_OFFSET, disk_loc.block_id, disk_loc.offset
            )));
        }
        let (root_type, term_count, arena_count, data_ptr) = {
            let bm = buffer_manager.read();
            let page_guard = bm.fetch_page(disk_loc.block_id)?;
            let page_data = page_guard.data();
            let offset = disk_loc.offset as usize;
            let end = offset.checked_add(ROOT_DESCRIPTOR_LEN).ok_or_else(|| {
                PersistentARTrieError::corrupted("L3.1 char enumerate: descriptor offset overflow")
            })?;
            if end > page_data.len() {
                return Err(PersistentARTrieError::corrupted(
                    "L3.1 char enumerate: descriptor extends past header block",
                ));
            }
            let mut d = [0u8; ROOT_DESCRIPTOR_LEN];
            d.copy_from_slice(&page_data[offset..end]);
            if d[1] > 1 {
                return Err(PersistentARTrieError::corrupted(format!(
                    "L3.1 char enumerate: invalid root final flag {}",
                    d[1]
                )));
            }
            let term_count = u32::from_le_bytes([d[2], d[3], d[4], d[5]]) as u64;
            let arena_count = u32::from_le_bytes([d[6], d[7], d[8], d[9]]);
            let data_ptr =
                u64::from_le_bytes([d[10], d[11], d[12], d[13], d[14], d[15], d[16], d[17]]);
            let storage_block_count = bm.storage().block_count()?;
            if arena_count > storage_block_count.saturating_sub(1) {
                return Err(PersistentARTrieError::corrupted(format!(
                    "L3.1 char enumerate: arena_count {} exceeds available arena blocks {}",
                    arena_count,
                    storage_block_count.saturating_sub(1)
                )));
            }
            (d[0], term_count, arena_count, data_ptr)
        };

        // (2) Load the arenas (mirror `load_root_from_disk`:118-134) — the char ctor does not.
        if arena_count > 0 {
            if let Some(ref arena_manager) = self.arena_manager {
                let mut am = arena_manager.write();
                am.clear_for_loading();
                for block_id in 1..=arena_count {
                    am.load_arena(block_id)?;
                }
                let count = am.arena_count();
                am.set_active_arena(count.saturating_sub(1));
            }
        }

        // (3) Walk.
        let mut all: BTreeMap<Vec<u32>, Option<V>> = BTreeMap::new();
        match root_type {
            ROOT_TYPE_EMPTY => { /* no records, no terms */ }
            ROOT_TYPE_NODE => {
                // UNIFORM DFS from the root record. A frame is `(node_ptr, path-to-this-node)`.
                let mut stack: Vec<(SwizzledPtr, Vec<u32>)> =
                    vec![(SwizzledPtr::from_raw(data_ptr), Vec::new())];
                while let Some((ptr, parent_path)) = stack.pop() {
                    let (is_final, value, prefix_units, children) =
                        self.read_char_record_fields(&ptr)?;
                    let mut here = parent_path;
                    here.extend_from_slice(&prefix_units);
                    if is_final {
                        all.insert(here.clone(), value);
                    }
                    for (edge, child_ptr) in children.into_iter().rev() {
                        let mut p = here.clone();
                        p.push(edge);
                        stack.push((child_ptr, p));
                    }
                }
            }
            other => {
                return Err(PersistentARTrieError::corrupted(format!(
                    "L3.1 char enumerate: unknown root type {}",
                    other
                )));
            }
        }

        // (4) Split out the empty term "".
        let empty_term: Option<Option<V>> = all.remove(&Vec::<u32>::new());
        Ok((all, empty_term, term_count))
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
        use super::serialization_char::{
            char_record_node_type, deserialize_char_node_record, resolve_legacy_v2_child_types,
            DeserializationContext,
        };
        use std::io::Cursor;

        let arena_manager = self
            .arena_manager
            .as_ref()
            .ok_or_else(|| PersistentARTrieError::internal("No arena manager for disk reading"))?;

        // Get arena slot from the disk location
        // block_id = arena_id + 1 (block 0 is file header)
        // offset = slot_id
        let disk_loc = node_ptr
            .disk_location()
            .ok_or_else(|| PersistentARTrieError::internal("Node pointer is swizzled or null"))?;
        let arena_id = disk_loc
            .block_id
            .checked_sub(1)
            .ok_or_else(|| PersistentARTrieError::internal("Invalid block_id 0 for arena node"))?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);

        // Read from arena
        let am = arena_manager.read();

        let node_data = am.read(slot)?;

        // Deserialize the CharNode using v2 format with context
        let deser_ctx = DeserializationContext::new(slot);
        let mut cursor = Cursor::new(node_data);
        let (mut char_node, wire_header) = deserialize_char_node_record(&mut cursor, &deser_ctx)?;
        resolve_legacy_v2_child_types(&wire_header, &mut char_node, |child_slot| {
            char_record_node_type(am.read(child_slot)?)
        })?;

        // Use cursor position to find where value data starts (v2 format is variable size).
        let offset = cursor.position() as usize;
        let value_length_end = offset.checked_add(4).ok_or_else(|| {
            PersistentARTrieError::corrupted("char value-length field offset overflow")
        })?;
        if value_length_end > node_data.len() {
            return Err(PersistentARTrieError::corrupted(
                "char value-length field extends beyond its node record",
            ));
        }

        // Read value_len and value_bytes
        let value_len = u32::from_le_bytes([
            node_data[offset],
            node_data[offset + 1],
            node_data[offset + 2],
            node_data[offset + 3],
        ]) as usize;

        let value: Option<V> = if value_len > 0 {
            let value_start = value_length_end;
            let value_end = value_start.checked_add(value_len).ok_or_else(|| {
                PersistentARTrieError::corrupted("char value byte range overflow")
            })?;
            if value_end > node_data.len() {
                return Err(PersistentARTrieError::corrupted(
                    "char value bytes extend beyond their node record",
                ));
            }
            let value_bytes = &node_data[value_start..value_end];
            Some(
                crate::serialization::bincode_compat::deserialize(value_bytes).map_err(|e| {
                    PersistentARTrieError::corrupted(format!(
                        "failed to deserialize char value: {e}"
                    ))
                })?,
            )
        } else {
            None
        };

        // Validate every key and nonresident pointer before constructing an
        // owning projection. Invalid Unicode must not be silently omitted.
        let mut child_data = Vec::new();
        child_data
            .try_reserve_exact(char_node.num_children())
            .map_err(|error| {
                PersistentARTrieError::allocation_failed(
                    "decoded char child metadata",
                    char_node.num_children(),
                    error,
                )
            })?;
        for (key, pointer) in char_node.iter_children() {
            let character = char::from_u32(key).ok_or_else(|| {
                PersistentARTrieError::corrupted(format!(
                    "decoded char child key {key:#x} is not a Unicode scalar value"
                ))
            })?;
            let child = NonResidentCharChild::try_from(pointer.clone()).map_err(|error| {
                PersistentARTrieError::corrupted(format!(
                    "decoded child {character:?} has invalid pointer state: {error}"
                ))
            })?;
            child_data.push((character, child));
        }

        drop(am);

        // Create the node
        let is_final = char_node.is_final();
        let mut result = CharTrieNodeInner::new();
        result.set_final(is_final);
        result.value = value;

        // CX/#43 (4A): preserve and validate the decoded path-compression prefix.
        // `inner_to_overlay` expands it into a traversal-aware chain.
        let prefix_length = char_node.header().prefix_len as usize;
        if prefix_length > super::nodes::CHAR_MAX_PREFIX_LEN {
            return Err(PersistentARTrieError::corrupted(format!(
                "decoded char prefix length {prefix_length} exceeds capacity {}",
                super::nodes::CHAR_MAX_PREFIX_LEN
            )));
        }
        let prefix = char_node.prefix().as_slice(prefix_length);
        result.set_compressed_prefix(prefix).map_err(|error| {
            PersistentARTrieError::corrupted(format!(
                "decoded char node has invalid compressed prefix: {error}"
            ))
        })?;

        // Insert decoded nonresident children without loading them. The owned
        // projection rejects resident/transitional states and duplicate keys so
        // malformed durable topology cannot manufacture Box ownership.
        for (c, child) in child_data {
            result
                .try_add_nonresident_child(c, child)
                .map_err(|error| {
                    PersistentARTrieError::corrupted(format!(
                        "invalid decoded child {c:?} in char node: {error}"
                    ))
                })?;
        }

        Ok(result)
    }

    /// Load an `OnDisk` overlay child back into an immutable overlay node
    /// (`Arc<PersistentCharNode<V>>`) — the **fault-in load+deserialize primitive**
    /// (design §2). Reuses the production/recovery-tested lazy decoder
    /// [`Self::load_char_node_from_disk_lazy`] (do NOT hand-roll a byte reader),
    /// then converts the decoded owned `CharTrieNodeInner<V>` into an overlay node
    /// via [`super::persist::inner_to_overlay`] (children stay `Child::OnDisk`, so
    /// the fault is single-level / lazy — exactly the overlay granularity).
    ///
    /// The returned node's finality / value / child-set equal the durable image's
    /// (`load(serialize(overlay_to_inner(n))) ≡ n`, design §2 round-trip), so a
    /// faulted node can never manufacture or drop a term. The caller (read-path
    /// `find_leaf_faulting` / write-path `build_path_recursive` OnDisk arm)
    /// CAS-installs it `InMem` via the single loser-safe root CAS — fault-in itself
    /// writes nothing to disk and advances no watermark (no-lost-write preserved,
    /// design §5).
    ///
    /// ZERO new `unsafe`: the only `unsafe` reached is the EXISTING lazy loader's,
    /// called through this safe `&self` boundary; the conversion is pure node
    /// copies + `Arc` allocation.
    ///
    /// **Flip F0:** un-gated to production. The fault-in read/write wiring that
    /// consumes it is now a production path (`route_overlay()`), so evicted overlay
    /// nodes must be loadable unconditionally.
    pub(super) fn load_overlay_node_from_disk(
        &self,
        disk_ptr: &SwizzledPtr,
    ) -> Result<Arc<super::nodes::PersistentCharNode<V>>> {
        let bm = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for overlay fault-in load")
        })?;
        let inner = self.load_char_node_from_disk_lazy(bm, disk_ptr)?;
        let top = super::persist::inner_to_overlay::<V>(&inner)?;
        // `top` is the exact in-memory representation of `disk_ptr`: either the decoded
        // uncompressed node or the head of a losslessly expanded compressed span. Stamp both cases
        // before publication so a winning fault can later be re-evicted. This does not grant
        // eviction authority by itself: the evictor also revalidates the exact root/registry
        // generation, path, disk address, and residency. All structural path copies clear their
        // stamp, preserving the stale-image guard after a write.
        top.set_durable_stamp(disk_ptr.to_raw());
        Ok(Arc::new(top))
    }
}
