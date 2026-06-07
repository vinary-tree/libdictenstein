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
//! representation ([`ChildNode`](super::transitions::ChildNode) / [`Node`] / value), then serializes it through the
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
                // Serialize the overlay root DIRECTLY with an ITERATIVE post-order
                // walk (no deep intermediate owned tree), so the on-disk image is
                // equivalent by construction to an owned snapshot of the same terms
                // WITHOUT recursing with key length. The overlay spine is
                // UN-path-compressed (one node per key unit), so a ~500-char term
                // builds a ~500-deep Arc spine; the prior recursive
                // `overlay_root_to_owned` ⇄ `overlay_node_to_child` +
                // `serialize_child_to_disk_with_path` pipeline overflowed the stack
                // (F6 flag-1b). [`Self::serialize_overlay_root_iterative`] flattens
                // the descent onto a heap work-stack and reuses the NON-recursive
                // single-node serializer [`Self::serialize_node_to_disk_with_value`],
                // producing the byte-identical image. `count_overlay_finals` is now
                // iterative too (same reason).
                let entry_count = count_overlay_finals::<V>(&root);
                let (rt, rp, isf) = self.serialize_overlay_root_iterative(&root)?;
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
                value,
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
                // Empty-string support (H1): serialize the root's `Option<V>` value via
                // the M4a node-record HAS_VALUE blob (the same mechanism the child path
                // uses), so a valued empty term ("") on the root survives checkpoint →
                // reopen. Value-less roots stay byte-identical (append_node_value(None)
                // is a no-op). The error is propagated, never swallowed (data-loss path).
                let value_bytes: Option<Vec<u8>> = match value {
                    Some(v) => Some(crate::serialization::bincode_compat::serialize(v).map_err(
                        |e| PersistentARTrieError::internal(format!("serialize root value: {e}")),
                    )?),
                    None => None,
                };
                let node_ptr =
                    self.serialize_node_to_disk_with_value(&node_copy, value_bytes.as_deref())?;
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
    // Overlay → disk serialization (ITERATIVE — the genuinely per-variant seam).
    //
    // F6 flag-1b: serialize the immutable overlay DIRECTLY with an iterative
    // post-order walk, instead of building a deep intermediate owned `ChildNode`
    // tree (`overlay_root_to_owned` ⇄ `overlay_node_to_child`) and serializing it
    // with the recursive `serialize_child_to_disk_with_path`. The overlay spine is
    // UN-path-compressed (one node per key unit), so a ~500-char term builds a
    // ~500-deep Arc spine; the prior recursive conversion + serialize + the
    // intermediate-tree drop each recursed with key length and OVERFLOWED the
    // stack. This single iterative post-order walk eliminates ALL THREE recursions
    // at once: it serializes each node AFTER its in-mem children (so their disk
    // `SwizzledPtr`s are known), reusing the NON-recursive single-node serializer
    // [`Self::serialize_node_to_disk_with_value`]. The produced on-disk image is
    // byte-identical to the prior pipeline: same children order
    // (`iter_children()`, sorted ascending), same post-order arena-allocation
    // order, same node size-class selection, same finality flags, same value
    // blobs, same root-branch handling — exactly as the correspondence tests
    // assert.
    // ====================================================================

    /// Serialize the IMMUTABLE overlay root iteratively and return the root
    /// descriptor `(root_type, root_ptr, is_final)` — the iterative twin of the
    /// prior `serialize_root(&overlay_root_to_owned(root))`. Reproduces
    /// `overlay_root_to_owned`'s three root branches AND `serialize_root`'s
    /// per-branch on-disk encoding EXACTLY:
    ///
    /// * root WITH children → `ROOT_TYPE_ART_NODE`: serialize each in-mem child
    ///   subtree (iteratively) to its disk ptr + reuse any on-disk child ptr
    ///   verbatim, build the owned `Node` of the right size class with those child
    ///   ptrs patched in, then serialize the root node WITH its typed `Option<V>`
    ///   value blob (the H1/H2 empty-"" support — propagated with `?`, NOT
    ///   swallowed, matching `serialize_root`). The root node record's final flag
    ///   is NOT set (the prior root path never set it on the node — finality rides
    ///   in the descriptor's `is_final`).
    /// * childless + final root → a childless `Node4` ART root marked final in the
    ///   descriptor (node record final flag still unset), carrying the root value.
    /// * childless + non-final root → `ROOT_TYPE_BUCKET` (an empty values-bucket),
    ///   byte-identical to `overlay_root_to_owned`'s `Bucket` arm.
    fn serialize_overlay_root_iterative(
        &self,
        root: &OverlayNode<ByteKey, V>,
    ) -> Result<(u8, u64, bool)> {
        let is_final = root.is_final();
        // Empty-string support (H2): the root's value is read DIRECTLY off the
        // overlay root as the typed `Option<V>` (no bincode round-trip), exactly
        // as `overlay_root_to_owned` did; `serialize_node_to_disk_with_value`
        // re-serializes it into the root node record's HAS_VALUE blob and the load
        // path reads it back. For membership (`V = ()`) this is `None`. The
        // serialize error is PROPAGATED (`?`), never swallowed — this is the
        // data-loss-critical root value, so it matches `serialize_root`'s `?`.
        let root_value: Option<V> = root.get_value();

        // Resolve the root's DIRECT children to disk ptrs (each in-mem child via the
        // iterative subtree serializer; each on-disk child reused verbatim), in
        // `iter_children()` (sorted-ascending) order — the same order
        // `overlay_children_to_owned` collected them, so arena-allocation order is
        // preserved.
        let child_ptrs = self.serialize_overlay_children_iterative(root)?;

        if child_ptrs.is_empty() {
            // Childless root. `overlay_root_to_owned`: final ⇒ childless ART root
            // marked final (in the descriptor); non-final ⇒ empty bucket root.
            if is_final {
                let node = Node::N4(Box::new(Node4::new()));
                let value_bytes = Self::serialize_root_value_bytes(root_value.as_ref())?;
                let node_ptr =
                    self.serialize_node_to_disk_with_value(&node, value_bytes.as_deref())?;
                return Ok((ROOT_TYPE_ART_NODE, node_ptr.to_raw(), is_final));
            }
            let ptr = self.serialize_bucket_to_disk(&StringBucket::with_values())?;
            return Ok((ROOT_TYPE_BUCKET, ptr.to_raw(), false));
        }

        // Root WITH children: build the owned `Node` of the right size class with the
        // resolved child ptrs patched in, then serialize the node WITH the root value
        // blob — byte-identical to `serialize_root`'s `ArtNode` arm.
        let node = Self::build_owned_node_with_child_ptrs(&child_ptrs);
        let value_bytes = Self::serialize_root_value_bytes(root_value.as_ref())?;
        let node_ptr = self.serialize_node_to_disk_with_value(&node, value_bytes.as_deref())?;
        Ok((ROOT_TYPE_ART_NODE, node_ptr.to_raw(), is_final))
    }

    /// Serialize the in-mem children of `node` (each to a disk `SwizzledPtr` via the
    /// iterative subtree serializer) and reuse any on-disk child ptr verbatim,
    /// returning `(edge, disk_ptr)` pairs in `iter_children()` (sorted-ascending)
    /// order. Shared by the root path and the per-node iterative builder.
    fn serialize_overlay_children_iterative(
        &self,
        node: &OverlayNode<ByteKey, V>,
    ) -> Result<Vec<(u8, SwizzledPtr)>> {
        let mut child_ptrs: Vec<(u8, SwizzledPtr)> = Vec::with_capacity(node.num_children());
        for (&edge, child) in node.iter_children() {
            if let Some(child_arc) = child.as_in_mem() {
                let ptr = self.serialize_overlay_subtree_iterative(child_arc)?;
                child_ptrs.push((edge, ptr));
            } else if let Some(on_disk) = child.as_on_disk() {
                // On-disk overlay children (a future fault-in/eviction path) carry an
                // already-serialized location; reuse it directly (the prior
                // `ChildNode::DiskRef` path did the same). Null fillers are never
                // yielded by `iter_children`, but guard defensively.
                if !on_disk.is_null() {
                    child_ptrs.push((edge, on_disk.clone()));
                }
            }
        }
        Ok(child_ptrs)
    }

    /// Serialize ONE non-root overlay subtree iteratively (post-order) and return
    /// the disk `SwizzledPtr` of its top node — the iterative twin of the prior
    /// `serialize_child_to_disk_with_path(&overlay_node_to_child(node), path)`,
    /// WITHOUT recursing with key length.
    ///
    /// # Work-stack post-order
    ///
    /// Each frame holds an overlay node, the in-mem children still to descend into
    /// (in REVERSE `iter_children()` order so they pop ascending — matching the
    /// recursive DFS's child visitation, hence the arena-allocation order), and the
    /// `(edge, disk_ptr)` slots resolved so far (on-disk children recorded
    /// immediately, in-mem children filled when their subtree frame completes). When
    /// a frame's children are all resolved, its owned `Node` is built + the node
    /// serialized via the NON-recursive [`Self::serialize_node_to_disk_with_value`],
    /// and the resulting ptr bubbles up to the parent frame's pending slot.
    fn serialize_overlay_subtree_iterative(
        &self,
        root_arc: &std::sync::Arc<OverlayNode<ByteKey, V>>,
    ) -> Result<SwizzledPtr> {
        // A pending child slot in a parent frame: an `edge` byte awaiting the disk
        // ptr its in-mem subtree will produce (`None` until that subtree completes).
        struct PendingChild {
            edge: u8,
            ptr: Option<SwizzledPtr>,
        }
        // A work-stack frame: one overlay node mid-descent.
        struct Frame<'a, V: DictionaryValue> {
            node: &'a OverlayNode<ByteKey, V>,
            // The edge byte from this frame's PARENT to this node (`None` for the
            // subtree root). Used to fill the parent's matching slot when this frame
            // finishes — strict DFS means the parent's slot for `parent_edge` is the
            // one to set.
            parent_edge: Option<u8>,
            // In-mem children still to descend into, REVERSED so `pop()` yields
            // ascending `iter_children()` order (matches the recursive DFS).
            pending_in_mem: Vec<(u8, &'a std::sync::Arc<OverlayNode<ByteKey, V>>)>,
            // All child slots in `iter_children()` (sorted-ascending) order; in-mem
            // slots start `ptr: None` and are filled as their subtrees finish,
            // on-disk slots are pre-filled.
            slots: Vec<PendingChild>,
        }

        // Build a frame for an overlay node: pre-fill on-disk child slots, queue the
        // in-mem children for descent, preserving `iter_children()` ordering.
        fn make_frame<'a, V: DictionaryValue>(
            node: &'a OverlayNode<ByteKey, V>,
            parent_edge: Option<u8>,
        ) -> Frame<'a, V> {
            let n = node.num_children();
            let mut slots: Vec<PendingChild> = Vec::with_capacity(n);
            let mut pending_in_mem: Vec<(u8, &'a std::sync::Arc<OverlayNode<ByteKey, V>>)> =
                Vec::with_capacity(n);
            for (&edge, child) in node.iter_children() {
                if let Some(child_arc) = child.as_in_mem() {
                    slots.push(PendingChild { edge, ptr: None });
                    pending_in_mem.push((edge, child_arc));
                } else if let Some(on_disk) = child.as_on_disk() {
                    if !on_disk.is_null() {
                        slots.push(PendingChild {
                            edge,
                            ptr: Some(on_disk.clone()),
                        });
                    }
                }
            }
            // Reverse so `pop()` descends in ascending edge order (the recursive DFS
            // visited children in ascending `iter_children()` order).
            pending_in_mem.reverse();
            Frame {
                node,
                parent_edge,
                pending_in_mem,
                slots,
            }
        }

        let mut stack: Vec<Frame<'_, V>> = Vec::new();
        stack.push(make_frame(root_arc.as_ref(), None));
        // The (parent_edge, disk_ptr) produced by the most-recently-completed child
        // subtree, to be recorded into its parent frame's matching pending slot.
        let mut completed: Option<(u8, SwizzledPtr)> = None;

        loop {
            let frame = stack
                .last_mut()
                .expect("serialize_overlay_subtree_iterative: non-empty work-stack");

            // Record a just-completed child subtree's ptr into this frame's slot.
            if let Some((edge, ptr)) = completed.take() {
                let slot = frame
                    .slots
                    .iter_mut()
                    .find(|s| s.edge == edge && s.ptr.is_none())
                    .expect("completed child edge has a matching unfilled parent slot");
                slot.ptr = Some(ptr);
            }

            // Descend into the next in-mem child, if any remain.
            if let Some((edge, child_arc)) = frame.pending_in_mem.pop() {
                stack.push(make_frame(child_arc.as_ref(), Some(edge)));
                continue;
            }

            // All children of this frame are resolved → serialize THIS node.
            let frame = stack
                .pop()
                .expect("serialize_overlay_subtree_iterative: frame to finalize");
            let child_ptrs: Vec<(u8, SwizzledPtr)> = frame
                .slots
                .into_iter()
                .map(|s| {
                    (
                        s.edge,
                        s.ptr.expect(
                            "every in-mem child slot is filled before its parent node is \
                             serialized (post-order invariant)",
                        ),
                    )
                })
                .collect();
            let node_ptr = self.serialize_overlay_node_to_disk(frame.node, &child_ptrs)?;

            match frame.parent_edge {
                // Bubble this node's ptr up to its parent frame, keyed by the edge
                // the parent used to reach it (strict DFS ⇒ that slot is unfilled).
                Some(edge) => {
                    completed = Some((edge, node_ptr));
                }
                // Subtree root → return its disk ptr.
                None => return Ok(node_ptr),
            }
        }
    }

    /// Serialize ONE overlay node (root-or-non-root distinction is the caller's)
    /// into byte's owned single-node disk record, given its children ALREADY
    /// resolved to disk `SwizzledPtr`s. The NON-recursive core that the iterative
    /// walk calls per node — the exact body of the prior
    /// `serialize_child_to_disk_with_path`'s `ArtNode` arm minus the recursion +
    /// dirty-tracking (the overlay capture builds a fresh image; the recursive
    /// path's `needs_persistence()`/cache shortcut never fires for a fresh
    /// overlay-converted node, so omitting it is image-equivalent).
    ///
    /// Builds the owned `Node` of the right size class with the child ptrs patched
    /// in, sets the node's final flag from the overlay node, serializes the node's
    /// `Option<V>` value blob (via `.ok()` — matching the prior child path, which
    /// swallowed a child value serialize error rather than propagating; the root
    /// path uses `?` separately), and serializes via the NON-recursive
    /// [`Self::serialize_node_to_disk_with_value`].
    fn serialize_overlay_node_to_disk(
        &self,
        node: &OverlayNode<ByteKey, V>,
        child_ptrs: &[(u8, SwizzledPtr)],
    ) -> Result<SwizzledPtr> {
        // A childless overlay node became a `ChildNode::ArtNode` with an empty
        // `children` Vec and `node = Node4::new()` (`overlay_node_to_child`'s
        // `unwrap_or_else`), so a leaf serializes as an empty Node4.
        let mut node_copy = if child_ptrs.is_empty() {
            Node::N4(Box::new(Node4::new()))
        } else {
            Self::build_owned_node_with_child_ptrs(child_ptrs)
        };

        // Final flag: the prior `serialize_child_to_disk_with_path` set it from the
        // `ChildNode::ArtNode { is_final }` (= the overlay node's finality).
        node_copy.header_mut().set_final(node.is_final());

        // Value blob: the prior child path computed the bincode of the overlay
        // node's value with `.ok()` (swallowing a serialize error → `None`), so
        // reproduce that exactly here (NOT `?` — that is the root path's behavior).
        let value_bytes: Option<Vec<u8>> = node
            .get_value()
            .and_then(|v| crate::serialization::bincode_compat::serialize(&v).ok());

        self.serialize_node_to_disk_with_value(&node_copy, value_bytes.as_deref())
    }

    /// Build an owned byte `Node` of the appropriate size class with one child slot
    /// per `(edge, ptr)` (the real disk ptr installed). The size-class selection +
    /// the `Node4::new().grow()...` build sequence are byte-identical to the prior
    /// `overlay_children_to_owned` (which built the node with null slots then patched
    /// via `find_child_mut`) followed by `serialize_*`'s patch step — here we install
    /// the ptr directly via `add_child`, producing the same node.
    fn build_owned_node_with_child_ptrs(child_ptrs: &[(u8, SwizzledPtr)]) -> Node {
        let base = Node4::new();
        let n = child_ptrs.len();
        if n <= 4 {
            let mut node = base;
            for (edge, ptr) in child_ptrs {
                let _ = node.add_child(*edge, ptr.clone());
            }
            Node::N4(Box::new(node))
        } else if n <= 16 {
            let mut node = base.grow();
            for (edge, ptr) in child_ptrs {
                let _ = node.add_child(*edge, ptr.clone());
            }
            Node::N16(Box::new(node))
        } else if n <= 48 {
            let mut node = base.grow().grow();
            for (edge, ptr) in child_ptrs {
                let _ = node.add_child(*edge, ptr.clone());
            }
            Node::N48(Box::new(node))
        } else {
            let mut node = base.grow().grow().grow();
            for (edge, ptr) in child_ptrs {
                let _ = node.add_child(*edge, ptr.clone());
            }
            Node::N256(Box::new(node))
        }
    }

    /// Serialize the ROOT's typed `Option<V>` value to the node-record value blob,
    /// PROPAGATING any error (`?`) — the data-loss-critical empty-"" root value path,
    /// matching `serialize_root`'s `value_bytes` handling exactly (NOT the child
    /// path's `.ok()`).
    fn serialize_root_value_bytes(value: Option<&V>) -> Result<Option<Vec<u8>>> {
        match value {
            Some(v) => Ok(Some(
                crate::serialization::bincode_compat::serialize(v).map_err(|e| {
                    PersistentARTrieError::internal(format!("serialize root value: {e}"))
                })?,
            )),
            None => Ok(None),
        }
    }
}

/// Count the final (terminal) overlay nodes reachable from `root` — the overlay
/// term count. The byte twin of char's `count_overlay_finals`. **ITERATIVE**
/// (explicit work-stack over `Child::InMem`) so it does not recurse with key
/// length — the un-path-compressed overlay spine is ~key-length deep, so the prior
/// recursion overflowed the stack on large terms (F6 flag-1b).
fn count_overlay_finals<V: DictionaryValue>(root: &OverlayNode<ByteKey, V>) -> u64 {
    let mut count = 0u64;
    let mut stack: Vec<&OverlayNode<ByteKey, V>> = Vec::new();
    stack.push(root);
    while let Some(node) = stack.pop() {
        if node.is_final() {
            count += 1;
        }
        for (_edge, child) in node.iter_children() {
            if let Some(child_arc) = child.as_in_mem() {
                stack.push(child_arc.as_ref());
            }
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
