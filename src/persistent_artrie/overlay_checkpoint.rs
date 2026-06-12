//! Byte seam impl of the shared [`OverlayCheckpoint`] checkpoint skeleton
//! (`overlay-durable-architecture.md`, trait 3). The byte twin of
//! `persistent_artrie::char::{overlay_write_mode (impl), persist}`.
//!
//! The generic [`OverlayCheckpoint::checkpoint_route_split`] default owns the
//! data-loss-critical skeleton (capture the IMMUTABLE OVERLAY — the SOLE live
//! representation since L3.3 deleted the owned tree — then publish via the
//! watermark-bounded RETAINING publisher). This module supplies ONLY the per-variant
//! capture + publish seams, which are GENUINELY per-variant (byte arena on-disk
//! format ≠ char arena format).
//!
//! **L3.3 — overlay-only.** `route_overlay()` is universally true (every byte ctor
//! installs the overlay), so the checkpoint always captures from the immutable
//! overlay + publishes the watermark-bounded RETAINING image. The historical owned
//! capture/publish arm, `serialize_root`, and the RES-4 route-split were deleted with
//! the owned tree.
//!
//! # The overlay capture produces byte's dense on-disk image
//!
//! [`PersistentARTrie::capture_overlay_snapshot`] walks the immutable overlay root
//! (`OverlayNode<ByteKey, V>`) and serializes each node DIRECTLY into byte's owned
//! on-disk node format via the iterative serializer
//! ([`PersistentARTrie::serialize_overlay_root_iterative`] →
//! [`PersistentARTrie::serialize_node_to_disk_with_value`]). For the same logical
//! data the on-disk image is byte-identical to what an owned tree built from the same
//! terms would have produced — exactly char's correctness property
//! (`capture_snapshot_immutable`). The serialize mirrors byte's documented
//! value-on-ArtNode behavior EXACTLY (`disk_load.rs`), so it neither regresses nor
//! pretends to exceed the prior path.

#![cfg(feature = "persistent-artrie")]

use std::sync::atomic::Ordering as AtomicOrdering;

use super::block_storage::BlockStorage;
use super::bucket::StringBucket;
use super::dict_impl::{PersistentARTrie, ROOT_TYPE_ART_NODE, ROOT_TYPE_BUCKET, ROOT_TYPE_EMPTY};
use super::error::{PersistentARTrieError, Result};
use super::nodes::{ArtNode, Node, Node4};
use super::swizzled_ptr::{NodeType, SwizzledPtr};
use super::wal::WalRecord;
use crate::persistent_artrie::core::key_encoding::ByteKey;
use crate::persistent_artrie::core::overlay::checkpoint::OverlayCheckpoint;
use crate::persistent_artrie::core::overlay::compressed_serialize::OverlayCompressedSerialize;
use crate::persistent_artrie::core::overlay::OverlayNode;
use crate::persistent_artrie::eviction::DiskLocationRegistry;
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
    /// **Overlay-arm capture only, eviction-ON (Phase 6 — the byte twin of char's
    /// `CheckpointSnapshot.eviction_registry`).** The freshly-built per-node disk-location
    /// registry, populated during the overlay serialize (`register` per InMem node, with
    /// `set_durable_stamp` stamping each live overlay node — the M-2a eviction-safety
    /// lynchpin). `Some(reg)` ONLY when an eviction coordinator is installed at
    /// [`PersistentARTrie::capture_overlay_snapshot`]; `None` on the owned arm AND on the
    /// eviction-OFF overlay arm (the existing byte opt-in durable tests are the named
    /// regression gate that it stays `None` there). The eviction-on retaining publisher
    /// moves it into the coordinator AFTER `verify_checkpoint_header` (publish-after-verify).
    /// NEVER serialized to disk (a runtime side-table; recovery never reads it).
    eviction_registry: Option<crate::persistent_artrie::eviction::DiskLocationRegistry>,
}

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    // ====================================================================
    // The checkpoint capture + publish (L3.3 — overlay-only): capture from the
    // IMMUTABLE overlay + publish RETAINING the WAL. The owned-tree capture/publish
    // and `serialize_root` were deleted with the owned tree.
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

        // Phase 6 (byte serialize-time registration, byte twin of char persist.rs:289):
        // build a FRESH per-trie disk-location registry IFF an eviction coordinator is
        // installed. `serialize_overlay_node_to_disk` `register`s each InMem node into it
        // (and `set_durable_stamp`s the live overlay node — the M-2a lynchpin). It stays
        // `None` on the eviction-OFF arm — the existing byte opt-in durable tests are the
        // M-5a regression gate that an eviction-OFF checkpoint publishes no registry.
        let mut eviction_registry = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
            .map(|_| crate::persistent_artrie::eviction::DiskLocationRegistry::new());

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
                // CX-universal: the regular checkpoint capture now serializes via the PATH-COMPRESSED
                // serializer (was the uncompressed `serialize_overlay_root_iterative`), passing the
                // eviction registry so an eviction-ON checkpoint compresses AND #6-stamps each chunk
                // at its true expanded depth — matching char's `capture_snapshot_immutable`. The byte
                // loader is already prefix-aware (folds `prefix_len>0` chunks back into chains on
                // reopen), and uncompressed `prefix_len=0` images still load (forward-compatible).
                //
                // Root-descriptor rule: IDENTICAL to `compact_publish_compressed_overlay` (the empty/
                // bucket override) and to the old `serialize_overlay_root_iterative` — a childless
                // NON-final root is an empty values-bucket (`ROOT_TYPE_BUCKET`, the byte loader's
                // convention); everything else is `ROOT_TYPE_ART_NODE`. NEVER `ROOT_TYPE_NODE` (that
                // is char's distinct scheme). DATA-LOSS callout: for a childless-FINAL root the
                // compressed serializer's `root_ptr` already carries the root value (the terminus
                // record serialized + registered at `path=[]`), so the `else` arm is correct; the
                // bucket override fires only for the childless NON-final (0-term) root, whose discarded
                // compressed record is harmless (empty registry — eviction never acts on it).
                let entry_count = count_overlay_finals::<V>(&root);
                let root_ptr =
                    self.serialize_overlay_snapshot_compressed(&root, eviction_registry.as_mut())?;
                let is_final = root.is_final();
                let (rt, rp) = if root.num_children() == 0 && !is_final {
                    let bucket_ptr = self.serialize_bucket_to_disk(&StringBucket::with_values())?;
                    (ROOT_TYPE_BUCKET, bucket_ptr.to_raw())
                } else {
                    (ROOT_TYPE_ART_NODE, root_ptr.to_raw())
                };
                (rt, rp, is_final, entry_count)
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

        let arena_count = self.flush_and_count_arenas()?;

        Ok(CheckpointSnapshot {
            root_type,
            is_final,
            term_count,
            arena_count,
            root_ptr,
            committed_watermark_at_capture: Some(watermark_at_capture),
            commit_seq_at_capture: Some(commit_seq_at_capture),
            // Phase 6: the registry built above (populated by the serialize walk) when a
            // coordinator is installed; `None` otherwise. The eviction-on publisher moves
            // it into the coordinator after `verify_checkpoint_header` (publish-after-verify).
            eviction_registry,
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
        let base_watermark = snapshot.committed_watermark_at_capture.ok_or_else(|| {
            PersistentARTrieError::internal(
                "publish_overlay_snapshot_retaining requires an immutable-overlay snapshot \
                 (committed_watermark_at_capture = Some); got an owned-tree snapshot",
            )
        })?;
        // C2 (recovery double-apply fix): the on-disk `Checkpoint.checkpoint_lsn` is an
        // IMAGE-COVERAGE fact (it drives the reopen drain-skip), NOT the durability watermark.
        // A post-recovery rebuild folds the archived records into this image but applies them
        // NO-WAL (so the durability watermark is genuinely 0); record max(watermark, coverage)
        // WITHOUT inflating the watermark — the #41 capture assert is untouched. `take` is
        // one-shot: only the FIRST post-recovery checkpoint carries it; it is 0 for every
        // normal checkpoint ⇒ `checkpoint_lsn = watermark`, byte-identical to before.
        let checkpoint_lsn =
            base_watermark.max(self.committed_watermark.take_recovery_image_coverage());

        // (1) Durable descriptor publish (the on-disk linearization point) + verify. #48: the
        // image self-describes its coverage (`checkpoint_lsn`), fsync'd atomically with it.
        self.publish_snapshot(snapshot, Some(checkpoint_lsn))?;
        self.verify_checkpoint_header()?;

        // (2) Record `checkpoint_lsn = watermark` so recovery skips deltas ≤ it, then
        //     sync — but RETAIN the WAL (no rotate/truncate).
        if let Some(ref wal_writer) = self.wal_writer {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let checkpoint_record_lsn = wal_writer
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
            // #49: mark the `Checkpoint` record's LSN committed (durable via the `sync()` above) so
            // the contiguous committed-watermark prefix does NOT stall behind it. Otherwise every
            // later steady-state checkpoint captures a watermark frozen at the first checkpoint's
            // predecessor LSN → under-claims image coverage → post-checkpoint counter deltas re-drain
            // on reopen (double-apply). Marking restores `watermark == committed-write frontier`. Safe:
            // synced BEFORE marking (#41 `watermark ≤ synced_frontier` holds), a control record is
            // nothing to lose. See docs/design/checkpoint-record-lsn-watermark-gap-49-design-2026-06-08.md.
            self.committed_watermark
                .mark_committed(checkpoint_record_lsn);
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

    /// **Overlay arm — publish (eviction-on).** Phase 6: the byte twin of char's
    /// `publish_immutable_snapshot_retaining_wal_with_eviction`. As
    /// [`Self::publish_overlay_snapshot_retaining`] PLUS publishing the eviction registry
    /// into the coordinator — ONLY AFTER `verify_checkpoint_header` proves the on-disk
    /// image durable (the publish-after-verify ordering: an evictor must never unswizzle a
    /// node onto a not-yet-durable location). CONSUMES the snapshot (the registry MOVES
    /// into the coordinator). The registry publication is an in-memory `RwLock::write`
    /// swap with ZERO fsync (no per-checkpoint fsync-count asymmetry vs the eviction-OFF
    /// publisher). Requires an immutable-overlay snapshot (`committed_watermark_at_capture
    /// = Some`); an owned-tree snapshot is rejected.
    ///
    /// SAFETY (the #41 + 1c chain): victims come ONLY from this post-verify registry
    /// (nodes durable ≤ the captured committed watermark), and the per-node `durable_stamp`
    /// guard (M-2a) refuses to evict any node overwritten since this checkpoint. A
    /// post-checkpoint durable write INVALIDATES the registry at the
    /// `append_mutation_wal_record` chokepoint (Phase 6 byte invalidation) BEFORE its
    /// visibility, so eviction then reclaims nothing from a dirtied registry (liveness,
    /// not safety).
    pub(crate) fn publish_overlay_snapshot_retaining_with_eviction(
        &self,
        snapshot: CheckpointSnapshot,
    ) -> Result<()> {
        let base_watermark = snapshot.committed_watermark_at_capture.ok_or_else(|| {
            PersistentARTrieError::internal(
                "publish_overlay_snapshot_retaining_with_eviction requires an immutable-overlay \
                 snapshot (committed_watermark_at_capture = Some); got an owned-tree snapshot",
            )
        })?;
        // C2 (see `publish_overlay_snapshot_retaining`): image-coverage frontier, one-shot,
        // does not inflate the watermark.
        let checkpoint_lsn =
            base_watermark.max(self.committed_watermark.take_recovery_image_coverage());

        // (1) Durable descriptor publish (the on-disk linearization point) + verify.
        //     `publish_snapshot(&snapshot)` BORROWS the snapshot before the move below.
        // #48: the image self-describes its coverage, fsync'd atomically with it.
        self.publish_snapshot(&snapshot, Some(checkpoint_lsn))?;
        self.verify_checkpoint_header()?;

        // (2) Publish the eviction registry — ONLY AFTER verify proves the image durable
        //     (publish-after-verify). The registry CONSUMES (moves) here;
        //     `update_disk_registry` is an in-memory `RwLock::write` swap with ZERO fsync.
        //     The byte twin of char's `update_disk_registry(registry)` tail. `register`
        //     (byte map) populated it; `force_eviction`'s `select_for_eviction` reads it.
        if let Some(registry) = snapshot.eviction_registry {
            if let Some(coordinator) = self
                .eviction_coordinator
                .lock()
                .expect("eviction_coordinator mutex poisoned")
                .as_ref()
            {
                coordinator.update_disk_registry(registry);
            }
        }

        // (3) Record `checkpoint_lsn = watermark` so recovery skips deltas ≤ it, then
        //     sync — but RETAIN the WAL (no rotate/truncate). Byte-identical to
        //     `publish_overlay_snapshot_retaining`'s WAL tail.
        if let Some(ref wal_writer) = self.wal_writer {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let checkpoint_record_lsn = wal_writer
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
            // #49: mark the `Checkpoint` record's LSN committed (durable via the `sync()` above) so
            // the contiguous committed-watermark prefix does not stall behind it — identical to
            // `publish_overlay_snapshot_retaining`. See
            // docs/design/checkpoint-record-lsn-watermark-gap-49-design-2026-06-08.md.
            self.committed_watermark
                .mark_committed(checkpoint_record_lsn);
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

        // (4) RESIDENT-BUDGET TAIL (Phase 7.5 — GO-LIVE; byte twin of char's). After
        //     publish+verify (1), registry-publish (2), and WAL Checkpoint sync (3) — so
        //     every registered disk_ptr is durable — evict the COLDEST registered byte
        //     overlay nodes down to the configured resident budget. Non-blocking
        //     loser-safe root-CAS; the 1c stamp guard + registry is_valid() gate keep it
        //     safe under concurrent writers. DEADLOCK-SAFETY: the coordinator is bound in
        //     a `let` so the eviction_coordinator guard drops at the `;` BEFORE the
        //     callback (`evict_overlay_nodes`) re-locks it for LRU bookkeeping (see char).
        let coordinator = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
            .map(std::sync::Arc::clone);
        if let Some(coordinator) = coordinator {
            if let Some(budget) = coordinator.resident_budget_bytes() {
                let resident = coordinator.byte_resident_estimate_bytes();
                if resident > budget {
                    let target = resident - budget;
                    let max_count = coordinator
                        .resident_budget_eviction_cap()
                        .unwrap_or(usize::MAX);
                    coordinator.force_eviction_bytes_resident(target, max_count, |nodes| {
                        crate::persistent_artrie::overlay_fault::evict_overlay_nodes(self, nodes, 4)
                    });
                }
            }
        }
        Ok(())
    }

    // ====================================================================
    // Shared helpers (used by the capture + publish path).
    // ====================================================================

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
    fn publish_snapshot(
        &self,
        snapshot: &CheckpointSnapshot,
        image_checkpoint_lsn: Option<u64>,
    ) -> Result<()> {
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
        // C2/#48: record the IMAGE-COVERAGE frontier in block-0, ATOMICALLY with the image (it
        // rides the same `dm.sync()` below). The overlay retaining publishers pass Some(_) so a
        // torn WAL `Checkpoint` record cannot poison the reopen drain-skip (the image
        // self-describes its coverage; reopen takes max(wal_record, this)). The owned arm passes
        // None — it truncates the WAL ⇒ no re-drain ⇒ no torn-window bug; byte-identical, no v2 upgrade.
        if let Some(cov) = image_checkpoint_lsn {
            dm.set_image_checkpoint_lsn(cov)?;
        }

        bm.flush_all()?;
        dm.sync()?;
        self.dirty.store(false, AtomicOrdering::Release);
        Ok(())
    }

    /// **L2.1 — CX compaction publish.** Serialize `source_root` (the SOURCE trie's overlay
    /// snapshot) COMPRESSED into THIS (staging) trie's arena via the path-compressing
    /// [`Self::serialize_overlay_snapshot_compressed`], then durably publish it as the block-0 root
    /// descriptor through the SAME audited [`Self::publish_snapshot`] tail `checkpoint()` uses — with
    /// NO owned staging tree, NO `insert_impl_no_wal`. The first production caller of the CX codec.
    ///
    /// Root-descriptor fields mirror [`Self::serialize_overlay_root_iterative`]'s heuristic: a
    /// childless non-final root ("" never a member, no children) → `ROOT_TYPE_BUCKET` (an empty
    /// values-bucket, byte-identical to the owned/iterative arm); otherwise `ROOT_TYPE_ART_NODE` with
    /// the CX serializer's root ptr (a branching or childless-final root). Finality rides the
    /// descriptor (`is_final`), the same as the owned image. The image-checkpoint-lsn is `None` (the
    /// staging WAL is discarded pre-rename — the owned-arm convention).
    pub(crate) fn compact_publish_compressed_overlay(
        &self,
        source_root: &std::sync::Arc<OverlayNode<ByteKey, V>>,
        term_count: u64,
    ) -> Result<()> {
        let root_ptr = self.serialize_overlay_snapshot_compressed(source_root, None)?;
        let is_final = source_root.is_final();
        let (root_type, root_ptr_raw) = if source_root.num_children() == 0 && !is_final {
            let bucket_ptr = self.serialize_bucket_to_disk(&StringBucket::with_values())?;
            (ROOT_TYPE_BUCKET, bucket_ptr.to_raw())
        } else {
            (ROOT_TYPE_ART_NODE, root_ptr.to_raw())
        };
        let arena_count = self.flush_and_count_arenas()?;
        let snapshot = CheckpointSnapshot {
            root_type,
            is_final,
            term_count,
            arena_count,
            root_ptr: root_ptr_raw,
            committed_watermark_at_capture: None,
            commit_seq_at_capture: None,
            eviction_registry: None,
        };
        self.publish_snapshot(&snapshot, None)
    }

    /// **L2.1 — CX compaction publish of an EMPTY source** (0 terms / no overlay root). Publishes an
    /// empty values-bucket root, byte-identical to what the owned-staging arm's `checkpoint()` of an
    /// empty owned tree produces (`ROOT_TYPE_BUCKET`), without an owned staging tree.
    pub(crate) fn compact_publish_empty(&self, term_count: u64) -> Result<()> {
        let bucket_ptr = self.serialize_bucket_to_disk(&StringBucket::with_values())?;
        let arena_count = self.flush_and_count_arenas()?;
        let snapshot = CheckpointSnapshot {
            root_type: ROOT_TYPE_BUCKET,
            is_final: false,
            term_count,
            arena_count,
            root_ptr: bucket_ptr.to_raw(),
            committed_watermark_at_capture: None,
            commit_seq_at_capture: None,
            eviction_registry: None,
        };
        self.publish_snapshot(&snapshot, None)
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
    ///
    /// Phase 6 (M-5a): `registry` is threaded through the whole walk; when `Some`, each
    /// serialized InMem node is `register`ed at its full path (the root at `[]`) and its
    /// live overlay node `set_durable_stamp`ed — the byte twin of char's
    /// `serialize_overlay_to_disk_iterative`. The root's path is the empty `Vec<u8>`.
    // Uncompressed baseline: SUPERSEDED in production by the CX-universal compressed
    // serializer; retained #[cfg(test)] as the density-comparison oracle.
    #[cfg(test)]
    fn serialize_overlay_root_iterative(
        &self,
        root: &OverlayNode<ByteKey, V>,
        mut registry: Option<&mut crate::persistent_artrie::eviction::DiskLocationRegistry>,
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

        // The root's full key path is empty (`[]`); children descend from it. Maintained
        // exactly as char's iterative walk maintains its `path` (push on descent into an
        // in-mem child, pop on completion) so each node registers at its real path.
        let mut path: Vec<u8> = Vec::new();

        // Resolve the root's DIRECT children to disk ptrs (each in-mem child via the
        // iterative subtree serializer; each on-disk child reused verbatim), in
        // `iter_children()` (sorted-ascending) order — the same order
        // `overlay_children_to_owned` collected them, so arena-allocation order is
        // preserved.
        let child_ptrs =
            self.serialize_overlay_children_iterative(root, &mut path, registry.as_deref_mut())?;

        if child_ptrs.is_empty() {
            // Childless root. `overlay_root_to_owned`: final ⇒ childless ART root
            // marked final (in the descriptor); non-final ⇒ empty bucket root.
            if is_final {
                let node = Node::N4(Box::new(Node4::new()));
                let value_bytes = Self::serialize_root_value_bytes(root_value.as_ref())?;
                // Register the (childless final) root at path `[]` + stamp it. The bucket
                // arm below is NOT registered (it is a values-bucket, not an overlay node
                // the evictor unswizzles — char's childless-non-final root is likewise not
                // a registered overlay node).
                let node_ptr = self.serialize_overlay_node_record_registering(
                    &node,
                    value_bytes.as_deref(),
                    root,
                    &path,
                    registry.as_deref_mut(),
                )?;
                return Ok((ROOT_TYPE_ART_NODE, node_ptr.to_raw(), is_final));
            }
            let ptr = self.serialize_bucket_to_disk(&StringBucket::with_values())?;
            return Ok((ROOT_TYPE_BUCKET, ptr.to_raw(), false));
        }

        // Root WITH children: build the owned `Node` of the right size class with the
        // resolved child ptrs patched in, then serialize the node WITH the root value
        // blob — byte-identical to `serialize_root`'s `ArtNode` arm. Register the root at
        // path `[]` + stamp it.
        let node = Self::build_owned_node_with_child_ptrs(&child_ptrs);
        let value_bytes = Self::serialize_root_value_bytes(root_value.as_ref())?;
        let node_ptr = self.serialize_overlay_node_record_registering(
            &node,
            value_bytes.as_deref(),
            root,
            &path,
            registry.as_deref_mut(),
        )?;
        Ok((ROOT_TYPE_ART_NODE, node_ptr.to_raw(), is_final))
    }

    /// Serialize the in-mem children of `node` (each to a disk `SwizzledPtr` via the
    /// iterative subtree serializer) and reuse any on-disk child ptr verbatim,
    /// returning `(edge, disk_ptr)` pairs in `iter_children()` (sorted-ascending)
    /// order. Shared by the root path and the per-node iterative builder.
    ///
    /// Phase 6 (M-5a): `path` is the full key path to `node` (the root passes `[]`);
    /// before descending into each in-mem child the child's edge is pushed, popped after
    /// the subtree completes — so every descendant registers at its real path.
    // Uncompressed baseline: SUPERSEDED in production by the CX-universal compressed
    // serializer; retained #[cfg(test)] as the density-comparison oracle.
    #[cfg(test)]
    fn serialize_overlay_children_iterative(
        &self,
        node: &OverlayNode<ByteKey, V>,
        path: &mut Vec<u8>,
        mut registry: Option<&mut crate::persistent_artrie::eviction::DiskLocationRegistry>,
    ) -> Result<Vec<(u8, SwizzledPtr)>> {
        let mut child_ptrs: Vec<(u8, SwizzledPtr)> = Vec::with_capacity(node.num_children());
        for (&edge, child) in node.iter_children() {
            if let Some(child_arc) = child.as_in_mem() {
                path.push(edge);
                let ptr = self.serialize_overlay_subtree_iterative(
                    child_arc,
                    path,
                    registry.as_deref_mut(),
                );
                path.pop();
                child_ptrs.push((edge, ptr?));
            } else if let Some(on_disk) = child.as_on_disk() {
                // On-disk overlay children (a fault-in/eviction path) carry an
                // already-serialized location; reuse it directly (the prior
                // `ChildNode::DiskRef` path did the same). NOT re-registered (the OnDisk
                // subtree is reused verbatim — convergence preserved, mirroring char's
                // `serialize_overlay_to_disk_iterative` which skips OnDisk children). Null
                // fillers are never yielded by `iter_children`, but guard defensively.
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
    ///
    /// Phase 6 (M-5a): `path` arrives holding the FULL key path to `root_arc` (the
    /// caller pushed the subtree-root edge before calling, and pops it after). Inside
    /// the walk the SAME `path` is maintained symmetrically (push on descent into a
    /// child, pop when that child frame finalizes), exactly as char's
    /// `serialize_overlay_to_disk_iterative` maintains its `Vec<char>` path; `registry`
    /// (when `Some`) is threaded to `serialize_overlay_node_to_disk` for per-node
    /// `register` + `set_durable_stamp`.
    // Uncompressed baseline: SUPERSEDED in production by the CX-universal compressed
    // serializer; retained #[cfg(test)] as the density-comparison oracle.
    #[cfg(test)]
    fn serialize_overlay_subtree_iterative(
        &self,
        root_arc: &std::sync::Arc<OverlayNode<ByteKey, V>>,
        path: &mut Vec<u8>,
        mut registry: Option<&mut crate::persistent_artrie::eviction::DiskLocationRegistry>,
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
            // Whether THIS walk pushed `parent_edge` onto `path` on descent (so it is
            // popped symmetrically on finalize). `false` for the subtree root (its edge
            // was pushed by the caller, who pops it). Mirrors char's `parent_pushed_path`.
            parent_pushed_path: bool,
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
            parent_pushed_path: bool,
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
                parent_pushed_path,
                pending_in_mem,
                slots,
            }
        }

        let mut stack: Vec<Frame<'_, V>> = Vec::new();
        // The subtree root's edge is already on `path` (the caller pushed it); this
        // frame did not push it ⇒ `parent_pushed_path = false`.
        stack.push(make_frame(root_arc.as_ref(), None, false));
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

            // Descend into the next in-mem child, if any remain. Push its edge onto
            // `path` first (every u8 edge is a valid path unit) so the descended frame
            // registers at its real path; that frame records it pushed (pops on finalize).
            if let Some((edge, child_arc)) = frame.pending_in_mem.pop() {
                path.push(edge);
                stack.push(make_frame(child_arc.as_ref(), Some(edge), true));
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
            // Serialize + register THIS node at its current `path` (which includes its
            // own edge from the parent), then pop that edge before bubbling up.
            let node_ptr = self.serialize_overlay_node_to_disk(
                frame.node,
                &child_ptrs,
                path,
                registry.as_deref_mut(),
            )?;
            if frame.parent_pushed_path {
                path.pop();
            }

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
    ///
    /// Phase 6 (M-5a): given `path` (the full key path to this node) and `registry`
    /// (when `Some`), the serialized node is `register`ed at `path` and its LIVE overlay
    /// node `set_durable_stamp`ed — the byte twin of char's
    /// `serialize_one_char_node_to_disk` register + stamp.
    // Uncompressed baseline: SUPERSEDED in production by the CX-universal compressed
    // serializer; retained #[cfg(test)] as the density-comparison oracle.
    #[cfg(test)]
    fn serialize_overlay_node_to_disk(
        &self,
        node: &OverlayNode<ByteKey, V>,
        child_ptrs: &[(u8, SwizzledPtr)],
        path: &[u8],
        registry: Option<&mut crate::persistent_artrie::eviction::DiskLocationRegistry>,
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

        self.serialize_overlay_node_record_registering(
            &node_copy,
            value_bytes.as_deref(),
            node,
            path,
            registry,
        )
    }

    /// Serialize ONE already-built owned `Node` record (with its optional value blob) to
    /// disk and, when `registry` is `Some`, REGISTER it at `path` + `set_durable_stamp`
    /// the LIVE overlay node `overlay` — the single Phase-6 registration site (the byte
    /// twin of char's `register_char` + `frame.node.set_durable_stamp(...)` at
    /// `serialize_one_char_node_to_disk`). Shared by the root node path (which builds the
    /// owned `Node` itself) and `serialize_overlay_node_to_disk` (non-root nodes).
    ///
    /// The registration is a pure side-effect: `result_ptr` and the bytes written are
    /// identical whether or not the registry is present (so the on-disk image — and the
    /// no-eviction tests — are byte-for-byte unaffected). `register` uses the BYTE map
    /// (NOT `register_char`). The `set_durable_stamp` is gated on `registry.is_some()` so
    /// a node is stamped IFF it was just registered (eviction enabled); the `Release`
    /// pairs with the evictor's `Acquire` via the registry-publish edge (M-2a).
    // Uncompressed baseline: SUPERSEDED in production by the CX-universal compressed
    // serializer; retained #[cfg(test)] as the density-comparison oracle.
    #[cfg(test)]
    fn serialize_overlay_node_record_registering(
        &self,
        node_record: &Node,
        value_bytes: Option<&[u8]>,
        overlay: &OverlayNode<ByteKey, V>,
        path: &[u8],
        registry: Option<&mut crate::persistent_artrie::eviction::DiskLocationRegistry>,
    ) -> Result<SwizzledPtr> {
        let (result_ptr, data_len) =
            self.serialize_node_to_disk_with_value_len(node_record, value_bytes)?;

        if let Some(reg) = registry {
            let node_type = match node_record {
                Node::N4(_) => NodeType::Node4,
                Node::N16(_) => NodeType::Node16,
                Node::N48(_) => NodeType::Node48,
                Node::N256(_) => NodeType::Node256,
            };
            reg.register(
                path.to_vec(),
                result_ptr.clone(),
                data_len,
                path.len(),
                node_type,
            );
            // M-2a durable stamp: record on the LIVE overlay node that this exact content
            // is now durable at `result_ptr`. The eviction guard later evicts this node
            // ONLY while `durable_stamp() == result_ptr.to_raw()` (i.e. while it has not
            // been overwritten since now — any overwrite path-copies it into a fresh
            // stamp-0 node). The byte twin of char's `frame.node.set_durable_stamp(...)`.
            overlay.set_durable_stamp(result_ptr.to_raw());
        }

        Ok(result_ptr)
    }

    /// CX (#43): serialize ONE already-built owned `Node` record to disk and, when `registry` is
    /// `Some`, REGISTER it at `path` — but DO NOT stamp any overlay node. The byte twin of char's
    /// `serialize_one_char_node_to_disk` (register WITHOUT stamp). The compressed serializer stamps
    /// the LIVE top-of-span / terminus nodes MANUALLY (a chunk node is SYNTHETIC — it has no live
    /// overlay node), so unlike [`Self::serialize_overlay_node_record_registering`] this MUST NOT
    /// stamp. `result_ptr` + the bytes written are identical whether or not the registry is present.
    fn serialize_one_byte_node_to_disk(
        &self,
        node_record: &Node,
        value_bytes: Option<&[u8]>,
        path: &[u8],
        registry: Option<&mut crate::persistent_artrie::eviction::DiskLocationRegistry>,
    ) -> Result<SwizzledPtr> {
        let (result_ptr, data_len) =
            self.serialize_node_to_disk_with_value_len(node_record, value_bytes)?;
        if let Some(reg) = registry {
            let node_type = match node_record {
                Node::N4(_) => NodeType::Node4,
                Node::N16(_) => NodeType::Node16,
                Node::N48(_) => NodeType::Node48,
                Node::N256(_) => NodeType::Node256,
            };
            reg.register(
                path.to_vec(),
                result_ptr.clone(),
                data_len,
                path.len(),
                node_type,
            );
        }
        Ok(result_ptr)
    }

    /// CX (#43): the BYTE path-compressing overlay→disk serializer — the twin of char's
    /// [`PersistentARTrieChar::serialize_overlay_snapshot_compressed`]. ITERATIVE post-order: each
    /// in-mem child is descended via [`peel_chain_byte`], which collapses a maximal single-child
    /// non-final no-value chain into `(chain_prefix, live_spine, terminus)`. The terminus serializes
    /// as a plain (prefix-less) node; the peeled `chain_prefix` collapses into a stack of dense chunk
    /// nodes ABOVE it via the proven [`crate::persistent_artrie::core::overlay::codec::chain_chunks`]
    /// (width `MAX_PREFIX_LEN + 1` — `<= MAX_PREFIX_LEN` prefix bytes + 1 out-edge per chunk; NEVER
    /// truncates — chains longer runs across multiple chunk nodes).
    ///
    /// `path` is the full root→node byte sequence in the EXPANDED (uncompressed) tree — the path the
    /// evictor + the uncompressed serializer walk. EVICTION-ON (`registry = Some`): each emitted node
    /// registers at its TRUE expanded depth (the terminus at `path.len()`; chunk `c` at
    /// `ends[c] = base + 1 + Σ_{i<c}(|P_i|+1)`, a prefix-slice of `path`) and `set_durable_stamp`s the
    /// corresponding LIVE node (the terminus → `frame.node`; chunk `c` → `live_spine[ends[c]-base-1]`),
    /// so the evictor can reclaim the whole compressed span as one `Child::OnDisk`. EVICTION-OFF
    /// (`None`): a pure structural serialize (the round-trip / density tests).
    ///
    /// **INVARIANT (data-loss-critical).** The emitted image uses **node-header prefix
    /// compression** (`header.prefix_len > 0`). It MUST only ever be read back via a
    /// prefix-AWARE loader: the overlay fault loader [`Self::load_overlay_node_from_disk`], or
    /// the F5 reopen path (`load_root_immutable` → `load_overlay_root_compressed`, whose
    /// `enumerate_terms_from_disk` walk folds `node.prefix()` into the path). The
    /// now-deleted owned readers were prefix-BLIND and would have SILENTLY TRUNCATED every
    /// compressed term; the overlay loaders are the only readers, and both are prefix-aware,
    /// so a CX image is always read losslessly.
    pub(crate) fn serialize_overlay_snapshot_compressed(
        &self,
        root: &std::sync::Arc<OverlayNode<ByteKey, V>>,
        registry: Option<&mut crate::persistent_artrie::eviction::DiskLocationRegistry>,
    ) -> Result<SwizzledPtr> {
        self.serialize_compressed_loop(root, registry)
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
    // Uncompressed baseline: SUPERSEDED in production by the CX-universal compressed
    // serializer; retained #[cfg(test)] as the density-comparison oracle.
    #[cfg(test)]
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

/// byte's projected single-node carrier: the `Node` (children baked in via `add_child`) + its
/// serialized value blob (`?`-propagated bincode of the overlay node's value).
pub(crate) struct ByteProjected {
    node: Node,
    value: Option<Vec<u8>>,
}

/// CX-universal seams for byte (eviction-ON capable): the shared compressed loop lives in
/// `OverlayCompressedSerialize::serialize_compressed_loop`; byte supplies the `Node`-arena projection
/// (+ value blob) + per-node serialize + the eviction durable-stamp. byte's `path` is `[u8]`
/// (`ByteKey::Unit`), so no codepoint lowering is needed (unlike char).
impl<V: DictionaryValue, S: BlockStorage> OverlayCompressedSerialize<ByteKey, V>
    for super::PersistentARTrie<V, S>
{
    type Projected = ByteProjected;

    fn project_node(
        node: &OverlayNode<ByteKey, V>,
        child_disk_ptrs: &[(u8, SwizzledPtr)],
    ) -> Result<Self::Projected> {
        let mut term_node = if child_disk_ptrs.is_empty() {
            Node::N4(Box::new(Node4::new()))
        } else {
            Self::build_owned_node_with_child_ptrs(child_disk_ptrs)
        };
        term_node.header_mut().set_final(node.is_final());
        let value: Option<Vec<u8>> =
            match node.get_value() {
                Some(v) => Some(crate::serialization::bincode_compat::serialize(&v).map_err(
                    |e| PersistentARTrieError::internal(&format!("serialize overlay value: {e}")),
                )?),
                None => None,
            };
        Ok(ByteProjected {
            node: term_node,
            value,
        })
    }

    fn project_chunk(
        _synth: &OverlayNode<ByteKey, V>,
        child_disk_ptrs: &[(u8, SwizzledPtr)],
        prefix: &[u8],
    ) -> Result<Self::Projected> {
        let mut chunk_node = Self::build_owned_node_with_child_ptrs(child_disk_ptrs);
        chunk_node.header_mut().prefix_len = prefix.len() as u8;
        *chunk_node.prefix_mut() = super::nodes::CompressedPrefix::from_bytes(prefix);
        Ok(ByteProjected {
            node: chunk_node,
            value: None,
        })
    }

    fn serialize_projected_node(
        &self,
        projected: &Self::Projected,
        _child_disk_ptrs: &[(u8, SwizzledPtr)],
        path: &[u8],
        registry: Option<&mut DiskLocationRegistry>,
    ) -> Result<SwizzledPtr> {
        // byte's `node` already carries its children, so `child_disk_ptrs` is unused here.
        self.serialize_one_byte_node_to_disk(
            &projected.node,
            projected.value.as_deref(),
            path,
            registry,
        )
    }

    fn new_synth_node() -> OverlayNode<ByteKey, V> {
        OverlayNode::<ByteKey, V>::new()
    }

    fn stamp_durable(live: &OverlayNode<ByteKey, V>, raw: u64) {
        live.set_durable_stamp(raw);
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
        // F4 (EC leaf): brief lock, immediately released — never held across CK/OR.
        self.eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .is_some()
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
}

#[cfg(test)]
mod cx_compressed_serialize_byte {
    //! CX (#43) — the BYTE twin of char's `persist::cx_compressed_serialize`. Round-trip
    //! term-exactness (incl. a chain longer than `MAX_PREFIX_LEN` ⇒ multi-node chunking, the
    //! no-truncation codec end-to-end), density (compressed dense-node count + bytes < uncompressed),
    //! and the #6 eviction-ON tests: F.1 evict-then-refault a compressed chunk, F.3 the load-side
    //! `prefix_len>0` stamp gate (uncompressed no-op = #39 unchanged; compressed stamped = re-evictable).
    //! Scratch is real disk (`target/test-tmp`), never tmpfs `/tmp`.
    use crate::persistent_artrie::core::block_storage::BlockStorage;
    use crate::persistent_artrie::core::durability::DurabilityPolicy;
    use crate::persistent_artrie::core::key_encoding::ByteKey;
    use crate::persistent_artrie::core::overlay::node::Child;
    use crate::persistent_artrie::core::overlay::OverlayNode;
    use crate::persistent_artrie::eviction::{DiskLocationRegistry, EvictionConfig};
    use crate::persistent_artrie::overlay_fault::evict_overlay_nodes;
    use crate::persistent_artrie::PersistentARTrie;
    use std::sync::Arc;

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch dir under target/test-tmp")
    }

    /// Build an UNCOMPRESSED overlay (one node per byte) for the given terms — the shape the overlay
    /// write path builds. Shared prefixes share nodes (immutable path-copy via `with_child`).
    fn build_overlay(terms: &[&str]) -> Arc<OverlayNode<ByteKey, ()>> {
        fn insert(
            node: Arc<OverlayNode<ByteKey, ()>>,
            bytes: &[u8],
        ) -> Arc<OverlayNode<ByteKey, ()>> {
            match bytes.split_first() {
                None => Arc::new((*node).clone().as_final()),
                Some((&edge, rest)) => {
                    let child = match node.find_child(edge).and_then(|c| c.as_in_mem()) {
                        Some(existing) => insert(existing.clone(), rest),
                        None => insert(Arc::new(OverlayNode::<ByteKey, ()>::new()), rest),
                    };
                    Arc::new((*node).clone().with_child(edge, Child::InMem(child)))
                }
            }
        }
        let mut root = Arc::new(OverlayNode::<ByteKey, ()>::new());
        for t in terms {
            root = insert(root, t.as_bytes());
        }
        root
    }

    /// Fault-walk the loaded overlay (resolving OnDisk children) and collect every term.
    fn collect_terms<S: BlockStorage>(
        trie: &PersistentARTrie<(), S>,
        node: &Arc<OverlayNode<ByteKey, ()>>,
        pfx: &mut Vec<u8>,
        out: &mut Vec<String>,
    ) {
        if node.is_final() {
            out.push(String::from_utf8(pfx.clone()).expect("utf8 term"));
        }
        let kids: Vec<(u8, Arc<OverlayNode<ByteKey, ()>>)> = node
            .iter_children()
            .map(|(&k, child)| {
                let n = match child.as_in_mem() {
                    Some(a) => a.clone(),
                    None => trie
                        .load_overlay_node_from_disk(child.as_on_disk().expect("on-disk child"))
                        .expect("fault child"),
                };
                (k, n)
            })
            .collect();
        for (k, child) in kids {
            pfx.push(k);
            collect_terms(trie, &child, pfx, out);
            pfx.pop();
        }
    }

    fn roundtrip(name: &str, terms: &[&str]) {
        let dir = scratch(name);
        let trie = PersistentARTrie::<()>::create(&dir.path().join("t.artb")).expect("create");
        let root = build_overlay(terms);
        let root_ptr = trie
            .serialize_overlay_snapshot_compressed(&root, None)
            .expect("serialize compressed");
        let loaded = trie
            .load_overlay_node_from_disk(&root_ptr)
            .expect("load compressed root");
        let mut got = Vec::new();
        collect_terms(&trie, &loaded, &mut Vec::new(), &mut got);
        got.sort();
        let mut expect: Vec<String> = terms.iter().map(|s| s.to_string()).collect();
        expect.sort();
        expect.dedup();
        assert_eq!(
            got, expect,
            "byte compressed round-trip term set mismatch for {name}"
        );
    }

    #[test]
    fn cx_roundtrip_long_chain_no_truncation() {
        // 26-byte chain (> MAX_PREFIX_LEN=12 ⇒ ≥2 chunk nodes) — the no-truncation codec end-to-end.
        roundtrip("byte-cx-chain", &["abcdefghijklmnopqrstuvwxyz"]);
    }

    #[test]
    fn cx_roundtrip_branching_and_shared_prefix() {
        // A chain ("cdefghijklmnop") below a FINAL branching node ("b"), shared prefixes, siblings.
        roundtrip(
            "byte-cx-branch",
            &["a", "ab", "abc", "abd", "b", "bcdefghijklmnop", "xyz"],
        );
    }

    #[test]
    fn cx_roundtrip_single_and_empty() {
        roundtrip("byte-cx-single", &["solo"]);
        roundtrip("byte-cx-empty-term", &[""]); // the empty string ⇒ a final root, no children
    }

    /// Density: the compressed serializer emits FEWER + SMALLER dense nodes than the uncompressed
    /// `serialize_overlay_root_iterative` for a chain (the compression WITNESS — not trivially
    /// uncompressed). Measured via the eviction registry, which records one entry per dense node at
    /// serialize time (BEFORE the loader expands chunks back into a chain).
    #[test]
    fn cx_density_lt_uncompressed_for_chains() {
        let dir = scratch("byte-cx-density");
        let trie = PersistentARTrie::<()>::create(&dir.path().join("t.artb")).expect("create");
        let overlay = build_overlay(&["abcdefghijklmnopqrstuvwxyz"]);
        let mut reg_c = DiskLocationRegistry::new();
        trie.serialize_overlay_snapshot_compressed(&overlay, Some(&mut reg_c))
            .expect("compressed serialize");
        let mut reg_u = DiskLocationRegistry::new();
        trie.serialize_overlay_root_iterative(overlay.as_ref(), Some(&mut reg_u))
            .expect("uncompressed serialize");
        assert!(
            reg_c.len() < reg_u.len(),
            "compressed dense-node count {} must be < uncompressed {}",
            reg_c.len(),
            reg_u.len()
        );
        assert!(
            reg_c.total_size_bytes() < reg_u.total_size_bytes(),
            "compressed dense bytes {} must be < uncompressed {}",
            reg_c.total_size_bytes(),
            reg_u.total_size_bytes()
        );
    }

    /// **CX #6 (F.1 — headline) evict-then-refault a COMPRESSED chunk node (byte).** Serialize the LIVE
    /// overlay COMPRESSED with an eviction registry, publish it, evict, then read the chain back. The
    /// chunk MUST evict (a wrong `ends[c]` depth / stamp ⇒ `NotEvictable` ⇒ a #6/#39 regression) AND
    /// the prefix must refault LOSSLESSLY. V=u64 so `get_lockfree` reads the exact value back.
    #[test]
    fn cx_6_evict_then_refault_compressed_chunk() {
        let dir = scratch("byte-cx6-evict-refault");
        let path = dir.path().join("t.artb");
        let mut trie = PersistentARTrie::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        trie.install_overlay();
        trie.bench_enable_eviction(EvictionConfig::without_memory_monitor())
            .expect("enable eviction");
        // A long single-byte-child chain (≥2 chunks) below a branch ('a' chain + 'b' sibling).
        let chain_term = "aqqqqqqqqqqqqqqqqqqqq"; // 'a' + 20×'q' → a multi-chunk chain
        trie.try_increment_cas_durable(chain_term.as_bytes(), 777)
            .expect("durable increment chain");
        trie.try_increment_cas_durable(b"b", 1)
            .expect("durable increment sibling");
        let trie = Arc::new(trie);

        // Build a COMPRESSED image + eviction registry from the LIVE overlay.
        let root = trie
            .lockfree_root
            .as_ref()
            .and_then(|r| r.load())
            .expect("overlay root present");
        let mut registry = DiskLocationRegistry::new();
        trie.serialize_overlay_snapshot_compressed(&root, Some(&mut registry))
            .expect("serialize compressed (eviction-ON)");

        // Publish the compressed registry to the coordinator.
        let coordinator = trie
            .eviction_coordinator
            .lock()
            .expect("coordinator mutex")
            .as_ref()
            .expect("eviction enabled")
            .clone();
        coordinator.update_disk_registry(registry);
        assert!(
            trie.evictable_node_count().unwrap_or(0) > 0,
            "the compressed registry must be published"
        );

        // Evict everything reachable.
        let (evicted, _) = coordinator
            .force_eviction_bytes(usize::MAX, |cands| evict_overlay_nodes(&*trie, cands, 4));
        assert!(
            evicted > 0,
            "CX #6: a compressed chunk node MUST evict (NotEvictable ⇒ wrong registry depth/stamp = #39 regression)"
        );

        // Refault: reading faults the evicted compressed chunk(s) + expands the span losslessly.
        assert_eq!(
            trie.get_lockfree(chain_term.as_bytes()),
            Some(777),
            "CX #6: the chain term VALUE must survive evict→refault (compressed span lossless)"
        );
        assert_eq!(trie.get_lockfree(b"b"), Some(1), "sibling term survives");
    }

    /// **CX #6 (F.3 — the gate no-op) load-side `prefix_len>0` stamp gate (byte).** A faulted node gets
    /// a `durable_stamp` IFF it was a compressed (`prefix_len>0`) chunk on disk: (a) an UNCOMPRESSED
    /// image yields ZERO stamps on fault (the pre-#6 production no-op ⇒ #39 unchanged); (b) a COMPRESSED
    /// chunk's expanded top carries `stamp == its disk_ptr` (the predicate the evictor needs to
    /// RE-evict a refaulted chunk).
    #[test]
    fn cx_6_load_stamp_gate_uncompressed_noop_compressed_stamps() {
        fn walk<S: BlockStorage>(
            trie: &PersistentARTrie<(), S>,
            node: &Arc<OverlayNode<ByteKey, ()>>,
            stamped: &mut usize,
            unstamped: &mut usize,
        ) {
            let kids: Vec<Child<ByteKey>> = node.iter_children().map(|(_, c)| c.clone()).collect();
            for child in kids {
                if let Some(on_disk) = child.as_on_disk() {
                    let raw = on_disk.to_raw();
                    let faulted = trie
                        .load_overlay_node_from_disk(on_disk)
                        .expect("fault child");
                    match faulted.durable_stamp() {
                        0 => *unstamped += 1,
                        stamp => {
                            assert_eq!(
                                stamp, raw,
                                "a stamped (compressed-chunk) node's stamp must equal the disk_ptr it faulted from"
                            );
                            *stamped += 1;
                        }
                    }
                    walk(trie, &faulted, stamped, unstamped);
                } else if let Some(in_mem) = child.as_in_mem() {
                    walk(trie, in_mem, stamped, unstamped);
                }
            }
        }

        // (a) UNCOMPRESSED: all-short branching terms → every chunk prefix_len=0 → ZERO stamps.
        let dir = scratch("byte-cx6-noop-uncompressed");
        let trie = PersistentARTrie::<()>::create(&dir.path().join("t.artb")).expect("create");
        let root = build_overlay(&["a", "b", "ca", "cb"]);
        let root_ptr = trie
            .serialize_overlay_snapshot_compressed(&root, None)
            .expect("serialize uncompressed");
        let loaded = trie
            .load_overlay_node_from_disk(&root_ptr)
            .expect("load uncompressed root");
        assert_eq!(
            loaded.durable_stamp(),
            0,
            "the uncompressed root itself must be unstamped on fault"
        );
        let (mut s, mut u) = (0usize, 0usize);
        walk(&trie, &loaded, &mut s, &mut u);
        assert_eq!(
            s, 0,
            "CX #6: an UNCOMPRESSED (prefix_len=0) image must yield ZERO durable stamps on fault (production no-op)"
        );
        assert!(u > 0, "sanity: at least one node was faulted");

        // (b) COMPRESSED: a long chain below a branch → ≥1 prefix_len>0 chunk → ≥1 stamp == its disk_ptr.
        let dir2 = scratch("byte-cx6-stamp-compressed");
        let trie2 = PersistentARTrie::<()>::create(&dir2.path().join("t.artb")).expect("create");
        let root2 = build_overlay(&["aqqqqqqqqqqqqqqqqqqqq", "b"]); // 'a' + 20×'q' chain + 'b' sibling
        let root2_ptr = trie2
            .serialize_overlay_snapshot_compressed(&root2, None)
            .expect("serialize compressed");
        let loaded2 = trie2
            .load_overlay_node_from_disk(&root2_ptr)
            .expect("load compressed root");
        let (mut s2, mut u2) = (0usize, 0usize);
        walk(&trie2, &loaded2, &mut s2, &mut u2);
        assert!(
            s2 > 0,
            "CX #6: a COMPRESSED (prefix_len>0) chunk must be stamped == its disk_ptr on fault (re-evictable)"
        );
        let _ = u2;
    }
}

#[cfg(test)]
mod format3_legacy_bucket_reopen {
    //! **B2 (L3.3b PRECONDITION)** — pins `enumerate_terms_from_disk`'s legacy
    //! **ROOT_TYPE_BUCKET-with-terms** decode branch (`disk_load.rs:~600`) as TESTED
    //! before L3.3b's B3 retires the differential oracle (`l31_differential_tests`) that
    //! currently covers it.
    //!
    //! "format-3" = a root `StringBucket` holding the whole (small) dictionary as suffix
    //! entries. It was produced ONLY by the now-collapsed owned serialize path; the
    //! production overlay serializers (`serialize_overlay_root_iterative`,
    //! `serialize_overlay_snapshot_compressed`) emit `ROOT_TYPE_BUCKET` ONLY for an EMPTY
    //! root (0 terms, childless, non-final), so a POPULATED root bucket can no longer be
    //! written in-process. This fixture hand-constructs one via the KEPT low-level
    //! primitives (`serialize_bucket_to_disk` + a hand-written `ROOT_TYPE_BUCKET`
    //! descriptor through `publish_snapshot`), then reopens through the PUBLIC `open()`
    //! path and asserts every term — including the empty string `""` — survives, and that
    //! the next `checkpoint()` rewrites the legacy image to the modern ART format
    //! losslessly. Both reopen regimes (Owned ⇒ `convert_owned_to_overlay_on_reopen`,
    //! Overlay ⇒ `load_root_immutable`) route through `load_overlay_root_compressed` →
    //! `enumerate_terms_from_disk`, so the ROOT_TYPE_BUCKET branch is covered regardless.
    //!
    //! Scratch is real disk (`target/test-tmp`), never tmpfs `/tmp` (disk-backed reopen).
    use super::CheckpointSnapshot;
    use crate::persistent_artrie::bucket::StringBucket;
    use crate::persistent_artrie::dict_impl::ROOT_TYPE_BUCKET;
    use crate::persistent_artrie::PersistentARTrie;
    use crate::serialization::bincode_compat;

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch dir under target/test-tmp")
    }

    #[test]
    fn legacy_root_bucket_reopens_with_all_terms_incl_empty() {
        // (suffix, value) entries of the legacy root bucket. `""` is a first-class bucket
        // entry (empty suffix → the empty-string term; H2 empty-string-value support).
        let entries: [(&[u8], u64); 4] = [(b"", 7), (b"alpha", 11), (b"beta", 22), (b"gamma", 33)];

        let dir = scratch("byte-fmt3-bucket-reopen");
        let path = dir.path().join("fmt3.artb");

        // --- Construct a legacy format-3 image ------------------------------------
        {
            // A live trie purely to borrow its buffer/arena managers for serialization.
            let trie = PersistentARTrie::<u64>::create(&path).expect("create");

            // Build the populated root StringBucket. Values are bincode-encoded `V`,
            // matching `enumerate_terms_from_disk`'s `deser` (bincode_compat::deserialize).
            let mut bucket = StringBucket::with_values();
            for (suffix, value) in entries {
                let value_bytes = bincode_compat::serialize(&value).expect("encode value");
                bucket
                    .insert(suffix, &value_bytes)
                    .expect("bucket insert (empty suffix is valid)");
            }

            // Serialize the bucket into a fresh arena slot, flush, and count arenas (the
            // descriptor's arena_count drives the reopen eager-preload validation).
            let bucket_ptr = trie
                .serialize_bucket_to_disk(&bucket)
                .expect("serialize bucket to disk");
            let arena_count = trie.flush_and_count_arenas().expect("flush + count arenas");

            // Publish a ROOT_TYPE_BUCKET block-0 descriptor (owned-arm convention: no
            // overlay watermark / commit_seq / eviction registry, and `None`
            // image-checkpoint-lsn so the WAL is untouched). This is the ONLY way to emit
            // a POPULATED root bucket after the owned serialize path was collapsed.
            let snapshot = CheckpointSnapshot {
                root_type: ROOT_TYPE_BUCKET,
                is_final: false, // `""` rides a bucket ENTRY, not the root's finality.
                term_count: entries.len() as u64,
                arena_count,
                root_ptr: bucket_ptr.to_raw(),
                committed_watermark_at_capture: None,
                commit_seq_at_capture: None,
                eviction_registry: None,
            };
            trie.publish_snapshot(&snapshot, None)
                .expect("publish ROOT_TYPE_BUCKET descriptor");
            // drop(trie) → close(): syncs the WAL + flushes already-clean buffer pages; it
            // does NOT re-checkpoint or re-serialize the (empty) in-memory overlay, so the
            // hand-published block-0 bucket descriptor is preserved on disk.
        }

        // --- Reopen via the public path -------------------------------------------
        // The block-0 descriptor is the source of truth; the term-empty WAL replays
        // nothing. enumerate_terms_from_disk's ROOT_TYPE_BUCKET branch runs here.
        let reopened = PersistentARTrie::<u64>::open(&path).expect("reopen legacy bucket image");
        for (suffix, value) in entries {
            assert!(
                reopened.contains_bytes(suffix),
                "legacy bucket term {:?} missing after reopen",
                String::from_utf8_lossy(suffix)
            );
            assert_eq!(
                reopened.get_value_bytes(suffix),
                Some(value),
                "legacy bucket value for {:?} wrong after reopen",
                String::from_utf8_lossy(suffix)
            );
        }

        // --- The next checkpoint rewrites the legacy image to the modern ART format -
        reopened
            .checkpoint()
            .expect("checkpoint rewrites the legacy bucket image");
        drop(reopened);
        let rewritten = PersistentARTrie::<u64>::open(&path).expect("reopen after rewrite");
        for (suffix, value) in entries {
            assert_eq!(
                rewritten.get_value_bytes(suffix),
                Some(value),
                "term {:?} lost when the legacy bucket image was rewritten to ART format",
                String::from_utf8_lossy(suffix)
            );
        }
    }
}
