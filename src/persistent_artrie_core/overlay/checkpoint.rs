//! `OverlayCheckpoint<K, V, S>` — the SHARED GENERIC checkpoint **route-split**
//! skeleton (Template Method), extracted from the char variant so byte can reuse
//! the data-loss-critical "capture the LIVE representation" decision rather than
//! copy-paste it (`docs/design/overlay-durable-architecture.md` §"The trait
//! family", trait 3).
//!
//! # What this trait is
//!
//! A **subtrait of [`LockFreeOverlay`]** providing the checkpoint route-split as a
//! default method. The INVARIANT skeleton — the `if route_overlay() { capture the
//! IMMUTABLE OVERLAY + publish retaining } else { assert!(!route_overlay()); capture
//! the OWNED tree + publish-and-reclaim }` decision, the RES-4 total-loss guard
//! assert, and the eviction-coordinator branch — lives here ONCE; the per-variant
//! steps (the [`CheckpointSnapshot`](Self::CheckpointSnapshot) capture + serialize,
//! which are GENUINELY per-variant because they touch the char/byte arena on-disk
//! format) are deferred to abstract SEAM hooks.
//!
//! # Why the route-split is data-loss-critical (RES-4)
//!
//! Under the overlay write mode the OWNED tree is EMPTY — the live data is in the
//! immutable overlay. Capturing the owned tree here would checkpoint NOTHING and
//! lose every term on the next reopen. So under `route_overlay()` the checkpoint
//! MUST capture from the overlay and publish via the watermark-bounded retaining
//! publisher (which records `checkpoint_lsn = committed watermark` — the only safe
//! reclaim bound under out-of-order lock-free commit, GAP_LEDGER #41 — and raises
//! the commit_seq floor). The `else` arm asserts `!route_overlay()` to tripwire any
//! future caller that reaches the owned capture while the overlay is the LIVE write
//! target (the legitimate kill-switch owned checkpoint, where an overlay root may
//! exist under `OwnedTree` mode, is the `false`-predicate case the assert permits).
//!
//! # Seam-boundary rationale (the "sensible > maximal" rule, design §0)
//!
//! The route-split DECISION + the RES-4 assert + the eviction branch are the GENERIC
//! skeleton (they read only `route_overlay()` + `has_eviction_coordinator()`). The
//! [`CheckpointSnapshot`](Self::CheckpointSnapshot) type, its capture (walking the
//! overlay/owned root into freshly-allocated arena slots) and its serialize/publish
//! are GENUINELY per-variant (char arena format ≠ byte arena format) — so they stay
//! SEAMS. The committed-watermark/retention LOGIC the publishers use is itself shared
//! (via [`crate::persistent_artrie_core::committed_watermark::CommittedWatermark`]);
//! only the on-disk serialize is variant code.
//!
//! # Note on the second (non-blocking) capture site
//!
//! The variant's non-blocking `Shared*::checkpoint` is a SEPARATE capture site that
//! inlines the SAME route-split with a write→read guard downgrade (the lock-free
//! overlay capture needs no write guard; the owned capture does). It calls the SAME
//! seam methods directly, so this trait does not subsume it — it only provides the
//! inherent `&mut self` `checkpoint()` route-split as a default. The seams stay
//! callable individually for the downgrade path.

use crate::persistent_artrie_core::error::Result;
use crate::persistent_artrie_core::key_encoding::KeyEncoding;
use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
use crate::value::DictionaryValue;

/// The SHARED GENERIC checkpoint route-split surface (design trait 3).
///
/// `K`/`V`/`S` as in [`LockFreeOverlay`]. [`Self::CheckpointSnapshot`] is the
/// per-variant frozen on-disk-image snapshot (char vs byte arena format) — an
/// associated type, since the route-split is generic but the captured image is not.
pub(crate) trait OverlayCheckpoint<K: KeyEncoding, V: DictionaryValue, S>:
    LockFreeOverlay<K, V, S>
{
    /// The variant's frozen, self-consistent checkpoint snapshot (serialized tree →
    /// fresh arena slots + descriptor). GENUINELY per-variant (char arena format ≠
    /// byte arena format), hence a seam type, not a shared struct.
    type CheckpointSnapshot;

    // ========================================================================
    // REQUIRED SEAM (variant provides) — the capture + publish halves of both
    // arms (per-variant on-disk format), plus the eviction-coordinator probe.
    // ========================================================================

    /// `true` iff an eviction coordinator is installed (selects the eviction-aware
    /// retaining publisher in the overlay arm).
    fn has_eviction_coordinator(&self) -> bool;

    /// **Overlay arm — capture.** Capture a frozen snapshot from the IMMUTABLE
    /// lock-free overlay (walk the overlay root → fresh arena slots), reading the
    /// committed watermark `Acquire` BEFORE the root load (the capture-ordering
    /// invariant — the snapshot ⊆ committed-durable-prefix subset direction). Char
    /// `capture_snapshot_immutable`.
    fn capture_overlay_snapshot(&self) -> Result<Self::CheckpointSnapshot>;

    /// **Overlay arm — publish (no eviction).** Publish the overlay snapshot's durable
    /// on-disk image + record `checkpoint_lsn = committed watermark` while RETAINING
    /// the WAL (non-double-counting via the `Checkpoint` record; raises the commit_seq
    /// floor). Char `publish_immutable_snapshot_retaining_wal`. Borrows the snapshot.
    fn publish_overlay_snapshot_retaining(&self, snapshot: &Self::CheckpointSnapshot) -> Result<()>;

    /// **Overlay arm — publish (eviction-on).** As [`Self::publish_overlay_snapshot_retaining`]
    /// PLUS eviction-registry publication (publish-after-verify). Consumes the snapshot
    /// (the registry moves into the coordinator). Char
    /// `publish_immutable_snapshot_retaining_wal_with_eviction`.
    fn publish_overlay_snapshot_retaining_with_eviction(
        &self,
        snapshot: Self::CheckpointSnapshot,
    ) -> Result<()>;

    /// **Owned arm — capture.** Capture a frozen snapshot from the OWNED tree
    /// (reclaims by the `next_lsn` convention; writers excluded by the write lock).
    /// Char `capture_snapshot`.
    fn capture_owned_snapshot(&self) -> Result<Self::CheckpointSnapshot>;

    /// **Owned arm — publish + reclaim.** Publish the owned snapshot durably, verify,
    /// publish the eviction registry, then WAL `Checkpoint` append + sync +
    /// archive/truncate (reclaim by `next_lsn`). Consumes the snapshot. Char
    /// `publish_durable_and_reclaim`.
    fn publish_owned_and_reclaim(&self, snapshot: Self::CheckpointSnapshot) -> Result<()>;

    // ========================================================================
    // DEFAULT-PROVIDED GENERIC METHOD — the data-loss-critical route-split.
    // ========================================================================

    /// **Checkpoint route-split (RES-4 total-loss guard).** The `&mut self` (owned/
    /// blocking) checkpoint skeleton:
    ///
    /// - Under `route_overlay()`: capture the IMMUTABLE OVERLAY (the live data — the
    ///   owned tree is empty here) and publish via the watermark-bounded retaining
    ///   publisher (eviction-aware when a coordinator is installed).
    /// - Otherwise: `assert!(!route_overlay())` (tripwire the owned capture under an
    ///   active overlay write mode — the RES-4 footgun), then capture the OWNED tree
    ///   and publish-and-reclaim.
    ///
    /// INERT pre-flip: `route_overlay()` is false until the production ctors flip, so
    /// the owned arm is byte-for-byte the prior checkpoint body. The variant's public
    /// `checkpoint()` is a thin wrapper calling this default.
    fn checkpoint_route_split(&mut self) -> Result<()> {
        if self.route_overlay() {
            let snapshot = self.capture_overlay_snapshot()?;
            if self.has_eviction_coordinator() {
                self.publish_overlay_snapshot_retaining_with_eviction(snapshot)
            } else {
                self.publish_overlay_snapshot_retaining(&snapshot)
            }
        } else {
            // Never silently checkpoint the owned tree while a lock-free overlay is
            // the LIVE write target (the RES-4 footgun). `!route_overlay()` is the
            // branch predicate — the assert documents + tripwires it (NOT
            // `lockfree_root().is_none()`, which would panic the legitimate kill-switch
            // owned checkpoint, where an overlay root may exist under `OwnedTree` mode).
            assert!(
                !self.route_overlay(),
                "owned checkpoint reached under an active lock-free overlay write mode \
                 (RES-4 total-loss guard)"
            );
            let snapshot = self.capture_owned_snapshot()?;
            self.publish_owned_and_reclaim(snapshot)
        }
    }
}
