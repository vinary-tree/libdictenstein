# F7-S4 crash-safe Owned→Overlay conversion-on-reopen — IMPLEMENTATION LEDGER

Implements the CONVERGED design at `docs/design/f7-owned-to-overlay-rotation.md` (v4,
Round-5 CONVERGED). This ledger tracks the precise execution: functions, signatures,
call-sites, ordering, and the two implementation obligations (OBL-1 fsync, OBL-2 image
checkpoint_lsn). Do NOT commit (owner reviews).

## Surface map (verified by reading)

### WAL layer
- `WalWriter` (sync, `wal/writer.rs`): `is_empty_after_header()==(next_lsn==1)` (370),
  `set_overlay_regime()` gates on `is_empty_after_header` (382), `rotate_to_archive` carries
  high next_lsn + floor + regime, only `flush()`es the fresh header (532-591), `collect_wal_segments`
  (594), `max_lsn_in_segments` (507), `prune_segments_if_needed` (oldest-first, max_segments=10) (629),
  `set_commit_seq_floor`/`commit_seq_floor`.
- `AsyncWalWriter` (`wal/async_writer.rs`): owns its own `next_lsn`/`synced_lsn` atoms;
  `is_empty_after_header()==(next_lsn==1)` (604), `set_overlay_regime` gates on async counter then
  delegates (611), `rotate_to_archive` holds writer lock → inner `rotate_to_archive` (685),
  `set_commit_seq_floor`/`commit_seq_floor` (642/648), `open` seeds next_lsn/synced from
  `max_lsn_in_segments(all)+1` (452). NOTE: async `rotate_to_archive` does NOT re-sync async
  next_lsn from the inner writer; inner carries the high next_lsn → async `is_empty_after_header`
  is FALSE post-rotate (this is the v4 FIX-D misclassification driver — but we gate on
  records_empty_on_disk, not the counter).

### Reconcile
- `reconcile_lww_with_regime(ops, loaded_from_disk, checkpoint_lsn, regime_of)` (recovery.rs:290) —
  per-LSN regime closure, skip `loaded && ckpt>0 && lsn<=ckpt`, Owned KEEP@lsn / Overlay DROP.
- `reconcile_lww(...)` = constant-regime wrapper.
- `rebuild_from_wal_segments_regime_aware` (recovery.rs ~1556-1653): collects segments, has the
  RES-3 prefix-gap guard (min_lsn>1 ⇒ fail loud), but HARDCODES `loaded_from_disk=false, ckpt=0`
  in its `reconcile_lww_with_regime` call (1633). DO NOT reuse for the FIX-B drain (OBL-2).

### F5 / overlay (flip.rs, the shared trait `LockFreeOverlay<K,V,S>`)
- `replay_records_lww_overlay(ops, loaded_from_disk, checkpoint_lsn, rank_regime)` (1240) — reconciles
  via `reconcile_lww` (constant regime) then applies into overlay.
- `install_prebuilt_overlay_root`, `build_overlay_root_from_owned`, `reestablish_overlay_from_owned`
  (KEEP), the 3 folds `reestablish_overlay_membership/_counter/_value` (DELETE — task #8).
- `load_root_immutable`: byte = `f5_loader.rs` (1 arg root_ptr), char = `f5_loader.rs` (2 args
  buffer_manager + root_ptr).

### Reopen arms (4)
- byte mmap `open_inner` (mmap_ctor.rs:355-699): use_f5 gate (532), F5 arm (591-631), legacy arm
  (632-696) incl. the Overlay-flip+dispatch (689-695). Owned currently falls through legacy → stays owned.
- byte io_uring (io_uring_ctor.rs:~150-419): twin; F5 (339-364), legacy+dispatch (410-416).
- char mmap `open_inner` (mmap_ctor.rs:366-601): F5 (509-534), legacy+dispatch (586-597).
  ALSO `open_with_depth` (661-862): F5 (804-...), legacy+dispatch (852-858).
- char io_uring `open_with_io_uring` (io_uring_ctor.rs:130-...): F5 (268-...), legacy+dispatch (319-324).
- recover-family ctors (char mmap_ctor recover_from_archives ~1440, open_with_recovery_config ~991):
  use `reestablish_overlay_from_owned` (KEEP — build-owned-in-memory then convert; NOT the
  reopen-Owned-image path). Untouched by F7.

### Compaction
- byte `compact()` (compaction_impl.rs:331-348): reopen Owned image (`Self::open`) then
  flip_to_overlay + reestablish_overlay_dispatch. RE-POINT to in-memory
  reestablish_overlay_from_owned on the fresh empty-WAL post-rename file (task #7).
- No char compact() with this pattern (verify).

### Trie wal_config
- byte: NO field; constructs `WalConfig::default()` locally (matches `AsyncWalWriter::open_or_create`).
- char: `wal_config` field (mod.rs:440), default `WalConfig::default()`.

## Execution steps (in order)

1. **records_empty_on_disk()** — add to `WalWriter` (file-len==WalHeader::SIZE on the on-disk file)
   AND `AsyncWalWriter` (delegate to inner). Used by converter cheap-vs-rotate + the stamp gate.
2. **WalWriter::rotate_and_restamp_overlay(config)** + **AsyncWalWriter::rotate_and_restamp_overlay** —
   under writer lock: rotate_to_archive (carries high next_lsn+floor+regime) → set_overlay_regime
   (records-empty gate via records_empty_on_disk) → re-assert set_commit_seq_floor(carried) →
   sync_all() fresh Overlay header (OBL-1).
3. **WalManaged seam**: `wal_records_empty_on_disk()` + `wal_rotate_and_restamp_overlay(config)` +
   `wal_collect_segments(config)` + `wal_max_lsn_in_segments(config)` delegators.
4. **Shared FIX-B drain** in flip.rs: `drain_archived_segments_into_overlay(segments, regime_of,
   loaded_from_disk, image_checkpoint_lsn)` — reads each segment, builds per-LSN regime map +
   records, RES-3 guard using image_checkpoint_lsn (FIX E), reconcile_lww_with_regime with the
   REAL (loaded_from_disk, image_checkpoint_lsn) (OBL-2), apply via apply_recovered_operation_overlay.
   Return Result<usize> (Err on prefix gap). Plus `convert_owned_to_overlay_on_reopen` default that
   the ctors call for rank==Owned.
5. **Wire FIX B + FIX C into all 4 Overlay arms + converter**; FIX C = base from full segments.
6. **Compaction re-point** (task #7).
7. **Deletions** (task #8): legacy Owned/Overlay-flip dispatch arms, reestablish_overlay_dispatch
   (byte+char), the 3 folds. Reconcile unsafe inventory (folds have none → no change needed, verify).

## Reconciliation: open_with_legacy_loader stays the ORACLE (both correspondence tests)
- EXISTING `persistent_f5_both_loaders_correspondence.rs` drives `open_with_legacy_loader`
  on an OVERLAY fixture expecting flip+reestablish (legacy path). NEW
  `persistent_owned_to_overlay_correspondence.rs` drives it on an OWNED fixture as the
  pre-F7 owned-reopen oracle (owned tree, no flip).
- Resolution (force_f5=false legacy loader):
  - Overlay branch → `flip_to_overlay()` + **`reestablish_overlay_from_owned()`** (the KEPT
    structural converter, == dispatch, strictly-more-correct) — NOT reestablish_overlay_dispatch.
  - Owned branch → owned-loader + owned `replay_records_lww`, STAYS owned (the oracle).
- Production (force_f5=true: `open`/`open_with_f5_loader`):
  - Overlay eligible → F5 build + FIX-B archive-aware drain (reconcile_and_drain_overlay).
  - Owned eligible → convert_owned_to_overlay_on_reopen.
  - ineligible V → legacy owned-loader stay-owned (cannot overlay).
- OBL-2 image_checkpoint_lsn = the recovery `checkpoint_lsn` captured PRE-rotate (it is read
  from the Owned active WAL Checkpoint record BEFORE the converter rotates ⇒ == image redo
  frontier). The converter receives this captured value; it never re-reads post-rotate.

## Progress
- [x] Step 1 records_empty_on_disk (WalWriter + AsyncWalWriter)
- [x] Step 2 rotate_and_restamp_overlay (WalWriter + AsyncWalWriter; OBL-1 sync_all)
- [x] Step 3 WalManaged seam (wal_records_empty_on_disk / wal_rotate_and_restamp_overlay / wal_collect_segments)
- [x] Step 4 shared FIX-B drain (drain_segments_into_overlay + reconcile_and_drain_overlay) + converter (convert_owned_to_overlay_on_reopen) + load_root_immutable_seam (byte+char)
- [x] Step 5 wire 4 arms (byte/char × mmap/io_uring) + FIX C (watermark base = max_lsn_in_segments)
- [x] Step 6 compaction re-point (byte compact() — Self::open auto-converts via records-empty cheap path)
- [x] Step 7 deletions (reestablish_overlay_dispatch byte+char; 3 folds + value_as_counter in flip.rs;
      char reestablish_overlay_membership_after_recovery + reestablish_overlay_after_recovery;
      replay_records_lww_overlay [superseded by drain]; all doc links fixed; 5 tests re-pointed to
      reestablish_overlay_from_owned; legacy-loader oracle Overlay branch uses reestablish_overlay_from_owned)
- [x] Build clean: persistent-artrie + io-uring-backend, 0 errors; smoke tests (6 re-pointed) PASS
- [x] FIX-D belt-and-suspenders: prune_segments_if_needed gains a `checkpoint_lsn` param +
      first-LSN sort; NEVER prunes un-subsumed segments (first_lsn > checkpoint_lsn). The
      foreground rotate_to_archive passes the carried OLD-header checkpoint_lsn; the background
      SegmentSyncManager sync-rotation prune passes Lsn::MAX (pre-F7 count/size behavior, orthogonal).
- [x] FIX-A widening (found by the crash proptest): the cheap-path stamp uses a NEW
      records-empty-gated stamp (WalWriter/Async set_overlay_regime_records_empty +
      WalManaged::wal_stamp_overlay_regime_records_empty), NOT the next_lsn==1-gated
      wal_stamp_overlay_regime — a post-crash-after-rotate (or post-checkpoint set_min_lsn)
      records-empty active carries a HIGH next_lsn that the old gate wrongly rejected.
- [x] Crash proptest (tests/persistent_owned_to_overlay_conversion_crash.rs): 11 PASS — full
      fail-point sweep (byte+char × ()/counter/String × 5 points), the 13× AfterRotateBeforeStamp
      loop (segment count never grows beyond baseline+1 = FIX D), BatchIncrement-once across
      checkpoint+crash (FIX C/OBL-2), RES-3 fail-loud on a pruned-prefix gap (FIX E).
      A process-global failpoint atomic ⇒ tests serialize through a Mutex.
- [x] Correspondence test (tests/persistent_owned_to_overlay_correspondence.rs): 8 PASS —
      converted-reopen == open_with_legacy_loader owned oracle (byte+char × ()/counter/String/Small),
      incl. "" + term-only members + counters > i64::MAX. `()` value normalized (vacuous; the
      owned image drops the degenerate () value, the overlay synthesizes it — observationally equal).
- [x] Full nextest + formal gate + fmt + sibling check — ALL GREEN

## Additional correctness fixes uncovered by the existing test suite (F7 routes eligible-V
## Owned files through the overlay, exposing latent F5/overlay gaps — all fixed, mirroring
## the proven owned applier):
- **Transaction filtering in the drain** (`drain_segments_into_overlay`): Owned segments may
  carry document-transaction records; records inside an incomplete/aborted tx must be DROPPED
  (the legacy owned replay tx-filters; the raw reconcile does not). The drain now resolves
  transactions per Owned segment via `RecoveryManager` and keeps only surviving LSNs. RES-3
  uses the PHYSICAL min lsn (pre-tx-filter) so a tx-dropped record is not a false gap.
- **term-only members in `iter_with_values`** (byte `public_iter.rs`): the overlay arm used
  the value-CARRYING enumerator which drops value-less finals; switched to enumerate-then-
  lookup (overlay-routed `get_value_bytes`) so a term-only member yields `(term, None)`.
- **absolute Increment SET vs ADD** (`apply_recovered_operation_overlay`): a single
  `WalRecord::Increment` carries the ABSOLUTE count (owned writes log the result, incl.
  decrements 5→0); replay must OVERWRITE, not accumulate. Now decodes the i64 into `V`
  DIRECTLY via `counter_codec` (the SAME path the owned applier uses — correct for ANY
  Counter V incl. `i64`, not just the `u64` overlay monomorph) and publishes a value SET.
- **corrupt-image fallback**: a corrupt root descriptor must fall back to an EMPTY image +
  WAL recovery (legacy parity), not propagate. byte ctors pass `effective_root_ptr = 0`
  when the eager pre-load failed; char `load_root_immutable` returns `(count, image_loaded)`
  and falls back internally. The seam returns `image_loaded` so the drain uses
  `loaded_from_disk=false` + frontier 0 (so it does not skip WAL records the absent image
  fails to cover). RES-3 loud guard scoped to the NO-IMAGE case (the descriptor does not
  store the redo frontier — OBL-2's ideal source — so a high min_lsn with an image present is
  treated as normal; FIX-D's prune exemption is the primary defense).

## Existing tests adjusted to the F7 contract (eligible-V Owned files now CONVERT on `open`):
- `m4b_old_owned_file_stays_owned_on_reopen` / `s5_12_old_owned_file_stays_owned_on_reopen`:
  now assert the file CONVERTS to Overlay (data intact); the stay-Owned oracle is the
  dedicated correspondence suite.
- `test_wal_archive_pruning_by_count`: now checkpoints so segments are subsumed (FIX-D
  exempts un-subsumed segments from pruning).
- 5 lazy-owned-corruption tests (`char_lazy_load_errors`, `char_lazy_insert_error`,
  `char_lazy_value_insert_and_remove_errors`, `char_lazy_traversal_failure`,
  `char_remove_prefix_lazy_collection_error`) + `char_remove_prefix_batched`: reopen via
  `open_with_legacy_loader` (the RETAINED owned-lazy / owned-WAL-shape path), since
  production `open` now eagerly converts (no lazy owned fault-on-access; overlay WAL shape).

## FINAL STATUS: full nextest 2699 passed / 0 failed / 3 skipped; `cargo check`
## (persistent-artrie / --no-default-features / +io-uring-backend) 0 errors; unsafe-inventory
## gate "match"; verify-formal-correspondence.sh exit 0; cargo fmt clean; sibling
## liblevenshtein-rust `cargo check` exit 0. NO new `unsafe`. Tree left dirty (owner reviews).

## Key implementation decisions (for the report)
- OBL-1: `WalWriter::rotate_and_restamp_overlay` fsyncs (`sync_all`) the fresh Overlay header after
  the stamp (the S2 durable commit point) + a defensive `set_commit_seq_floor` re-assert (also fsyncs).
- OBL-2: the converter + the Overlay F5 arm thread the RECOVERY `checkpoint_lsn` (read PRE-rotate from
  the active WAL Checkpoint record = the dense-image redo frontier) as `image_checkpoint_lsn` into
  `reconcile_and_drain_overlay` → `drain_segments_into_overlay` → `reconcile_lww_with_regime`
  (loaded_from_disk, image_checkpoint_lsn) — NOT the post-rotate active record, NOT
  rebuild_from_wal_segments_regime_aware (which hardcodes false/0).
- FIX D: converter cheap-vs-rotate keys on `records_empty_on_disk` (file len), not is_empty_after_header
  (next_lsn==1) → a post-crash high-next_lsn header-only active takes the CHEAP (no-rotate) path,
  so the crash-loop never mints empty segments.
- FIX E: `drain_segments_into_overlay` RES-3 prefix-gap guard (min_lsn > image_frontier+1 ⇒
  PersistentARTrieError::corrupted), using the IMAGE frontier (OBL-2).
- Gating: production (`open`/`open_with_f5_loader`, force_f5=true OR const-keyed io_uring/open_with_depth):
  Owned-eligible → converter; Overlay-eligible → F5 + archive-aware drain; ineligible → owned stay.
  `open_with_legacy_loader` (force_f5=false): owned-loader stay-owned (Owned) / flip +
  reestablish_overlay_from_owned (Overlay) = the pre-F7 oracle.
