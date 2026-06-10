//! Disk-loading helpers for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (Phase-5 byte sub-module). The reopen path that
//! reads the dense on-disk image through the `BlockStorage` / `BufferManager` /
//! `ArenaManager` layers, called from the `mmap_ctor` / `io_uring_ctor` / `f5_loader`
//! siblings during `open*` flows.
//!
//! Methods covered:
//! - `read_root_descriptor` / `read_root_descriptor_arena_count`
//! - `enumerate_terms_from_disk` (the SOLE reopen path — dense image → (term, value))
//! - `load_single_art_node_data` / `load_single_child_data` (its single-node readers)
//!
//! **L3.3c:** the owned-tree loaders (`load_root_from_disk*`, the recursive
//! `load_art_node_with_children*` / `load_child_from_disk*`, the iterative variant) that
//! built a `TrieRoot` / `ChildNode` were deleted with the owned tree.

use std::sync::Arc;

use crate::sync_compat::RwLock;
use crate::value::DictionaryValue;

use super::arena_manager::{ArenaManager, ArenaSlot};
use super::block_storage::BlockStorage;
use super::bucket::StringBucket;
use super::buffer_manager::BufferManager;
use super::dict_impl::{
    PersistentARTrie, SingleChildData, ROOT_TYPE_ART_NODE, ROOT_TYPE_BUCKET, ROOT_TYPE_EMPTY,
};
use super::error::{PersistentARTrieError, Result};
use super::nodes::Node;
use super::serialization;
use super::serialization::v2::DeserializationContext;
use super::swizzled_ptr::{NodeType, SwizzledPtr};

pub(super) const ROOT_DESCRIPTOR_OFFSET: usize = 64;
pub(super) const ROOT_DESCRIPTOR_LEN: usize = 18;

pub(super) fn read_root_descriptor<S: BlockStorage>(
    storage: &S,
    root_ptr: u64,
) -> Result<[u8; ROOT_DESCRIPTOR_LEN]> {
    let ptr = SwizzledPtr::from_raw(root_ptr);
    if ptr.is_null() || ptr.is_swizzled() {
        return Err(PersistentARTrieError::corrupted(
            "Invalid root descriptor pointer: null or already swizzled",
        ));
    }

    let location = ptr.disk_location().ok_or_else(|| {
        PersistentARTrieError::corrupted("Could not decode root descriptor disk location")
    })?;

    if location.block_id != 0 || location.offset as usize != ROOT_DESCRIPTOR_OFFSET {
        return Err(PersistentARTrieError::corrupted(format!(
            "Root descriptor pointer must target block 0 offset {}, got block {} offset {}",
            ROOT_DESCRIPTOR_OFFSET, location.block_id, location.offset
        )));
    }

    let mut descriptor = [0u8; ROOT_DESCRIPTOR_LEN];
    storage.read_bytes(location.block_id, location.offset as usize, &mut descriptor)?;
    Ok(descriptor)
}

pub(super) fn read_root_descriptor_arena_count<S: BlockStorage>(
    storage: &S,
    root_ptr: u64,
) -> Result<u32> {
    let descriptor = read_root_descriptor(storage, root_ptr)?;
    Ok(u32::from_le_bytes([
        descriptor[6],
        descriptor[7],
        descriptor[8],
        descriptor[9],
    ]))
}

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    // L3.3c: removed — the owned-tree disk loaders (`load_root_from_disk`,
    // `load_art_node_with_children`, `load_child_from_disk`, `load_root_from_disk_with_arena`,
    // `load_art_node_with_children_from_arena`, `load_child_from_disk_with_arena`,
    // `load_art_node_with_children_from_arena_iterative`) built the deleted owned `TrieRoot` /
    // `ChildNode` representation. `enumerate_terms_from_disk` (the SOLE reopen path) +
    // `load_single_art_node_data` / `load_single_child_data` (its single-node record readers)
    // are KEPT below.

    /// **L3.1 — direct dense→(term, value) enumerator (NO `TrieRoot`).**
    ///
    /// Reads the dense on-disk image (root descriptor + ART node records + `StringBucket` leaf
    /// records — all three formats: un-compressed overlay, CX node-prefix-compressed, and legacy
    /// owned bucket-suffix) DIRECTLY into a `(term-units → Option<V>)` map + the empty term "",
    /// WITHOUT materializing a `TrieRoot`. This is the overlay-codec replacement for the
    /// (now-deleted) `load_root_from_disk_with_arena` + owned→overlay-converter owned-scratch
    /// pair: fed to `build_overlay_root_from_terms` it yields the resident overlay root directly,
    /// and let L3.3 delete the owned decoder + `TrieRoot` + the D1 owned readers outright.
    ///
    /// ONE eager DFS over the arena records (explicit work-stack — stack-safe at depth). Each ART
    /// node folds its own compressed `prefix` into the accumulated path BEFORE recording finality /
    /// descending (the L2.1 fold), so node-prefix-compressed (CX) images reconstruct losslessly;
    /// each `StringBucket` entry contributes `path ++ suffix` (legacy format-3). A final node / a
    /// bucket entry yields its `read_node_value`-or-`None` ONCE, so the membership∪value union is
    /// intrinsic (no separate passes). The fully-eager record readers resolve every child, so a
    /// dense image carries no `ChildNode::DiskRef` to resolve here.
    ///
    /// Returns `(terms_without_empty, empty_term, term_count)`. `empty_term`: `Some(Some(v))`
    /// valued "", `Some(None)` membership "", `None` absent (the `BTreeMap` value-of-"" is itself
    /// `Option<V>`, so `remove("")` yields exactly `Option<Option<V>>`).
    pub(super) fn enumerate_terms_from_disk(
        buffer_manager: &Arc<RwLock<BufferManager<S>>>,
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        root_ptr: u64,
    ) -> Result<(
        std::collections::BTreeMap<Vec<u8>, Option<V>>,
        Option<Option<V>>,
        u64,
    )> {
        use std::collections::BTreeMap;

        let mut all: BTreeMap<Vec<u8>, Option<V>> = BTreeMap::new();

        // (1) Read + parse the fixed 18-byte root descriptor (block 0, offset 64) — the same
        // descriptor `load_root_from_disk_with_arena` reads.
        let ptr = SwizzledPtr::from_raw(root_ptr);
        if ptr.is_null() || ptr.is_swizzled() {
            return Err(PersistentARTrieError::corrupted(
                "enumerate_terms_from_disk: invalid root pointer (null or swizzled)",
            ));
        }
        let location = ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted("enumerate_terms_from_disk: undecodable root location")
        })?;
        if location.block_id != 0 || location.offset as usize != ROOT_DESCRIPTOR_OFFSET {
            return Err(PersistentARTrieError::corrupted(format!(
                "enumerate_terms_from_disk: root descriptor must target block 0 offset {}, got block {} offset {}",
                ROOT_DESCRIPTOR_OFFSET, location.block_id, location.offset
            )));
        }
        let descriptor_buf = {
            let bm = buffer_manager.read();
            let page = bm.fetch_page(location.block_id)?;
            let page_data = page.data();
            let offset = location.offset as usize;
            let end = offset.checked_add(ROOT_DESCRIPTOR_LEN).ok_or_else(|| {
                PersistentARTrieError::corrupted(
                    "enumerate_terms_from_disk: descriptor offset overflow",
                )
            })?;
            if end > page_data.len() {
                return Err(PersistentARTrieError::corrupted(
                    "enumerate_terms_from_disk: descriptor extends past header block",
                ));
            }
            let mut buf = [0u8; ROOT_DESCRIPTOR_LEN];
            buf.copy_from_slice(&page_data[offset..end]);
            buf
        };
        if descriptor_buf[1] > 1 {
            return Err(PersistentARTrieError::corrupted(format!(
                "enumerate_terms_from_disk: invalid root final flag {}",
                descriptor_buf[1]
            )));
        }
        let root_type = descriptor_buf[0];
        let root_is_final = descriptor_buf[1] != 0;
        let term_count = u32::from_le_bytes([
            descriptor_buf[2],
            descriptor_buf[3],
            descriptor_buf[4],
            descriptor_buf[5],
        ]) as u64;
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

        // Deserialize an optional opaque value blob to `Option<V>`, PROPAGATING errors (the
        // data-loss path — mirrors `load_root_from_disk_with_arena`'s `?`, NOT the lossy `.ok()`).
        let deser = |bytes: Option<Vec<u8>>| -> Result<Option<V>> {
            match bytes {
                Some(vb) => Ok(Some(
                    crate::serialization::bincode_compat::deserialize::<V>(&vb).map_err(|e| {
                        PersistentARTrieError::corrupted(format!(
                            "enumerate_terms_from_disk: value deserialize failed: {:?}",
                            e
                        ))
                    })?,
                )),
                None => Ok(None),
            }
        };

        // (2) Walk per root type.
        match root_type {
            ROOT_TYPE_EMPTY => { /* no records, no terms */ }
            ROOT_TYPE_BUCKET => {
                // Root bucket: each entry is a FULL term (the path so far is empty).
                let bucket_ptr = SwizzledPtr::from_raw(data_ptr);
                let bucket_loc = bucket_ptr.disk_location().ok_or_else(|| {
                    PersistentARTrieError::corrupted(
                        "enumerate_terms_from_disk: invalid root bucket pointer",
                    )
                })?;
                let arena_id = bucket_loc.block_id.checked_sub(1).ok_or_else(|| {
                    PersistentARTrieError::corrupted(
                        "enumerate_terms_from_disk: invalid block_id 0 for root bucket",
                    )
                })?;
                let slot = ArenaSlot::new(arena_id, bucket_loc.offset);
                let bucket = {
                    let am = arena_manager.read();
                    let data = am.read(slot)?;
                    StringBucket::from_bytes(data).map_err(|e| {
                        PersistentARTrieError::corrupted(format!(
                            "enumerate_terms_from_disk: root bucket load: {:?}",
                            e
                        ))
                    })?
                };
                for i in 0..bucket.len() {
                    if let Some(entry) = bucket.get_entry(i) {
                        let term = bucket.get_suffix(&entry).to_vec();
                        let value = deser(bucket.get_value(&entry).map(|b| b.to_vec()))?;
                        all.insert(term, value);
                    }
                }
            }
            ROOT_TYPE_ART_NODE => {
                // Root node: `is_final` from the DESCRIPTOR, value from the root RECORD — matching
                // `load_root_from_disk_with_arena`'s `TrieRoot::ArtNode` construction exactly.
                let root_node_ptr = SwizzledPtr::from_raw(data_ptr);
                let (root_node, _record_final, root_value_bytes, root_children) =
                    Self::load_single_art_node_data(arena_manager, &root_node_ptr)?;
                let mut root_path: Vec<u8> = Vec::new();
                let plen = root_node.header().prefix_len as usize;
                if plen > 0 {
                    root_path.extend_from_slice(&root_node.prefix().bytes[..plen]);
                }
                if root_is_final {
                    all.insert(root_path.clone(), deser(root_value_bytes)?);
                }
                // Iterative DFS. A frame is `(child_ptr, path-INCLUDING-the-edge-to-it)`.
                let mut stack: Vec<(SwizzledPtr, Vec<u8>)> =
                    Vec::with_capacity(root_children.len());
                for (edge, child_ptr) in root_children.into_iter().rev() {
                    let mut p = root_path.clone();
                    p.push(edge);
                    stack.push((child_ptr, p));
                }
                while let Some((child_ptr, parent_path)) = stack.pop() {
                    match Self::load_single_child_data(arena_manager, &child_ptr)? {
                        SingleChildData::Bucket(bucket) => {
                            for i in 0..bucket.len() {
                                if let Some(entry) = bucket.get_entry(i) {
                                    let mut term = parent_path.clone();
                                    term.extend_from_slice(bucket.get_suffix(&entry));
                                    let value =
                                        deser(bucket.get_value(&entry).map(|b| b.to_vec()))?;
                                    all.insert(term, value);
                                }
                            }
                        }
                        SingleChildData::ArtNodePartial {
                            node,
                            is_final,
                            child_ptrs,
                            value,
                        } => {
                            // Fold THIS node's compressed prefix at entry (the L2.1 fold).
                            let mut here = parent_path;
                            let plen = node.header().prefix_len as usize;
                            if plen > 0 {
                                here.extend_from_slice(&node.prefix().bytes[..plen]);
                            }
                            if is_final {
                                all.insert(here.clone(), deser(value)?);
                            }
                            for (edge, gchild) in child_ptrs.into_iter().rev() {
                                let mut p = here.clone();
                                p.push(edge);
                                stack.push((gchild, p));
                            }
                        }
                    }
                }
            }
            other => {
                return Err(PersistentARTrieError::corrupted(format!(
                    "enumerate_terms_from_disk: unknown root type {}",
                    other
                )));
            }
        }

        // (3) Split out the empty term "". `all`'s value type IS `Option<V>`, so `remove`
        // returns exactly `Option<Option<V>>` = the `empty_term` shape.
        let empty_term: Option<Option<V>> = all.remove(&Vec::<u8>::new());
        Ok((all, empty_term, term_count))
    }

    /// Load a single ART node's data from arena WITHOUT loading children.
    ///
    /// This is a helper for iterative loading. Returns the node info and
    /// the list of child pointers that need to be loaded.
    fn load_single_art_node_data(
        arena_manager: &Arc<RwLock<ArenaManager<S>>>,
        node_ptr: &SwizzledPtr,
    ) -> Result<(Node, bool, Option<Vec<u8>>, Vec<(u8, SwizzledPtr)>)> {
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

        // Empty-string support (H1): capture the root node's optional value blob (the
        // empty term "" carries its value on the root record) BEFORE `drop(am)` (it
        // borrows `node_data`, which borrows `am`). A value-less (legacy) root has
        // HAS_VALUE clear → `read_node_value` returns None — back-compat. This mirrors
        // `load_single_child_data`, which already reads child values (M4a).
        let value = serialization::v2::read_node_value(node_data);

        // Collect child pointers before dropping the arena lock
        let child_data: Vec<(u8, SwizzledPtr)> = node
            .iter_children()
            .filter(|(_, ptr)| !ptr.is_null())
            .map(|(key, ptr)| (key, ptr.clone()))
            .collect();

        drop(am);

        Ok((node, is_final, value, child_data))
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

                // M4a / D-VAL: capture the leaf value BEFORE `drop(am)` below (it
                // borrows `data`, which borrows `am`).
                let value = serialization::v2::read_node_value(data);

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
                    value,
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
}
