//! Vocab overlay → disk serializer (V2 of the overlay flip).
//!
//! A faithful port of char's proven `serialize_overlay_to_disk_iterative`
//! (`persistent_artrie_char::persist`) — the ITERATIVE post-order (work-stack) arena DFS —
//! instantiated at the concrete overlay value `u64`. It REUSES vocab's existing per-node
//! char serialization (`build_disk_char_node_static` + `serialize_char_node_v2`), so the
//! on-disk node format is byte-identical to the CHAR arena v2 format. That is deliberate:
//! reopen (V5) reads it back with char's `enumerate_char_terms_from_disk`, which is the
//! exact inverse of `serialize_char_node_v2` + the appended `[value_len:u32][value_bytes]`.
//!
//! Differences from char's serializer, all SAFE:
//! - **No `DiskLocationRegistry` / `durable_stamp`** — vocab's overlay is never evicted
//!   (`OverlayFaulter::fault_overlay_slot` returns `None`), so there is no eviction
//!   coordinator to register on-disk locations with. Char's own contract notes the written
//!   bytes + returned ptr are IDENTICAL whether or not the registry is present, so dropping
//!   it does not change the image.
//! - **Fixed `NodeType::CharNode4` in the disk `SwizzledPtr`** — matches vocab's existing
//!   `serialize_vocab_node_to_disk`; the authoritative node type is encoded in the node
//!   bytes (read back by the deserializer), so the ptr's type tag is an unused disk hint.

use std::sync::Arc;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
use crate::persistent_artrie::NodeType;
use crate::persistent_artrie_char::arena_manager::ArenaSlot;
use crate::persistent_artrie_char::nodes::persistent_node::PersistentCharNode;
use crate::persistent_artrie_char::nodes::CharNode;
use crate::persistent_artrie_char::persist::overlay_inner_single_node_with_prefix;
use crate::persistent_artrie_char::relative_encoding::SerializationContext;
use crate::persistent_artrie_char::serialization_char::{
    deserialize_char_node_v2, serialize_char_node_v2, DeserializationContext,
};
use crate::persistent_artrie_char::types::CharTrieNodeInner;
use crate::persistent_artrie_core::eviction::DiskLocationRegistry;
use crate::persistent_artrie_core::key_encoding::CharKey;
use crate::persistent_artrie_core::overlay::compressed_serialize::OverlayCompressedSerialize;

// The vocab overlay node = char overlay node at V = u64 (the vocabulary index).
type VocabOverlayNode = PersistentCharNode<u64>;

impl<S: BlockStorage> super::dict_impl::PersistentVocabARTrie<S> {
    /// Build a `CharNode` with disk `SwizzledPtr`s for serialization.
    ///
    /// Relocated from the deleted owned `disk_io.rs` (V6) — the overlay serializer is now the
    /// sole reuser. `Self::`-associated (no `self`/`S`); the overlay image must be byte-identical
    /// to the CHAR arena v2 format.
    pub(super) fn build_disk_char_node_static(
        original: &CharNode,
        disk_children: &[(u32, SwizzledPtr)],
    ) -> CharNode {
        use crate::persistent_artrie_char::nodes::{CharBucket, CharNode16, CharNode4, CharNode48};

        let mut new_node = match original {
            CharNode::N4(_) => CharNode::N4(Box::new(CharNode4::new())),
            CharNode::N16(_) => CharNode::N16(Box::new(CharNode16::new())),
            CharNode::N48(_) => CharNode::N48(Box::new(CharNode48::new())),
            CharNode::Bucket(_) => CharNode::Bucket(Box::new(CharBucket::new())),
        };

        {
            let new_header = new_node.header_mut();
            let orig_header = original.header();
            new_header.prefix_len = orig_header.prefix_len;
            new_header.flags = orig_header.flags;
        }

        *new_node.prefix_mut() = *original.prefix();

        for &(key, ref ptr) in disk_children {
            match new_node.add_child_growing(key, ptr.clone()) {
                Ok(Some(grown)) => new_node = grown,
                Ok(None) => {}
                Err(e) => {
                    eprintln!(
                        "Warning: failed to add child in build_disk_char_node_static: {:?}",
                        e
                    );
                }
            }
        }

        new_node
    }

    /// Serialize the immutable overlay rooted at `root` into the dense char-arena image,
    /// returning the root `SwizzledPtr`. ITERATIVE post-order (work-stack) so depth does not
    /// recurse with branch depth. Each node's children are resolved to disk ptrs BEFORE the
    /// node itself is serialized (post-order invariant). Mirrors char's
    /// `serialize_overlay_to_disk_iterative`.
    // Uncompressed serializer: SUPERSEDED in production by `serialize_overlay_snapshot_compressed`
    // (CX-universal). Retained as the baseline for the V2-serialize round-trip test.
    #[cfg(test)]
    pub(super) fn serialize_overlay_to_disk(
        &self,
        root: &Arc<VocabOverlayNode>,
    ) -> Result<SwizzledPtr> {
        // A pending child slot in a parent frame: the edge `key` awaiting the disk ptr its
        // in-mem subtree will produce (`None` until that subtree completes).
        struct PendingChild {
            key: u32,
            ptr: Option<SwizzledPtr>,
        }
        // A work-stack frame: one overlay node mid-descent, held by OWNED `Arc`.
        struct Frame {
            node: Arc<VocabOverlayNode>,
            parent_key: Option<u32>,
            parent_pushed_path: bool,
            // In-mem children still to descend into, REVERSED so `pop()` yields ascending
            // `iter_children()` order (matches the recursive DFS).
            pending_in_mem: Vec<(u32, Arc<VocabOverlayNode>)>,
            // All child slots in `iter_children()` order; in-mem slots start `None`, on-disk
            // slots pre-filled (NULL on-disk fillers skipped).
            slots: Vec<PendingChild>,
        }
        fn make_frame(
            node: Arc<VocabOverlayNode>,
            parent_key: Option<u32>,
            parent_pushed_path: bool,
        ) -> Frame {
            let n = node.num_children();
            let mut slots: Vec<PendingChild> = Vec::with_capacity(n);
            let mut pending_in_mem: Vec<(u32, Arc<VocabOverlayNode>)> = Vec::with_capacity(n);
            for (&key, child) in node.iter_children() {
                if let Some(child_arc) = child.as_in_mem() {
                    slots.push(PendingChild { key, ptr: None });
                    pending_in_mem.push((key, Arc::clone(child_arc)));
                } else if let Some(on_disk) = child.as_on_disk() {
                    if !on_disk.is_null() {
                        slots.push(PendingChild {
                            key,
                            ptr: Some(on_disk.clone()),
                        });
                    }
                }
            }
            pending_in_mem.reverse();
            Frame {
                node,
                parent_key,
                parent_pushed_path,
                pending_in_mem,
                slots,
            }
        }

        // The full key path of the CURRENT node, maintained exactly as the recursive walk
        // (edge char pushed before descending into an in-mem child).
        let mut path: Vec<char> = Vec::new();
        let mut stack: Vec<Frame> = Vec::new();
        stack.push(make_frame(Arc::clone(root), None, false));
        let mut completed: Option<(u32, SwizzledPtr)> = None;

        loop {
            let frame = stack
                .last_mut()
                .expect("serialize_overlay_to_disk: non-empty work-stack");

            if let Some((key, ptr)) = completed.take() {
                let slot = frame
                    .slots
                    .iter_mut()
                    .find(|s| s.key == key && s.ptr.is_none())
                    .expect("completed child key has a matching unfilled parent slot");
                slot.ptr = Some(ptr);
            }

            if let Some((key, child_arc)) = frame.pending_in_mem.pop() {
                let pushed = char::from_u32(key).map(|ch| path.push(ch)).is_some();
                stack.push(make_frame(child_arc, Some(key), pushed));
                continue;
            }

            // All children resolved → serialize THIS node.
            let frame = stack
                .pop()
                .expect("serialize_overlay_to_disk: frame to finalize");
            let child_disk_ptrs: Vec<(u32, SwizzledPtr)> = frame
                .slots
                .into_iter()
                .map(|s| {
                    (
                        s.key,
                        s.ptr.expect(
                            "every in-mem child slot is filled before its parent node is \
                             serialized (post-order invariant)",
                        ),
                    )
                })
                .collect();
            let inner = overlay_inner_single_node(frame.node.as_ref(), &child_disk_ptrs);
            let node_ptr = self.serialize_one_overlay_node(&inner, &child_disk_ptrs)?;
            // NB no `durable_stamp` (vocab overlay is never evicted; see module docs).

            if frame.parent_pushed_path {
                path.pop();
            }
            match frame.parent_key {
                Some(key) => completed = Some((key, node_ptr)),
                None => return Ok(node_ptr),
            }
        }
    }

    /// CX-universal: PATH-COMPRESSED overlay serialize. Vocab is EVICTION-OFF (overlays are never
    /// evicted), so this forwards to the ONE generic `OverlayCompressedSerialize::serialize_compressed_loop`
    /// with NO registry; the per-variant seams in the `impl OverlayCompressedSerialize` block below
    /// supply vocab's char-arena projection + per-node serialize. The proven no-truncation chunking +
    /// edge convention now live ONCE in the shared loop (not re-ported here).
    pub(super) fn serialize_overlay_snapshot_compressed(
        &self,
        root: &Arc<VocabOverlayNode>,
    ) -> Result<SwizzledPtr> {
        OverlayCompressedSerialize::<CharKey, u64>::serialize_compressed_loop(self, root, None)
    }

    /// Serialize ONE overlay node (children ALREADY resolved to disk ptrs) into the arena, in
    /// the CHAR v2 format: `serialize_char_node_v2(node)` bytes, then `[value_len:u32]` +
    /// `[bincode(value)]`. Mirrors char's `serialize_one_char_node_to_disk` MINUS the registry.
    fn serialize_one_overlay_node(
        &self,
        node: &CharTrieNodeInner<u64>,
        child_disk_ptrs: &[(u32, SwizzledPtr)],
    ) -> Result<SwizzledPtr> {
        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for overlay serialization")
        })?;

        let parent_arena_id = arena_manager.read().next_slot().arena_id;
        let (parent_slot, arena_node_count) = {
            let mgr = arena_manager.read();
            let slot = mgr.next_slot();
            let node_count = mgr
                .get_arena(parent_arena_id)
                .map(|a| a.node_count())
                .unwrap_or(0);
            (slot, node_count)
        };

        // Encoding mode: full (avoids relative-offset underflow when the parent is near an
        // arena start), else sequential (consecutive same-arena children), else relative.
        let ctx = if parent_slot.slot_id < child_disk_ptrs.len() as u32 {
            SerializationContext::full_encoding(parent_slot)
        } else if let Some(first_child) =
            check_sequential_char_children(child_disk_ptrs, parent_arena_id, arena_node_count)
        {
            SerializationContext::sequential(parent_slot, first_child)
        } else {
            SerializationContext::new(parent_slot)
        };

        let disk_node = Self::build_disk_char_node_static(&node.node, child_disk_ptrs);

        let value_bytes: Vec<u8> = if let Some(ref value) = node.value {
            crate::serialization::bincode_compat::serialize(value).map_err(|e| {
                PersistentARTrieError::internal(&format!("Failed to serialize value: {}", e))
            })?
        } else {
            Vec::new()
        };

        let mut node_buffer = Vec::new();
        serialize_char_node_v2(&disk_node, &mut node_buffer, &ctx)?;

        let build_data = |node_buf: &[u8], value_buf: &[u8]| -> Vec<u8> {
            let total_size = node_buf.len() + 4 + value_buf.len();
            let mut data = Vec::with_capacity(total_size);
            data.extend_from_slice(node_buf);
            data.extend_from_slice(&(value_buf.len() as u32).to_le_bytes());
            data.extend_from_slice(value_buf);
            data
        };

        let data = build_data(&node_buffer, &value_bytes);
        let slot = arena_manager.write().allocate(&data)?;

        // Arena-overflow re-serialize: if allocation landed in a different slot than predicted,
        // re-encode with the actual slot to keep relative offsets valid.
        let final_slot = if slot != ctx.parent_slot {
            let corrected_ctx = SerializationContext::new(slot);
            let mut corrected_buffer = Vec::new();
            serialize_char_node_v2(&disk_node, &mut corrected_buffer, &corrected_ctx)?;
            let corrected_data = build_data(&corrected_buffer, &value_bytes);
            if corrected_data.len() == data.len() {
                arena_manager.write().update(slot, &corrected_data)?;
                slot
            } else {
                arena_manager.write().allocate(&corrected_data)?
            }
        } else {
            slot
        };

        // block_id = arena_id + 1 (block 0 is the file header); the type tag is an unused disk
        // hint (the real type is in the node bytes), so a fixed CharNode4 matches vocab's owned
        // serializer.
        Ok(SwizzledPtr::on_disk(
            final_slot.arena_id + 1,
            final_slot.slot_id,
            NodeType::CharNode4,
        ))
    }

    /// Read ONE overlay record's fields from the arena (the inverse of
    /// `serialize_one_overlay_node`): deserialize the `CharNode`, then the appended
    /// `[value_len:u32][bincode value]`. Verbatim port of char's `read_char_record_fields`
    /// at `V = u64`.
    fn read_overlay_record_fields(
        &self,
        node_ptr: &SwizzledPtr,
    ) -> Result<(bool, Option<u64>, Vec<u32>, Vec<(u32, SwizzledPtr)>)> {
        use std::io::Cursor;

        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("vocab overlay enumerate: no arena manager")
        })?;
        let disk_loc = node_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("vocab overlay enumerate: swizzled/null node ptr")
        })?;
        let arena_id = disk_loc.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::corrupted("vocab overlay enumerate: invalid block_id 0")
        })?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);

        let am = arena_manager.read();
        let node_data = am.read(slot)?;
        let deser_ctx = DeserializationContext::new(slot);
        let mut cursor = Cursor::new(node_data);
        let char_node = deserialize_char_node_v2(&mut cursor, &deser_ctx)?;

        // Value blob follows the node bytes (variable-size v2): [value_len:u32][value_bytes].
        let offset = cursor.position() as usize;
        if offset + 4 > node_data.len() {
            return Err(PersistentARTrieError::corrupted(
                "vocab overlay enumerate: value_len extends past node record",
            ));
        }
        let value_len = u32::from_le_bytes([
            node_data[offset],
            node_data[offset + 1],
            node_data[offset + 2],
            node_data[offset + 3],
        ]) as usize;
        let value: Option<u64> = if value_len > 0 {
            let value_start = offset + 4;
            let value_end = value_start.checked_add(value_len).ok_or_else(|| {
                PersistentARTrieError::corrupted("vocab overlay enumerate: value length overflow")
            })?;
            if value_end > node_data.len() {
                return Err(PersistentARTrieError::corrupted(
                    "vocab overlay enumerate: value bytes extend past node record",
                ));
            }
            Some(
                crate::serialization::bincode_compat::deserialize(
                    &node_data[value_start..value_end],
                )
                .map_err(|e| {
                    PersistentARTrieError::corrupted(format!(
                        "vocab overlay enumerate: value deserialize failed: {}",
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

    /// Enumerate `(term-units → id)` from the dense overlay image rooted at `root_ptr` — the
    /// root NODE `SwizzledPtr.to_raw()` (vocab stores the root node directly; unlike char it
    /// needs NO block-0 root descriptor, since `term_count`/`arena_count` already live in the
    /// VOCB header). ONE eager DFS (explicit work-stack): each node folds its prefix into the
    /// path, yields its value if final, pushes children at `+edge`. The arenas must already be
    /// loaded. Returns the term map + the split-out empty term "". The inverse of
    /// `serialize_overlay_to_disk`; fed to `build_overlay_root_from_terms` it reestablishes the
    /// resident overlay (V5 reopen).
    pub(super) fn enumerate_overlay_terms_from_disk(
        &self,
        root_ptr: u64,
    ) -> Result<(
        std::collections::BTreeMap<Vec<u32>, Option<u64>>,
        Option<Option<u64>>,
    )> {
        use std::collections::BTreeMap;

        let mut all: BTreeMap<Vec<u32>, Option<u64>> = BTreeMap::new();
        if root_ptr == 0 {
            return Ok((all, None)); // empty overlay
        }
        let mut stack: Vec<(SwizzledPtr, Vec<u32>)> =
            vec![(SwizzledPtr::from_raw(root_ptr), Vec::new())];
        while let Some((ptr, parent_path)) = stack.pop() {
            let (is_final, value, prefix_units, children) =
                self.read_overlay_record_fields(&ptr)?;
            let mut here = parent_path;
            here.extend_from_slice(&prefix_units);
            if is_final {
                all.insert(here.clone(), value);
            }
            // Push children REVERSED so `pop()` visits ascending edge order.
            for (edge, child_ptr) in children.into_iter().rev() {
                let mut p = here.clone();
                p.push(edge);
                stack.push((child_ptr, p));
            }
        }
        let empty_term: Option<Option<u64>> = all.remove(&Vec::<u32>::new());
        Ok((all, empty_term))
    }

    /// Reestablish the in-memory overlay from the on-disk v2 image (the V5 reopen seam):
    /// enumerate the dense image, build the overlay root + the reverse map (id->term) in one
    /// pass, install both, and stamp the Overlay regime so `route_overlay()` -> true. The arenas
    /// must already be loaded by the caller.
    pub(super) fn reestablish_overlay_from_image(&mut self, root_ptr: u64) -> Result<()> {
        use crate::persistent_artrie_core::key_encoding::CharKey;
        use crate::persistent_artrie_core::overlay::f5_build::build_overlay_root_from_terms;

        let (terms, empty) = self.enumerate_overlay_terms_from_disk(root_ptr)?;
        let rev = dashmap::DashMap::new();
        let mut entries: Vec<(Vec<u32>, Option<u64>)> = Vec::with_capacity(terms.len());
        for (units, id_opt) in terms {
            if let Some(id) = id_opt {
                let term: String = units.iter().filter_map(|&u| char::from_u32(u)).collect();
                rev.insert(id, term);
            }
            entries.push((units, id_opt));
        }
        if let Some(Some(id)) = empty {
            rev.insert(id, String::new());
        }
        let overlay_root = build_overlay_root_from_terms::<CharKey, u64, _>(entries, empty);
        self.install_prebuilt_overlay_root_inherent(overlay_root);
        self.reverse_term_map = Some(rev);
        if let Some(ref wal) = self.wal_writer {
            if let Err(e) = wal.set_overlay_regime() {
                log::warn!(
                    "vocab reestablish_overlay: could not stamp Overlay regime: {:?}",
                    e
                );
            }
        }
        Ok(())
    }

    /// V3 — rank-aware overlay WAL replay (the flip crash-recovery seam). Drains WAL records
    /// `>` `checkpoint_lsn` (earlier are already in the image): applies ONLY Inserts that have a
    /// durable `CommitRank` (a torn Insert-without-rank never committed -> DROPPED), into the
    /// overlay (no-WAL, idempotent — re-applying an image term is a no-op), and restores the id +
    /// commit_seq floors (`next_index` > every replayed id; `commit_seq` >= every generation).
    /// Returns `(records_seen, inserts_applied)`.
    pub(super) fn replay_wal_into_overlay_rank_aware(
        &mut self,
        wal_path: &std::path::Path,
        checkpoint_lsn: u64,
    ) -> Result<(usize, usize)> {
        use crate::persistent_artrie::wal::{WalReader, WalRecord};
        use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
        use std::collections::HashSet;
        use std::sync::atomic::Ordering;

        let reader = WalReader::new(wal_path)?;
        let mut pending: Vec<(u64, String, u64)> = Vec::new();
        let mut ranked: HashSet<u64> = HashSet::new();
        let mut max_generation: u64 = 0;
        let mut records_seen = 0usize;
        for rr in reader.iter() {
            let (lsn, record) = rr?;
            if lsn <= checkpoint_lsn {
                continue;
            }
            match record {
                WalRecord::Insert { term, value } => {
                    // Count only DATA records: the overlay RETAINS the WAL (no truncate), so a
                    // Checkpoint/CommitRank past checkpoint_lsn must NOT make a clean reopen
                    // report "recovered" (mode.is_normal() must hold after a clean checkpoint).
                    records_seen += 1;
                    if let Some(vb) = value {
                        if vb.len() >= 8 {
                            let id = u64::from_le_bytes(vb[..8].try_into().expect("len>=8"));
                            let t = String::from_utf8(term).map_err(|e| {
                                PersistentARTrieError::corrupted(format!(
                                    "vocab overlay replay: invalid UTF-8 term: {e}"
                                ))
                            })?;
                            pending.push((lsn, t, id));
                        }
                    }
                }
                WalRecord::CommitRank {
                    data_lsn,
                    generation,
                    ..
                } => {
                    ranked.insert(data_lsn);
                    max_generation = max_generation.max(generation);
                }
                WalRecord::Checkpoint {
                    checkpoint_lsn: new_lsn,
                    ..
                } => {
                    self.synced_lsn.fetch_max(new_lsn, Ordering::AcqRel);
                }
                _ => {}
            }
            self.next_lsn.fetch_max(lsn + 1, Ordering::AcqRel);
        }

        // Apply ONLY ranked Inserts (idempotent vs the image; count only genuinely-new terms).
        let mut applied = 0usize;
        let mut max_id: u64 = 0;
        for (lsn, term, id) in pending {
            if !ranked.contains(&lsn) {
                continue; // torn Insert without a durable CommitRank -> uncommitted -> drop
            }
            max_id = max_id.max(id);
            if self.get_index_lockfree(&term).is_none() {
                let units: Vec<u32> = term.chars().map(|c| c as u32).collect();
                self.overlay_publish_value(&units, id);
                if let Some(ref rev) = self.reverse_term_map {
                    rev.insert(id, term);
                }
                self.entry_count.fetch_add(1, Ordering::AcqRel);
                applied += 1;
            }
        }

        // Restore the floors so post-restart inserts neither reuse an id nor under-rank.
        if max_id > 0 {
            self.next_index.fetch_max(max_id + 1, Ordering::AcqRel);
        }
        self.commit_seq.fetch_max(max_generation, Ordering::AcqRel);
        Ok((records_seen, applied))
    }
}

/// CX-universal seams for vocab (EVICTION-OFF): the shared compressed loop lives in
/// `OverlayCompressedSerialize::serialize_compressed_loop`; vocab supplies only the char-arena
/// projection + per-node serialize. `path`/`registry`/`stamp_durable` are inert (vocab is never
/// evicted, so the forwarder always passes `None`).
impl<S: BlockStorage> OverlayCompressedSerialize<CharKey, u64>
    for super::dict_impl::PersistentVocabARTrie<S>
{
    type Projected = CharTrieNodeInner<u64>;

    fn project_node(
        node: &VocabOverlayNode,
        child_disk_ptrs: &[(u32, SwizzledPtr)],
    ) -> Result<Self::Projected> {
        Ok(overlay_inner_single_node(node, child_disk_ptrs))
    }

    fn project_chunk(
        synth: &VocabOverlayNode,
        child_disk_ptrs: &[(u32, SwizzledPtr)],
        prefix: &[u32],
    ) -> Result<Self::Projected> {
        Ok(overlay_inner_single_node_with_prefix::<u64>(
            synth,
            child_disk_ptrs,
            prefix,
        ))
    }

    fn serialize_projected_node(
        &self,
        projected: &Self::Projected,
        child_disk_ptrs: &[(u32, SwizzledPtr)],
        _path: &[u32],
        _registry: Option<&mut DiskLocationRegistry>,
    ) -> Result<SwizzledPtr> {
        self.serialize_one_overlay_node(projected, child_disk_ptrs)
    }

    fn new_synth_node() -> VocabOverlayNode {
        VocabOverlayNode::new()
    }

    fn stamp_durable(_live: &VocabOverlayNode, _raw: u64) {
        // No-op: vocab overlays are never evicted (registry is always None on the forwarder).
    }
}

/// Build the single-node `CharTrieNodeInner<u64>` (disk children added) for an overlay node —
/// supplies finality + value; the disk children fix the node TYPE for `build_disk_char_node_static`.
/// Mirrors char's `overlay_inner_single_node`.
fn overlay_inner_single_node(
    node: &VocabOverlayNode,
    child_disk_ptrs: &[(u32, SwizzledPtr)],
) -> CharTrieNodeInner<u64> {
    let mut inner = CharTrieNodeInner::<u64>::default();
    inner.node.header_mut().set_final(node.is_final());
    inner.value = node.get_value();
    for &(key, ref ptr) in child_disk_ptrs {
        if let Some(grown) = inner
            .node
            .add_child_growing(key, ptr.clone())
            .expect("overlay_inner_single_node: add on-disk child within capacity")
        {
            inner.node = grown;
        }
    }
    inner
}

/// Return `Some(first_slot)` iff the children are ≥2, all on disk in the parent's arena, and
/// occupy CONSECUTIVE slots within arena bounds (enables sequential-sibling encoding). Static
/// port of char's `check_sequential_char_children`.
fn check_sequential_char_children(
    child_ptrs: &[(u32, SwizzledPtr)],
    parent_arena_id: u32,
    arena_node_count: u32,
) -> Option<ArenaSlot> {
    if child_ptrs.len() < 2 {
        return None;
    }
    let mut slots: Vec<ArenaSlot> = Vec::with_capacity(child_ptrs.len());
    for (_, ptr) in child_ptrs {
        let loc = match ptr.disk_location() {
            Some(loc) => loc,
            None => return None,
        };
        if loc.block_id != parent_arena_id {
            return None;
        }
        slots.push(ArenaSlot::new(loc.block_id, loc.offset));
    }
    slots.sort_by_key(|s| s.slot_id);
    let first = slots[0];
    for (i, slot) in slots.iter().enumerate() {
        if slot.slot_id != first.slot_id + i as u32 {
            return None;
        }
    }
    let count = slots.len() as u32;
    if first.slot_id.checked_add(count.saturating_sub(1)).is_none() {
        return None;
    }
    let last_slot = first.slot_id + count - 1;
    if last_slot >= arena_node_count {
        return None;
    }
    Some(first)
}

#[cfg(test)]
mod tests {
    use crate::persistent_vocab_artrie::PersistentVocabARTrie;

    /// Round-trip: build a lock-free overlay, serialize it to the arena, enumerate it back, and
    /// verify every `(term → id)` survives. Exercises the data-loss-critical serialize↔enumerate
    /// pair (V2 serialize + V5 reopen-read) in-process (the arena is resident after `allocate`).
    #[test]
    fn overlay_serialize_enumerate_roundtrip() {
        // Real-disk scratch (the mmap arena cannot be tmpfs-backed; /tmp is tmpfs here).
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-scratch");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join(format!("vocab_overlay_rt_{}.vocab", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        vocab.install_overlay();

        // Shared prefixes, branches, and a proper-prefix term ("app" ⊂ "apple"/"applet").
        let terms = ["apple", "app", "applet", "banana", "band", "can", "candy"];
        let mut expected: Vec<(Vec<u32>, u64)> = Vec::with_capacity(terms.len());
        for t in &terms {
            let id = vocab.insert(t).expect("overlay insert");
            expected.push((t.chars().map(|c| c as u32).collect(), id));
        }

        let root = vocab
            .lockfree_root
            .as_ref()
            .and_then(|r| r.load())
            .expect("overlay root present");
        let root_ptr = vocab
            .serialize_overlay_to_disk(&root)
            .expect("serialize overlay");
        let (enumerated, empty) = vocab
            .enumerate_overlay_terms_from_disk(root_ptr.to_raw())
            .expect("enumerate overlay");

        assert!(empty.is_none(), "no empty term in this fixture");
        assert_eq!(enumerated.len(), terms.len(), "every term round-trips");
        for (units, id) in &expected {
            assert_eq!(
                enumerated.get(units).copied(),
                Some(Some(*id)),
                "term {:?} preserved its id {}",
                units,
                id
            );
        }

        let _ = std::fs::remove_file(&path);
    }

    /// End-to-end flip round-trip: a manually-flipped vocab inserts (lock-free Order-A overlay),
    /// checkpoints (v2 overlay image), drops, and reopens — the reestablished overlay must serve
    /// every term->id forward AND id->term reverse, with the right entry_count. Validates V4b
    /// (checkpoint_overlay) + V5 (open v2-routing + reestablish_overlay_from_image). The global
    /// create-flip is the single-lock-free closing task; here the flip is manual so existing v1
    /// tests are untouched.
    #[test]
    fn flip_checkpoint_reopen_roundtrip() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-scratch");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join(format!("vocab_flip_rt_{}.vocab", std::process::id()));
        let cleanup = |p: &std::path::Path| {
            let _ = std::fs::remove_file(p);
            let _ = std::fs::remove_file(p.with_extension("vocab.wal"));
            let _ = std::fs::remove_file(p.with_extension("vocab.idx"));
        };
        cleanup(&path);

        let terms = ["alpha", "beta", "al", "alp", "gamma", "be", "alphabet"];
        let mut expected: Vec<(String, u64)> = Vec::with_capacity(terms.len());
        {
            let mut vocab = PersistentVocabARTrie::create(&path).expect("create");
            assert!(vocab.route_overlay(), "overlay is the live rep after flip");
            for t in &terms {
                let id = vocab.insert(t).expect("overlay insert"); // routes to insert_overlay
                expected.push((t.to_string(), id));
            }
            for (t, id) in &expected {
                assert_eq!(vocab.get_index(t), Some(*id), "in-mem forward");
                assert_eq!(
                    vocab.get_term(*id).as_deref(),
                    Some(t.as_str()),
                    "in-mem reverse"
                );
            }
            vocab.checkpoint().expect("checkpoint_overlay"); // -> v2 image
        }

        let (vocab, _report) =
            PersistentVocabARTrie::open_with_recovery(&path).expect("reopen v2 image");
        assert!(
            vocab.route_overlay(),
            "overlay is the live rep after reopen"
        );
        for (t, id) in &expected {
            assert_eq!(vocab.get_index(t), Some(*id), "reopened forward {t:?}");
            assert_eq!(
                vocab.get_term(*id).as_deref(),
                Some(t.as_str()),
                "reopened reverse id {id}"
            );
        }
        assert_eq!(vocab.len(), terms.len(), "reopened entry_count");
        drop(vocab);
        cleanup(&path);
    }

    /// CX-universal (vocab): the production checkpoint now uses the PATH-COMPRESSED serializer
    /// (`serialize_overlay_snapshot_compressed`). Insert long single-child chains whose lengths
    /// straddle the `CHAR_MAX_PREFIX_LEN = 6` chunk boundary (so they compress into MULTIPLE chunk
    /// nodes), a shared-prefix branch (a multi-chunk shared head that forks), and an astral-codepoint
    /// chain; checkpoint (compressed v2 image), drop, reopen — every term->id forward AND id->term
    /// reverse must survive byte-EXACT (the char loader vocab reuses expands the prefix chunks back
    /// into chains; NO truncation). A lengthened/shortened key would change the collected terms, so
    /// this is the end-to-end no-truncation witness for the vocab compressed path.
    #[test]
    fn cx_compressed_long_chain_reopen_roundtrip() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-scratch");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join(format!("vocab_cx_longchain_{}.vocab", std::process::id()));
        let cleanup = |p: &std::path::Path| {
            let _ = std::fs::remove_file(p);
            let _ = std::fs::remove_file(p.with_extension("vocab.wal"));
            let _ = std::fs::remove_file(p.with_extension("vocab.idx"));
        };
        cleanup(&path);

        // Distinct first chars ⇒ pure single-child chunk-chains. Lengths straddle the chunk width
        // (prefix cap 6 ⇒ width 7): 1 (no chunk), 7 (one chunk), 13 (two), 21 (three), 40 (six).
        let alphabet = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMN"; // 50 chars
        let mut terms: Vec<String> = Vec::new();
        for (i, &len) in [1usize, 7, 13, 21, 40].iter().enumerate() {
            let first = (b'A' + i as u8) as char;
            let body: String = alphabet.chars().take(len.saturating_sub(1)).collect();
            terms.push(format!("{first}{body}"));
        }
        // Shared-prefix branch: a multi-chunk shared head ("sharedprefix", 12) that forks z.. / q..
        terms.push("sharedprefixzzzzzzzz".to_string());
        terms.push("sharedprefixqqqqqqqq".to_string());
        // Astral-codepoint chain (unit = codepoint, not byte): 10 × U+1F3AF.
        terms.push("\u{1F3AF}".repeat(10));

        let mut expected: Vec<(String, u64)> = Vec::with_capacity(terms.len());
        {
            let mut vocab = PersistentVocabARTrie::create(&path).expect("create");
            assert!(vocab.route_overlay(), "overlay is the live rep");
            for t in &terms {
                let id = vocab.insert(t).expect("overlay insert");
                expected.push((t.clone(), id));
            }
            vocab.checkpoint().expect("compressed checkpoint"); // -> compressed v2 image
        }

        let (vocab, _report) =
            PersistentVocabARTrie::open_with_recovery(&path).expect("reopen compressed v2 image");
        assert!(
            vocab.route_overlay(),
            "overlay is the live rep after reopen"
        );
        for (t, id) in &expected {
            assert_eq!(
                vocab.get_index(t),
                Some(*id),
                "reopened forward {t:?} (no truncation)"
            );
            assert_eq!(
                vocab.get_term(*id).as_deref(),
                Some(t.as_str()),
                "reopened reverse id {id}"
            );
        }
        assert_eq!(vocab.len(), terms.len(), "reopened entry_count");
        drop(vocab);
        cleanup(&path);
    }

    /// Crash recovery (V3): inserts AFTER a checkpoint live only in the WAL. A crash snapshot
    /// (file copy taken BEFORE the drop-checkpoint folds them into the image) reopened via
    /// open_with_recovery must replay the ranked WAL tail into the overlay so EVERY term (pre-
    /// and post-checkpoint) survives, and the id floor must be restored (no post-recovery id
    /// collision). Validates V3 (replay_wal_into_overlay_rank_aware).
    #[test]
    fn flip_crash_recovery_replays_wal_tail() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-scratch");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let pid = std::process::id();
        let path_a = dir.join(format!("vocab_crash_a_{}.vocab", pid));
        let path_b = dir.join(format!("vocab_crash_b_{}.vocab", pid));
        let cleanup = |p: &std::path::Path| {
            let _ = std::fs::remove_file(p);
            let _ = std::fs::remove_file(p.with_extension("vocab.wal"));
            let _ = std::fs::remove_file(p.with_extension("vocab.idx"));
        };
        cleanup(&path_a);
        cleanup(&path_b);

        let first = ["aa", "ab", "ba", "bb"];
        let second = ["car", "cat", "dog", "do"]; // post-checkpoint, WAL-only
        let mut expected: Vec<(String, u64)> = Vec::new();
        {
            let mut vocab = PersistentVocabARTrie::create(&path_a).expect("create");
            for t in &first {
                expected.push((t.to_string(), vocab.insert(t).expect("insert")));
            }
            vocab.checkpoint().expect("checkpoint"); // image = first batch only
            for t in &second {
                expected.push((t.to_string(), vocab.insert(t).expect("insert")));
            }
            vocab.sync().expect("sync WAL"); // post-checkpoint records durable in the WAL
                                             // Crash snapshot: copy image + WAL BEFORE the drop-checkpoint folds the tail in.
            std::fs::copy(&path_a, &path_b).expect("copy image");
            std::fs::copy(
                path_a.with_extension("vocab.wal"),
                path_b.with_extension("vocab.wal"),
            )
            .expect("copy wal");
            // vocab A drops here (checkpoints A; irrelevant to the B snapshot).
        }

        let (mut vocab, _report) =
            PersistentVocabARTrie::open_with_recovery(&path_b).expect("crash recovery reopen");
        assert!(vocab.route_overlay(), "overlay live after crash recovery");
        // EVERY term survives — the post-checkpoint batch ONLY via the V3 WAL-tail replay.
        for (t, id) in &expected {
            assert_eq!(vocab.get_index(t), Some(*id), "recovered forward {t:?}");
            assert_eq!(
                vocab.get_term(*id).as_deref(),
                Some(t.as_str()),
                "recovered reverse {id}"
            );
        }
        assert_eq!(
            vocab.len(),
            first.len() + second.len(),
            "recovered entry_count"
        );
        // id floor restored: a fresh insert must not collide with any recovered id.
        let new_id = vocab.insert("zzz").expect("post-recovery insert");
        assert!(
            expected.iter().all(|(_, id)| *id != new_id),
            "post-recovery id {new_id} collided with a recovered id"
        );
        drop(vocab);
        cleanup(&path_a);
        cleanup(&path_b);
    }
}
