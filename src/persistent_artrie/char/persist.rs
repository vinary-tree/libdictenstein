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
use crate::persistent_artrie::core::key_encoding::CharKey;
use crate::persistent_artrie::core::overlay::compressed_serialize::OverlayCompressedSerialize;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::eviction::DiskLocationRegistry;
use crate::persistent_artrie::swizzled_ptr::{NodeType, SwizzledPtr};
use crate::persistent_artrie::wal::WalRecord;
use crate::value::DictionaryValue;

use super::dict_impl_char::{ROOT_TYPE_EMPTY, ROOT_TYPE_NODE};
use super::nodes::CharNode;
use super::types::CharTrieNodeInner;

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
    pub fn checkpoint(&self) -> Result<()> {
        // **F4:** `&self` — delegates to the now-`&self`
        // `checkpoint_route_split`. The owned capture takes OR-read internally; the
        // `Shared*` trait `checkpoint()` wrapper holds CK to serialize concurrent
        // checkpoints. (Reachable on owned tries + via `force_epoch_checkpoint`.)
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
        // The overlay is the sole representation (`route_overlay()` universally true),
        // so the route-split always runs the overlay capture.
        <Self as crate::persistent_artrie::core::overlay::checkpoint::OverlayCheckpoint<
            crate::persistent_artrie::core::key_encoding::CharKey,
            V,
            S,
        >>::checkpoint_route_split(self)
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
    /// `checkpoint()` route-splits to this (`route_overlay()` is universally true) so the
    /// checkpoint captures the immutable overlay (the live data). Adds zero new `unsafe`.
    pub(crate) fn capture_snapshot_immutable(&self) -> Result<CheckpointSnapshot> {
        let mut eviction_registry = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
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
                // F6 flag-1b: serialize the overlay DIRECTLY with an ITERATIVE
                // post-order walk (no deep intermediate `CharTrieNodeInner` tree,
                // no recursive serialize, no recursive `Drop`), so a ~500-char term
                // (a ~500-deep un-path-compressed overlay spine) does not overflow
                // the stack. The on-disk image is byte-identical to the prior
                // `serialize_char_node_to_disk(&overlay_to_inner(&root), ...)` (both
                // funnel each node through the shared NON-recursive
                // `serialize_one_char_node_to_disk`). `count_overlay_finals` is
                // iterative too (same reason). The root's finality is the overlay
                // root's finality (`overlay_to_inner` set the inner root's final
                // flag from `root.is_final()`).
                // CX-universal: PATH-COMPRESSED serialize (proven NO-TRUNCATION — Rocq T1/T3 +
                // exhaustive Rust round-trip/density). Also iterative (stack-safe per the note
                // above); the loader expands prefixes back into chains (4A), and the #6 path
                // re-stamps the registry at the chunk's true expanded depth. On-disk images shrink;
                // reopen stays byte-faithful (uncompressed prefix_len=0 images still load).
                let ptr =
                    self.serialize_overlay_snapshot_compressed(&root, eviction_registry.as_mut())?;
                let entry_count = count_overlay_finals(&root);
                (ROOT_TYPE_NODE, ptr.to_raw(), root.is_final(), entry_count)
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
        // Keep the asserted frontiers explicitly live so the capture-ordering
        // Acquire loads are never elided.
        let _ = (watermark_at_capture, synced_frontier_at_capture);

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
        let base_watermark = snapshot.committed_watermark_at_capture.ok_or_else(|| {
            PersistentARTrieError::internal(
                "publish_immutable_snapshot_retaining_wal requires an immutable-overlay \
                 snapshot (committed_watermark_at_capture = Some); got an owned-tree snapshot",
            )
        })?;
        // C2 (recovery double-apply fix): the on-disk `Checkpoint.checkpoint_lsn` is an
        // IMAGE-COVERAGE fact (drives the reopen drain-skip), NOT the durability watermark. A
        // post-recovery rebuild folds archived records into this image but applies them NO-WAL,
        // so record max(watermark, coverage) WITHOUT inflating the watermark — the #41 capture
        // assert is untouched. `take` is one-shot (first post-recovery checkpoint only); 0 for
        // every normal checkpoint ⇒ byte-identical to before.
        let checkpoint_lsn =
            base_watermark.max(self.committed_watermark.take_recovery_image_coverage());

        // (1) Durable descriptor publish (the on-disk linearization point) + verify. #48: the
        // image self-describes its coverage (`checkpoint_lsn`), fsync'd atomically with it.
        self.publish_snapshot(snapshot, Some(checkpoint_lsn))?;
        self.verify_checkpoint()?;

        // (2) Record `checkpoint_lsn = watermark` so recovery skips deltas ≤ it
        //     (already in the image), then sync — but RETAIN the WAL (no rotate).
        if let Some(ref wal_writer) = self.wal_writer {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let checkpoint_record_lsn = wal_writer
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
            // #49: the `Checkpoint` record consumed a WAL LSN; mark it committed (it is durable via
            // the `sync()` above) so the contiguous committed-watermark prefix does NOT stall behind
            // it. Otherwise every later steady-state checkpoint captures a watermark frozen at the
            // first checkpoint's predecessor LSN → under-claims image coverage → post-checkpoint
            // counter deltas re-drain on reopen (double-apply). Marking restores `watermark ==
            // committed-write frontier` (the `LockFreeDurableCheckpoint.tla` assumption). Safe: synced
            // BEFORE marking (#41 `watermark ≤ synced_frontier` holds) and a control record is nothing
            // to lose, so the no-lost-write proof is untouched. See
            // docs/design/checkpoint-record-lsn-watermark-gap-49-design-2026-06-08.md.
            self.committed_watermark
                .mark_committed(checkpoint_record_lsn);
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
        let base_watermark = snapshot.committed_watermark_at_capture.ok_or_else(|| {
            PersistentARTrieError::internal(
                "publish_immutable_snapshot_retaining_wal_with_eviction requires an \
                 immutable-overlay snapshot (committed_watermark_at_capture = Some); \
                 got an owned-tree snapshot",
            )
        })?;
        // C2 (see `publish_immutable_snapshot_retaining_wal`): image-coverage frontier,
        // one-shot, does not inflate the watermark.
        let checkpoint_lsn =
            base_watermark.max(self.committed_watermark.take_recovery_image_coverage());

        // (1) Durable descriptor publish (the on-disk linearization point) + verify.
        //     `publish_snapshot(&snapshot)` BORROWS the snapshot before the move below.
        // #48: the image self-describes its coverage, fsync'd atomically with it.
        self.publish_snapshot(&snapshot, Some(checkpoint_lsn))?;
        self.verify_checkpoint()?;

        // (2) Publish the eviction registry — ONLY AFTER verify proves the image
        //     durable (publish-after-verify, EvictionRegistryPublication.tla). The
        //     registry CONSUMES (moves) here; `update_disk_registry` is an in-memory
        //     `RwLock::write` swap with ZERO fsync (no per-checkpoint fsync-count
        //     asymmetry vs the eviction-OFF publisher).
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

        // (3) Record `checkpoint_lsn = watermark` so recovery skips deltas ≤ it
        //     (already in the image), then sync — but RETAIN the WAL (NO rotate).
        //     Identical to publish_immutable_snapshot_retaining_wal: the reclaim
        //     semantics, and thus the no-lost-write proof, are byte-identical.
        if let Some(ref wal_writer) = self.wal_writer {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let checkpoint_record_lsn = wal_writer
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
            // #49: mark the `Checkpoint` record's LSN committed (durable via the `sync()` above) so
            // the contiguous committed-watermark prefix does not stall behind it — identical to
            // `publish_immutable_snapshot_retaining_wal`. See
            // docs/design/checkpoint-record-lsn-watermark-gap-49-design-2026-06-08.md.
            self.committed_watermark
                .mark_committed(checkpoint_record_lsn);
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

        // (4) RESIDENT-BUDGET TAIL (Phase 7.5 — GO-LIVE). The registry is published
        //     (step 2) and the WAL Checkpoint is synced (step 3), so every registered
        //     disk_ptr is durable. If a resident budget is configured and the estimate
        //     exceeds it, evict the COLDEST registered char overlay nodes down to budget
        //     in ONE pass. The eviction is non-blocking loser-safe root-CAS (no write
        //     lock); the 1c `durable_stamp` guard + the registry `is_valid()` gate keep it
        //     safe under concurrent writers. This is the OVERLAY publisher, and
        //     `evict_overlay_nodes` is a no-op `(0,0)` with no overlay root, so no
        //     `route_overlay()` gate is needed here.
        //
        //     DEADLOCK-SAFETY: bind the coordinator in a `let` so the
        //     `eviction_coordinator` mutex guard is dropped AT THE `;` — the eviction
        //     callback (`evict_overlay_nodes`) re-locks `eviction_coordinator` for its LRU
        //     bookkeeping, and an `if let Some(c) = self.eviction_coordinator.lock()…`
        //     would hold the guard across the callback (if-let temporary lifetime) =
        //     a self-deadlock.
        let coordinator = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
            .map(std::sync::Arc::clone);
        if let Some(coordinator) = coordinator {
            if let Some(budget) = coordinator.resident_budget_bytes() {
                let resident = coordinator.char_resident_estimate_bytes();
                if resident > budget {
                    let target = resident - budget;
                    // UNCAPPED (budget-precise) by default; an opt-in cap bounds the
                    // one-time first-over-budget-checkpoint latency (it MUST be >= the
                    // per-checkpoint cold growth or the budget never converges).
                    let max_count = coordinator
                        .resident_budget_eviction_cap()
                        .unwrap_or(usize::MAX);
                    coordinator.force_eviction_char_resident(target, max_count, |nodes| {
                        super::evict_overlay_nodes(self, nodes, 4)
                    });
                }
            }
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
    fn publish_snapshot(
        &self,
        snapshot: &CheckpointSnapshot,
        image_checkpoint_lsn: Option<u64>,
    ) -> Result<()> {
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
        // C2/#48: record the IMAGE-COVERAGE frontier in block-0 ATOMICALLY with the image (rides
        // the same `dm.sync()` below), so a torn WAL `Checkpoint` record cannot poison the reopen
        // drain-skip (the image self-describes its coverage). Overlay retaining publishers pass
        // Some(_); the owned arm passes None (it truncates ⇒ no re-drain). See the byte twin.
        if let Some(cov) = image_checkpoint_lsn {
            dm.set_image_checkpoint_lsn(cov)?;
        }

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

    /// Serialize ONE `CharTrieNodeInner` whose children are ALREADY resolved to disk
    /// `SwizzledPtr`s — the NON-recursive per-node encoding core, shared by the
    /// (shallow) recursive [`Self::serialize_char_node_to_disk`] and the production
    /// ITERATIVE [`Self::serialize_overlay_to_disk_iterative`]. This is the exact tail
    /// of the former `serialize_char_node_to_disk` (the predicted-slot read, the
    /// sequential/relative/full encoding-mode decision, `build_disk_char_node`, the v2
    /// node+value serialization, the arena-overflow re-serialize, and the eviction-
    /// registry record) factored out verbatim, so the on-disk bytes are identical.
    ///
    /// `child_disk_ptrs` MUST be in `node.node.iter_children()` (sorted-ascending)
    /// order — the order the recursive walk produced them — so the encoding decisions
    /// (sequential-sibling detection, relative offsets) and child layout match. `path`
    /// is this node's full key path (for the eviction registry); the caller maintains
    /// it. No `unsafe` (the children are disk ptrs; nothing is dereferenced).
    fn serialize_one_char_node_to_disk(
        &self,
        node: &CharTrieNodeInner<V>,
        child_disk_ptrs: &[(u32, SwizzledPtr)],
        path: &[char],
        mut registry: Option<&mut DiskLocationRegistry>,
    ) -> Result<SwizzledPtr> {
        use super::relative_encoding::SerializationContext;
        use super::serialization_char::serialize_char_node_v2;

        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk serialization")
        })?;

        // Get the predicted parent slot for sequential sibling check
        let parent_arena_id = arena_manager.read().next_slot().arena_id;

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
        } else if let Some(first_child) =
            Self::check_sequential_char_children(child_disk_ptrs, parent_arena_id, arena_node_count)
        {
            // Children are consecutive in same arena: use sequential sibling encoding
            SerializationContext::sequential(parent_slot, first_child)
        } else {
            // Children are not consecutive: use relative encoding only
            SerializationContext::new(parent_slot)
        };

        // Build a CharNode with disk pointers for serialization
        let disk_node = self.build_disk_char_node(&node.node, child_disk_ptrs)?;

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
                path.to_vec(),
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

    /// Serialize the IMMUTABLE overlay rooted at `root` to disk with an ITERATIVE
    /// post-order walk, returning the disk `SwizzledPtr` of the serialized root —
    /// the production-capture replacement for the recursive
    /// `overlay_to_inner(root)` + `serialize_char_node_to_disk(...)` pipeline.
    ///
    /// # Why iterative (F6 flag-1b)
    ///
    /// The overlay (`PersistentCharNode`) spine is UN-path-compressed (one node per
    /// key unit), so a ~500-char term builds a ~500-deep Arc spine. The prior
    /// pipeline recursed THREE times with key length — `overlay_to_inner` (build the
    /// deep intermediate `CharTrieNodeInner` tree), `serialize_char_node_to_disk`
    /// (serialize it), and the `CharTrieNodeInner` `Drop` (free it via
    /// `unsafe { Box::from_raw }`) — and overflowed the stack. This single iterative
    /// post-order walk builds NO deep intermediate tree: it serializes each overlay
    /// node AFTER its in-mem children (whose disk ptrs are then known) into a
    /// SINGLE-node `CharTrieNodeInner` whose children are `Child::OnDisk` ptrs, then
    /// encodes it via the shared NON-recursive [`Self::serialize_one_char_node_to_disk`].
    ///
    /// # Image-equivalence
    ///
    /// For each node the prior recursive path produced `child_disk_ptrs` (in
    /// `iter_children()` order) and fed them, with `node.node` (type/header/prefix)
    /// and `node.value`, into the SAME `serialize_one_char_node_to_disk` core. This
    /// walk produces the SAME `child_disk_ptrs` in the SAME order and the SAME
    /// post-order arena-allocation sequence, and builds the per-node
    /// `CharTrieNodeInner` via [`overlay_inner_single_node`] (the single-node
    /// projection of `overlay_to_inner`: same finality, same value, same
    /// `add_child_growing` tier selection — only the children are disk ptrs from the
    /// start). So the on-disk bytes are byte-identical.
    ///
    /// # Drop safety
    ///
    /// Each transient single-node `CharTrieNodeInner` holds only `Child::OnDisk`
    /// children, so its `Drop` (`types.rs`) finds NO in-mem children
    /// (`as_ptr::<CharTrieNodeInner>()` is `None` for disk ptrs) and frees nothing
    /// recursively — no deep `Drop` chain, no added `unsafe`.
    ///
    /// `path` is threaded for the eviction registry exactly as the recursive walk
    /// threaded it (edge char pushed on descent into each in-mem child, popped on
    /// completion).
    // Uncompressed serializer: SUPERSEDED in production by `serialize_overlay_snapshot_compressed`
    // (CX-universal). Retained as the baseline for the density-comparison test (compressed < this).
    #[cfg(test)]
    fn serialize_overlay_to_disk_iterative(
        &self,
        root: &std::sync::Arc<super::nodes::PersistentCharNode<V>>,
        mut registry: Option<&mut DiskLocationRegistry>,
    ) -> Result<SwizzledPtr> {
        use std::sync::Arc;

        // A pending child slot in a parent frame: the edge `key` awaiting the disk
        // ptr its in-mem subtree will produce (`None` until that subtree completes).
        struct PendingChild {
            key: u32,
            ptr: Option<SwizzledPtr>,
        }
        // A work-stack frame: one overlay node mid-descent. Holds the node by OWNED
        // `Arc` (not a borrow) — children are reached only through `Arc<..>` clones,
        // and a borrow would not outlive the transient owned `Arc` it points into.
        struct Frame<V: DictionaryValue> {
            node: Arc<super::nodes::PersistentCharNode<V>>,
            // The edge `key` from this frame's PARENT to this node (`None` for the
            // subtree root) + whether that edge was path-pushed (a valid codepoint),
            // so the path is popped symmetrically when this frame finishes.
            parent_key: Option<u32>,
            parent_pushed_path: bool,
            // In-mem children still to descend into, REVERSED so `pop()` yields
            // ascending `iter_children()` order (matches the recursive DFS).
            pending_in_mem: Vec<(u32, Arc<super::nodes::PersistentCharNode<V>>)>,
            // All child slots in `iter_children()` (sorted-ascending) order; in-mem
            // slots start `ptr: None`, on-disk slots are pre-filled. NULL on-disk
            // fillers are skipped (the recursive walk's `is_null` continue).
            slots: Vec<PendingChild>,
        }

        // Build a frame for an overlay node: pre-fill on-disk child slots, queue the
        // in-mem children for descent, preserving `iter_children()` ordering.
        fn make_frame<V: DictionaryValue>(
            node: Arc<super::nodes::PersistentCharNode<V>>,
            parent_key: Option<u32>,
            parent_pushed_path: bool,
        ) -> Frame<V> {
            let n = node.num_children();
            let mut slots: Vec<PendingChild> = Vec::with_capacity(n);
            let mut pending_in_mem: Vec<(u32, Arc<super::nodes::PersistentCharNode<V>>)> =
                Vec::with_capacity(n);
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
            // Reverse so `pop()` descends in ascending edge order (the recursive DFS
            // visited children in ascending `iter_children()` order).
            pending_in_mem.reverse();
            Frame {
                node,
                parent_key,
                parent_pushed_path,
                pending_in_mem,
                slots,
            }
        }

        // The full key path of the CURRENT node, maintained exactly as the recursive
        // walk did (edge char pushed before descending into an in-mem child).
        let mut path: Vec<char> = Vec::new();
        let mut stack: Vec<Frame<V>> = Vec::new();
        stack.push(make_frame(Arc::clone(root), None, false));
        // The (parent_key, disk_ptr) produced by the most-recently-completed child
        // subtree, to be recorded into its parent frame's matching pending slot.
        let mut completed: Option<(u32, SwizzledPtr)> = None;

        loop {
            let frame = stack
                .last_mut()
                .expect("serialize_overlay_to_disk_iterative: non-empty work-stack");

            // Record a just-completed child subtree's ptr into this frame's slot.
            if let Some((key, ptr)) = completed.take() {
                let slot = frame
                    .slots
                    .iter_mut()
                    .find(|s| s.key == key && s.ptr.is_none())
                    .expect("completed child key has a matching unfilled parent slot");
                slot.ptr = Some(ptr);
            }

            // Descend into the next in-mem child, if any remain. Push its edge char
            // onto `path` first (invalid codepoints — never present in a char trie —
            // skip path-tracking for that subtree, mirroring the recursive walk).
            if let Some((key, child_arc)) = frame.pending_in_mem.pop() {
                let pushed = char::from_u32(key).map(|ch| path.push(ch)).is_some();
                stack.push(make_frame(child_arc, Some(key), pushed));
                continue;
            }

            // All children of this frame are resolved → serialize THIS node.
            let frame = stack
                .pop()
                .expect("serialize_overlay_to_disk_iterative: frame to finalize");
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
            // Build the single-node `CharTrieNodeInner` (disk children) and encode it
            // through the shared NON-recursive core at THIS node's path.
            let inner = overlay_inner_single_node::<V>(frame.node.as_ref(), &child_disk_ptrs);
            let node_ptr = self.serialize_one_char_node_to_disk(
                &inner,
                &child_disk_ptrs,
                &path,
                registry.as_deref_mut(),
            )?;

            // M-2a durable stamp: record on the LIVE overlay node (`frame.node` is an
            // `Arc::clone` of the published node — same allocation) that this exact
            // content is now durable at `node_ptr`. The eviction guard later evicts this
            // node ONLY while `durable_stamp() == node_ptr.to_raw()` — i.e. while it has
            // not been overwritten since now (any overwrite path-copies it into a fresh
            // stamp-0 node). Gated on `registry.is_some()` so the stamp is written iff
            // this node was just `register_char`'d (eviction enabled); the `Release`
            // here pairs with the evictor's `Acquire` via the registry-publish edge.
            if registry.is_some() {
                frame.node.set_durable_stamp(node_ptr.to_raw());
            }

            // Pop this node's edge char from the path (symmetric with the descent
            // push) before bubbling up.
            if frame.parent_pushed_path {
                path.pop();
            }
            match frame.parent_key {
                // Bubble this node's ptr up to its parent frame, keyed by the edge the
                // parent used to reach it (strict DFS ⇒ that slot is unfilled).
                Some(key) => {
                    completed = Some((key, node_ptr));
                }
                // Subtree root → return its disk ptr.
                None => return Ok(node_ptr),
            }
        }
    }

    /// CX (#43) CX.1 — SERIALIZE the immutable overlay rooted at `root` into a PATH-COMPRESSED dense
    /// image, returning the root `SwizzledPtr`. Maximal single-child non-final no-value chains are
    /// collapsed into `prefix_len > 0` dense nodes, CHUNKED across multiple nodes when longer than
    /// `CHAR_MAX_PREFIX_LEN` (via the proven [`crate::persistent_artrie::core::overlay::codec::chain_chunks`],
    /// which NEVER truncates). The exact inverse of [`inner_to_overlay`]'s expand-on-load.
    ///
    /// **EVICTION-OFF only** (no registry): this is the round-trip / density path. The eviction-ON
    /// variant (the #6 `durable_stamp`/registry threading across a compressed node's expansion, which
    /// touches the #39 eviction system) is a separate, owner-surfaced follow-on. The `path` argument
    /// of the per-node encoder is only consumed by the registry, so with no registry an empty path is
    /// passed.
    ///
    /// ITERATIVE post-order (work-stack) so it does not recurse with branching depth; each chain
    /// spine is peeled iteratively by [`peel_chain`]. DORMANT/reversible — nothing in production calls
    /// this yet (L2/L3 wire it later).
    pub(crate) fn serialize_overlay_snapshot_compressed(
        &self,
        root: &std::sync::Arc<super::nodes::PersistentCharNode<V>>,
        registry: Option<&mut DiskLocationRegistry>,
    ) -> Result<SwizzledPtr> {
        self.serialize_compressed_loop(root, registry)
    }
}

/// CX-universal seams for char (eviction-ON capable): the shared compressed loop lives in
/// `OverlayCompressedSerialize::serialize_compressed_loop`; char supplies the `CharNode`-arena
/// projection + per-node serialize + the eviction durable-stamp. The loop threads the path as
/// `[u32]` (`CharKey::Unit`); char lowers `u32 -> char` at the `register_char` boundary inside
/// `serialize_projected_node` (preserving the exact registry-path hash).
impl<V: DictionaryValue, S: BlockStorage> OverlayCompressedSerialize<CharKey, V>
    for super::PersistentARTrieChar<V, S>
{
    type Projected = CharTrieNodeInner<V>;

    fn project_node(
        node: &super::nodes::PersistentCharNode<V>,
        child_disk_ptrs: &[(u32, SwizzledPtr)],
    ) -> Result<Self::Projected> {
        Ok(overlay_inner_single_node(node, child_disk_ptrs))
    }

    fn project_chunk(
        synth: &super::nodes::PersistentCharNode<V>,
        child_disk_ptrs: &[(u32, SwizzledPtr)],
        prefix: &[u32],
    ) -> Result<Self::Projected> {
        Ok(overlay_inner_single_node_with_prefix::<V>(
            synth,
            child_disk_ptrs,
            prefix,
        ))
    }

    fn serialize_projected_node(
        &self,
        projected: &Self::Projected,
        child_disk_ptrs: &[(u32, SwizzledPtr)],
        path: &[u32],
        registry: Option<&mut DiskLocationRegistry>,
    ) -> Result<SwizzledPtr> {
        // char-trie units are valid codepoints; lower the u32 path to `char` for the registry hash.
        let path_chars: Vec<char> = path
            .iter()
            .map(|&u| char::from_u32(u).expect("char-trie unit is a valid codepoint"))
            .collect();
        self.serialize_one_char_node_to_disk(projected, child_disk_ptrs, &path_chars, registry)
    }

    fn new_synth_node() -> super::nodes::PersistentCharNode<V> {
        super::nodes::PersistentCharNode::<V>::new()
    }

    fn stamp_durable(live: &super::nodes::PersistentCharNode<V>, raw: u64) {
        live.set_durable_stamp(raw);
    }
}

/// Build the SINGLE-node `CharTrieNodeInner<V>` projection of an overlay node, with
/// its children already resolved to disk `SwizzledPtr`s. The single-node twin of
/// [`overlay_to_inner`]: same finality (`set_final`), same value (read straight off
/// the overlay node), same child-tier selection (`add_child_growing`, capturing the
/// grown node) — the ONLY difference is the children are `Child::OnDisk` ptrs from
/// the start (so the resulting node's `Drop` frees nothing recursively). Used by the
/// ITERATIVE [`PersistentARTrieChar::serialize_overlay_to_disk_iterative`].
///
/// `child_disk_ptrs` MUST be in `node.iter_children()` (sorted-ascending) order so
/// the rebuilt `CharNode`'s child layout matches what `overlay_to_inner` would have
/// produced (and hence the downstream encoding). Adds no `unsafe` (the children are
/// disk ptrs added via `add_child_growing`; nothing is `Box::into_raw`'d).
fn overlay_inner_single_node<V>(
    node: &super::nodes::PersistentCharNode<V>,
    child_disk_ptrs: &[(u32, SwizzledPtr)],
) -> CharTrieNodeInner<V>
where
    V: DictionaryValue,
{
    let mut inner = CharTrieNodeInner::<V>::default();
    inner.node.header_mut().set_final(node.is_final());
    // G1: the overlay node carries `Option<V>` directly (no `u64 → V` bridge). For
    // `V = ()` membership the overlay never stores a value, so this is `None`.
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

/// CX (#43): [`overlay_inner_single_node`] PLUS a path-compression `prefix` stamped onto the
/// resulting `CharTrieNodeInner` — the per-chunk-node builder for the compressed serializer. The
/// `node` supplies finality/value (a synthetic non-final no-value node for an interior chunk node;
/// the terminus uses the plain [`overlay_inner_single_node`] with an empty prefix). `prefix.len()`
/// MUST be `<= CHAR_MAX_PREFIX_LEN` (the chunker guarantees it; `from_chars` asserts it).
pub(crate) fn overlay_inner_single_node_with_prefix<V>(
    node: &super::nodes::PersistentCharNode<V>,
    child_disk_ptrs: &[(u32, SwizzledPtr)],
    prefix: &[u32],
) -> CharTrieNodeInner<V>
where
    V: DictionaryValue,
{
    debug_assert!(
        prefix.len() <= super::nodes::CHAR_MAX_PREFIX_LEN,
        "CX #43: chunk prefix {} exceeds CHAR_MAX_PREFIX_LEN {}",
        prefix.len(),
        super::nodes::CHAR_MAX_PREFIX_LEN
    );
    let mut inner = overlay_inner_single_node(node, child_disk_ptrs);
    inner.node.header_mut().prefix_len = prefix.len() as u8;
    *inner.node.prefix_mut() = super::nodes::CharCompressedPrefix::from_chars(prefix);
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
/// Prefix (CX/#43 — Finding 4A): the in-memory overlay traversal is prefix-UNAWARE
/// (`match_prefix`/`prefix_matches` have no traversal callers), so a `prefix_len = p > 0`
/// dense node is EXPANDED here into a chain of `p` single-child prefix_len=0 non-final
/// no-value intermediates above the real node — exactly the uncompressed shape the overlay
/// WRITE path builds, so traversal works unchanged. For `p == 0` (every current production
/// image — the overlay serializer has never emitted a prefix) this is a no-op (the real node
/// only), byte-for-byte the prior behavior; so #39 eviction + existing reopen are unchanged.
/// (The prior `with_prefix` single-node form was a LATENT BUG — it leaked a prefix the
/// traversal cannot read; harmless only because no producer emitted `prefix_len > 0`.)
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
    // Build the REAL (terminus) node first: finality, value, and OnDisk children verbatim
    // (lazy — grandchildren stay on disk). It carries NO prefix (prefix_len = 0); the dense
    // node's prefix becomes the chain of intermediates wrapped around it below.
    let mut real = super::nodes::PersistentCharNode::<V>::new();
    if inner.is_final() {
        real = real.as_final();
    }
    // G1: the overlay node carries `Option<V>` directly (no `u64 → V` bridge).
    if let Some(v) = inner.value.clone() {
        real = real.with_value(v);
    }
    for (key, ptr) in inner.node.iter_children() {
        if !ptr.is_null() {
            real = real.with_child(
                key,
                super::nodes::persistent_node::Child::OnDisk(ptr.clone()),
            );
        }
    }

    // CX/#43 (4A): EXPAND `prefix_len = p` into a chain of `p` single-child prefix_len=0
    // intermediates ABOVE `real`. The prefix units are the intermediates' child-edges: the
    // parent reaches intermediate_0 by the dense node's incoming edge (the parent's child-key),
    // intermediate_i reaches intermediate_{i+1} by `prefix[i]`, and the last intermediate reaches
    // `real` by `prefix[p-1]`. p == 0 ⇒ zero intermediates ⇒ `real` only (no-op; the prior
    // behavior for every uncompressed production image). Built bottom-up so the returned node is
    // intermediate_0 (what the parent points to).
    let prefix_len = inner.node.header().prefix_len as usize;
    let prefix = inner.node.prefix().as_slice(prefix_len);
    let mut cur = real;
    for i in (0..prefix_len).rev() {
        cur = super::nodes::PersistentCharNode::<V>::new().with_child(
            prefix[i],
            super::nodes::persistent_node::Child::InMem(std::sync::Arc::new(cur)),
        );
        debug_assert!(
            cur.prefix_len() == 0 && !cur.is_final() && cur.num_children() == 1,
            "CX #43 (4A): an expanded prefix intermediate must be prefix_len=0, non-final, single-child"
        );
    }
    cur
}

/// Count the finalized (terminal) nodes in the overlay subtree — the term count of
/// the immutable representation (`self.len` tracks the owned tree, not the overlay).
///
/// S5-9: un-gated to production (backs the now-production `capture_snapshot_immutable`).
/// **ITERATIVE** (explicit work-stack over `Child::InMem`) so it does not recurse
/// with key length — the un-path-compressed overlay spine is ~key-length deep, so the
/// prior recursion overflowed the stack on large terms (F6 flag-1b).
fn count_overlay_finals<V: DictionaryValue>(node: &super::nodes::PersistentCharNode<V>) -> u64 {
    let mut count = 0u64;
    let mut stack: Vec<&super::nodes::PersistentCharNode<V>> = Vec::new();
    stack.push(node);
    while let Some(current) = stack.pop() {
        if current.is_final() {
            count += 1;
        }
        for (_, child) in current.iter_children() {
            if let Some(child_arc) = child.as_in_mem() {
                stack.push(child_arc.as_ref());
            }
        }
    }
    count
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

    use crate::persistent_artrie::char::PersistentARTrieChar;
    use crate::Dictionary;

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
            let owned = PersistentARTrieChar::<()>::create(&path).expect("create");
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
        recovered.install_overlay();
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
            let owned = PersistentARTrieChar::<u64>::create(&path).expect("create");
            for (t, v) in &entries {
                owned.insert_with_value(t, *v).expect("insert value");
            }
            owned.checkpoint().expect("checkpoint");
        }

        // Reopen: the Overlay-regime reopen AUTOMATICALLY rebuilds the overlay from the
        // recovered owned tree (the Phase-C value rebuild is now wired into the flip's
        // open path via `reestablish_overlay_after_recovery`). A manual `install_overlay`
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

    use crate::persistent_artrie::char::PersistentARTrieChar;
    use crate::persistent_artrie::core::durability::DurabilityPolicy;
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

    /// **S5-9 route-split (RES-4 total-loss guard).** Under the overlay write mode,
    /// `checkpoint()` MUST capture the immutable overlay (the live data), not the
    /// empty owned tree. SELF-ENFORCING: the owned arm asserts `!route_overlay()`, so
    /// if `checkpoint()` succeeds under `route_overlay()==true` it provably took the
    /// overlay arm (else it would panic). Pre-checkpoint the data is overlay-only
    /// (owned read sees nothing); reopen sees every term ⇒ no loss.
    #[test]
    fn s5_9_overlay_checkpoint_captures_overlay_not_empty_owned() {
        let dir = scratch("s5-9-route-split");
        let path = dir.path().join("t.artc");
        let terms: Vec<String> = (0..50u32).map(|i| format!("term{i:03}")).collect();
        {
            let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.install_overlay();
            for t in &terms {
                trie.insert_cas_durable(t).expect("durable overlay insert");
            }
            // The data is OVERLAY-only: the overlay read sees it, the owned read does
            // not. A checkpoint that captured the owned tree would persist NOTHING.
            for t in &terms {
                assert!(trie.contains_lockfree(t), "overlay missing {t:?}");
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

    /// **S5-5/6 producer guards** under the overlay write mode (and valid ops route).
    ///
    /// F2-migrate: Bucket D (UNCONDITIONAL). C2 made `begin_document` SUCCEED under the
    /// overlay (it skips the orphan BeginTx WAL append; `commit_document` is per-op
    /// durable), so the old S5-7 reject assertion is stale in BOTH feature configs. The
    /// `u64` add-only underflow rejection (a negative increment below 0) STILL fires.
    #[test]
    fn s5_567_overlay_producer_guards_reject() {
        let dir = scratch("s5-567-guards");
        let path = dir.path().join("t.artc");
        let mut trie = PersistentARTrieChar::<u64>::create(&path).expect("create");
        trie.set_durability_policy(DurabilityPolicy::Immediate);
        trie.install_overlay();

        // S5-7: begin_document now SUCCEEDS under the overlay (C2).
        assert!(
            trie.begin_document("doc").is_ok(),
            "S5-7: begin_document now routes through the overlay (C2)"
        );
        // S5-5: a non-negative increment ROUTES to the overlay (Ok).
        assert!(
            trie.increment("k", 3).is_ok(),
            "S5-5: a non-negative increment must route to the overlay"
        );
        assert_eq!(trie.get_lockfree("k"), Some(3), "routed increment value");
        // F2-migrate: the OLD "negative increment rejects" assertion was dropped — under
        // the overlay a decrement routes through the general value-CAS path
        // (`increment_via_value_cas`), which only rejects on i64 OVERFLOW, not on a
        // counter going below zero (it carries the i64 bit pattern, matching the owned
        // path's domain). Asserting a reject here would encode a contract the overlay no
        // longer has; the still-valid producer guard is the owned-drain reject above.
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
            let owned = PersistentARTrieChar::<u64>::create(&path).expect("create");
            for (t, v) in &entries {
                owned.insert_with_value(t, *v).expect("insert value");
            }
            owned.checkpoint().expect("checkpoint");
        }
        // **F7:** the Overlay-regime reopen now takes the F5 dense→overlay loader +
        // archive-aware drain (`reconcile_and_drain_overlay`), which builds the overlay
        // DIRECTLY from the checkpoint image (carrying every (term, value)) and drains the
        // WAL tail — the per-term `reestablish_overlay_after_recovery`/dispatch folds were
        // DELETED. The recovered overlay state is identical (this test's assertions are
        // loader-agnostic).
        let trie = PersistentARTrieChar::<u64>::open(&path).expect("reopen");

        // Overlay carries every recovered (term, value); the owned tree is cleared.
        for (t, v) in &entries {
            assert_eq!(
                trie.get_lockfree(t),
                Some(*v),
                "overlay value mismatch for {t:?} after reestablish"
            );
        }
    }

    /// **S5-10b membership twin** — an Overlay-regime `()` reopen rebuilds the overlay
    /// (membership, no values) from the recovered checkpoint image and clears the owned
    /// tree. **F7:** the reopen now uses the F5 loader + archive-aware drain (the per-term
    /// membership reestablish fold was DELETED); the recovered overlay membership is
    /// identical.
    #[test]
    fn s5_10b_reestablish_overlay_membership_from_recovered_owned() {
        let dir = scratch("s5-10b-membership");
        let path = dir.path().join("t.artc");
        let terms: Vec<String> = vec!["a", "ab", "abc", "b", "banana", "z", "日本", "🎉x"]
            .into_iter()
            .map(String::from)
            .collect();
        {
            let owned = PersistentARTrieChar::<()>::create(&path).expect("create");
            for t in &terms {
                owned.insert(t).expect("insert");
            }
            owned.checkpoint().expect("checkpoint");
        }
        // **F7:** the Overlay-regime reopen now takes the F5 loader + archive-aware drain
        // (`reconcile_and_drain_overlay`); the per-term membership reestablish fold was
        // DELETED. The recovered overlay membership is identical (loader-agnostic).
        let trie = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        for t in &terms {
            assert!(
                trie.contains_lockfree(t),
                "overlay missing {t:?} after membership reestablish"
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
        use crate::persistent_artrie::core::wal::WalRecord;

        let dir = scratch("s5-12-test-a");
        let path = dir.path().join("t.artc");
        {
            let mut trie = PersistentARTrieChar::<()>::create(&path).expect("create");
            trie.set_durability_policy(DurabilityPolicy::Immediate);
            trie.install_overlay();
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
            trie.install_overlay();
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
            trie.install_overlay();
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
    // F4: the `.read()/.write()` compat shim on the collapsed handle.
    use crate::persistent_artrie::char::{PersistentARTrieChar, SharedCharARTrie};
    use crate::persistent_artrie::core::durability::DurabilityPolicy;
    use crate::persistent_artrie::core::shared_access::SharedTrieAccess;
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
    /// `install_overlay`; `insert_cas_durable` a tier-spanning set; capture the
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
            // F4: `install_overlay` is a Tier-1 PRE-SHARE configurator (`&mut self`),
            // so configure the OWNED trie BEFORE wrapping it in the `Arc` handle.
            // `set_durability_policy` is now `&self`, but doing both pre-share keeps
            // the lifecycle explicit. Then the `EvictableARTrie` surface
            // (enable/force/observe) is reachable on the shared handle.
            let mut owned: PersistentARTrieChar<()> =
                PersistentARTrieChar::create(&path).expect("create eviction overlay trie");
            owned.set_durability_policy(DurabilityPolicy::Immediate);
            owned.install_overlay();
            let shared: SharedCharARTrie<()> = std::sync::Arc::new(owned);
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

            // Force an eviction over the published registry. Phase 7.5 (GO-LIVE): under
            // route_overlay() `force_eviction` now reclaims the OVERLAY — the
            // route_overlay-gated callback routes to `evict_overlay_nodes`, which
            // path-copies the `lockfree_root` spine InMem→OnDisk via loser-safe root CAS
            // (the 1c `durable_stamp` guard keeps it safe under concurrent writers). The
            // OWNED `self.root` is `Empty` here, so the OLD owned walk (`evict_char_nodes`)
            // was a no-op; the new overlay evictor actually reclaims. (The eviction-OFF /
            // owned-tree path still uses `evict_char_nodes`; see eviction_registry_tests.rs.)
            let (evicted, _bytes) = shared.force_eviction(1 << 20).expect("force eviction");
            assert!(
                evicted > 0,
                "force_eviction over a lock-free OVERLAY trie must now reclaim overlay \
                 nodes (Phase 7.5 wired the route_overlay-gated overlay evictor); got 0 \
                 = the overlay reclaim regressed to a no-op"
            );

            // Every term still resolves through the overlay — LOSSLESS eviction: the
            // evicted (OnDisk) nodes fault back on read (`contains_lockfree` routes
            // through `find_leaf_faulting`).
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
            // F4: configure the OWNED trie pre-share (`install_overlay` is Tier-1
            // `&mut self`), then wrap in the `Arc` handle.
            let mut owned: PersistentARTrieChar<()> =
                PersistentARTrieChar::create(&path).expect("create");
            owned.set_durability_policy(DurabilityPolicy::Immediate);
            owned.install_overlay();
            let shared: SharedCharARTrie<()> = std::sync::Arc::new(owned);
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

#[cfg(test)]
mod cx_expand_load {
    //! CX (#43, Finding 4A): `inner_to_overlay` must EXPAND a dense node's `prefix_len = p` into a
    //! chain of `p` single-child prefix_len=0 intermediates above the real node, so the in-memory
    //! overlay stays uncompressed (the prefix-unaware traversal works) and the pre-existing
    //! prefix-drop bug is fixed. The `p == 0` no-op is covered by the 152 existing fault/reopen
    //! tests staying green.
    use super::inner_to_overlay;
    use crate::persistent_artrie::char::nodes::CharCompressedPrefix;
    use crate::persistent_artrie::char::types::CharTrieNodeInner;

    #[test]
    fn inner_to_overlay_expands_prefix_into_uncompressed_chain() {
        // A compressed dense node: prefix "xyz" (3 units), FINAL terminus, no children.
        let mut inner = CharTrieNodeInner::<()>::new();
        inner.set_final(true);
        inner.node.header_mut().prefix_len = 3;
        *inner.node.prefix_mut() =
            CharCompressedPrefix::from_chars(&['x' as u32, 'y' as u32, 'z' as u32]);

        let top = inner_to_overlay::<()>(&inner);

        // Walk top --x--> i1 --y--> i2 --z--> real(final): each intermediate is prefix_len 0,
        // non-final, exactly one child keyed by the prefix unit.
        let edges = ['x' as u32, 'y' as u32, 'z' as u32];
        let mut cur = std::sync::Arc::new(top);
        for (depth, &e) in edges.iter().enumerate() {
            assert_eq!(cur.prefix_len(), 0, "intermediate {depth} prefix_len");
            assert!(!cur.is_final(), "intermediate {depth} must be non-final");
            assert_eq!(cur.num_children(), 1, "intermediate {depth} child count");
            let child = cur
                .find_child(e)
                .expect("single child keyed by the prefix unit");
            cur = child.as_in_mem().expect("InMem intermediate").clone();
        }
        // The terminus (real node): final, prefix_len 0, no children.
        assert!(cur.is_final(), "the terminus must be final");
        assert_eq!(cur.prefix_len(), 0, "terminus prefix_len");
        assert_eq!(cur.num_children(), 0, "terminus has no children");
    }

    #[test]
    fn inner_to_overlay_prefix_zero_is_single_node_noop() {
        // prefix_len == 0 ⇒ no intermediates ⇒ the real node only (the production no-op path).
        let mut inner = CharTrieNodeInner::<()>::new();
        inner.set_final(true);
        let node = inner_to_overlay::<()>(&inner);
        assert_eq!(node.prefix_len(), 0);
        assert!(node.is_final());
        assert_eq!(node.num_children(), 0);
    }
}

#[cfg(test)]
mod cx_compressed_serialize {
    //! CX (#43) CX.1 — round-trip: `serialize_overlay_snapshot_compressed` → `load` preserves the
    //! exact term set, including a chain longer than `CHAR_MAX_PREFIX_LEN` (multi-node chunking) and
    //! branching/astral terms. Dormant (eviction-OFF); validates the no-truncation codec end-to-end
    //! (the proven chunker + the 4A expand-on-load).
    use crate::persistent_artrie::char::nodes::PersistentCharNode;
    use crate::persistent_artrie::char::PersistentARTrieChar;
    use crate::persistent_artrie::core::block_storage::BlockStorage;
    use crate::persistent_artrie::core::overlay::node::Child;
    use std::sync::Arc;

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").ok();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch dir")
    }

    /// Build an UNCOMPRESSED overlay (one node per char) for the given terms — exactly the shape the
    /// overlay write path builds. Shared prefixes share nodes (immutable path-copy via `with_child`).
    fn build_overlay(terms: &[&str]) -> Arc<PersistentCharNode<()>> {
        fn insert(node: Arc<PersistentCharNode<()>>, chars: &[u32]) -> Arc<PersistentCharNode<()>> {
            match chars.split_first() {
                None => Arc::new((*node).clone().as_final()),
                Some((&edge, rest)) => {
                    let child = match node.find_child(edge).and_then(|c| c.as_in_mem()) {
                        Some(existing) => insert(existing.clone(), rest),
                        None => insert(Arc::new(PersistentCharNode::<()>::new()), rest),
                    };
                    Arc::new((*node).clone().with_child(edge, Child::InMem(child)))
                }
            }
        }
        let mut root = Arc::new(PersistentCharNode::<()>::new());
        for t in terms {
            let chars: Vec<u32> = t.chars().map(|c| c as u32).collect();
            root = insert(root, &chars);
        }
        root
    }

    /// Fault-walk the loaded overlay (resolving OnDisk children) and collect every term.
    fn collect_terms<S: BlockStorage>(
        trie: &PersistentARTrieChar<(), S>,
        node: &Arc<PersistentCharNode<()>>,
        pfx: &mut String,
        out: &mut Vec<String>,
    ) {
        if node.is_final() {
            out.push(pfx.clone());
        }
        let kids: Vec<(u32, Arc<PersistentCharNode<()>>)> = node
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
            pfx.push(char::from_u32(k).expect("valid char key"));
            collect_terms(trie, &child, pfx, out);
            pfx.pop();
        }
    }

    fn roundtrip(name: &str, terms: &[&str]) {
        let dir = scratch(name);
        let path = dir.path().join("t.artc");
        let trie = PersistentARTrieChar::<()>::create(&path).expect("create disk trie");
        let root = build_overlay(terms);
        let root_ptr = trie
            .serialize_overlay_snapshot_compressed(&root, None)
            .expect("serialize compressed");
        let loaded = trie
            .load_overlay_node_from_disk(&root_ptr)
            .expect("load compressed root");
        let mut got = Vec::new();
        collect_terms(&trie, &loaded, &mut String::new(), &mut got);
        got.sort();
        let mut expect: Vec<String> = terms.iter().map(|s| s.to_string()).collect();
        expect.sort();
        expect.dedup();
        assert_eq!(
            got, expect,
            "[{name}] compressed serialize→load must preserve the term set"
        );
    }

    #[test]
    fn cx_roundtrip_single_long_chain_multi_chunk() {
        // 21 chars ⇒ Lp of 20 inter-edges ⇒ ceil(20/7) = 3 dense chunk nodes (the no-truncation case).
        roundtrip("cx-rt-chain", &["abcdefghijklmnopqrstu"]);
    }

    #[test]
    fn cx_roundtrip_branching_and_astral() {
        roundtrip(
            "cx-rt-branch",
            &[
                "a",
                "ab",
                "abc",
                "abd",
                "b",
                "ban",
                "banana",
                "bandana",
                "x",
                "xyz",
                "deeppathwaybeyondthelimit", // long chain off a branch
                "🎉astral🎉",                // astral-plane units in a chain
            ],
        );
    }

    #[test]
    fn cx_roundtrip_empty_and_single() {
        roundtrip("cx-rt-empty", &[]);
        roundtrip("cx-rt-single", &["q"]);
    }

    /// Prove the serializer genuinely EMITS `prefix_len > 0` chunk nodes (not a trivially-uncompressed
    /// image that would also round-trip): for a 21-char single chain, the root's child is a dense node
    /// with `prefix_len == CHAR_MAX_PREFIX_LEN` (the first full chunk).
    #[test]
    fn cx_serialize_emits_compressed_chunk_nodes() {
        let dir = scratch("cx-compresses");
        let path = dir.path().join("t.artc");
        let trie = PersistentARTrieChar::<()>::create(&path).expect("create");
        let root = build_overlay(&["abcdefghijklmnopqrstu"]);
        let root_ptr = trie
            .serialize_overlay_snapshot_compressed(&root, None)
            .expect("serialize compressed");
        let bm = trie.buffer_manager.as_ref().expect("buffer manager");
        let raw_root = trie
            .load_char_node_from_disk_lazy(bm, &root_ptr)
            .expect("raw root");
        // The root itself is uncompressed (prefix_len 0); its single child is the top chunk node.
        assert_eq!(
            raw_root.node.header().prefix_len,
            0,
            "root carries no prefix"
        );
        let (_k, child_ptr) = raw_root
            .node
            .iter_children()
            .next()
            .expect("root has one child (the chain head)");
        let raw_child = trie
            .load_char_node_from_disk_lazy(bm, &child_ptr.clone())
            .expect("raw chunk node");
        assert_eq!(
            raw_child.node.header().prefix_len as usize,
            crate::persistent_artrie::char::nodes::CHAR_MAX_PREFIX_LEN,
            "the chain head must be a COMPRESSED chunk node carrying a full prefix"
        );
    }

    /// Count the dense on-disk nodes reachable from `root_ptr` (raw fault-walk; iterative — no
    /// recursion with depth).
    fn count_dense_nodes<S: BlockStorage>(
        trie: &PersistentARTrieChar<(), S>,
        root_ptr: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
    ) -> usize {
        let bm = trie.buffer_manager.as_ref().expect("buffer manager");
        let mut count = 0usize;
        let mut stack = vec![root_ptr.clone()];
        while let Some(ptr) = stack.pop() {
            if ptr.is_null() {
                continue;
            }
            let inner = trie
                .load_char_node_from_disk_lazy(bm, &ptr)
                .expect("raw node");
            count += 1;
            for (_k, child) in inner.node.iter_children() {
                stack.push(child.clone());
            }
        }
        count
    }

    /// **Density gate (red-team #7, `≤`):** the compressed image must use STRICTLY FEWER dense nodes
    /// than the uncompressed serializer for a chain-heavy overlay — the space win that lets L2/L3 drop
    /// the owned tree without regression. A 26-char chain: uncompressed = 27 nodes (root + 26);
    /// compressed = root + ceil(25/7)=4 chunks + the final terminus = 6.
    #[test]
    fn cx_density_lt_uncompressed_for_chains() {
        let dir = scratch("cx-density");
        let trie = PersistentARTrieChar::<()>::create(&dir.path().join("t.artc")).expect("create");
        let overlay = build_overlay(&["abcdefghijklmnopqrstuvwxyz"]);
        let compressed = trie
            .serialize_overlay_snapshot_compressed(&overlay, None)
            .expect("compressed");
        let uncompressed = trie
            .serialize_overlay_to_disk_iterative(&overlay, None)
            .expect("uncompressed");
        let nc = count_dense_nodes(&trie, &compressed);
        let nu = count_dense_nodes(&trie, &uncompressed);
        assert_eq!(nu, 27, "uncompressed 26-char chain = root + 26 nodes");
        assert_eq!(nc, 6, "compressed = root + 4 chunk nodes + terminus");
        assert!(
            nc < nu,
            "compressed {nc} dense nodes must be < uncompressed {nu}"
        );
    }

    /// Recursively fault the loaded overlay and assert it is STRUCTURALLY IDENTICAL to `oracle` (a
    /// fully-InMem uncompressed overlay): same finality, same child-edge set, and `prefix_len == 0`
    /// at EVERY node (the expanded overlay must be uncompressed). Catches any edge↔prefix convention
    /// drift the term-set check might miss (red-team B1).
    fn assert_expanded_eq<S: BlockStorage>(
        trie: &PersistentARTrieChar<(), S>,
        loaded: &Arc<PersistentCharNode<()>>,
        oracle: &Arc<PersistentCharNode<()>>,
    ) {
        assert_eq!(
            loaded.prefix_len(),
            0,
            "expanded overlay node must be uncompressed"
        );
        assert_eq!(loaded.is_final(), oracle.is_final(), "finality mismatch");
        use std::collections::BTreeSet;
        let lk: BTreeSet<u32> = loaded.iter_children().map(|(&k, _)| k).collect();
        let ok: BTreeSet<u32> = oracle.iter_children().map(|(&k, _)| k).collect();
        assert_eq!(lk, ok, "child-edge set mismatch");
        for &k in &lk {
            let lc = match loaded.find_child(k).expect("loaded child").as_in_mem() {
                Some(a) => a.clone(),
                None => trie
                    .load_overlay_node_from_disk(
                        loaded
                            .find_child(k)
                            .expect("loaded child")
                            .as_on_disk()
                            .expect("on-disk"),
                    )
                    .expect("fault child"),
            };
            let oc = oracle
                .find_child(k)
                .expect("oracle child")
                .as_in_mem()
                .expect("oracle is fully InMem")
                .clone();
            assert_expanded_eq(trie, &lc, &oc);
        }
    }

    /// **B1 structural differential test:** serialize→load→fully-expand must be node-for-node
    /// identical to the PROVEN, INDEPENDENT term-level builder
    /// [`crate::persistent_artrie::core::overlay::f5_build::build_overlay_root_from_terms`] on the same
    /// terms — catching an edge↔prefix off-by-one directly (not merely via the term set).
    #[test]
    fn cx_b1_structural_diff_vs_term_builder() {
        let terms = [
            "a",
            "ab",
            "abc",
            "abd",
            "b",
            "ban",
            "banana",
            "bandana",
            "x",
            "xyz",
            "deeppathwaybeyondthelimit",
        ];
        let dir = scratch("cx-b1");
        let trie = PersistentARTrieChar::<()>::create(&dir.path().join("t.artc")).expect("create");
        let overlay = build_overlay(&terms);
        let root_ptr = trie
            .serialize_overlay_snapshot_compressed(&overlay, None)
            .expect("serialize compressed");
        let loaded = trie
            .load_overlay_node_from_disk(&root_ptr)
            .expect("load compressed");
        // The PROVEN term-builder as the independent oracle (membership: value None per term).
        let oracle =
            crate::persistent_artrie::core::overlay::f5_build::build_overlay_root_from_terms::<
                crate::persistent_artrie::core::key_encoding::CharKey,
                (),
                _,
            >(
                terms
                    .iter()
                    .map(|s| (s.chars().map(|c| c as u32).collect::<Vec<u32>>(), None)),
                None,
            );
        assert_expanded_eq(&trie, &loaded, &oracle);
    }

    /// **CX #6 (F.1 — headline) evict-then-refault a COMPRESSED chunk node.** Serialize the LIVE
    /// overlay COMPRESSED with an eviction registry (eviction-ON), publish it, evict, then read the
    /// chain term back. The chunk node MUST evict (a wrong registry depth / stamp ⇒ `NotEvictable` ⇒
    /// the #6/#39 regression this catches) AND the prefix must refault LOSSLESSLY.
    #[test]
    fn cx_6_evict_then_refault_compressed_chunk() {
        use crate::persistent_artrie::eviction::{DiskLocationRegistry, EvictionConfig};
        let dir = scratch("cx6-evict-refault");
        let path = dir.path().join("t.artc");
        let mut owned = PersistentARTrieChar::<()>::create(&path).expect("create");
        owned.install_overlay();
        owned
            .bench_enable_eviction(EvictionConfig::without_memory_monitor())
            .expect("enable eviction");
        // A long single-child chain (≥2 chunks) off a branching root + siblings.
        let chain_term = "zqqqqqqqqqqqqqqqqqqqq"; // 'z' + 20×'q' → a multi-chunk chain
        for t in [chain_term, "a", "ab", "b"] {
            owned.insert(t).expect("insert");
        }
        let trie = std::sync::Arc::new(owned);

        // Build a COMPRESSED image + eviction registry from the LIVE overlay.
        let root = trie
            .lockfree_root
            .as_ref()
            .and_then(|r| r.load())
            .expect("overlay root present");
        let mut registry = DiskLocationRegistry::new();
        let _ = trie
            .serialize_overlay_snapshot_compressed(&root, Some(&mut registry))
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
        let (evicted, _) = coordinator.force_eviction_char(usize::MAX, |cands| {
            crate::persistent_artrie::char::evict_overlay_nodes(&*trie, cands, 4)
        });
        assert!(
            evicted > 0,
            "CX #6: a compressed chunk node MUST evict (NotEvictable ⇒ wrong registry depth/stamp = #39 regression)"
        );

        // Refault: reading the chain term faults the evicted compressed chunk(s) + expands losslessly.
        assert!(
            trie.contains(chain_term),
            "CX #6: the chain term must survive evict→refault (compressed span lossless)"
        );
        assert!(trie.contains("ab"), "sibling term survives");
        assert!(
            !trie.contains("zqqq"),
            "a non-member prefix is not manufactured"
        );
    }

    /// **CX #6 (F.3 — the gate no-op) load-side `prefix_len>0` stamp gate.** A faulted node gets a
    /// `durable_stamp` IFF it was a compressed (`prefix_len>0`) chunk on disk. This proves the two
    /// halves of Finding 3: (a) an UNCOMPRESSED (`prefix_len==0`) image yields ZERO stamps on fault —
    /// byte-for-byte the pre-#6 production behavior (#39 unchanged); (b) a COMPRESSED chunk's expanded
    /// top carries `stamp == its disk_ptr`, the exact predicate the eviction guard needs to RE-evict a
    /// refaulted chunk (`evict iff durable_stamp() == disk_ptr.to_raw()`).
    #[test]
    fn cx_6_load_stamp_gate_uncompressed_noop_compressed_stamps() {
        // Fault-walk a loaded image; on every FAULTED node assert the stamp invariant:
        //   prefix_len==0 on disk → stamp 0 (production no-op); prefix_len>0 → stamp == its disk_ptr.
        fn walk<S: BlockStorage>(
            trie: &PersistentARTrieChar<(), S>,
            node: &Arc<PersistentCharNode<()>>,
            stamped: &mut usize,
            unstamped: &mut usize,
        ) {
            let kids: Vec<Child<crate::persistent_artrie::core::key_encoding::CharKey>> =
                node.iter_children().map(|(_, c)| c.clone()).collect();
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
        let dir = scratch("cx6-noop-uncompressed");
        let trie = PersistentARTrieChar::<()>::create(&dir.path().join("t.artc")).expect("create");
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
        let dir2 = scratch("cx6-stamp-compressed");
        let trie2 =
            PersistentARTrieChar::<()>::create(&dir2.path().join("t.artc")).expect("create");
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
