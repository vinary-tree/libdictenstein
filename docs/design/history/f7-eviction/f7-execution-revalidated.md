# F7 owned-tree deletion — re-validated execution plan (post-counter-migration, @ 20e8e27)

Re-validation of `slice3-f5-f7-execution-plan.md` against the CURRENT code (after the
counter-u64 migration rewrote the same files). Source of truth for execution.

## Headline correction
**S3 IS DONE.** `const USE_F5_REOPEN_LOADER = true` (flip.rs:801; one literal, both
variants inherit). Production Overlay-regime reopen already uses the F5 dense→overlay
loader. The libgrammstein "false" was a STALE doc-comment / worktree copy. **F5
(S1+S2+S3) fully landed; only S4–S10 remain.**

## Counter-interaction audit (clean — no conflict, no loss)
Each counter op is `if route_overlay() { OVERLAY funnel; return } <OWNED funnel>`; the
migration touched BOTH arms but they separate cleanly:
- KEEP (overlay, retained): `counter_codec` (whole module), `flip.rs::counter_value_from_i64`
  (:1159, the leaf-decode fix), `replay_records_lww_overlay`/`apply_recovered_operation_overlay`,
  the overlay increment funnels (byte atomic_ops:66/289/443, document_tx:227), the `<u64>`
  monomorph (lockfree_cas:1167, LOCKFREE_COUNTER_MAX=u64::MAX).
- DELETE (owned, removed by F7): byte mutation_core apply_recovered_operation_no_wal Increment
  arm + recompute_recovered_increment; char value_from_recovered_i64 + apply_core_recovered_
  operation_no_wal Increment arm; the owned arms of atomic_ops/document_tx.
- **Do NOT over-delete `counter_codec`** when removing owned appliers — it has overlay callers.

## Risks
- **R1 (HIGH, scope):** char recovery-family ctors `mmap_ctor.rs:1244` (recover_from_wal) +
  `:1489` (recover_from_archives) are NOT F5-migrated — owned-rebuild → reestablish. To fully
  delete the reestablish machinery (no residual, per /goal) they MUST be migrated to F5 first
  (build overlay from the rebuilt-owned, like the main reopen). [option a = full; option b =
  keep them as a documented residual = smaller but leaves an owned path = /goal "incompletion".]
- **R2 (data-loss):** S7's owned-checkpoint-arm deletion intersects compaction-staging
  `checkpoint()`. KEEP `capture_owned_snapshot`/`publish_owned_and_reclaim` for staging; sequence
  S7 AFTER S9 (or re-point staging to call capture directly). Deleting the owned capture before
  re-pointing → compaction silently checkpoints an empty overlay = total image loss.
- **R3 (KEEP/DELETE boundary):** the `owned_*` SEAM readers (flip.rs owned_first_units:156,
  owned_units_under:165, owned_units_with_values_under:173, owned_has_empty_term_value:183 +
  impls) feed F5's build_overlay_root_from_owned — KEEP. The runtime fallbacks (char query_api
  owned_try_contains:54/owned_get:116/owned_try_get:169) are the S6 deletes. Same prefix,
  opposite fate.
- **R4:** keep `open_with_legacy_loader` as the both-loaders test oracle after deletion.
- **R6/unsafe:** reconcile UNSAFE_INVENTORY.tsv + UNSAFE_CONTRACTS.tsv in the SAME commit as
  each deletion that removes an `unsafe` (char mutation_core:23-24 owned traversal, IF inside
  deleted fns; char persist.rs:26 serialize = KEEP; byte node16.rs:3 node type = KEEP).
- **R7 (#41):** no S4–S10 step touches the checkpoint_lsn=committed-watermark capture ordering.
- **R8:** cross-repo build-check stays READ-ONLY (re-run after S9/S10 — they touch pub surface).

## Recommended sequence (each independently green + committable)
[R1(a) full] migrate recovery-family ctors to F5 → S4 (delete legacy reopen arms + reestablish
folds + clear_owned + owned reopen path + D1 gate) → S5 (delete owned mutators + replay_records_lww
+ owned appliers; KEEP minimal staging insert + counter_codec) → S6 (delete owned read fallbacks;
KEEP seam readers) → S9a–d (route_overlay()→const true, delete owned arms per subsystem: doc-tx,
merge, read/write core, ctors; 267 occ / 43 files) → S7 (delete owned checkpoint arm + C2 assert;
staging re-pointed to capture_owned_snapshot) → S8 (delete owned root field + OR lock; staging gets
a distinct builder) → **S10 (FINAL, IRREVERSIBLE): delete kill_switch + OverlayWriteMode + field;
compaction staging → construction-time owned ctor; the only non-test kill-switch call-site is
compaction_impl.rs:209; + the high-concurrency real-disk soak (#41 witness)).**

Per-commit gate: nextest feature-on AND feature-off; doctests; verify-formal-correspondence.sh
exit 0; verify-unsafe-boundary-inventory.sh (set-equality); fmt --check; cross-repo READ-ONLY
build-check; state the #41 watermark-ordering guard.

# === S4 BLOCKER + owner decision (A): Owned->Overlay rotation-on-reopen ===
S4 (delete owned reopen, no residual) requires Owned-regime files (compaction images +
legacy/kill-switched) to reopen INTO the overlay. F5 cannot convert a NON-EMPTY Owned
WAL: `install_prebuilt_overlay_root`'s V-2 check refuses a non-Overlay WAL, and
`WalWriter::set_overlay_regime()` formally REJECTS in-place stamping of a non-empty Owned
WAL (orphan-keep vs orphan-drop correctness; torn-magic). In-tree counterexample: byte
`compact()` reopens an Owned dense image (compaction_impl.rs:331) then re-flips.
OWNER DECISION (A): build a crash-safe **Owned->Overlay WAL-rotation-on-reopen** primitive
(archive Owned tail -> recreate Overlay-stamped active -> F5-build from image -> replay the
archived Owned tail INTO the overlay with Owned orphan-KEEP semantics), carrying the
committed-watermark / commit_seq_floor across the rotation. Data-loss-critical: full
Plan -> red-team-to-convergence -> implement -> verify BEFORE the S4 deletions. Then S4
deletes reestablish_dispatch + folds + clear_owned (re-pointed) + the legacy reopen arm.
Also re-point byte compaction (compaction_impl.rs:341-348) onto the rotation/converter.
