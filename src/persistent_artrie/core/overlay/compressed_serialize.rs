//! CX-universal: the ONE path-compressed overlay-checkpoint serializer, generic over
//! `K: KeyEncoding`.
//!
//! The post-order chain-peeling work-stack loop is byte-for-byte the same across
//! the persistent ARTrie variants (byte / char / vocab / u64) because they
//! operate on the unified [`OverlayNode<K, V>`]. Only the leaves differ, and they
//! differ by on-disk format (byte `PART` `Node` tiers, char `ARTC` `CharNode`
//! tiers, vocab sidecars, and native-u64 CX records). So the loop lives once
//! here (the default method
//! [`OverlayCompressedSerialize::serialize_compressed_loop`]) and each variant
//! supplies the format-specific seams: peel is shared ([`peel_chain_generic`]),
//! chunking is shared
//! ([`crate::persistent_artrie::core::overlay::codec::chain_chunks`]), and the
//! per-variant trait methods cover node projection, single-node serialization,
//! and eviction durable-stamping.
//!
//! DATA-LOSS-CRITICAL: the edge convention (peel terminus = `num_children()!=1 || is_final ||
//! has_value`, OnDisk sole child ends the chain), the `K::MAX_PREFIX_LEN` chunk
//! width, and the `ends[c] = base+1+Σ_{i<c}(|P_i|+1)` true-depth
//! registry/stamp index are preserved from the proven byte/char/vocab originals
//! and are reused by the native-u64 profile.

/// The result of following a prefix link chain: the units consumed, every node
/// passed through, and the node the chain terminates at.
type PrefixChain<K, V> = (
    Vec<<K as KeyEncoding>::Unit>,
    Vec<Arc<OverlayNode<K, V>>>,
    Arc<OverlayNode<K, V>>,
);

use std::sync::Arc;

use crate::persistent_artrie::core::eviction::DiskLocationRegistry;
use crate::persistent_artrie::core::key_encoding::KeyEncoding;
use crate::persistent_artrie::core::overlay::codec::chain_chunks;
use crate::persistent_artrie::core::overlay::node::OverlayNode;
use crate::persistent_artrie::error::Result;
use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
use crate::value::DictionaryValue;

/// Peel a maximal **single-child non-final no-value** chain starting at `start`, returning
/// `(chain_units, live_spine, terminus)`. `chain_units` is the edge unit-string of the peeled links;
/// EMPTY iff `start` is itself the terminus. `live_spine[j]` is the live chain-link reached by
/// `chain_units[j-1]` (`live_spine[0]` = the chain head); `live_spine.len() == chain_units.len()` and
/// the terminus is NOT included (returned separately). The terminus is the first node that is NOT a
/// prefix-link — final, valued, `!= 1` child, OR whose sole child is `OnDisk` (the serializer NEVER
/// faults disk: an OnDisk sole child ends the chain, its `SwizzledPtr` passing through verbatim).
/// ITERATIVE (walks the uncompressed spine). The generic twin of the three identical originals
/// (char `peel_chain`, byte `peel_chain_byte`, vocab's reuse).
pub(crate) fn peel_chain_generic<K: KeyEncoding, V: DictionaryValue>(
    start: Arc<OverlayNode<K, V>>,
) -> PrefixChain<K, V> {
    let mut units: Vec<K::Unit> = Vec::new();
    let mut live: Vec<Arc<OverlayNode<K, V>>> = Vec::new();
    let mut cur = start;
    loop {
        // A prefix-link: exactly one child, not final, no value.
        if cur.num_children() != 1 || cur.is_final() || cur.has_value() {
            return (units, live, cur);
        }
        // Its sole child — continue ONLY while it is InMem (never fault disk during serialize).
        let sole = {
            let mut it = cur.iter_children();
            let (&edge, child) = it.next().expect("num_children() == 1 => exactly one child");
            child.as_in_mem().map(|arc| (edge, Arc::clone(arc)))
        };
        match sole {
            Some((edge, child_arc)) => {
                live.push(Arc::clone(&cur));
                units.push(edge);
                cur = child_arc;
            }
            // Sole child is OnDisk => `cur` is the terminus (its OnDisk child passes through).
            None => return (units, live, cur),
        }
    }
}

/// A pending child slot in a parent frame: the `key` awaiting the disk ptr its in-mem subtree
/// produces (`None` until that subtree completes).
struct PendingChild<K: KeyEncoding> {
    key: K::Unit,
    ptr: Option<SwizzledPtr>,
}

/// A work-stack frame: one peeled-chain terminus mid-descent (the root has an empty chain), held by
/// OWNED `Arc`, plus the peeled chain prefix collapsed into chunks ABOVE it.
struct Frame<K: KeyEncoding, V: DictionaryValue> {
    node: Arc<OverlayNode<K, V>>,
    parent_key: Option<K::Unit>,
    chain_prefix: Vec<K::Unit>,
    live_spine: Vec<Arc<OverlayNode<K, V>>>,
    base_depth: usize,
    pushed_units: usize,
    pending_in_mem: super::OverlayChildren<K, V>,
    slots: Vec<PendingChild<K>>,
}

fn make_frame<K: KeyEncoding, V: DictionaryValue>(
    node: Arc<OverlayNode<K, V>>,
    parent_key: Option<K::Unit>,
    chain_prefix: Vec<K::Unit>,
    live_spine: Vec<Arc<OverlayNode<K, V>>>,
    base_depth: usize,
    pushed_units: usize,
) -> Frame<K, V> {
    let n = node.num_children();
    let mut slots: Vec<PendingChild<K>> = Vec::with_capacity(n);
    let mut pending_in_mem: super::OverlayChildren<K, V> = Vec::with_capacity(n);
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
    // REVERSED so `pop()` yields ascending `iter_children()` order (matches the recursive DFS).
    pending_in_mem.reverse();
    Frame {
        node,
        parent_key,
        chain_prefix,
        live_spine,
        base_depth,
        pushed_units,
        pending_in_mem,
        slots,
    }
}

/// The single generic path-compressed overlay serializer. Implementors supply the format-specific
/// seams; the shared post-order loop lives in the default [`Self::serialize_compressed_loop`].
pub(crate) trait OverlayCompressedSerialize<K: KeyEncoding, V: DictionaryValue> {
    /// The variant's projected single-node value carrier handed to [`Self::serialize_projected_node`].
    /// char/vocab: `CharTrieNodeInner<V>`; byte: a `{node, value}` struct.
    type Projected;

    /// Project `node` into a single-node carrier (finality + value + the already-resolved on-disk
    /// child ptrs), NO prefix. char: `overlay_inner_single_node`; byte: build `Node` + value blob.
    fn project_node(
        node: &OverlayNode<K, V>,
        child_disk_ptrs: &[(K::Unit, SwizzledPtr)],
    ) -> Result<Self::Projected>;

    /// As [`Self::project_node`] but stamps a path-compression `prefix` (a synthetic non-final
    /// no-value chunk carrier). `prefix.len() <= K::MAX_PREFIX_LEN`.
    fn project_chunk(
        synth: &OverlayNode<K, V>,
        child_disk_ptrs: &[(K::Unit, SwizzledPtr)],
        prefix: &[K::Unit],
    ) -> Result<Self::Projected>;

    /// Serialize ONE projected node to a fresh arena slot, returning its disk ptr. Registers at
    /// `path` (full expanded depth) IFF `registry.is_some()`. Eviction-OFF variants (vocab) ignore
    /// `path`/`registry`.
    fn serialize_projected_node(
        &self,
        projected: &Self::Projected,
        child_disk_ptrs: &[(K::Unit, SwizzledPtr)],
        path: &[K::Unit],
        registry: Option<&mut DiskLocationRegistry>,
    ) -> Result<SwizzledPtr>;

    /// A fresh synthetic non-final no-value node for chunk carriers (`OverlayNode::<K, V>::new()`).
    fn new_synth_node() -> OverlayNode<K, V>;

    /// Stamp `live`'s durable_stamp with `raw` (called per emitted node when eviction-ON). char/byte:
    /// `live.set_durable_stamp(raw)`; vocab: NO-OP (vocab is never evicted, always passes `None`).
    fn stamp_durable(live: &OverlayNode<K, V>, raw: u64);

    /// Register an ALREADY-serialized, durable-CLEAN node that dirty-skip is REUSING this
    /// checkpoint: record its existing `ptr` + on-disk size in the (freshly rebuilt) eviction
    /// `registry` WITHOUT re-appending a fresh arena slot.
    ///
    /// Called ONLY when eviction is ON (`registry.is_some()`) and the live node's
    /// `durable_stamp()` is nonzero — i.e. the node's bytes already sit at `ptr` from a prior
    /// checkpoint and (by the M-2a stamp invariant) its whole subtree is byte-identical. Retaining
    /// FULL registration here (size = the EXACT on-disk slot length, so it equals what a fresh
    /// serialize would have recorded) keeps the resident-budget census faithful while the
    /// growth-causing arena `allocate` is elided.
    ///
    /// Default: unreachable — a variant that durable-stamps its nodes AND threads an eviction
    /// registry (char, byte) MUST override this. Eviction-OFF variants (u64, vocab) never stamp
    /// and always pass `None`, so the dirty-skip branch never reaches this for them.
    fn register_reused(
        &self,
        _ptr: &SwizzledPtr,
        _path: &[K::Unit],
        _registry: &mut DiskLocationRegistry,
    ) -> Result<()> {
        Err(
            crate::persistent_artrie::error::PersistentARTrieError::internal(
                "register_reused (dirty-skip) has no default impl; a variant that durable-stamps \
                 its nodes and threads an eviction registry must override it",
            ),
        )
    }

    /// THE shared post-order loop + chain-collapse. Byte-faithful to the three inlined originals.
    fn serialize_compressed_loop(
        &self,
        root: &Arc<OverlayNode<K, V>>,
        mut registry: Option<&mut DiskLocationRegistry>,
    ) -> Result<SwizzledPtr> {
        // The full key path of the CURRENT node (edge + chain pushed before descending).
        let mut path: Vec<K::Unit> = Vec::new();
        // The root is never peeled (it is always the on-disk entry node); its children are.
        let mut stack: Vec<Frame<K, V>> = vec![make_frame(
            Arc::clone(root),
            None,
            Vec::new(),
            Vec::new(),
            0,
            0,
        )];
        let mut completed: Option<(K::Unit, SwizzledPtr)> = None;

        loop {
            let frame = stack
                .last_mut()
                .expect("serialize_compressed: non-empty work-stack");

            if let Some((key, ptr)) = completed.take() {
                let slot = frame
                    .slots
                    .iter_mut()
                    .find(|s| s.key == key && s.ptr.is_none())
                    .expect("completed child key has a matching unfilled slot");
                slot.ptr = Some(ptr);
            }

            // Descend into the next in-mem child — PEELING its chain first. Push the WHOLE consumed
            // segment `[edge] ++ chain_prefix` onto `path` ONCE (the out-edge is never double-counted).
            if let Some((edge, child_arc)) = frame.pending_in_mem.pop() {
                let (chain_prefix, live_spine, terminus) = peel_chain_generic::<K, V>(child_arc);
                let base_depth = path.len();
                let mut pushed = 0usize;
                for &u in std::iter::once(&edge).chain(chain_prefix.iter()) {
                    path.push(u);
                    pushed += 1;
                }
                stack.push(make_frame(
                    terminus,
                    Some(edge),
                    chain_prefix,
                    live_spine,
                    base_depth,
                    pushed,
                ));
                continue;
            }

            // All children resolved → serialize THIS terminus, then collapse its peeled chain.
            let frame = stack
                .pop()
                .expect("serialize_compressed: frame to finalize");
            let child_disk_ptrs: Vec<(K::Unit, SwizzledPtr)> = frame
                .slots
                .into_iter()
                .map(|s| {
                    (
                        s.key,
                        s.ptr
                            .expect("post-order: every in-mem child slot filled before its parent"),
                    )
                })
                .collect();

            // (1) The terminus node — NO prefix. Registers at the FULL `path`; #6-stamps the LIVE
            // terminus when eviction-ON.
            //
            // DIRTY-SKIP: when eviction is ON and the LIVE terminus is durable-CLEAN (its
            // `durable_stamp()` still names the disk ptr a prior checkpoint wrote it to), the M-2a
            // invariant guarantees the whole subtree is byte-identical to that image (any write
            // path-copies + stamp-clears every ancestor). So REUSE that ptr instead of re-appending
            // a fresh arena slot — the fix for the unbounded per-checkpoint growth. FULL
            // registration is retained (`register_reused`) so the resident census stays faithful;
            // only the growth-causing `allocate` is elided. Cross-arena children this leaves behind
            // are handled by the relative/full encoding (see check_sequential_char_children).
            let reuse_ptr: Option<SwizzledPtr> = if registry.is_some() {
                let stamp = frame.node.durable_stamp();
                (stamp != 0).then(|| SwizzledPtr::from_raw(stamp))
            } else {
                None
            };
            let terminus_ptr = if let Some(ptr) = reuse_ptr {
                if let Some(reg) = registry.as_deref_mut() {
                    self.register_reused(&ptr, &path, reg)?;
                }
                ptr
            } else {
                let projected = Self::project_node(frame.node.as_ref(), &child_disk_ptrs)?;
                let ptr = self.serialize_projected_node(
                    &projected,
                    &child_disk_ptrs,
                    &path,
                    registry.as_deref_mut(),
                )?;
                if registry.is_some() {
                    Self::stamp_durable(frame.node.as_ref(), ptr.to_raw());
                }
                ptr
            };

            // (2) Collapse the peeled chain into a chunk stack ABOVE the terminus (bottom-up). Each
            // chunk carries <= K::MAX_PREFIX_LEN inter-edge units as its prefix + one out-edge. Empty
            // chain ⇒ the terminus is the top. #6: each chunk registers at its TRUE expanded depth
            // `ends[c] = base+1+Σ_{i<c}(|P_i|+1)` and #6-stamps its LIVE top-of-span node.
            let top_ptr = if frame.chain_prefix.is_empty() {
                terminus_ptr
            } else {
                let chunks = chain_chunks(&frame.chain_prefix, K::MAX_PREFIX_LEN);
                let chain_head = frame.base_depth + 1;
                let mut ends: Vec<usize> = Vec::with_capacity(chunks.len());
                let mut acc = chain_head;
                for ch in &chunks {
                    ends.push(acc);
                    acc += ch.prefix.len() + 1;
                }
                debug_assert_eq!(
                    acc,
                    frame.base_depth + 1 + frame.chain_prefix.len(),
                    "CX #6: Σ chunk widths must equal the chain length (no-truncation witness)"
                );
                let synth = Self::new_synth_node();
                let mut child_ptr = terminus_ptr;
                for (c, chunk) in chunks.iter().enumerate().rev() {
                    // idx = ends[c] - base - 1 = Σ_{i<c}(|P_i|+1) = this chunk's top-of-span live node.
                    let idx = ends[c] - frame.base_depth - 1;
                    let top_live = frame.live_spine.get(idx);
                    let chunk_path = &path[..ends[c]];
                    // DIRTY-SKIP (chunk twin of the terminus reuse above): a durable-clean chain
                    // link's stamp names the chunk ptr a prior checkpoint wrote; the M-2a invariant
                    // makes the whole span-below byte-identical, so reuse it rather than re-serialize.
                    let reuse_ptr: Option<SwizzledPtr> = if registry.is_some() {
                        top_live
                            .map(|n| n.durable_stamp())
                            .filter(|&stamp| stamp != 0)
                            .map(SwizzledPtr::from_raw)
                    } else {
                        None
                    };
                    let next_ptr = if let Some(ptr) = reuse_ptr {
                        if let Some(reg) = registry.as_deref_mut() {
                            self.register_reused(&ptr, chunk_path, reg)?;
                        }
                        ptr
                    } else {
                        let child_slots = [(chunk.edge, child_ptr.clone())];
                        let chunk_proj = Self::project_chunk(&synth, &child_slots, chunk.prefix)?;
                        let np = self.serialize_projected_node(
                            &chunk_proj,
                            &child_slots,
                            chunk_path,
                            registry.as_deref_mut(),
                        )?;
                        if registry.is_some() {
                            if let Some(tl) = top_live {
                                Self::stamp_durable(tl.as_ref(), np.to_raw());
                            }
                        }
                        np
                    };
                    child_ptr = next_ptr;
                }
                child_ptr
            };

            // Symmetric pop of THIS frame's pushed `[edge] ++ chain_prefix` segment.
            for _ in 0..frame.pushed_units {
                path.pop();
            }

            match frame.parent_key {
                Some(key) => completed = Some((key, top_ptr)),
                None => return Ok(top_ptr),
            }
        }
    }
}
