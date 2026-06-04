//! Byte seam impl of the shared [`OverlayCheckpoint`] route-split skeleton
//! (`overlay-durable-architecture.md`, trait 3, step M2b). The byte twin of
//! `persistent_artrie_char::{overlay_write_mode (impl), persist}`.
//!
//! The generic [`OverlayCheckpoint::checkpoint_route_split`] default owns the
//! data-loss-critical RES-4 route-split DECISION (capture the LIVE representation —
//! under `route_overlay()` the OWNED tree is EMPTY, the live data is in the
//! immutable overlay, so capturing the owned tree would checkpoint NOTHING and lose
//! every term on reopen) + the total-loss-guard assert. This module supplies ONLY
//! the per-variant capture + publish seams, which are GENUINELY per-variant (byte
//! arena on-disk format ≠ char arena format).
//!
//! **M2b scope (opt-in, REVERSIBLE — INERT pre-flip).** `route_overlay()` is
//! `false` until the production ctors flip (M4), so the route-split default runs the
//! OWNED arm — byte-for-byte the prior `checkpoint()` body (serialize the owned tree
//! + WAL checkpoint/truncate). The OVERLAY arm (capture from the immutable overlay +
//! the watermark-bounded RETAINING publisher) is BUILT here so it compiles and is
//! correct-by-construction for the M4 flip, but is not reached in M2b.
//!
//! # The overlay capture is equivalent-by-construction to an owned snapshot
//!
//! [`PersistentARTrie::capture_overlay_snapshot`] walks the immutable overlay root
//! (`OverlayNode<ByteKey, V>`) and converts each node into byte's OWNED on-disk node
//! representation ([`ChildNode`] / [`Node`] / value), then serializes it through the
//! EXISTING owned serializer ([`PersistentARTrie::serialize_child_to_disk_with_path`]
//! / [`PersistentARTrie::serialize_node_to_disk`]). So for the same logical data the
//! on-disk image is equivalent by construction to a `capture_owned_snapshot()` of an
//! owned tree built from the same terms — exactly char's correctness property
//! (`capture_snapshot_immutable` ≡ `capture_snapshot`). The conversion mirrors byte's
//! owned serialize behavior EXACTLY (including byte's documented "ART-node value
//! serialization is future work" — `disk_load.rs`; the overlay capture inherits the
//! same value-on-ArtNode behavior, so it neither regresses nor pretends to exceed the
//! owned path), so the image is byte-identical to the owned one for the same terms.

#![cfg(feature = "persistent-artrie")]

use std::sync::atomic::Ordering as AtomicOrdering;

use super::block_storage::BlockStorage;
use super::bucket::StringBucket;
use super::dict_impl::{
    PersistentARTrie, TrieRoot, ROOT_TYPE_ART_NODE, ROOT_TYPE_BUCKET, ROOT_TYPE_EMPTY,
};
use super::error::{PersistentARTrieError, Result};
use super::nodes::{ArtNode, Node, Node4};
use super::swizzled_ptr::{NodeType, SwizzledPtr};
use super::transitions::ChildNode;
use super::wal::WalRecord;
use crate::persistent_artrie_core::key_encoding::ByteKey;
use crate::persistent_artrie_core::overlay::checkpoint::OverlayCheckpoint;
use crate::persistent_artrie_core::overlay::OverlayNode;
use crate::value::DictionaryValue;

/// An immutable, self-consistent byte checkpoint snapshot (the byte twin of char's
/// `CheckpointSnapshot`). Captured during checkpoint Phase A by serializing the
/// in-memory representation (owned tree OR immutable overlay) into freshly-allocated
/// arena slots (copy-on-serialize, so the captured `root_ptr` + arena image is
/// frozen). The durable-publish phase consumes only these owned values.
pub(crate) struct CheckpointSnapshot {
    /// Root descriptor type byte (`ROOT_TYPE_EMPTY` / `ROOT_TYPE_BUCKET` / `ROOT_TYPE_ART_NODE`).
    root_type: u8,
    /// Whether the root node is itself terminal/final.
    is_final: bool,
    /// Term count at the snapshot point (descriptor + header agree).
    term_count: u64,
    /// Number of arenas after serialization (block IDs derive from this).
    arena_count: u32,
    /// Raw `SwizzledPtr` of the serialized root.
    root_ptr: u64,
    /// `next_lsn` observed at capture (owned arm reclaims by this convention).
    next_lsn_at_capture: u64,
    /// **Overlay-arm capture only.** The committed watermark captured (`Acquire`)
    /// BEFORE the root load — the capture-ordering invariant (snapshot ⊆
    /// committed-durable-prefix). `Some(w)` for [`PersistentARTrie::capture_overlay_snapshot`];
    /// `None` for the owned [`PersistentARTrie::capture_owned_snapshot`] (which reclaims
    /// by `next_lsn`). The retaining-WAL publisher records `checkpoint_lsn = w` so
    /// recovery skips WAL deltas ≤ `w` (already folded into the image) — the
    /// watermark-based `checkpoint_lsn` that makes publishing while retaining the WAL
    /// non-double-counting (GAP_LEDGER #41).
    committed_watermark_at_capture: Option<u64>,
    /// **Overlay-arm capture only (the A3 commit_seq floor).** The durable global
    /// `commit_seq` observed (`Acquire`) in the SAME window as the watermark and
    /// BEFORE the root load. `Some(c)` for the overlay capture; `None` for the owned
    /// capture (which never advances `commit_seq`). The retaining publisher raises the
    /// WAL `commit_seq_floor` to this so a post-checkpoint overlay op out-ranks every
    /// pre-checkpoint survivor on a later rebuild.
    commit_seq_at_capture: Option<u64>,
}

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    // ====================================================================
    // OWNED arm (the false/INERT-pre-flip arm) — capture + publish the OWNED
    // tree, byte-identical to the prior `checkpoint()` / `persist_to_disk` body.
    // ====================================================================

    /// **Owned arm — capture.** Serialize the OWNED tree (`self.root`) into fresh
    /// arena slots and return the frozen descriptor. The byte twin of char's
    /// `capture_snapshot`. Takes `&self` (all mutation goes through the
    /// interior-mutable arena/buffer managers), reproducing `persist_to_disk`'s
    /// serialize half WITHOUT the `&mut`-only dirty-tracking clear (which the public
    /// `checkpoint()` wrapper performs after the route-split). Reclaims by the
    /// `next_lsn` convention, so the watermark/commit_seq fields are `None`.
    pub(crate) fn capture_owned_snapshot(&self) -> Result<CheckpointSnapshot> {
        if self.buffer_manager.is_none() {
            return Err(PersistentARTrieError::internal(
                "No buffer manager for disk serialization",
            ));
        }
        let next_lsn_at_capture = self.next_lsn.load(AtomicOrdering::Acquire);

        let (root_type, root_ptr, is_final) = self.serialize_root(&self.root)?;
        let arena_count = self.flush_and_count_arenas()?;
        let term_count = self.term_count.load(AtomicOrdering::Acquire) as u64;

        Ok(CheckpointSnapshot {
            root_type,
            is_final,
            term_count,
            arena_count,
            root_ptr,
            next_lsn_at_capture,
            committed_watermark_at_capture: None,
            commit_seq_at_capture: None,
        })
    }

    /// **Owned arm — publish + reclaim.** Publish the owned snapshot durably (the
    /// descriptor + flush + fsync linearization point), then WAL `Checkpoint` append
    /// + sync + truncate (reclaim by `next_lsn`). The byte twin of char's
    /// `publish_durable_and_reclaim`. Byte-identical to the prior `checkpoint()` WAL
    /// tail. Takes `&self` (all calls go through interior-mutable managers / the
    /// `Arc<AsyncWalWriter>`).
    pub(crate) fn publish_owned_and_reclaim(&self, snapshot: CheckpointSnapshot) -> Result<()> {
        // Phase B: publish the descriptor + flush + fsync (the on-disk linearization
        // point) + clear the dirty flag.
        self.publish_snapshot(&snapshot)?;

        // C2 invariant (the byte twin of char's `publish_durable_and_reclaim` assert):
        // under the owned `&mut self` checkpoint no `L1.write` mutator can run between
        // capture and here, so `next_lsn` is unchanged — the WAL frontier and the
        // descriptor's snapshot agree, and the truncate below only archives covered
        // records. A violation (a writer racing the owned checkpoint) could lose that
        // write (GAP_LEDGER #41), so fail loud rather than silently lose.
        assert_eq!(
            self.next_lsn.load(AtomicOrdering::Acquire),
            snapshot.next_lsn_at_capture,
            "checkpoint: next_lsn changed between capture and WAL publish — a writer raced \
             the owned checkpoint (C2 invariant violated); the WAL reclaim could lose that write"
        );

        // Phase C: WAL checkpoint + truncate (reclaim by `next_lsn`, the original
        // owned convention). Under the owned `&mut self` checkpoint no writer can
        // race, so `next_lsn` is the safe `checkpoint_lsn` here (this is the byte
        // owned path — the overlay path uses the watermark instead).
        if let Some(ref wal_writer) = self.wal_writer {
            let checkpoint_lsn = self
                .next_lsn
                .load(AtomicOrdering::Acquire)
                .saturating_sub(1);
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            wal_writer
                .append(WalRecord::Checkpoint {
                    checkpoint_lsn,
                    timestamp,
                })
                .map_err(|e| {
                    PersistentARTrieError::io_error(
                        "checkpoint_append",
                        "WAL",
                        std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                    )
                })?;
            wal_writer.sync().map_err(|e| {
                PersistentARTrieError::io_error(
                    "checkpoint_sync",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            wal_writer.truncate().map_err(|e| {
                PersistentARTrieError::io_error(
                    "checkpoint_truncate",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            let next_lsn = checkpoint_lsn.saturating_add(1);
            wal_writer.set_min_lsn(next_lsn);
            self.next_lsn.store(next_lsn, AtomicOrdering::Release);
        }
        Ok(())
    }

    // ====================================================================
    // OVERLAY arm (the route_overlay()==true arm — UNREACHABLE in M2b, BUILT for
    // M4) — capture from the IMMUTABLE overlay + publish RETAINING the WAL.
    // ====================================================================

    /// **Overlay arm — capture.** Capture a frozen snapshot from the IMMUTABLE
    /// lock-free overlay (walk the overlay root → fresh arena slots via the
    /// overlay→owned converter), reading the committed watermark + commit_seq
    /// `Acquire` BEFORE the root load (the capture-ordering invariant). The byte
    /// twin of char's `capture_snapshot_immutable`.
    pub(crate) fn capture_overlay_snapshot(&self) -> Result<CheckpointSnapshot> {
        if self.buffer_manager.is_none() {
            return Err(PersistentARTrieError::internal(
                "No buffer manager for disk serialization",
            ));
        }
        let next_lsn_at_capture = self.next_lsn.load(AtomicOrdering::Acquire);

        // ═══════════════════════════════════════════════════════════════════
        //  THE SNAPSHOT-LSN CAPTURE ORDERING (the byte twin of char's "single most
        //  dangerous line"). The committed watermark + commit_seq are read `Acquire`
        //  STRICTLY BEFORE loading the atomic overlay root (also `Acquire`). This
        //  ordering — watermark/commit_seq FIRST, then root — makes the captured
        //  snapshot a subset of the committed-durable prefix, so
        //  `checkpoint_lsn := watermark` can NEVER reclaim a WAL record the snapshot
        //  does not contain (GAP_LEDGER #41; the publication chain
        //  snapshot ⊆ published-root ⊆ committed-prefix(watermark) is established by
        //  the Order-A Acquire/Release pairs in `insert_cas_durable`). DO NOT REORDER.
        // ═══════════════════════════════════════════════════════════════════
        let watermark_at_capture = self.committed_watermark.watermark();
        let synced_frontier_at_capture: u64 = self
            .wal_writer
            .as_ref()
            .map(|w| w.synced_lsn())
            .unwrap_or(0);
        let commit_seq_at_capture = self.commit_seq.load(AtomicOrdering::Acquire);

        let overlay_root = self.lockfree_root.as_ref().and_then(|root| root.load());
        let (root_type, root_ptr, is_final, term_count) = match overlay_root {
            None => (ROOT_TYPE_EMPTY, 0u64, false, 0u64),
            Some(root) => {
                // Convert the overlay root → byte's owned `TrieRoot` representation,
                // then serialize via the EXISTING owned serializer (the same path
                // `persist_to_disk` uses), so the on-disk image is equivalent by
                // construction to an owned snapshot of the same terms.
                let owned_root = Self::overlay_root_to_owned(&root);
                let entry_count = count_overlay_finals::<V>(&root);
                let (rt, rp, isf) = self.serialize_root(&owned_root)?;
                (rt, rp, isf, entry_count)
            }
        };

        // Executable refinement of the capture-ordering invariant: the watermark
        // captured BEFORE the root load never exceeds the durably-synced WAL frontier
        // captured in the same window (a watermark above the synced frontier would mean
        // a committed LSN is not yet durable — an Order-A / mark_committed misuse — and
        // reclaiming to it could archive an un-synced write). Fail loud, never silently
        // lose. The byte twin of char's `capture_snapshot_immutable` assert.
        assert!(
            watermark_at_capture <= synced_frontier_at_capture,
            "capture_overlay_snapshot: committed watermark {watermark_at_capture} exceeds the \
             durably-synced WAL frontier {synced_frontier_at_capture} — a committed LSN is not \
             yet durable (Order-A / mark_committed misuse); reclaiming to this watermark could \
             archive an un-synced write (GAP_LEDGER #41 capture-ordering invariant violated)"
        );
        let _ = (next_lsn_at_capture, synced_frontier_at_capture);

        let arena_count = self.flush_and_count_arenas()?;

        Ok(CheckpointSnapshot {
            root_type,
            is_final,
            term_count,
            arena_count,
            root_ptr,
            next_lsn_at_capture,
            committed_watermark_at_capture: Some(watermark_at_capture),
            commit_seq_at_capture: Some(commit_seq_at_capture),
        })
    }

    /// **Overlay arm — publish (no eviction).** Publish the overlay snapshot's
    /// durable on-disk image + record `checkpoint_lsn = committed watermark` while
    /// RETAINING the entire WAL (non-double-counting via the `Checkpoint` record;
    /// raises the commit_seq floor). The byte twin of char's
    /// `publish_immutable_snapshot_retaining_wal`. Deliberately NO `truncate`: the
    /// WAL tail `> w` is retained in full, so recovery sees image(≤w) ⊕ WAL(>w) with
    /// NO overlap and NO gap → every acknowledged write survives exactly once.
    pub(crate) fn publish_overlay_snapshot_retaining(
        &self,
        snapshot: &CheckpointSnapshot,
    ) -> Result<()> {
        let checkpoint_lsn = snapshot.committed_watermark_at_capture.ok_or_else(|| {
            PersistentARTrieError::internal(
                "publish_overlay_snapshot_retaining requires an immutable-overlay snapshot \
                 (committed_watermark_at_capture = Some); got an owned-tree snapshot",
            )
        })?;

        // (1) Durable descriptor publish (the on-disk linearization point) + verify.
        self.publish_snapshot(snapshot)?;
        self.verify_checkpoint_header()?;

        // (2) Record `checkpoint_lsn = watermark` so recovery skips deltas ≤ it, then
        //     sync — but RETAIN the WAL (no rotate/truncate).
        if let Some(ref wal_writer) = self.wal_writer {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            wal_writer
                .append(WalRecord::Checkpoint {
                    checkpoint_lsn,
                    timestamp,
                })
                .map_err(|e| {
                    PersistentARTrieError::io_error(
                        "overlay_checkpoint_append",
                        "WAL",
                        std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                    )
                })?;
            wal_writer.sync().map_err(|e| {
                PersistentARTrieError::io_error(
                    "overlay_checkpoint_sync",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            // A3 floor: durably raise the WAL commit_seq floor to the value captured
            // in the watermark window, so a post-checkpoint overlay op out-ranks every
            // pre-checkpoint survivor across a later rotate.
            if let Some(floor) = snapshot.commit_seq_at_capture {
                wal_writer.set_commit_seq_floor(floor).map_err(|e| {
                    PersistentARTrieError::io_error(
                        "overlay_checkpoint_floor",
                        "WAL",
                        std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                    )
                })?;
            }
            // Deliberately NO rotate/truncate (retain-WAL → reversible + non-double-counting).
        }
        Ok(())
    }

    /// **Overlay arm — publish (eviction-on).** Byte has no eviction-registry path on
    /// the overlay snapshot yet (`eviction_coordinator` is always `None` for byte —
    /// see the ctors), so this is identical to
    /// [`Self::publish_overlay_snapshot_retaining`] (the byte twin of char's
    /// `publish_immutable_snapshot_retaining_wal_with_eviction`, minus the registry
    /// publication char does — a Phase-D concern not yet wired for byte).
    pub(crate) fn publish_overlay_snapshot_retaining_with_eviction(
        &self,
        snapshot: CheckpointSnapshot,
    ) -> Result<()> {
        self.publish_overlay_snapshot_retaining(&snapshot)
    }

    // ====================================================================
    // Shared helpers (used by both arms).
    // ====================================================================

    /// Serialize a `TrieRoot` (owned or overlay-converted) into fresh arena slots,
    /// returning `(root_type, root_ptr, is_final)`. The serialize half of byte's
    /// `persist_to_disk`, factored to `&self` + taking the root by reference so the
    /// overlay arm can serialize a converted root WITHOUT mutating `self.root`.
    fn serialize_root(&self, root: &TrieRoot<V>) -> Result<(u8, u64, bool)> {
        match root {
            TrieRoot::Bucket(bucket) => {
                let ptr = self.serialize_bucket_to_disk(bucket)?;
                Ok((ROOT_TYPE_BUCKET, ptr.to_raw(), false))
            }
            TrieRoot::ArtNode {
                node,
                children,
                is_final,
                value: _,
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
                Ok((ROOT_TYPE_ART_NODE, node_ptr.to_raw(), *is_final))
            }
        }
    }

    /// Flush dirty arena slots and return the post-flush arena count (the block IDs
    /// derive from sequential allocation). The arena-flush half of `persist_to_disk`.
    fn flush_and_count_arenas(&self) -> Result<u32> {
        if let Some(ref arena_manager) = self.arena_manager {
            let _ = arena_manager.write().flush_dirty_slots()?;
        }
        Ok(if let Some(ref arena_manager) = self.arena_manager {
            arena_manager.read().arena_count() as u32
        } else {
            0
        })
    }

    /// Publish a captured snapshot's descriptor to block 0, update the header
    /// root-pointer + entry-count, flush all pages, fsync the data file (the on-disk
    /// linearization point), and clear the dirty flag. Shared by both arms. The byte
    /// twin of char's `publish_snapshot`; byte-identical to `persist_to_disk`'s
    /// descriptor-write tail.
    fn publish_snapshot(&self, snapshot: &CheckpointSnapshot) -> Result<()> {
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for disk serialization")
        })?;

        let mut descriptor = [0u8; 18];
        descriptor[0] = snapshot.root_type;
        descriptor[1] = if snapshot.is_final { 1 } else { 0 };
        descriptor[2..6].copy_from_slice(&(snapshot.term_count as u32).to_le_bytes());
        descriptor[6..10].copy_from_slice(&snapshot.arena_count.to_le_bytes());
        descriptor[10..18].copy_from_slice(&snapshot.root_ptr.to_le_bytes());

        const DESCRIPTOR_OFFSET: usize = 64;
        let bm = buffer_manager.write();
        let dm = bm.storage();
        dm.write_bytes(0, DESCRIPTOR_OFFSET, &descriptor)?;

        let root_descriptor_ptr =
            SwizzledPtr::on_disk(0, DESCRIPTOR_OFFSET as u32, NodeType::Bucket);
        dm.set_root_ptr(root_descriptor_ptr.to_raw())?;
        dm.set_entry_count(snapshot.term_count)?;

        bm.flush_all()?;
        dm.sync()?;
        self.dirty.store(false, AtomicOrdering::Release);
        Ok(())
    }

    /// Re-read the file header from disk and verify its checksum (the overlay arm's
    /// durability check, before retaining the WAL). The byte twin of char's
    /// `verify_checkpoint`.
    fn verify_checkpoint_header(&self) -> Result<()> {
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for checkpoint verification")
        })?;
        let bm = buffer_manager.read();
        let dm = bm.storage();
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

    // ====================================================================
    // Overlay → owned conversion (the genuinely per-variant seam).
    // ====================================================================

    /// Convert the IMMUTABLE overlay root (`OverlayNode<ByteKey, V>`) into byte's
    /// owned `TrieRoot` representation, ready for the EXISTING owned serializer. The
    /// byte twin of char's `overlay_to_inner` (which builds a `CharTrieNodeInner`).
    /// Mirrors byte's owned node format EXACTLY (children carried in both the `Node`
    /// child slots — for `find_child_mut` during serialize — and the `children` Vec),
    /// so the produced image is equivalent by construction to an owned snapshot of the
    /// same terms (including byte's documented ART-node value-serialization
    /// limitation, inherited unchanged).
    fn overlay_root_to_owned(root: &OverlayNode<ByteKey, V>) -> TrieRoot<V> {
        let (node, children, is_final, _value_bytes) = Self::overlay_children_to_owned(root);
        // The `TrieRoot`/`ChildNode` ART `value` differ: `TrieRoot::ArtNode.value` is
        // `Option<V>` (deserialized), `ChildNode::ArtNode.value` is `Option<Vec<u8>>`
        // (serialized). Byte's owned serializer (`serialize_root` / `persist_to_disk`)
        // IGNORES the root-level value (`value: _` / `let _ = value`) — root/ART-node
        // value serialization is byte's documented future work — so we pass `None` for
        // the root `Option<V>` (the converted `_value_bytes` would only matter once
        // ART-node value serialization lands; it is inherited unchanged from the owned
        // path, neither regressed nor exceeded). The leaf VALUES (in `ChildNode`s) DO
        // carry their `Option<Vec<u8>>` for when that path is completed.
        let root_value: Option<V> = None;
        match node {
            Some(node) => TrieRoot::ArtNode {
                node,
                children,
                is_final,
                value: root_value,
            },
            // A childless root: the overlay root with no children and not final is the
            // empty trie (empty root bucket); if final, byte represents the empty term
            // in the root node, so we produce a childless ART root marked final.
            None => {
                if is_final {
                    TrieRoot::ArtNode {
                        node: Node::N4(Box::new(Node4::new())),
                        children: Vec::new(),
                        is_final,
                        value: root_value,
                    }
                } else {
                    TrieRoot::Bucket(StringBucket::with_values())
                }
            }
        }
    }

    /// Convert a single overlay node's children into byte's owned `(Node, children
    /// Vec, is_final, value)` tuple. `Node` is `None` when the node has no children
    /// (the caller decides the leaf representation). Recursive.
    fn overlay_children_to_owned(
        node: &OverlayNode<ByteKey, V>,
    ) -> (Option<Node>, Vec<(u8, ChildNode)>, bool, Option<Vec<u8>>) {
        let is_final = node.is_final();
        let value: Option<Vec<u8>> = node.get_value().and_then(|v| {
            crate::serialization::bincode_compat::serialize(&v).ok()
        });

        // Collect converted children (recursively).
        let mut children: Vec<(u8, ChildNode)> = Vec::new();
        for (&edge, child) in node.iter_children() {
            if let Some(child_arc) = child.as_in_mem() {
                let owned_child = Self::overlay_node_to_child(child_arc);
                children.push((edge, owned_child));
            } else if let Some(on_disk) = child.as_on_disk() {
                // On-disk overlay children (a future fault-in/eviction path) carry an
                // already-serialized location; reuse it directly as a DiskRef. Null
                // fillers are never yielded by `iter_children`, but guard defensively.
                if !on_disk.is_null() {
                    children.push((edge, ChildNode::DiskRef { ptr: on_disk.clone() }));
                }
            }
        }

        if children.is_empty() {
            return (None, children, is_final, value);
        }

        // Build the `Node` of the appropriate size class with a slot per child edge
        // (null ptr — the serializer patches the real ptr via `find_child_mut`). The
        // exact byte node-building pattern from `transitions.rs::bucket_to_art`.
        let base = Node4::new();
        let node_enum: Node = if children.len() <= 4 {
            let mut n = base;
            for (edge, _) in &children {
                let _ = n.add_child(*edge, SwizzledPtr::null());
            }
            Node::N4(Box::new(n))
        } else if children.len() <= 16 {
            let mut n = base.grow();
            for (edge, _) in &children {
                let _ = n.add_child(*edge, SwizzledPtr::null());
            }
            Node::N16(Box::new(n))
        } else if children.len() <= 48 {
            let mut n = base.grow().grow();
            for (edge, _) in &children {
                let _ = n.add_child(*edge, SwizzledPtr::null());
            }
            Node::N48(Box::new(n))
        } else {
            let mut n = base.grow().grow().grow();
            for (edge, _) in &children {
                let _ = n.add_child(*edge, SwizzledPtr::null());
            }
            Node::N256(Box::new(n))
        };

        (Some(node_enum), children, is_final, value)
    }

    /// Convert a single non-root overlay node into a byte owned `ChildNode`. A node
    /// with children becomes a `ChildNode::ArtNode` (with the `Node` child slots + the
    /// `children` Vec); a childless node becomes a `ChildNode::ArtNode` with empty
    /// children (a final/non-final leaf), so finality + value carry exactly as byte's
    /// owned ART leaves do.
    fn overlay_node_to_child(node: &OverlayNode<ByteKey, V>) -> ChildNode {
        let (maybe_node, children, is_final, value) = Self::overlay_children_to_owned(node);
        let node = maybe_node.unwrap_or_else(|| Node::N4(Box::new(Node4::new())));
        ChildNode::ArtNode {
            node,
            is_final,
            value,
            children,
        }
    }
}

/// Count the final (terminal) overlay nodes reachable from `root` — the overlay
/// term count. The byte twin of char's `count_overlay_finals`.
fn count_overlay_finals<V: DictionaryValue>(root: &OverlayNode<ByteKey, V>) -> u64 {
    let mut count = 0u64;
    if root.is_final() {
        count += 1;
    }
    for (_edge, child) in root.iter_children() {
        if let Some(child_arc) = child.as_in_mem() {
            count += count_overlay_finals::<V>(child_arc);
        }
    }
    count
}

// ============================================================================
// Byte seam impl of the shared OverlayCheckpoint route-split skeleton.
// ============================================================================

impl<V: DictionaryValue, S: BlockStorage> OverlayCheckpoint<ByteKey, V, S>
    for PersistentARTrie<V, S>
{
    type CheckpointSnapshot = CheckpointSnapshot;

    #[inline]
    fn has_eviction_coordinator(&self) -> bool {
        self.eviction_coordinator.is_some()
    }

    #[inline]
    fn capture_overlay_snapshot(&self) -> Result<CheckpointSnapshot> {
        PersistentARTrie::capture_overlay_snapshot(self)
    }

    #[inline]
    fn publish_overlay_snapshot_retaining(&self, snapshot: &CheckpointSnapshot) -> Result<()> {
        PersistentARTrie::publish_overlay_snapshot_retaining(self, snapshot)
    }

    #[inline]
    fn publish_overlay_snapshot_retaining_with_eviction(
        &self,
        snapshot: CheckpointSnapshot,
    ) -> Result<()> {
        PersistentARTrie::publish_overlay_snapshot_retaining_with_eviction(self, snapshot)
    }

    #[inline]
    fn capture_owned_snapshot(&self) -> Result<CheckpointSnapshot> {
        PersistentARTrie::capture_owned_snapshot(self)
    }

    #[inline]
    fn publish_owned_and_reclaim(&self, snapshot: CheckpointSnapshot) -> Result<()> {
        PersistentARTrie::publish_owned_and_reclaim(self, snapshot)
    }
}
