# Slice 3 (F5 + F7) — RE-VALIDATED against current code (2026-06-08, post-eviction a46d9c1)

Supersedes slice3-f5-f7-execution-plan.md for the done-state + ordering. The plan was written @ f0af377;
8 commits since (incl. the rotation e909eab + F2-default-on 8b5023f + eviction a46d9c1) overtook ~40% of it.

## DONE (do NOT redo — verified against code)
- **S1 F5-land:** f5_loader.rs (byte+char) load_root_immutable + build_overlay_root_from_owned +
  replay_records_lww_overlay. **S2 F5-prove:** tests/persistent_f5_both_loaders_correspondence.rs (17/17).
  **S3 F5-switch:** `const USE_F5_REOPEN_LOADER = true` (flip.rs:800), all 4 ctors gated.
- **S4 reestablish deletion (MOSTLY):** reestablish_overlay_dispatch + the 3 folds DELETED (tombstones);
  recovery ctors → reestablish_overlay_from_owned (831ab51). Owned→Overlay rotation-on-reopen DONE (e909eab,
  crash proptest 11/11). F5's data-loss-critical WAL path = `drain_segments_into_overlay`/`reconcile_and_drain_overlay`
  (archive-aware, per-segment regime) — `replay_records_lww_overlay` is SUPERSEDED-by-drain (dead).
- **overlay_eligible_v()→true** (8b5023f, was listed under S9) ⇒ NO ineligible-V owned production path left.
- **Eviction (a46d9c1):** route_overlay-gated swap with owned `else` arms RETAINED (S9 must delete them).

## REMAINING (the actual Slice-3 work)
- **S4-residual (S):** delete production legacy reopen fall-through arms; KEEP open_with_legacy_loader (oracle).
  clear_owned STAYS (live in the KEPT reestablish_overlay_from_owned converter — NOT an S4 delete).
- **S5 (M, DATA-LOSS-CRITICAL → dedicated red-team):** delete owned WAL mutators (insert_impl/remove_impl) +
  recovery appliers (byte apply_recovered_operation_no_wal/recompute_recovered_increment; char
  apply_core_recovered_operation_no_wal Increment arm) + replay_records_lww (both). MUST prove these are
  UNREACHABLE post-rotation (recovery now drains via the overlay) — a surviving caller = silent recovery loss.
  KEEP byte *_impl_no_wal (compaction staging) + counter_codec (overlay callers). Reconcile UNSAFE_INVENTORY
  rows 23-24 (char-mutation-core-traversal/-unique-borrow) IN THE SAME COMMIT (set-equality gate); verify each.
- **S7 (S):** delete owned checkpoint `else` arm in checkpoint_route_split (checkpoint.rs:142-155) + the RES-4
  assert (:148). KEEP capture_owned_snapshot + publish_owned_and_reclaim (byte staging). AFTER the staging re-point.
- **MERGED S9+S6+S8-collapse+S10 (L–XL, DATA-LOSS-CRITICAL → dedicated red-team) — the entangled flip:**
  - S9: route_overlay()→`const true` (flip.rs:348; byte overlay_write_mode.rs:870; char :624). Compiler then
    flags every dead owned_X arm. S6 = delete those (owned_try_contains/owned_get/owned_try_get + !route_overlay arms).
  - DELETE the eviction owned `else` arms (the user's flagged deltas): byte shared_trait_impl.rs async :291-317 +
    force_eviction :399-401; char mod.rs start_char :2143-2145 + force_eviction :2233-2235 + the unused quiescence
    locals. → evict_char_nodes / evict_node_at_path / find_parent_mut become dead → delete. (Vocab evict_node_at_path
    mod.rs:869 = OUT of scope, untouched.)
  - S8-collapse: collapse the OR RwLock on `root` (only remaining access = f5_loader get_mut scratch + the converter
    seam readers + byte staging + capture_owned_snapshot — each → &mut/scratch-local or safe non-locked). The
    **root FIELD STAYS** (see SCOPE below).
  - S10: replace compaction `create`+`kill_switch_to_owned` (compaction_impl.rs:209) with a CONSTRUCTION-TIME
    owned-only staging builder (overlay-not-installed); delete kill_switch_to_owned + OverlayWriteMode enum + the
    field + route_overlay split. ENTANGLEMENT: route_overlay→const-true BREAKS byte staging (kill-switched →
    lockfree_root().is_none() → checkpoint would route empty overlay = TOTAL IMAGE LOSS) → S9 + S10 MUST land
    TOGETHER (or S9 keeps a staging_owned construction flag route_overlay consults). The plan's S9-then-S10 order
    does NOT survive this dependency.

## SCOPE BOUNDARY (honest — surface to owner)
S8 "delete owned `root` field outright / no residual" is NOT achievable in Slice 3: F5 transient scratch + the
KEPT converter seam readers + byte compaction staging + the legacy oracle all read self.root. Literal-zero-owned
= **C-opt-2** (a path-compressing overlay→dense serializer + an F5 that reads multi-unit-prefix overlay nodes) —
a separate multi-week DATA-LOSS-CRITICAL format effort, EXPLICITLY out of Slice-3 scope. Achievable Slice-3
end-state: owned RUNTIME deleted (mutators, read arms, checkpoint arm, route-split, kill-switch, eviction owned
arms) + OR-RwLock collapsed; the owned `root` FIELD + NODE types + serialize/deserialize KEPT (the on-disk
format + F5 scratch + byte dense-staging backend). C-opt-1 compaction staging is BYTE-ONLY (char has no compact()).

## VERIFICATION (per commit): full suite (default = feature-on now) + `--no-default-features` (feature-off) +
doctests + verify-formal-correspondence.sh exit 0 + unsafe-inventory set-equality + fmt + cross-repo READ-ONLY
build-check (liblevenshtein-rust). #41 checkpoint_lsn=committed-watermark capture ordering UNTOUCHED (state per commit).

## RED-TEAM FOCI (the 2 data-loss-critical sub-steps; the rest is compiler-driven mechanical):
1. **S5 recovery-applier reachability** — prove apply_recovered_operation_no_wal/recompute_recovered_increment/
   replay_records_lww have NO surviving (non-deleted, non-overlay-routed) caller post-rotation.
2. **Merged S9+S10** — (a) the compaction-staging construction-time owned distinction (R2 total-image-loss: does
   the staging trie still capture OWNED not empty-overlay with OverlayWriteMode gone?); (b) the OR-lock collapse
   soundness (every &self path needing self.root.read() exclusion must be deleted / &mut-only / safe non-locked).
