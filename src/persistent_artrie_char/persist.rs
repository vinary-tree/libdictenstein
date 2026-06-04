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

/// An immutable, self-consistent checkpoint snapshot captured during checkpoint
/// **Phase A** (serialize the in-memory tree into freshly-allocated arena slots
/// — copy-on-serialize, so the captured `root_ptr` + arena image is frozen).
/// The durable-publish phase consumes only these owned values, so it never
/// re-reads mutable trie state.
///
/// The non-blocking `SharedCharARTrie::checkpoint` captures this under an
/// exclusive `RwLock` write guard, then **downgrades** the guard to a read guard
/// (admitting concurrent readers) for the durable-publish + WAL phases — using
/// exactly this frozen snapshot, so those phases never re-read mutable trie state.
pub(crate) struct CheckpointSnapshot {
    /// Root descriptor type byte (`ROOT_TYPE_EMPTY` / `ROOT_TYPE_NODE`).
    root_type: u8,
    /// Whether the root node is itself a terminal/final node.
    is_final: bool,
    /// Term count at the snapshot point (used for both the descriptor's
    /// `term_count` field and the header `entry_count`, so they agree).
    entry_count: u64,
    /// Number of arenas after serialization (block IDs derive from this).
    arena_count: u32,
    /// Raw `SwizzledPtr` of the serialized root.
    root_ptr: u64,
    /// `next_lsn` observed at capture. The WAL `Checkpoint` record uses
    /// `next_lsn` at publish time; under the write-then-downgraded-read guard no
    /// `L1.write` mutator can run, so `next_lsn` cannot change — we
    /// `debug_assert_eq!` this at publish to fail loudly if that invariant is
    /// ever violated (e.g. a lock-free WAL-appending writer is exposed on the
    /// shared handle), which would otherwise risk a lost write.
    next_lsn_at_capture: u64,
    /// **Migration Phase E (immutable-overlay capture only).** The committed
    /// watermark captured (Acquire) BEFORE the root load (the capture-ordering
    /// invariant). `Some(w)` for [`Self::capture_snapshot_immutable`]; `None` for
    /// the owned [`Self::capture_snapshot`] (which reclaims by the `next_lsn`
    /// convention instead). The retaining-WAL publisher writes a `Checkpoint`
    /// record with `checkpoint_lsn = w` so recovery skips WAL deltas ≤ `w` (already
    /// folded into the published image) and replays only the tail `> w` — the
    /// watermark-based `checkpoint_lsn` the plan §4 mandates, which is what makes
    /// publishing a counter image while retaining the WAL non-double-counting.
    committed_watermark_at_capture: Option<u64>,
    /// **S5-2 (A3 commit_seq floor).** The durable global `commit_seq` observed
    /// (Acquire) in the SAME capture window as the watermark and BEFORE the root
    /// load. `Some(c)` for [`Self::capture_snapshot_immutable`]; `None` for the owned
    /// [`Self::capture_snapshot`] (which never advances `commit_seq`, so there is no
    /// floor to raise). The retaining-WAL publisher raises the WAL `commit_seq_floor`
    /// to this value (monotone, carried across rotate) so a post-checkpoint overlay op
    /// out-ranks every pre-checkpoint survivor on a later rebuild.
    commit_seq_at_capture: Option<u64>,
    /// Freshly-built disk-location registry (only when eviction is enabled),
    /// published to the eviction coordinator after durability is verified.
    eviction_registry: Option<DiskLocationRegistry>,
}

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
        // Owned/blocking checkpoint: the whole sequence runs under the caller's
        // exclusive `&mut self` borrow (= the trie write lock held throughout
        // when reached via `SharedCharARTrie`'s write guard). The non-blocking
        // `SharedCharARTrie::checkpoint` instead captures the snapshot under a
        // write guard, atomically DOWNGRADES it to a read guard, then runs
        // `publish_durable_and_reclaim` so concurrent readers proceed during the
        // (fsync-bound) publish phase.
        //
        // **M1 (overlay-durable-architecture.md, trait 3):** the RES-4 route-split
        // DECISION (under the overlay write mode the OWNED tree is empty — the live
        // data is in the immutable overlay; capturing the owned tree would checkpoint
        // NOTHING and lose every term on reopen, so route to the overlay capture +
        // watermark-bounded retaining publisher) + the total-loss-guard assert now
        // live ONCE in the SHARED GENERIC
        // [`OverlayCheckpoint::checkpoint_route_split`]; this method is a thin wrapper
        // calling it. The per-variant capture/publish seams delegate to the SAME char
        // inherent methods the prior inline body called, so it is byte-identical.
        // INERT pre-flip: `route_overlay()` is false until S5-12 wires the production
        // ctors, so the owned arm is byte-for-byte the prior body.
        <Self as crate::persistent_artrie_core::overlay::checkpoint::OverlayCheckpoint<
            crate::persistent_artrie_core::key_encoding::CharKey,
            V,
            S,
        >>::checkpoint_route_split(self)
    }

    /// Checkpoint **Phase B+C** — publish the captured snapshot durably, then
    /// record + reclaim the WAL.
    ///
    /// Takes `&self` so it can run under either the owned `&mut self` checkpoint
    /// or a downgraded read guard (the non-blocking path). Consumes the snapshot
    /// (moves the eviction registry into the coordinator). The sequence is
    /// byte-identical to the prior blocking checkpoint tail: publish descriptor
    /// + flush + fsync (the linearization point) → verify header checksum →
    /// publish the eviction registry → WAL `Checkpoint` append + sync +
    /// archive/truncate → clear the dirty flag.
    pub(crate) fn publish_durable_and_reclaim(&self, snapshot: CheckpointSnapshot) -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Phase B: publish the snapshot to disk and fsync (linearization point).
        self.publish_snapshot(&snapshot)?;

        // Verify checkpoint - re-read header and verify checksum. Ensures the
        // sync() actually succeeded and the data is durable.
        self.verify_checkpoint()?;

        // Durability verified: publish the freshly-built disk-location registry
        // to the eviction coordinator. Eviction can then reclaim in-memory node
        // boxes (unswizzling them to these on-disk locations) under memory
        // pressure or an explicit force_eviction. Built only when eviction is
        // enabled; a no-op otherwise.
        if let Some(registry) = snapshot.eviction_registry {
            if let Some(ref coordinator) = self.eviction_coordinator {
                coordinator.update_disk_registry(registry);
            }
        }

        // Phase C: WAL operations (only after verification passes).
        //
        // `checkpoint_lsn` is read here as `next_lsn` (the original convention).
        // Under the write-then-downgraded-read guard of the non-blocking path —
        // and trivially under the owned `&mut self` path — no `L1.write` mutator
        // can run between capture and here, so `next_lsn` is unchanged from
        // capture; the WAL frontier and the descriptor's snapshot therefore
        // agree, and `rotate_to_archive` only ever archives covered records. The
        // assert below turns any violation of that invariant (which could
        // archive a racing write out of recovery's reach — the GAP_LEDGER #41
        // footgun) into a loud failure instead of silent data loss.
        // S5-8: promoted debug_assert → always-on assert. The #41 footgun (a write
        // racing the checkpoint ⇒ WAL reclaim archives it out of recovery's reach) is
        // data-loss-critical; once the overlay is production a fail-stop panic is
        // strictly safer than silent loss. Order-A + the owned `&mut self` exclusion
        // guarantee the invariant, so this cannot spuriously fire.
        assert_eq!(
            self.next_lsn.load(AtomicOrdering::Acquire),
            snapshot.next_lsn_at_capture,
            "checkpoint: next_lsn changed between capture and WAL publish — a \
             writer raced the checkpoint (C2 invariant violated); the WAL \
             reclaim could lose that write"
        );
        if let Some(ref wal_writer) = self.wal_writer {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let record = WalRecord::Checkpoint {
                checkpoint_lsn: self.next_lsn.load(AtomicOrdering::Acquire),
                timestamp,
            };
            // Write checkpoint record
            wal_writer
                .append(record)
                .map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;
            // Sync WAL
            wal_writer
                .sync()
                .map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;
            // Archive or truncate WAL based on configuration
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
        // Disk serialization requires a buffer manager (disk-backed mode).
        // Checked up-front so an in-memory-only trie errors before any arena
        // serialization side effects, preserving the prior behavior.
        if self.buffer_manager.is_none() {
            return Err(PersistentARTrieError::internal(
                "No buffer manager for disk serialization",
            ));
        }

        // Phase A: capture a frozen snapshot (serialize tree -> fresh arenas).
        let snapshot = self.capture_snapshot()?;
        // Phase B: publish the snapshot durably (descriptor + flush + sync).
        self.publish_snapshot(&snapshot)?;

        Ok(snapshot.eviction_registry)
    }

    /// Checkpoint **Phase A** — capture a frozen, self-consistent snapshot.
    ///
    /// Serializes the in-memory tree into freshly-allocated arena slots
    /// (copy-on-serialize: every node gets a new slot, so the produced
    /// `root_ptr` + arena image is immutable and self-consistent for this
    /// checkpoint) and flushes dirty arena slots to buffer-manager pages.
    /// Returns the descriptor values the publish phase needs. Takes `&self`
    /// (all mutation goes through the interior-mutable arena/buffer managers),
    /// which lets a later phase run this under a shared trie borrow.
    pub(crate) fn capture_snapshot(&self) -> Result<CheckpointSnapshot> {
        // `next_lsn` at capture; asserted unchanged at publish (see field doc).
        let next_lsn_at_capture = self.next_lsn.load(AtomicOrdering::Acquire);

        let mut eviction_registry = self
            .eviction_coordinator
            .as_ref()
            .map(|_| DiskLocationRegistry::new());

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

        // Capture the term count ONCE so the descriptor's term_count and the
        // header entry_count are guaranteed to agree for this snapshot.
        let entry_count = self.len.load(AtomicOrdering::Acquire) as u64;

        Ok(CheckpointSnapshot {
            root_type,
            is_final,
            entry_count,
            arena_count,
            root_ptr,
            next_lsn_at_capture,
            // Owned-tree capture reclaims by the `next_lsn` convention, not a
            // watermark (writers are excluded by the write lock), so this is None.
            committed_watermark_at_capture: None,
            // Owned capture never advances `commit_seq` (S5-2) ⇒ no floor to raise.
            commit_seq_at_capture: None,
            eviction_registry,
        })
    }

    /// **Migration Phase B (test-only):** capture a checkpoint snapshot from the
    /// IMMUTABLE lock-free overlay representation instead of the owned tree.
    ///
    /// Each overlay `PersistentCharNode` is converted to an owned production
    /// `CharTrieNodeInner<V>` ([`overlay_to_inner`]) and then serialized through
    /// the EXISTING [`Self::serialize_char_node_to_disk`] — so for the same
    /// logical data the on-disk image is **equivalent by construction** to a
    /// `capture_snapshot()` of an owned tree built from the same terms (proven by
    /// the correspondence test below). This is the capability that lets a future
    /// phase make the immutable representation the checkpoint source for all `V`;
    /// it is `cfg(test)` until that flip (Phase E) wires it into `checkpoint()`.
    ///
    /// G1: the overlay node now carries `Option<V>` directly, so the converter
    /// reads the value off the node — the former `map_value: Fn(u64) -> V` bridge
    /// is gone. For `V = ()` membership tries the overlay never holds a value.
    ///
    /// S5-9: un-gated to production (was `#[cfg(any(test, feature="bench-internals"))]`).
    /// `checkpoint()` route-splits to this under `route_overlay()` so a post-flip
    /// checkpoint captures the immutable overlay (the live data) instead of the empty
    /// owned tree. Adds zero new `unsafe`. Inert until S5-12 flips the production
    /// ctors (route_overlay() is false pre-flip).
    pub(crate) fn capture_snapshot_immutable(&self) -> Result<CheckpointSnapshot> {
        let next_lsn_at_capture = self.next_lsn.load(AtomicOrdering::Acquire);

        let mut eviction_registry = self
            .eviction_coordinator
            .as_ref()
            .map(|_| DiskLocationRegistry::new());

        // ═══════════════════════════════════════════════════════════════════
        //  THE SNAPSHOT-LSN CAPTURE ORDERING — "the single most dangerous line
        //  in the design" (plan §4). Read with the utmost care before editing.
        // ═══════════════════════════════════════════════════════════════════
        //
        // We capture the committed watermark `Acquire` STRICTLY BEFORE loading
        // the atomic overlay root (also `Acquire`). This ordering — watermark
        // FIRST, then root — is the executable refinement of the TLA invariant
        // `NoLostWriteUnderLockFreeCommit` (`LockFreeDurableCheckpoint.tla`):
        // it makes the captured snapshot a subset of the committed-durable
        // prefix, so `checkpoint_lsn := watermark` can NEVER reclaim a WAL
        // record that the snapshot does not contain (the GAP_LEDGER #41
        // data-loss footgun, which the `_Unsafe.cfg` appended-frontier model
        // exhibits as a concrete losing trace).
        //
        // WHY THE ORDERING ALONE SUFFICES (and why we cannot max over per-node
        // LSNs):  the immutable overlay `PersistentCharNode` carries NO per-node
        // LSN — it stores only finality + an `Option<V>` value (the G1 overlay
        // is `u64`-only; membership carries no value). So unlike a node-versioned
        // store, there is no per-node `lsn` field to take a `max` over. The
        // safety argument is instead PURELY the publication chain, each link of
        // which is established by an `Acquire`/`Release` pair in the proven
        // Order-A path (`insert_cas_durable`):
        //
        //   snapshot ⊆ published-root ⊆ committed-prefix(watermark_at_capture)
        //
        //   (1) snapshot ⊆ published-root.  Order A makes a write visible ONLY
        //       by CAS-publishing a new root whose spine contains the new leaf
        //       (`lockfree_cas.rs`: append+sync DURABLE → root CAS → mark).
        //       Every term in the snapshot we load was published by some such
        //       CAS that linearized at-or-before our `root.load()`.
        //   (2) published-root ⊆ committed-prefix.  A term is visible in the
        //       loaded root ⇒ its publishing CAS already landed ⇒ its WAL LSN
        //       was appended-and-synced DURABLE *before* that CAS (Order A) ⇒
        //       and `mark_committed(lsn)` runs immediately after the CAS. The
        //       contiguous-prefix watermark therefore covers that LSN AS SOON AS
        //       the contiguous run closes. The ONE subtlety the watermark exists
        //       to handle: out-of-order commit can leave a published write's LSN
        //       temporarily ABOVE the contiguous watermark (an earlier LSN has
        //       not yet `mark_committed`). That is exactly why we reclaim by the
        //       WATERMARK, not the appended frontier: any visible-but-above-
        //       watermark write has lsn > watermark_at_capture, so it is RETAINED
        //       in the WAL (never archived) and replayed on recovery — no loss.
        //       Conversely every lsn ≤ watermark_at_capture is, by the watermark
        //       contract, fully committed/durable, so archiving up to it is safe.
        //
        // Because the watermark is read FIRST, any root we subsequently load can
        // only be NEWER-or-equal (monotonic publication), so the snapshot can
        // only contain MORE writes than the watermark proves durable — and those
        // extra writes are precisely the lsn > watermark tail that stays in the
        // WAL. Reordering these two loads (root before watermark) would break the
        // subset direction and reopen #41. DO NOT REORDER.
        let watermark_at_capture = self.committed_watermark.watermark();
        // The DURABLY-SYNCED WAL frontier, captured in the same capture-ordering
        // window (before the root load). This — NOT the trie's `self.next_lsn`
        // counter — is the frontier the watermark lives in: every committed LSN
        // came from `append_to_wal_returning_lsn`, which both appends AND syncs it
        // durable (Order A), then `mark_committed`s it. `self.next_lsn` is a
        // SEPARATE, owned-mutation counter that the lock-free durable path never
        // advances, so it is the WRONG bound (it stays at its initial value while
        // the WAL writer's own LSN domain — surfaced as `synced_lsn()` — advances).
        // `None` (no WAL) ⇒ no durable LSNs can exist, so the frontier is 0 and the
        // watermark must also be 0.
        let synced_frontier_at_capture: u64 = self
            .wal_writer
            .as_ref()
            .map(|w| w.synced_lsn())
            .unwrap_or(0);

        // S5-2 (A3 floor): the durable global commit_seq, captured (Acquire) in the
        // SAME pre-root-load window as the watermark. commit_seq claims are monotone in
        // CAS order (fetch_add loop-top), so this value is ≥ every survivor generation
        // folded into the about-to-be-loaded root ⇒ raising the WAL floor to it makes a
        // post-checkpoint op out-rank all of them. Reading it BEFORE the root load is
        // required (after would risk a floor above an in-snapshot survivor). DO NOT
        // REORDER past the root load below.
        let commit_seq_at_capture = self.commit_seq.load(AtomicOrdering::Acquire);

        let overlay_root = self.lockfree_root.as_ref().and_then(|root| root.load());
        let (root_type, root_ptr, is_final, entry_count) = match overlay_root {
            None => (ROOT_TYPE_EMPTY, 0u64, false, 0u64),
            Some(root) => {
                let inner_root = overlay_to_inner::<V>(&root);
                let mut path: Vec<char> = Vec::new();
                let ptr = self.serialize_char_node_to_disk(
                    &inner_root,
                    &mut path,
                    eviction_registry.as_mut(),
                )?;
                let entry_count = count_overlay_finals(&root);
                (
                    ROOT_TYPE_NODE,
                    ptr.to_raw(),
                    inner_root.node.is_final(),
                    entry_count,
                )
            }
        };

        // ── Executable refinement of the capture-ordering invariant ──────────
        // What we CAN assert (the overlay has no per-node LSN to max over, per the
        // long comment above): the committed watermark captured BEFORE the root
        // load never exceeds the DURABLY-SYNCED WAL frontier captured in the same
        // window. This is the tight, correct refinement of
        //   snapshot ⊆ published-root ⊆ committed-prefix(watermark) ⊆ durable-WAL
        // — reclaiming the WAL up to `watermark` is safe ONLY IF every LSN ≤
        // watermark is already durably synced, i.e. `watermark ≤ synced_frontier`.
        // A watermark above the synced frontier would mean we `mark_committed`'d an
        // LSN the WAL had not actually synced (an Order-A violation / mark misuse),
        // and reclaiming to it could archive an un-synced write out of recovery's
        // reach (the GAP_LEDGER #41 footgun). We turn that into a loud failure here
        // rather than silent data loss. (`debug_assert!` is the lock-free analogue
        // of the shipped owned-path `next_lsn`-unchanged assert in
        // `publish_durable_and_reclaim`, replacing write-exclusion with a watermark
        // ≤ durable-frontier bound.)
        //
        // NOTE — domain correctness (the bug this very assert CAUGHT during the
        // soak): the bound is the WAL writer's `synced_lsn()`, NOT the trie's
        // `self.next_lsn`. Those are different LSN counters; the lock-free durable
        // path advances only the WAL writer's, leaving `self.next_lsn` at its
        // initial value, so comparing the watermark against `self.next_lsn` was a
        // domain mismatch that this debug_assert surfaced loudly.
        // S5-8: promoted debug_assert → always-on assert. The lock-free analogue of
        // the owned #41 guard above — a committed watermark beyond the durably-synced
        // frontier would let WAL reclaim archive an un-synced write. Data-loss-critical
        // once the overlay is production; Order-A + mark_committed (only after the
        // append is durable) guarantee `watermark ≤ synced_frontier`, so it cannot
        // spuriously fire. Fail-stop is strictly safer than silent loss.
        assert!(
            watermark_at_capture <= synced_frontier_at_capture,
            "capture_snapshot_immutable: committed watermark {watermark_at_capture} \
             exceeds the durably-synced WAL frontier {synced_frontier_at_capture} — \
             a committed LSN is not yet durable (Order-A / mark_committed misuse); \
             reclaiming to this watermark could archive an un-synced write \
             (GAP_LEDGER #41 capture-ordering invariant violated)"
        );
        // `next_lsn_at_capture` is consumed below; keep it (and the asserted frontiers)
        // explicitly live so the capture-ordering Acquire loads are never elided.
        let _ = (
            watermark_at_capture,
            synced_frontier_at_capture,
            next_lsn_at_capture,
        );

        if let Some(ref arena_manager) = self.arena_manager {
            arena_manager.write().flush_dirty_slots()?;
        }
        let arena_count: u32 = if let Some(ref arena_manager) = self.arena_manager {
            arena_manager.read().arena_count() as u32
        } else {
            0
        };

        Ok(CheckpointSnapshot {
            root_type,
            is_final,
            entry_count,
            arena_count,
            root_ptr,
            next_lsn_at_capture,
            // The watermark captured BEFORE the root load — the safe `checkpoint_lsn`
            // the retaining-WAL publisher records so recovery skips deltas ≤ it.
            committed_watermark_at_capture: Some(watermark_at_capture),
            // The commit_seq captured in the same window (S5-2); the publisher raises
            // the WAL floor to it.
            commit_seq_at_capture: Some(commit_seq_at_capture),
            eviction_registry,
        })
    }

    /// **Migration Phase E (test-only):** publish an immutable-overlay snapshot's
    /// durable on-disk image and record `checkpoint_lsn = committed watermark`,
    /// **while RETAINING the entire WAL** — the provably-safe checkpoint to run
    /// CONCURRENTLY with lock-free Order-A writers in the reversible-hardening soak.
    ///
    /// The shipped [`Self::publish_durable_and_reclaim`] rotates/truncates the WAL
    /// by `next_lsn` and asserts `next_lsn` is unchanged since capture — both of
    /// which are INCOMPATIBLE with concurrent lock-free writers (writers bump the
    /// WAL frontier mid-checkpoint, which is the entire reason the committed
    /// watermark exists). Destructive watermark-bounded WAL *truncation* is the
    /// owner-gated IRREVERSIBLE flip, out of scope here. So this helper does the
    /// SAFE, REVERSIBLE subset:
    ///
    ///   1. publish the descriptor + fsync the data file (the on-disk image
    ///      advances and is verified durable);
    ///   2. append a `Checkpoint` WAL record carrying `checkpoint_lsn = w` (the
    ///      watermark captured BEFORE the root load — `plan §4`'s mandated safe
    ///      `checkpoint_lsn`), then sync it — but DO NOT rotate/truncate. The full
    ///      WAL stays on disk.
    ///
    /// The `Checkpoint` record is what makes this NON-DOUBLE-COUNTING for counters:
    /// recovery skips WAL records with `lsn ≤ checkpoint_lsn` (already folded into
    /// the published image) and replays only the tail `lsn > w`. Without it,
    /// recovery would load the image's counts AND re-apply every retained
    /// `BatchIncrement` delta on top → an inflated count (the exact bug the counter
    /// soak caught: c0 reopened to 115 instead of 60). Membership inserts are
    /// idempotent so they tolerated the missing record, but deltas are not.
    ///
    /// Because the watermark is the contiguous committed-durable prefix and the WAL
    /// tail `> w` is retained in full, recovery sees image(≤w) ⊕ WAL(>w) with NO
    /// overlap and NO gap → every acknowledged write survives exactly once under
    /// ANY interleaving. It only ever ADDS durability (retains more WAL than a
    /// truncating reclaim would), so it cannot lose a write — the Task-4 contract.
    ///
    /// Requires the snapshot to come from [`Self::capture_snapshot_immutable`]
    /// (which sets `committed_watermark_at_capture`); an owned-tree snapshot
    /// (`None`) is rejected, since its `next_lsn` convention is the wrong
    /// `checkpoint_lsn` here.
    ///
    /// REVERSIBLE BENCH GATE: also exposed under the existing `bench-internals`
    /// feature (still `pub(crate)`) so the `lockfree_flip_benchmark` can drive
    /// the TREATMENT immutable-snapshot publish without the Phase-E flip.
    /// S5-9: un-gated to production; `checkpoint()` route-splits to this under
    /// `route_overlay()`. Inert until the S5-12 flip.
    pub(crate) fn publish_immutable_snapshot_retaining_wal(
        &self,
        snapshot: &CheckpointSnapshot,
    ) -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        // The eviction registry is intentionally NOT published here: this helper
        // is for the durability soak, which does not enable eviction (so the
        // snapshot's `eviction_registry` is always `None`). Publishing it is a
        // Phase-D concern orthogonal to the durability contract and would require
        // the registry to be `Clone` (it is not), so it is left to the
        // owner-gated flip's `publish_durable_and_reclaim`.
        debug_assert!(
            snapshot.eviction_registry.is_none(),
            "publish_immutable_snapshot_retaining_wal is the eviction-disabled soak \
             publisher; an eviction registry here means it was called on an \
             eviction-enabled trie, which must use publish_durable_and_reclaim"
        );

        // The safe `checkpoint_lsn` is the watermark captured before the root load.
        let checkpoint_lsn = snapshot.committed_watermark_at_capture.ok_or_else(|| {
            PersistentARTrieError::internal(
                "publish_immutable_snapshot_retaining_wal requires an immutable-overlay \
                 snapshot (committed_watermark_at_capture = Some); got an owned-tree snapshot",
            )
        })?;

        // (1) Durable descriptor publish (the on-disk linearization point) + verify.
        self.publish_snapshot(snapshot)?;
        self.verify_checkpoint()?;

        // (2) Record `checkpoint_lsn = watermark` so recovery skips deltas ≤ it
        //     (already in the image), then sync — but RETAIN the WAL (no rotate).
        if let Some(ref wal_writer) = self.wal_writer {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            wal_writer
                .append(WalRecord::Checkpoint {
                    checkpoint_lsn,
                    timestamp,
                })
                .map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;
            wal_writer
                .sync()
                .map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;
            // S5-2 (A3 floor): durably raise the WAL commit_seq floor to the value
            // captured in the watermark window, so a post-checkpoint overlay op
            // out-ranks every pre-checkpoint survivor across a later rotate. Monotone
            // (raise-only); carried across rotate. `None` for an owned snapshot.
            if let Some(floor) = snapshot.commit_seq_at_capture {
                wal_writer.set_commit_seq_floor(floor).map_err(|e| {
                    PersistentARTrieError::WalError {
                        reason: format!("{:?}", e),
                    }
                })?;
            }
            // Deliberately NO rotate_to_archive: the WAL (incl. the tail > w) is
            // retained in full. That is what keeps this reversible (no destructive
            // truncation) while remaining non-double-counting (the Checkpoint
            // record gates the replay).
        }
        Ok(())
    }

    /// **EVICTION-ON reversible publisher** — the durable retain-WAL checkpoint of
    /// [`Self::publish_immutable_snapshot_retaining_wal`] PLUS eviction-registry
    /// publication, for benchmarking/testing the eviction-ON immutable-snapshot
    /// checkpoint path WITHOUT the owner-gated production flip
    /// (`g4-eviction-on-immutable-checkpoint.md`).
    ///
    /// The shipped [`Self::publish_immutable_snapshot_retaining_wal`] deliberately
    /// REFUSES a registry (`debug_assert!(eviction_registry.is_none())`): it is the
    /// eviction-DISABLED durability soak publisher. The owned-tree
    /// [`Self::publish_durable_and_reclaim`] DOES publish the registry, but its
    /// reclaim is lock-free-incompatible (it reclaims by `next_lsn`, which the
    /// lock-free durable path never advances, and asserts `next_lsn` unchanged,
    /// which a concurrent `insert_cas_durable` violates). This publisher is the
    /// one-line gap closed: the watermark-bounded **retain-WAL** reclaim of the
    /// retain-WAL publisher (record `checkpoint_lsn = committed watermark`, RETAIN
    /// the WAL, NO destructive `rotate_to_archive`) plus the registry publication
    /// the owned path already does (`coordinator.update_disk_registry`).
    ///
    /// Reclaim/durability semantics are therefore BYTE-IDENTICAL to the
    /// already-proven [`Self::publish_immutable_snapshot_retaining_wal`]: the
    /// single most dangerous line — recording `checkpoint_lsn = watermark` and
    /// retaining the WAL — is UNMOVED. The committed-watermark no-lost-write proof
    /// (`LockFreeDurableCheckpoint.tla` `NoLostWriteUnderLockFreeCommit`,
    /// re-derived under registry publication + eviction in
    /// `LockFreeDurableCheckpointEviction.tla`) carries: the registry is invisible
    /// to recovery (`RecoveredSet` never reads it), so publishing it cannot change
    /// the conclusion. The registry is published ONLY AFTER `verify_checkpoint()`
    /// proves the on-disk image durable (the `EvictionRegistryPublication.tla`
    /// publish-after-verify ordering), and every durable mutation INVALIDATES it at
    /// the `append_to_wal_inner` chokepoint BEFORE its visibility CAS — so a racing
    /// writer dirties the published registry before its write is visible, and
    /// eviction (gated on `is_valid()`) then reclaims nothing: a liveness loss, not
    /// a safety loss.
    ///
    /// Takes the snapshot BY VALUE because `update_disk_registry` consumes the
    /// registry (mirrors the owned `publish_durable_and_reclaim(snapshot)`).
    /// Requires an immutable-overlay snapshot (`committed_watermark_at_capture =
    /// Some`); an owned-tree snapshot is rejected (its `next_lsn` convention is the
    /// wrong `checkpoint_lsn` here).
    ///
    /// S5-9: un-gated to production; `checkpoint()` route-splits to this under
    /// `route_overlay()` when eviction is enabled. This performs NO flip and does NO
    /// destructive WAL truncation (the retain-WAL semantics are byte-identical to
    /// `publish_immutable_snapshot_retaining_wal`). Inert until the S5-12 flip.
    pub(crate) fn publish_immutable_snapshot_retaining_wal_with_eviction(
        &self,
        snapshot: CheckpointSnapshot,
    ) -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        // The safe `checkpoint_lsn` is the committed watermark captured BEFORE the
        // root load (the data-loss-critical invariant); an owned-tree snapshot
        // (`None`) is the wrong convention here and is rejected.
        let checkpoint_lsn = snapshot.committed_watermark_at_capture.ok_or_else(|| {
            PersistentARTrieError::internal(
                "publish_immutable_snapshot_retaining_wal_with_eviction requires an \
                 immutable-overlay snapshot (committed_watermark_at_capture = Some); \
                 got an owned-tree snapshot",
            )
        })?;

        // (1) Durable descriptor publish (the on-disk linearization point) + verify.
        //     `publish_snapshot(&snapshot)` BORROWS the snapshot before the move below.
        self.publish_snapshot(&snapshot)?;
        self.verify_checkpoint()?;

        // (2) Publish the eviction registry — ONLY AFTER verify proves the image
        //     durable (publish-after-verify, EvictionRegistryPublication.tla). The
        //     registry CONSUMES (moves) here; `update_disk_registry` is an in-memory
        //     `RwLock::write` swap with ZERO fsync (no per-checkpoint fsync-count
        //     asymmetry vs the eviction-OFF publisher).
        if let Some(registry) = snapshot.eviction_registry {
            if let Some(ref coordinator) = self.eviction_coordinator {
                coordinator.update_disk_registry(registry);
            }
        }

        // (3) Record `checkpoint_lsn = watermark` so recovery skips deltas ≤ it
        //     (already in the image), then sync — but RETAIN the WAL (NO rotate).
        //     Identical to publish_immutable_snapshot_retaining_wal: the reclaim
        //     semantics, and thus the no-lost-write proof, are byte-identical.
        if let Some(ref wal_writer) = self.wal_writer {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            wal_writer
                .append(WalRecord::Checkpoint {
                    checkpoint_lsn,
                    timestamp,
                })
                .map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;
            wal_writer
                .sync()
                .map_err(|e| PersistentARTrieError::WalError {
                    reason: format!("{:?}", e),
                })?;
            // S5-2 (A3 floor): raise the WAL commit_seq floor (same as the
            // retaining-WAL publisher). `commit_seq_at_capture` is `Copy`, so it
            // survives the earlier `eviction_registry` partial-move.
            if let Some(floor) = snapshot.commit_seq_at_capture {
                wal_writer.set_commit_seq_floor(floor).map_err(|e| {
                    PersistentARTrieError::WalError {
                        reason: format!("{:?}", e),
                    }
                })?;
            }
            // Deliberately NO rotate_to_archive: destructive watermark-bounded WAL
            // truncation is the owner-gated IRREVERSIBLE flip, out of scope here.
        }
        Ok(())
    }

    /// **REVERSIBLE BENCH SHIM** (gated entirely behind the existing
    /// `bench-internals` feature). The TREATMENT (lock-free-flip) checkpoint as a
    /// single `()` -returning primitive a bench *binary* (an external crate that
    /// cannot name the `pub(crate)` [`CheckpointSnapshot`]) can call: it captures
    /// the immutable-overlay snapshot via [`Self::capture_snapshot_immutable`]
    /// and publishes it durably (WAL-retaining) via
    /// [`Self::publish_immutable_snapshot_retaining_wal`] — exactly the two steps
    /// the Phase-E flip would wire into `checkpoint()`, with NO write lock held
    /// against concurrent `insert_cas_durable` writers. Returns `Ok(())` on a
    /// successful durable publish.
    ///
    /// This exists ONLY to make the path measurable from `benches/`; it performs
    /// no flip and is compiled out unless `bench-internals` is enabled. Deleting
    /// this method (and the two `bench-internals` cfg disjuncts above) fully
    /// reverts the bench-instrumentation surface.
    // `cfg(any(test, feature = "bench-internals"))`: the wrapped helpers
    // (`capture_snapshot_immutable` / `publish_immutable_snapshot_retaining_wal`)
    // are already `any(test, …)`-gated, so widening this thin shim lets the
    // in-crate OE1–OE4 overlay-eviction correspondence tests publish an overlay
    // checkpoint under the DEFAULT `cargo test` (no `bench-internals`). The
    // `bench-internals` path is unchanged.
    #[cfg(any(test, feature = "bench-internals"))]
    pub fn bench_immutable_checkpoint(&self) -> Result<()> {
        let snapshot = self.capture_snapshot_immutable()?;
        self.publish_immutable_snapshot_retaining_wal(&snapshot)
    }

    /// **REVERSIBLE BENCH SHIM — EVICTION-ON** (gated entirely behind the existing
    /// `bench-internals` feature). The eviction-ON counterpart of
    /// [`Self::bench_immutable_checkpoint`]: captures the immutable-overlay
    /// snapshot via [`Self::capture_snapshot_immutable`] (which builds the
    /// `DiskLocationRegistry` when eviction is enabled) and publishes it durably
    /// (WAL-retaining) WITH eviction-registry publication via
    /// [`Self::publish_immutable_snapshot_retaining_wal_with_eviction`] — the two
    /// steps the eviction-ON flip would wire into `checkpoint()`, with NO write
    /// lock held against concurrent `insert_cas_durable` writers and NO destructive
    /// WAL truncation. Used by the `lockfree_flip_benchmark` `--eviction` TREATMENT
    /// arm. Deleting this method + the `bench_enable_eviction` enabler + the
    /// `bench-internals` cfg disjunct on the publisher fully reverts the
    /// eviction-ON bench surface.
    // `cfg(any(test, feature = "bench-internals"))`: see `bench_immutable_checkpoint`
    // above — widened so the OE1–OE4 overlay-eviction correspondence tests can
    // publish the eviction-ON overlay registry under the default `cargo test`.
    #[cfg(any(test, feature = "bench-internals"))]
    pub fn bench_immutable_checkpoint_with_eviction(&self) -> Result<()> {
        let snapshot = self.capture_snapshot_immutable()?;
        self.publish_immutable_snapshot_retaining_wal_with_eviction(snapshot)
    }

    /// Checkpoint **Phase B** — publish the captured snapshot durably.
    ///
    /// Writes the 18-byte root descriptor to block 0, updates the header
    /// root-pointer + entry-count, then flushes all pages and fsyncs the data
    /// file. This is the on-disk linearization point of the checkpoint.
    /// Checkpoint-level dirty state is cleared only after the WAL
    /// checkpoint/rotation step succeeds in `checkpoint()`. Takes `&self`.
    fn publish_snapshot(&self, snapshot: &CheckpointSnapshot) -> Result<()> {
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for disk serialization")
        })?;

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
        descriptor[0] = snapshot.root_type;
        descriptor[1] = if snapshot.is_final { 1 } else { 0 };
        descriptor[2..6].copy_from_slice(&(snapshot.entry_count as u32).to_le_bytes());
        descriptor[6..10].copy_from_slice(&snapshot.arena_count.to_le_bytes());
        descriptor[10..18].copy_from_slice(&snapshot.root_ptr.to_le_bytes());

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
        dm.set_entry_count(snapshot.entry_count)?;

        // Flush all pages to ensure durability. This publishes the root
        // descriptor, but checkpoint-level dirty state is cleared only after
        // the WAL checkpoint/rotation step succeeds in `checkpoint()`.
        bm.flush_all()?;
        dm.sync()?;
        Ok(())
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

/// Convert an immutable lock-free overlay node (`PersistentCharNode`) into an
/// owned production `CharTrieNodeInner<V>` subtree, recursively (Phase-B helper).
///
/// The conversion lets the immutable overlay be checkpointed through the EXISTING
/// `serialize_char_node_to_disk` (equivalence by construction). Children are added
/// via `add_child_growing`, which grows the ART tier (N4→N16→…) and returns the
/// grown node — captured here to replace `inner.node` (unlike `CharTrieNodeInner`'s
/// `Clone`, which pre-sizes the tier and can ignore the return).
///
/// S5-9: un-gated to production (backs the now-production `capture_snapshot_immutable`).
/// Adds no `unsafe` (`Box::into_raw` + `SwizzledPtr::in_memory` are safe).
fn overlay_to_inner<V>(node: &super::nodes::PersistentCharNode<V>) -> CharTrieNodeInner<V>
where
    V: DictionaryValue,
{
    let mut inner = CharTrieNodeInner::<V>::default();
    inner.node.header_mut().set_final(node.is_final());
    // G1: the overlay node now carries `Option<V>` directly, so the value is read
    // straight off the node — no `u64 → V` (`map_value`) bridge. For `V = ()`
    // membership the overlay never stores a value, so this is `None`.
    inner.value = node.get_value();
    for (&key, child) in node.iter_children() {
        if let Some(child_arc) = child.as_in_mem() {
            let child_inner = overlay_to_inner::<V>(child_arc);
            let child_ptr = SwizzledPtr::in_memory(Box::into_raw(Box::new(child_inner)));
            if let Some(grown) = inner
                .node
                .add_child_growing(key, child_ptr)
                .expect("overlay_to_inner: add in-memory child within capacity")
            {
                inner.node = grown;
            }
        } else if let Some(on_disk) = child.as_on_disk() {
            // On-disk overlay children (from a future fault-in/eviction path) carry
            // an already-serialized location; reuse it directly. Null fillers are
            // never yielded by `iter_children`, but guard defensively.
            if !on_disk.is_null() {
                if let Some(grown) = inner
                    .node
                    .add_child_growing(key, on_disk.clone())
                    .expect("overlay_to_inner: add on-disk child within capacity")
                {
                    inner.node = grown;
                }
            }
        }
    }
    inner
}

/// Convert ONE owned production `CharTrieNodeInner<V>` back into an immutable
/// lock-free overlay node (`PersistentCharNode<V>`), keeping its children as
/// `Child::OnDisk(SwizzledPtr)` references (single-level / lazy — exactly the
/// overlay granularity). This is the **structural inverse builder** of
/// [`overlay_to_inner`]'s single-node projection: where `overlay_to_inner` reads
/// an overlay node's finality / value / child-set into an inner node,
/// `inner_to_overlay` reads them back out into a fresh overlay node.
///
/// FAULT-IN ROLE (design §2): the bytes at a `Child::OnDisk(ptr)` location were
/// written by `serialize_char_node_to_disk` from `overlay_to_inner(n)`;
/// `load_char_node_from_disk_lazy` is its proven inverse *decoder* (yielding the
/// owned `CharTrieNodeInner<V>` with children still OnDisk); `inner_to_overlay`
/// is the inverse *builder* that turns that decoded inner back into an overlay
/// node. Composed, `load_overlay_node_from_disk` gives
/// `load(serialize(overlay_to_inner(n))) ≡ n` for finality / value / child-set —
/// the round-trip equivalence the Phase-2 unit test + OE5 pin byte-for-byte.
///
/// Children: each non-null child SwizzledPtr is carried across verbatim as
/// `Child::OnDisk(ptr.clone())` (mirror of `overlay_to_inner`'s `Child::OnDisk`
/// arm, reversed) — NON-RECURSIVE, so deeper nodes stay on disk until they are
/// themselves faulted (the lazy discipline; one fetch per node per eviction
/// epoch). `iter_children` never yields null fillers, but we guard defensively.
///
/// Prefix: the overlay representation that `overlay_to_inner` serializes never
/// path-compresses (it builds via `add_child_growing`, which leaves the prefix
/// empty), so on the round-trip the prefix is empty; we still propagate any
/// non-empty prefix faithfully so the builder is a total inverse.
///
/// **Flip F0:** un-gated to production (the fault-in primitive that consumes it is
/// now a production path).
///
/// MAINTENANCE COUPLING: mirrors [`overlay_to_inner`]; keep the two in lockstep.
pub(super) fn inner_to_overlay<V>(
    inner: &CharTrieNodeInner<V>,
) -> super::nodes::PersistentCharNode<V>
where
    V: DictionaryValue,
{
    // Start from the (possibly non-empty) prefix. The overlay round-trip path
    // produces empty prefixes, but propagate faithfully for a total inverse.
    let prefix_len = inner.node.header().prefix_len as usize;
    let mut node = if prefix_len > 0 {
        super::nodes::PersistentCharNode::<V>::with_prefix(inner.node.prefix().as_slice(prefix_len))
    } else {
        super::nodes::PersistentCharNode::<V>::new()
    };

    if inner.is_final() {
        node = node.as_final();
    }
    // G1: the overlay node carries `Option<V>` directly (no `u64 → V` bridge).
    if let Some(v) = inner.value.clone() {
        node = node.with_value(v);
    }
    for (key, ptr) in inner.node.iter_children() {
        if !ptr.is_null() {
            node = node.with_child(
                key,
                super::nodes::persistent_node::Child::OnDisk(ptr.clone()),
            );
        }
    }
    node
}

/// Count the finalized (terminal) nodes in the overlay subtree — the term count of
/// the immutable representation (`self.len` tracks the owned tree, not the overlay).
///
/// S5-9: un-gated to production (backs the now-production `capture_snapshot_immutable`).
fn count_overlay_finals<V: DictionaryValue>(node: &super::nodes::PersistentCharNode<V>) -> u64 {
    let mut count = u64::from(node.is_final());
    for (_, child) in node.iter_children() {
        if let Some(child_arc) = child.as_in_mem() {
            count += count_overlay_finals(child_arc);
        }
    }
    count
}

#[cfg(test)]
mod immutable_checkpoint_correspondence {
    //! **Migration Phase B correspondence.** A checkpoint captured from the
    //! IMMUTABLE lock-free overlay representation (`capture_snapshot_immutable`)
    //! must reopen to a dictionary equal to one captured from the owned tree
    //! (`checkpoint`) built from the same terms. This proves the immutable
    //! representation is a sound checkpoint source — the foundation for later
    //! making it the default (Phases C–F). Scratch lives on real disk
    //! (`target/test-tmp`), never `/tmp` (tmpfs).

    use crate::persistent_artrie_char::PersistentARTrieChar;
    use crate::{Dictionary, MappedDictionary};

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp")
    }

    /// `V = ()` membership: build the two reps from the same terms, checkpoint each
    /// via its own path, reopen both, and assert identical membership.
    #[test]
    fn membership_immutable_checkpoint_reopens_equal_to_owned() {
        // Terms spanning every ART tier (N4/N16/N48/Bucket via the wide fan) +
        // shared spines + Unicode, so the converter exercises tier growth.
        let mut terms: Vec<String> = vec![
            "a", "ab", "abc", "abd", "abe", "b", "ban", "banana", "bandana", "z", "日本", "🎉",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        // A wide fan under "w" to force N4→N16→N48→Bucket growth in the converter.
        for i in 0..60u32 {
            terms.push(format!("w{i:02}"));
        }

        // Owned representation.
        let dir_o = scratch("imm-ckpt-owned");
        let path_o = dir_o.path().join("owned.artc");
        {
            let mut owned = PersistentARTrieChar::<()>::create(&path_o).expect("create owned");
            for t in &terms {
                owned.insert(t).expect("owned insert");
            }
            owned.checkpoint().expect("owned checkpoint");
        }

        // Immutable (lock-free overlay) representation.
        let dir_i = scratch("imm-ckpt-immutable");
        let path_i = dir_i.path().join("immutable.artc");
        let (snap_entry_count, snap_root_type) = {
            let mut imm = PersistentARTrieChar::<()>::create(&path_i).expect("create immutable");
            imm.enable_lockfree();
            for t in &terms {
                imm.insert_cas(t);
            }
            let snapshot = imm
                .capture_snapshot_immutable()
                .expect("capture immutable snapshot");
            let counts = (snapshot.entry_count, snapshot.root_type);
            imm.publish_durable_and_reclaim(snapshot)
                .expect("publish immutable snapshot");
            counts
        };
        assert_eq!(
            snap_entry_count,
            terms.len() as u64,
            "immutable snapshot term count must equal the inserted-term count"
        );
        assert_ne!(snap_root_type, 0, "non-empty trie must have a node root");

        // Reopen both and compare membership.
        let owned = PersistentARTrieChar::<()>::open(&path_o).expect("reopen owned");
        let imm = PersistentARTrieChar::<()>::open(&path_i).expect("reopen immutable");
        for t in &terms {
            assert!(
                Dictionary::contains(&owned, t),
                "owned reopen missing term {t:?}"
            );
            assert!(
                Dictionary::contains(&imm, t),
                "immutable-checkpoint reopen missing term {t:?} (Phase-B equivalence broken)"
            );
        }
        assert!(!Dictionary::contains(&imm, "absent-term"));
        assert!(!Dictionary::contains(&imm, "w"));
        assert_eq!(
            Dictionary::len(&owned),
            Dictionary::len(&imm),
            "owned vs immutable-checkpoint reopen term counts differ"
        );
    }

    /// `V = u64` counters: the value blob (`bincode(u64)`) must round-trip
    /// identically through the immutable-rep checkpoint. G1: the overlay leaf
    /// carries the count directly in `Option<u64>`; `capture_snapshot_immutable()`
    /// copies it onto the converted node, which the existing serializer writes as
    /// the appended value blob — exactly as the owned tree does.
    #[test]
    fn counter_immutable_checkpoint_reopens_equal_to_owned() {
        let entries: Vec<(String, u64)> = vec![
            ("a", 1u64),
            ("ab", 2),
            ("abc", 30),
            ("abd", 0), // a final node whose count is 0 (still HAS_VALUE)
            ("b", 4),
            ("banana", 5000),
            ("bandana", 7),
            ("z", 9),
            ("日本", 42),
            ("🎉", 100),
        ]
        .into_iter()
        .map(|(t, v)| (t.to_string(), v))
        .collect();

        // Owned representation (V = u64).
        let dir_o = scratch("imm-ckpt-owned-u64");
        let path_o = dir_o.path().join("owned.artc");
        {
            let mut owned = PersistentARTrieChar::<u64>::create(&path_o).expect("create owned u64");
            for (t, v) in &entries {
                owned.insert_with_value(t, *v);
            }
            owned.checkpoint().expect("owned checkpoint");
        }

        // Immutable (overlay) representation: set each count to its value via a
        // single increment from 0, then checkpoint from the immutable snapshot.
        let dir_i = scratch("imm-ckpt-immutable-u64");
        let path_i = dir_i.path().join("immutable.artc");
        {
            let mut imm =
                PersistentARTrieChar::<u64>::create(&path_i).expect("create immutable u64");
            imm.enable_lockfree();
            for (t, v) in &entries {
                imm.increment_cas(t, *v);
            }
            let snapshot = imm
                .capture_snapshot_immutable()
                .expect("capture immutable u64 snapshot");
            assert_eq!(snapshot.entry_count, entries.len() as u64);
            imm.publish_durable_and_reclaim(snapshot)
                .expect("publish immutable u64 snapshot");
        }

        // Reopen both and compare values.
        let owned = PersistentARTrieChar::<u64>::open(&path_o).expect("reopen owned u64");
        let imm = PersistentARTrieChar::<u64>::open(&path_i).expect("reopen immutable u64");
        for (t, v) in &entries {
            assert_eq!(
                owned.get_value(t),
                Some(*v),
                "owned reopen value mismatch for {t:?}"
            );
            assert_eq!(
                imm.get_value(t),
                Some(*v),
                "immutable-checkpoint reopen value mismatch for {t:?} (Phase-B value-blob equivalence broken)"
            );
        }
        assert_eq!(imm.get_value("absent"), None);
    }
}

#[cfg(test)]
mod overlay_faultin_load_roundtrip {
    //! **Fault-in Phase 2 — load+converter round-trip (design §2).** The fault-in
    //! load primitive [`super::super::PersistentARTrieChar::load_overlay_node_from_disk`]
    //! must satisfy `load(serialize(overlay_to_inner(n))) ≡ n` for finality /
    //! value / child-set, where `serialize` is the EXISTING production
    //! `serialize_char_node_to_disk` and `load` reuses the EXISTING lazy decoder
    //! `load_char_node_from_disk_lazy`. This pins the round-trip equivalence that
    //! makes "a faulted node equals its durable bytes" true (the TLA
    //! `FaultEqualsDurable` witness), so fault-in can never manufacture or drop a
    //! term. Both `V = ()` (membership) and `V = u64` (the bincode value blob)
    //! are exercised. Scratch is real disk (`target/test-tmp`), never `/tmp`.

    use crate::persistent_artrie_char::nodes::persistent_node::{Child, PersistentCharNode};
    use crate::persistent_artrie_char::PersistentARTrieChar;
    use crate::persistent_artrie_core::overlay::node::OverlayNode;
    use crate::value::DictionaryValue;

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp")
    }

    /// Compare the public structure (finality, value, child key-set, per-child
    /// OnDisk discriminant) of two overlay nodes — the equivalence the round-trip
    /// must preserve at single-node (lazy) granularity. The loaded node's children
    /// are always `OnDisk` (lazy); the original's children we build as `OnDisk`
    /// too, so the discriminant matches.
    fn assert_overlay_node_eq<V: DictionaryValue + PartialEq + std::fmt::Debug>(
        loaded: &OverlayNode<crate::persistent_artrie_core::key_encoding::CharKey, V>,
        original: &OverlayNode<crate::persistent_artrie_core::key_encoding::CharKey, V>,
    ) {
        assert_eq!(
            loaded.is_final(),
            original.is_final(),
            "fault-in round-trip: finality diverged"
        );
        assert_eq!(
            loaded.get_value(),
            original.get_value(),
            "fault-in round-trip: value diverged"
        );
        let mut loaded_keys: Vec<u32> = loaded.iter_children().map(|(k, _)| *k).collect();
        let mut orig_keys: Vec<u32> = original.iter_children().map(|(k, _)| *k).collect();
        loaded_keys.sort_unstable();
        orig_keys.sort_unstable();
        assert_eq!(
            loaded_keys, orig_keys,
            "fault-in round-trip: child key-set diverged"
        );
        for (k, child) in loaded.iter_children() {
            assert!(
                child.as_on_disk().is_some(),
                "fault-in round-trip: loaded child {k} must stay OnDisk (lazy)"
            );
        }
    }

    /// Build an overlay node, serialize it through the production
    /// `serialize_char_node_to_disk`, then fault it back in via
    /// `load_overlay_node_from_disk` and assert structural equivalence.
    ///
    /// To produce real on-disk child SwizzledPtrs for the parent's child slots,
    /// we first serialize a couple of standalone child leaves (getting their disk
    /// ptrs), then attach those ptrs as `Child::OnDisk` on the parent's overlay
    /// node before serializing the parent. This mirrors exactly what eviction +
    /// the lazy serializer produce for an overlay node whose children are on disk.
    fn roundtrip_one<V>(prefix: &str, value: Option<V>, child_keys: &[u32])
    where
        V: DictionaryValue + PartialEq + std::fmt::Debug,
    {
        let dir = scratch(prefix);
        let path = dir.path().join("t.artc");
        let mut trie = PersistentARTrieChar::<V>::create(&path).expect("create disk-backed trie");

        // Serialize standalone child leaves to obtain real OnDisk SwizzledPtrs.
        let mut original = PersistentCharNode::<V>::new().as_final();
        if let Some(v) = value.clone() {
            original = original.with_value(v);
        }
        for &k in child_keys {
            // A minimal final child leaf; serialize it to disk and capture its ptr.
            let child_overlay = PersistentCharNode::<V>::new().as_final();
            let child_inner = super::overlay_to_inner::<V>(&child_overlay);
            let mut p: Vec<char> = Vec::new();
            let child_ptr = trie
                .serialize_char_node_to_disk(&child_inner, &mut p, None)
                .expect("serialize child leaf to disk");
            assert!(
                child_ptr.disk_location().is_some(),
                "serialized child must have a disk location"
            );
            original = original.with_child(k, Child::OnDisk(child_ptr));
        }

        // Serialize the parent overlay node (via overlay_to_inner -> production
        // serializer), then fault it back in.
        let parent_inner = super::overlay_to_inner::<V>(&original);
        let mut pp: Vec<char> = Vec::new();
        let parent_ptr = trie
            .serialize_char_node_to_disk(&parent_inner, &mut pp, None)
            .expect("serialize parent overlay node to disk");
        assert!(parent_ptr.disk_location().is_some());

        let loaded = trie
            .load_overlay_node_from_disk(&parent_ptr)
            .expect("fault-in load_overlay_node_from_disk");

        assert_overlay_node_eq::<V>(&loaded, &original);
    }

    /// `V = ()` membership: a final node with several OnDisk children round-trips.
    #[test]
    fn membership_node_load_roundtrip_preserves_structure() {
        let keys: Vec<u32> = ['a', 'b', 'z', '日', '🎉']
            .iter()
            .map(|c| *c as u32)
            .collect();
        roundtrip_one::<()>("faultin-rt-unit", None, &keys);
    }

    /// `V = u64` valued: the bincode value blob must round-trip identically,
    /// including a value of 0 on a final node (HAS_VALUE set, value == 0).
    #[test]
    fn valued_node_load_roundtrip_preserves_value_and_structure() {
        let keys: Vec<u32> = ['x', 'y'].iter().map(|c| *c as u32).collect();
        roundtrip_one::<u64>("faultin-rt-u64", Some(4242u64), &keys);
        // Edge: value 0 on a final node.
        roundtrip_one::<u64>("faultin-rt-u64-zero", Some(0u64), &['q' as u32]);
    }

    /// A leaf with NO children (the common terminal case) round-trips: finality
    /// preserved, empty child-set.
    #[test]
    fn childless_final_leaf_load_roundtrip() {
        roundtrip_one::<()>("faultin-rt-leaf", None, &[]);
        roundtrip_one::<u64>("faultin-rt-leaf-u64", Some(7u64), &[]);
    }
}

#[cfg(test)]
mod immutable_recovery_correspondence {
    //! **Migration Phase C — recovery rebuild of the immutable (overlay) root.**
    //!
    //! Because Phase B kept the on-disk format unchanged (the immutable rep is
    //! serialized through the SAME `serialize_char_node_to_disk`), recovery uses
    //! the EXISTING owned-tree loader — no descriptor version bit is needed. This
    //! phase proves the lock-free overlay can be **reconstituted after recovery**
    //! (the bootstrap an overlay-default architecture needs on open): reopen a
    //! checkpointed trie, rebuild the overlay from the recovered terms, and assert
    //! the overlay answers identically to the recovered owned tree. (A structural,
    //! on-disk-children-preserving lazy load is a Phase-E refinement.) Scratch is
    //! real disk (`target/test-tmp`), never `/tmp` (tmpfs).

    use crate::persistent_artrie_char::PersistentARTrieChar;
    use crate::{Dictionary, MappedDictionary};

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp")
    }

    /// `V = ()` membership: after recovery, the rebuilt overlay must answer
    /// membership identically to the recovered owned tree.
    #[test]
    fn membership_overlay_rebuilt_from_recovered_matches_owned() {
        let mut terms: Vec<String> = vec!["a", "ab", "abc", "b", "banana", "z", "日本", "🎉"]
            .into_iter()
            .map(String::from)
            .collect();
        for i in 0..40u32 {
            terms.push(format!("k{i:02}"));
        }

        let dir = scratch("phase-c-membership");
        let path = dir.path().join("t.artc");
        {
            let mut owned = PersistentARTrieChar::<()>::create(&path).expect("create");
            for t in &terms {
                owned.insert(t).expect("insert");
            }
            owned.checkpoint().expect("checkpoint");
        }

        // Recover the owned tree, then rebuild the overlay from its terms.
        let mut recovered = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        let recovered_terms: Vec<String> = recovered.iter().collect();
        assert_eq!(
            recovered_terms.len(),
            terms.len(),
            "recovery lost terms before overlay rebuild"
        );
        recovered.enable_lockfree();
        for t in &recovered_terms {
            recovered.insert_cas(t);
        }

        // The rebuilt overlay answers membership identically to the recovered tree.
        for t in &terms {
            assert!(
                Dictionary::contains(&recovered, t),
                "recovered owned tree missing {t:?}"
            );
            assert!(
                recovered.contains_lockfree(t),
                "rebuilt overlay missing recovered term {t:?} (Phase-C rebuild broken)"
            );
        }
        assert!(!recovered.contains_lockfree("absent-term"));
    }

    /// `V = u64` counters: the rebuilt overlay must carry the recovered values.
    #[test]
    fn counter_overlay_rebuilt_from_recovered_matches_owned() {
        let entries: Vec<(String, u64)> = vec![
            ("a", 1u64),
            ("ab", 2),
            ("abc", 30),
            ("b", 4),
            ("banana", 5000),
            ("z", 9),
            ("日本", 42),
        ]
        .into_iter()
        .map(|(t, v)| (t.to_string(), v))
        .collect();

        let dir = scratch("phase-c-counter");
        let path = dir.path().join("t.artc");
        {
            let mut owned = PersistentARTrieChar::<u64>::create(&path).expect("create");
            for (t, v) in &entries {
                owned.insert_with_value(t, *v);
            }
            owned.checkpoint().expect("checkpoint");
        }

        // Reopen: the Overlay-regime reopen AUTOMATICALLY rebuilds the overlay from the
        // recovered owned tree (the Phase-C value rebuild is now wired into the flip's
        // open path via `reestablish_overlay_after_recovery`). A manual `enable_lockfree`
        // + `increment_cas` rebuild here would DOUBLE-count on top of the automatic one.
        let recovered = PersistentARTrieChar::<u64>::open(&path).expect("reopen");

        // The rebuilt overlay carries each recovered value — read via the overlay-routed
        // `get_value` and the direct `get_lockfree`.
        for (t, v) in &entries {
            assert_eq!(
                recovered.get_value(t),
                Some(*v),
                "routed get_value mismatch for {t:?}"
            );
            assert_eq!(
                recovered.get_lockfree(t),
                Some(*v),
                "rebuilt overlay value mismatch for {t:?} (Phase-C value rebuild broken)"
            );
        }
    }
}

#[cfg(test)]
mod multi_writer_checkpointer_soak {
    //! **Migration Phase E — multi-writer ‖ checkpointer durability soak (the
    //! #41-closed witness under lock-free writers).**
    //!
    //! N writer threads run the Order-A durable overlay paths
    //! (`insert_cas_durable` for membership, `try_increment_cas_durable` for
    //! counters) CONCURRENTLY with one checkpointer thread that repeatedly
    //! captures an immutable overlay snapshot (`capture_snapshot_immutable` — the
    //! watermark-before-root capture-ordering path with its snapshot-LSN assert)
    //! and publishes its durable on-disk image while RETAINING the full WAL
    //! (`publish_immutable_snapshot_retaining_wal`). After bounded rounds the trie
    //! is dropped WITHOUT a final reclaim and reopened: EVERY acknowledged write
    //! must survive — exact term set for membership, exact summed counts for
    //! counters.
    //!
    //! Why this is safe AND a real test of the capture path: the checkpointer
    //! advances the on-disk checkpoint image concurrently with committing writers
    //! (exercising the dangerous capture-before-load ordering under contention),
    //! but never reclaims the WAL (watermark-bounded reclaim is the owner-gated
    //! irreversible flip). So recovery has the checkpoint image AND the full WAL
    //! tail; durability can only ever be ADDED, never lost, under any interleaving
    //! — which is exactly the property asserted. A single checkpointer avoids
    //! concurrent arena re-serialization (the arena/buffer managers are
    //! interior-`RwLock`, so this is memory-safe regardless, but one checkpointer
    //! keeps the on-disk image well-defined). Bounded, deterministic, seconds-long.
    //!
    //! Scratch is real disk (`target/test-tmp`), never `/tmp` (tmpfs on this host),
    //! with a modest node budget.

    use crate::persistent_artrie_char::PersistentARTrieChar;
    use crate::persistent_artrie_core::durability::DurabilityPolicy;
    use crate::{Dictionary, MappedDictionary};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp")
    }

    /// **S5-9 route-split (RES-4 total-loss guard).** Under the overlay write mode,
    /// `checkpoint()` MUST capture the immutable overlay (the live data), not the
    /// empty owned tree. SELF-ENFORCING: the owned arm asserts `!route_overlay()`, so
    /// if `checkpoint()` succeeds under `route_overlay()==true` it provably took the
    /// overlay arm (else it would panic). Pre-checkpoint the data is overlay-only
    /// (owned read sees nothing); reopen sees every term ⇒ no loss.
    #[test]
    fn s5_9_overlay_checkpoint_captures_overlay_not_empty_owned() {
        use super::super::overlay_write_mode::OverlayWriteMode;
        let dir = scratch("s5-9-route-split");
        let path = dir.path().join("t.artc");
        let terms: Vec<String> = (0..50u32).map(|i| format!("term{i:03}")).collect();
        {
            let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
            for t in &terms {
                trie.insert_cas_durable(t).expect("durable overlay insert");
            }
            // The data is OVERLAY-only: the overlay read sees it, the owned read does
            // not. A checkpoint that captured the owned tree would persist NOTHING.
            for t in &terms {
                assert!(trie.contains_lockfree(t), "overlay missing {t:?}");
                // E1: `Dictionary::contains` now routes to the overlay (the read-flip),
                // so peek the OWNED tree directly via the un-routed reader to prove the
                // data is overlay-only (the owned tree never received it).
                assert!(
                    !trie.owned_try_contains(t).expect("owned read"),
                    "owned tree must NOT hold {t:?} (data is overlay-only)"
                );
            }
            // Succeeding here proves the overlay arm was taken (owned arm would panic
            // its !route_overlay() assert).
            trie.checkpoint()
                .expect("overlay checkpoint via S5-9 route-split");
        }
        let recovered = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        for t in &terms {
            assert!(
                Dictionary::contains(&recovered, t),
                "S5-9 route-split lost {t:?} (RES-4 total-loss regression)"
            );
        }
    }

    /// **S5-5/6/7 producer guards** fire under the overlay write mode (and a valid
    /// op still routes).
    #[test]
    fn s5_567_overlay_producer_guards_reject() {
        use super::super::overlay_write_mode::OverlayWriteMode;
        let dir = scratch("s5-567-guards");
        let path = dir.path().join("t.artc");
        let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        trie.enable_lockfree();
        trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);

        // S5-7 begin_document, S5-6 merge, S5-5 negative increment all REJECT.
        assert!(
            trie.begin_document("doc").is_err(),
            "S5-7: begin_document must reject under the overlay"
        );
        assert!(
            trie.merge_lockfree_values_to_persistent().is_err(),
            "S5-6: merge_lockfree_values_to_persistent must reject under the overlay"
        );
        assert!(
            trie.increment("k", -1).is_err(),
            "S5-5: a negative increment must reject under the overlay"
        );
        // ...but a non-negative increment ROUTES to the overlay (Ok).
        assert!(
            trie.increment("k", 3).is_ok(),
            "S5-5: a non-negative increment must route to the overlay"
        );
        assert_eq!(trie.get_lockfree("k"), Some(3), "routed increment value");
    }

    /// **S5-10b** — `reestablish_overlay_after_recovery` (u64) rebuilds the immutable
    /// overlay from the recovered OWNED tree, carries every value, and clears the
    /// owned tree LAST. Streaming by first code-point incl. multi-byte first units
    /// (RES-6 disjoint cover). No-WAL (increment_cas is the non-durable overlay
    /// path), so the recovered terms are not re-logged.
    #[test]
    fn s5_10b_reestablish_overlay_from_recovered_owned_u64() {
        let dir = scratch("s5-10b-reestablish");
        let path = dir.path().join("t.artc");
        // NB: the char trie's insert rejects the empty term (`chars.is_empty()`), so
        // "" is never a stored term — exercise multi-byte first units instead.
        let entries: Vec<(String, u64)> = vec![
            ("a", 1u64),
            ("ab", 2),
            ("abc", 30),
            ("b", 4),
            ("banana", 5000),
            ("z", 9),
            ("日本", 42),
            ("🎉x", 11),
        ]
        .into_iter()
        .map(|(t, v)| (t.to_string(), v))
        .collect();

        // Build an OWNED u64 trie (no overlay), checkpoint, reopen (recovered owned).
        {
            let mut owned = PersistentARTrieChar::<u64>::create(&path).expect("create");
            for (t, v) in &entries {
                owned.insert_with_value(t, *v);
            }
            owned.checkpoint().expect("checkpoint");
        }
        // The Overlay-regime reopen AUTOMATICALLY runs `reestablish_overlay_after_recovery`
        // (via `reestablish_overlay_dispatch`, u64 → the value-carrying variant — the
        // function under test). A second manual call would be redundant + double-rebuild.
        let trie = PersistentARTrieChar::<u64>::open(&path).expect("reopen");

        // Overlay carries every recovered (term, value); the owned tree is cleared.
        for (t, v) in &entries {
            assert_eq!(
                trie.get_lockfree(t),
                Some(*v),
                "overlay value mismatch for {t:?} after reestablish"
            );
        }
        // E1: `Dictionary::contains` routes to the overlay; check the owned tree directly.
        assert!(
            !trie.owned_try_contains("a").expect("owned read")
                && !trie.owned_try_contains("banana").expect("owned read"),
            "owned tree must be cleared LAST after a successful reestablish"
        );
    }

    /// **S5-10b membership twin** — `reestablish_overlay_membership_after_recovery`
    /// rebuilds the overlay (membership, no values) from the recovered owned tree and
    /// clears the owned tree.
    #[test]
    fn s5_10b_reestablish_overlay_membership_from_recovered_owned() {
        let dir = scratch("s5-10b-membership");
        let path = dir.path().join("t.artc");
        let terms: Vec<String> = vec!["a", "ab", "abc", "b", "banana", "z", "日本", "🎉x"]
            .into_iter()
            .map(String::from)
            .collect();
        {
            let mut owned = PersistentARTrieChar::<()>::create(&path).expect("create");
            for t in &terms {
                owned.insert(t).expect("insert");
            }
            owned.checkpoint().expect("checkpoint");
        }
        // The Overlay-regime reopen AUTOMATICALLY runs the membership reestablish (via
        // `reestablish_overlay_dispatch`, () → the membership twin — the function under
        // test). A second manual call would be redundant.
        let trie = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        for t in &terms {
            assert!(
                trie.contains_lockfree(t),
                "overlay missing {t:?} after membership reestablish"
            );
        }
        // E1: `Dictionary::contains` routes to the overlay; check the owned tree directly.
        assert!(
            !trie.owned_try_contains("a").expect("owned read"),
            "owned tree must be cleared after a successful membership reestablish"
        );
    }

    /// **S5-12 (V-3)**: `reestablish_overlay_dispatch` routes a u64 trie to the
    /// VALUE-carrying reestablish (NOT the value-dropping membership twin) — the
    /// Any-downcast dispatch is correct. Values must survive.
    #[test]
    fn s5_12_v3_dispatch_routes_u64_to_value_carrying_reestablish() {
        let dir = scratch("s5-12-v3-dispatch");
        let path = dir.path().join("t.artc");
        let entries: Vec<(String, u64)> = vec![("a", 1u64), ("ab", 22), ("z", 999)]
            .into_iter()
            .map(|(t, v)| (t.to_string(), v))
            .collect();
        {
            let mut owned = PersistentARTrieChar::<u64>::create(&path).expect("create");
            for (t, v) in &entries {
                owned.insert_with_value(t, *v);
            }
            owned.checkpoint().expect("checkpoint");
        }
        let mut trie = PersistentARTrieChar::<u64>::open(&path).expect("reopen");
        trie.enable_lockfree();
        trie.reestablish_overlay_dispatch()
            .expect("dispatch reestablish");
        for (t, v) in &entries {
            assert_eq!(
                trie.get_lockfree(t),
                Some(*v),
                "V-3 dispatch dropped the value for {t:?} (routed to the membership twin?)"
            );
        }
    }

    /// **S5-12 Test A — the A2 end-to-end PRIMARY gate.** An Overlay-regime WAL with a
    /// RANKED survivor (`insert_cas_durable` ⇒ durable Insert + CommitRank, acked) and a
    /// durable UNRANKED orphan (an Insert with NO following CommitRank — exactly the
    /// two-append-window crash state) ⇒ a real reopen DROPS the orphan and KEEPS the
    /// survivor (the regime-aware reconcile, end-to-end on a real on-disk WAL).
    #[test]
    fn s5_12_test_a_overlay_reopen_drops_unranked_orphan_keeps_ranked() {
        use super::super::overlay_write_mode::OverlayWriteMode;
        use crate::persistent_artrie_core::wal::WalRecord;

        let dir = scratch("s5-12-test-a");
        let path = dir.path().join("t.artc");
        {
            let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            trie.set_overlay_write_mode(OverlayWriteMode::LockFreeOverlay);
            // RANKED survivor: insert_cas_durable appends Insert + CommitRank (acked).
            assert!(trie.insert_cas_durable("survivor").expect("durable insert"));
            // Durable UNRANKED orphan: an Insert with NO following CommitRank — the
            // two-append-window crash state recovery must drop under Overlay.
            trie.append_to_wal_returning_lsn(WalRecord::Insert {
                term: b"orphan".to_vec(),
                value: None,
            })
            .expect("append durable orphan");
        }
        // Reopen: the Overlay-regime replay (regime-aware reconcile) DROPS the orphan.
        let recovered = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        assert!(
            Dictionary::contains(&recovered, "survivor"),
            "the ranked survivor must survive reopen"
        );
        assert!(
            !Dictionary::contains(&recovered, "orphan"),
            "the unranked orphan must be DROPPED on Overlay reopen (A2, end-to-end)"
        );
    }

    /// Membership soak: N writers `insert_cas_durable` disjoint shared-prefix keys
    /// ‖ a checkpointer loops capture+publish; reopen ⇒ every acknowledged term
    /// survives (exact set).
    #[test]
    fn membership_writers_concurrent_with_checkpointer_all_survive_reopen() {
        let dir = scratch("soak-membership");
        let path = dir.path().join("t.artc");
        let n_writers = 4usize;
        let per_writer = 80usize; // 320 keys — bounded, seconds.

        let acknowledged: Vec<String> = {
            let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            let trie = Arc::new(trie);
            // +1 for the checkpointer so it starts alongside the writers.
            let barrier = Arc::new(Barrier::new(n_writers + 1));
            let writers_done = Arc::new(AtomicBool::new(false));

            // Checkpointer: capture + publish (retaining WAL) until writers finish,
            // then a couple of final rounds to race the tail.
            let checkpointer = {
                let trie = Arc::clone(&trie);
                let barrier = Arc::clone(&barrier);
                let writers_done = Arc::clone(&writers_done);
                thread::spawn(move || {
                    barrier.wait();
                    let mut rounds = 0u32;
                    loop {
                        // Capture the immutable overlay snapshot (exercises the
                        // watermark-before-root capture-ordering + its assert) and
                        // publish the durable image, retaining the full WAL.
                        if let Ok(snapshot) = trie.capture_snapshot_immutable() {
                            let _ = trie.publish_immutable_snapshot_retaining_wal(&snapshot);
                        }
                        rounds += 1;
                        if writers_done.load(Ordering::Acquire) && rounds > 2 {
                            break;
                        }
                        thread::yield_now();
                    }
                })
            };

            let handles: Vec<_> = (0..n_writers)
                .map(|w| {
                    let trie = Arc::clone(&trie);
                    let barrier = Arc::clone(&barrier);
                    thread::spawn(move || {
                        barrier.wait();
                        let mut acked = Vec::with_capacity(per_writer);
                        for i in 0..per_writer {
                            // Shared "s" prefix → CAS contention on the spine.
                            let key = format!("s{w}_{i:04}");
                            if trie.insert_cas_durable(&key).expect("durable insert") {
                                acked.push(key);
                            }
                        }
                        acked
                    })
                })
                .collect();

            let acked: Vec<String> = handles
                .into_iter()
                .flat_map(|h| h.join().expect("writer thread"))
                .collect();
            writers_done.store(true, Ordering::Release);
            checkpointer.join().expect("checkpointer thread");
            // DROP WITHOUT a final reclaim — durability rests on WAL + published image.
            drop(trie);
            acked
        };

        assert_eq!(
            acknowledged.len(),
            n_writers * per_writer,
            "every distinct durable key must be newly acknowledged exactly once"
        );

        // Reopen: every acknowledged key must be recoverable (WAL replay and/or
        // the published checkpoint image).
        let reopened = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        for key in &acknowledged {
            assert!(
                Dictionary::contains(&reopened, key),
                "acknowledged durable key {key:?} lost after writers‖checkpointer reopen (#41 reborn)"
            );
        }
        assert!(!Dictionary::contains(&reopened, "never-acknowledged"));
    }

    /// Counter soak: N writers `try_increment_cas_durable` on DISTINCT keys
    /// (each by a known delta, fixed step count) ‖ a checkpointer loops the
    /// immutable CAPTURE; reopen ⇒ each key's count equals its exact summed deltas.
    ///
    /// Why the checkpointer here CAPTURES but does NOT publish a value image (it
    /// does for the idempotent membership soak): the immutable overlay carries no
    /// per-node LSN, so a captured snapshot cannot be trimmed to exactly the
    /// committed-watermark prefix — it may contain a delta with `lsn > watermark`
    /// (committed out-of-order, already in the published root but not yet under the
    /// contiguous watermark). Publishing that as a value image while ALSO retaining
    /// the WAL tail (`lsn > watermark`) would replay that delta a SECOND time →
    /// inflated count (the exact bug an earlier draft hit: c0 = 115 vs 60).
    /// Idempotent membership inserts tolerate the overlap; commutative-but-not-
    /// idempotent deltas do not. Trimming the image to ≤ watermark is the
    /// per-node-LSN closure the IRREVERSIBLE Phase-E flip adds (out of scope). So
    /// here the checkpointer still exercises the dangerous concurrent
    /// `capture_snapshot_immutable` path (its capture-ordering watermark/root load +
    /// the snapshot-LSN `debug_assert!` + the overlay walk under live CAS), which is
    /// the thing being hardened, while durability rests on pure WAL replay — keeping
    /// the assertion deterministic and exact.
    #[test]
    fn counter_writers_concurrent_with_checkpointer_sum_exactly_after_reopen() {
        let dir = scratch("soak-counter");
        let path = dir.path().join("t.artc");
        let n_writers = 4usize;
        let per_writer = 60u64; // 240 durable increments total.

        {
            let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.enable_lockfree();
            let trie = Arc::new(trie);
            let barrier = Arc::new(Barrier::new(n_writers + 1));
            let writers_done = Arc::new(AtomicBool::new(false));

            let checkpointer = {
                let trie = Arc::clone(&trie);
                let barrier = Arc::clone(&barrier);
                let writers_done = Arc::clone(&writers_done);
                thread::spawn(move || {
                    barrier.wait();
                    let mut rounds = 0u32;
                    loop {
                        // Capture-only (see the method doc above): exercises the
                        // hardened capture-ordering path + snapshot-LSN assert
                        // under live writers without publishing a double-counting
                        // value image. Durability is WAL-only for counters.
                        let _ = trie.capture_snapshot_immutable();
                        rounds += 1;
                        if writers_done.load(Ordering::Acquire) && rounds > 2 {
                            break;
                        }
                        thread::yield_now();
                    }
                })
            };

            let handles: Vec<_> = (0..n_writers)
                .map(|w| {
                    let trie = Arc::clone(&trie);
                    let barrier = Arc::clone(&barrier);
                    let delta = (w as u64) + 1; // distinct delta per writer
                    thread::spawn(move || {
                        barrier.wait();
                        let key = format!("c{w}");
                        for _ in 0..per_writer {
                            trie.try_increment_cas_durable(&key, delta)
                                .expect("durable increment");
                        }
                    })
                })
                .collect();

            for h in handles {
                h.join().expect("writer thread");
            }
            writers_done.store(true, Ordering::Release);
            checkpointer.join().expect("checkpointer thread");
            drop(trie);
        }

        // Reopen: each distinct key's count must equal per_writer * its delta.
        let reopened = PersistentARTrieChar::<u64>::open(&path).expect("reopen");
        for w in 0..n_writers {
            let delta = (w as u64) + 1;
            assert_eq!(
                reopened.get_value(&format!("c{w}")),
                Some(per_writer * delta),
                "counter c{w} lost/wrong after writers‖checkpointer reopen \
                 (Order-A durable increment under concurrent checkpoint broken)"
            );
        }
        assert_eq!(reopened.get_value("never-incremented"), None);
    }
}

#[cfg(test)]
mod immutable_eviction_checkpoint_correspondence {
    //! **EVICTION-ON immutable-snapshot checkpoint correspondence**
    //! (`docs/design/g4-eviction-on-immutable-checkpoint.md` §5b; TLA model
    //! `formal-verification/tla+/LockFreeDurableCheckpointEviction.tla`).
    //!
    //! These tests exercise the new
    //! [`PersistentARTrieChar::publish_immutable_snapshot_retaining_wal_with_eviction`]
    //! publisher — the watermark-bounded RETAIN-WAL reclaim (byte-identical to the
    //! proven eviction-OFF [`publish_immutable_snapshot_retaining_wal`]) PLUS
    //! eviction-registry publication. The two properties under test:
    //!
    //! - **T1** closes the GAP the eviction-OFF publisher leaves: that
    //!   `capture_snapshot_immutable` builds a NON-EMPTY eviction registry over the
    //!   immutable overlay snapshot (`registry.char_len() > 0`), that the publisher
    //!   makes it live (`evictable_node_count() > 0`), that a forced eviction over
    //!   it still resolves every term, and that dropping WITHOUT a destructive
    //!   reclaim then reopening loses nothing.
    //! - **T2** is the runtime witness for the NEW combo the publisher introduces:
    //!   concurrent `insert_cas_durable` writers ‖ an eviction-checkpointer looping
    //!   capture + `publish_*_with_eviction` (retain) + a racing `force_eviction`.
    //!   Reopen ⇒ the exact acknowledged set survives (membership is idempotent;
    //!   counters are CAPTURE-only — see the soak module — so this is a membership
    //!   trie).
    //!
    //! The trie handle is `SharedCharARTrie<()>` (= `Arc<RwLock<PersistentARTrieChar>>`)
    //! so the `EvictableARTrie` enable/force-eviction/observe surface is reachable;
    //! the `&self` lock-free + new-publisher methods are called through the
    //! read/write guards. Scratch is real disk (`target/test-tmp`), never `/tmp`
    //! (tmpfs on this host).

    use crate::artrie_trait::EvictableARTrie;
    use crate::persistent_artrie::eviction::EvictionConfig;
    use crate::persistent_artrie_char::{PersistentARTrieChar, SharedCharARTrie};
    use crate::persistent_artrie_core::durability::DurabilityPolicy;
    use crate::Dictionary;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp")
    }

    /// **T1** — eviction-enabled overlay membership trie, `Immediate`,
    /// `enable_lockfree`; `insert_cas_durable` a tier-spanning set; capture the
    /// immutable snapshot (assert its registry is NON-EMPTY — the GAP closed);
    /// publish with eviction (assert `evictable_node_count() > 0`); force an
    /// eviction (every term still resolves via reload); drop WITHOUT a destructive
    /// reclaim; reopen; assert EVERY acknowledged term present.
    #[test]
    fn immutable_eviction_checkpoint_reopens_losing_nothing() {
        let dir = scratch("imm-evict-t1");
        let path = dir.path().join("t.artc");

        // Tier-spanning terms: a wide fan under "w" (N4→N16→N48→Bucket growth) +
        // shared spines + Unicode, so the registry has many node paths to register.
        let mut terms: Vec<String> = vec![
            "a", "ab", "abc", "abd", "b", "ban", "banana", "bandana", "z", "日本", "🎉",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        for i in 0..80u32 {
            terms.push(format!("w{i:02}"));
        }

        let acknowledged: Vec<String> = {
            // Build the trie via the production `SharedCharARTrie` handle so the
            // `EvictableARTrie` surface (enable/force/observe) is reachable.
            let shared: SharedCharARTrie<()> =
                crate::artrie_trait::ARTrie::create(&path).expect("create eviction overlay trie");
            {
                let mut guard = shared.write();
                guard.set_durability_policy(DurabilityPolicy::Immediate);
                guard.enable_lockfree();
            }
            // Enable eviction (production wiring: shares the trie epoch manager).
            shared
                .enable_eviction(EvictionConfig::without_memory_monitor())
                .expect("enable eviction");

            // Order-A durable lock-free inserts (no write lock).
            let mut acked = Vec::with_capacity(terms.len());
            for t in &terms {
                if shared.read().insert_cas_durable(t).expect("durable insert") {
                    acked.push(t.clone());
                }
            }

            // Capture the immutable overlay snapshot. THE GAP: the registry it
            // builds over the overlay must be NON-EMPTY when eviction is enabled.
            let snapshot = shared
                .read()
                .capture_snapshot_immutable()
                .expect("capture immutable snapshot");
            let registry_len = snapshot
                .eviction_registry
                .as_ref()
                .map(|r| r.char_len())
                .expect("eviction enabled ⇒ snapshot carries a registry");
            assert!(
                registry_len > 0,
                "capture_snapshot_immutable built an EMPTY eviction registry — the \
                 eviction-ON GAP is NOT closed (expected the overlay snapshot to \
                 register its node paths)"
            );

            // Publish with eviction (retain WAL): publishes the registry to the
            // coordinator after verify, records checkpoint_lsn = watermark, retains
            // the WAL. After this the coordinator must report evictable nodes.
            shared
                .read()
                .publish_immutable_snapshot_retaining_wal_with_eviction(snapshot)
                .expect("publish immutable snapshot with eviction");
            assert!(
                shared.read().evictable_node_count().unwrap_or(0) > 0,
                "publish_*_with_eviction did not publish a non-empty registry \
                 (evictable_node_count == 0)"
            );

            // Force an eviction over the published registry. The registry IS the
            // selectable pool (`evictable_node_count() > 0` above), and selection
            // finds candidates; but the actual unswizzle (`evict_node_at_path`)
            // walks the OWNED `self.root` tree, which is `Empty` on a pure lock-free
            // overlay trie (the data lives in `lockfree_root`). So over an overlay
            // trie `force_eviction` is structurally a clean NO-OP — there are no
            // OWNED in-memory node boxes to reclaim — and must not error or panic.
            // (This is the honest architectural truth of the reversible bench path:
            // the registry-publication GAP is closed — asserted above — while
            // in-memory reclamation of overlay nodes is a Phase-E flip concern that
            // wires the overlay into the owned eviction walk. The eviction-OFF
            // CONTROL/owned-tree path DOES reclaim; see eviction_registry_tests.rs.)
            let (evicted, _bytes) = shared.force_eviction(1 << 20).expect("force eviction");
            assert_eq!(
                evicted, 0,
                "force_eviction over a lock-free OVERLAY trie should be a structural \
                 no-op (owned self.root is Empty); got {evicted} — if the overlay was \
                 wired into the owned eviction walk this expectation must be revisited"
            );

            // Every term still resolves through the overlay (the registry publish +
            // the no-op eviction left the overlay membership intact).
            for t in &terms {
                assert!(
                    shared.read().contains_lockfree(t),
                    "term {t:?} unresolvable after eviction-ON publish (overlay membership broken)"
                );
            }

            // DROP WITHOUT a destructive reclaim — durability rests on the WAL +
            // the published checkpoint image. `disable_eviction` first so the
            // background eviction thread is joined cleanly before the Arc drops.
            shared.disable_eviction().expect("disable eviction");
            drop(shared);
            acked
        };

        assert_eq!(
            acknowledged.len(),
            terms.len(),
            "every distinct durable term must be newly acknowledged exactly once"
        );

        // Reopen: EVERY acknowledged term must be present (WAL replay and/or the
        // published checkpoint image — the eviction registry is NOT recovery state).
        let reopened = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        for t in &acknowledged {
            assert!(
                Dictionary::contains(&reopened, t),
                "acknowledged term {t:?} lost after eviction-ON checkpoint reopen \
                 (#41 reborn / registry leaked into recovery)"
            );
        }
        assert!(!Dictionary::contains(&reopened, "absent-term"));
        assert!(!Dictionary::contains(&reopened, "w"));
    }

    /// **T2** — N `insert_cas_durable` writers ‖ a checkpointer looping
    /// capture + `publish_*_with_eviction` (retain) + a racing `force_eviction`;
    /// reopen ⇒ the exact acknowledged set survives. This is the runtime witness
    /// for the NEW combo (force_eviction ‖ live insert_cas_durable under the new
    /// publisher); a flake here would surface the eviction-vs-CAS-writer race
    /// (design §8 risk 3).
    #[test]
    fn writers_concurrent_with_eviction_checkpointer_all_survive_reopen() {
        let dir = scratch("imm-evict-t2");
        let path = dir.path().join("t.artc");
        let n_writers = 4usize;
        let per_writer = 80usize; // 320 keys — bounded, seconds.

        let acknowledged: Vec<String> = {
            let shared: SharedCharARTrie<()> =
                crate::artrie_trait::ARTrie::create(&path).expect("create");
            {
                let mut guard = shared.write();
                guard.set_durability_policy(DurabilityPolicy::Immediate);
                guard.enable_lockfree();
            }
            shared
                .enable_eviction(EvictionConfig::without_memory_monitor())
                .expect("enable eviction");

            // +1 for the checkpointer so it starts alongside the writers.
            let barrier = Arc::new(Barrier::new(n_writers + 1));
            let writers_done = Arc::new(AtomicBool::new(false));

            // Eviction-checkpointer: loop capture + publish-with-eviction (retain
            // WAL) + a racing force_eviction until writers finish, then a couple of
            // final rounds to race the tail.
            let checkpointer = {
                let shared = Arc::clone(&shared);
                let barrier = Arc::clone(&barrier);
                let writers_done = Arc::clone(&writers_done);
                thread::spawn(move || {
                    barrier.wait();
                    let mut rounds = 0u32;
                    loop {
                        // Capture the immutable overlay snapshot (exercises the
                        // watermark-before-root capture-ordering + its assert) and
                        // publish the durable image WITH eviction, retaining the WAL.
                        if let Ok(snapshot) = shared.read().capture_snapshot_immutable() {
                            let _ = shared
                                .read()
                                .publish_immutable_snapshot_retaining_wal_with_eviction(snapshot);
                        }
                        // Race a forced eviction against the live CAS writers (the
                        // registry is invalidated by each durable write before its
                        // visibility CAS, so this is liveness-not-safety; it must
                        // never crash / lose a write).
                        let _ = shared.force_eviction(1 << 20);
                        rounds += 1;
                        if writers_done.load(Ordering::Acquire) && rounds > 2 {
                            break;
                        }
                        thread::yield_now();
                    }
                })
            };

            let handles: Vec<_> = (0..n_writers)
                .map(|w| {
                    let shared = Arc::clone(&shared);
                    let barrier = Arc::clone(&barrier);
                    thread::spawn(move || {
                        barrier.wait();
                        let mut acked = Vec::with_capacity(per_writer);
                        for i in 0..per_writer {
                            // Shared "s" prefix → CAS contention on the spine.
                            let key = format!("s{w}_{i:04}");
                            if shared
                                .read()
                                .insert_cas_durable(&key)
                                .expect("durable insert")
                            {
                                acked.push(key);
                            }
                        }
                        acked
                    })
                })
                .collect();

            let acked: Vec<String> = handles
                .into_iter()
                .flat_map(|h| h.join().expect("writer thread"))
                .collect();
            writers_done.store(true, Ordering::Release);
            checkpointer.join().expect("checkpointer thread");
            // DROP WITHOUT a final reclaim — durability rests on WAL + published image.
            shared.disable_eviction().expect("disable eviction");
            drop(shared);
            acked
        };

        assert_eq!(
            acknowledged.len(),
            n_writers * per_writer,
            "every distinct durable key must be newly acknowledged exactly once"
        );

        // Reopen: every acknowledged key must be recoverable.
        let reopened = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        for key in &acknowledged {
            assert!(
                Dictionary::contains(&reopened, key),
                "acknowledged durable key {key:?} lost after writers‖eviction-checkpointer \
                 reopen (#41 reborn / eviction-vs-CAS race)"
            );
        }
        assert!(!Dictionary::contains(&reopened, "never-acknowledged"));
    }
}
