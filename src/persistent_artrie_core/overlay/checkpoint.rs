//! `OverlayCheckpoint<K, V, S>` — the SHARED GENERIC checkpoint skeleton
//! (Template Method), extracted from the char variant so byte can reuse the
//! data-loss-critical "capture the LIVE representation" logic rather than
//! copy-paste it (`docs/design/overlay-durable-architecture.md` §"The trait
//! family", trait 3).
//!
//! # What this trait is
//!
//! A **subtrait of [`LockFreeOverlay`]** providing the checkpoint skeleton as a
//! default method. The INVARIANT skeleton — capture the IMMUTABLE OVERLAY snapshot,
//! then publish via the watermark-bounded retaining publisher (eviction-aware when a
//! coordinator is installed) — lives here ONCE; the per-variant steps (the
//! [`CheckpointSnapshot`](Self::CheckpointSnapshot) capture + serialize, which are
//! GENUINELY per-variant because they touch the char/byte arena on-disk format) are
//! deferred to abstract SEAM hooks.
//!
//! # Why this is data-loss-critical (RES-4, now structurally resolved)
//!
//! The live data is in the immutable overlay. The checkpoint MUST capture from the
//! overlay and publish via the watermark-bounded retaining publisher (which records
//! `checkpoint_lsn = committed watermark` — the only safe reclaim bound under
//! out-of-order lock-free commit, GAP_LEDGER #41 — and raises the commit_seq floor).
//! The historical RES-4 footgun (a route-split that could silently checkpoint the
//! EMPTY owned tree while the overlay is the live write target, losing every term on
//! the next reopen) is **gone**: L3.3 deleted the owned tree, so there is no owned
//! capture arm to mis-select. `route_overlay()` is universally true (every ctor
//! installs the overlay); the skeleton's `debug_assert!` documents that invariant.
//!
//! # Seam-boundary rationale (the "sensible > maximal" rule, design §0)
//!
//! The skeleton + the eviction branch are the GENERIC part (they read only
//! `route_overlay()` + `has_eviction_coordinator()`). The
//! [`CheckpointSnapshot`](Self::CheckpointSnapshot) type, its capture (walking the
//! overlay root into freshly-allocated arena slots) and its serialize/publish are
//! GENUINELY per-variant (char arena format ≠ byte arena format) — so they stay
//! SEAMS. The committed-watermark/retention LOGIC the publishers use is itself shared
//! (via [`crate::persistent_artrie_core::committed_watermark::CommittedWatermark`]);
//! only the on-disk serialize is variant code.
//!
//! # Note on the second (non-blocking) capture site
//!
//! The variant's non-blocking `Shared*::checkpoint` is a SEPARATE capture site that
//! inlines the SAME overlay capture + publish. It calls the SAME seam methods
//! directly, so this trait does not subsume it — it only provides the inherent
//! `&self` `checkpoint()` skeleton as a default.

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
    fn publish_overlay_snapshot_retaining(&self, snapshot: &Self::CheckpointSnapshot)
        -> Result<()>;

    /// **Overlay arm — publish (eviction-on).** As [`Self::publish_overlay_snapshot_retaining`]
    /// PLUS eviction-registry publication (publish-after-verify). Consumes the snapshot
    /// (the registry moves into the coordinator). Char
    /// `publish_immutable_snapshot_retaining_wal_with_eviction`.
    fn publish_overlay_snapshot_retaining_with_eviction(
        &self,
        snapshot: Self::CheckpointSnapshot,
    ) -> Result<()>;

    // ========================================================================
    // DEFAULT-PROVIDED GENERIC METHOD — the data-loss-critical checkpoint skeleton.
    // ========================================================================

    /// **Checkpoint skeleton (L3.3 — overlay-only).** Capture the IMMUTABLE OVERLAY
    /// snapshot, then publish via the watermark-bounded retaining publisher
    /// (eviction-aware when a coordinator is installed).
    ///
    /// Since L3.3 deleted the owned tree, `route_overlay()` is universally true (every
    /// constructor installs the overlay for all `V`), so there is no owned-capture arm
    /// left — the RES-4 total-loss footgun it guarded
    /// (silently checkpointing the empty owned tree while the overlay is the live write
    /// target) is structurally gone. The `debug_assert!` documents the invariant.
    ///
    /// **F4:** `&self` (all capture/publish seams are already `&self`). The
    /// concurrent-checkpoint serialization (`checkpoint_lock`, CK) is taken by the
    /// `Shared*` trait `checkpoint()` wrapper.
    fn checkpoint_route_split(&self) -> Result<()> {
        debug_assert!(
            self.route_overlay(),
            "L3.3: checkpoint requires an installed lock-free overlay (the owned tree is gone)"
        );
        let snapshot = self.capture_overlay_snapshot()?;
        if self.has_eviction_coordinator() {
            self.publish_overlay_snapshot_retaining_with_eviction(snapshot)
        } else {
            self.publish_overlay_snapshot_retaining(&snapshot)
        }
    }
}
